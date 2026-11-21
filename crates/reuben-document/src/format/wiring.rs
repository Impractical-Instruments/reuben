//! Wire compatibility without wiring: may this source feed that target, and if not, why — asked
//! against a document the loader has resolved up to pass 2, through the loader's own endpoint
//! resolution and its own type rule.
//!
//! Only the type rule is answered. A wire that would close a cycle, or anything else a whole-
//! document validation rejects for reasons other than port types, is the edit's to report.

use std::collections::BTreeMap;
use std::fmt;

use super::{
    check_output_feed, parse_wire, resolve_input, resolve_output, wire_type_compatible, Found,
    InputValue, LoadCtx, LoadError, LoadErrors, NormalizedDoc, Registry, Resolved,
};
use crate::resources::ResourceResolver;
use reuben_core::descriptor::PortType;
use reuben_core::graph::NodeKey;

/// The end a wire leaves from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireSource {
    /// A document node's output port — on a `subpatch` node, one of its child's interface
    /// output names.
    Output { node: String, port: String },
    /// One of this document's own `interface.inputs` pipes, by name.
    InterfaceInput(String),
}

impl WireSource {
    /// The wire-ref an edit writes for this source: `/node.port`, or an input pipe's minted
    /// address, which is `/` plus its name.
    pub fn wire_ref(&self) -> String {
        match self {
            WireSource::Output { node, port } => format!("{node}.{port}"),
            WireSource::InterfaceInput(name) => format!("/{name}"),
        }
    }
}

/// The end a wire lands on.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum WireTarget {
    /// A document node's input port — on a `subpatch` node, one of its child's interface input
    /// names.
    Input { node: String, port: String },
    /// One of this document's own `interface.outputs` pipes, by name: the wire is its `from`.
    InterfaceOutput(String),
}

impl fmt::Display for WireTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WireTarget::Input { node, port } => write!(f, "{node}.{port}"),
            WireTarget::InterfaceOutput(name) => write!(f, "interface output `{name}`"),
        }
    }
}

/// A document's wire endpoints, resolved once: every node minted, every `subpatch` child loaded
/// and its face synthesized. Each question after that is map lookups and the type rule — no
/// document read and no child load.
pub struct WirePorts<'d> {
    doc: &'d NormalizedDoc,
    resolved: Resolved,
}

impl<'d> WirePorts<'d> {
    /// Resolve `doc`'s endpoints the way a load does — `resolver` loads its `subpatch` children.
    /// Fails with the first error a load finds before it reaches the wires.
    pub fn resolve(
        doc: &'d NormalizedDoc,
        registry: &Registry,
        resolver: &dyn ResourceResolver,
    ) -> Result<Self, LoadError> {
        let mut found = Found::default();
        let resolved = doc.resolve_ports(
            registry,
            Some(resolver),
            &mut LoadCtx::default(),
            None,
            &mut found,
        );
        found.finish(()).map_err(LoadErrors::into_first)?;
        Ok(WirePorts { doc, resolved })
    }

    /// Whether the loader would accept a wire `from` → `to`, judged on port types: `Ok` when it
    /// would, else the error the load would fail with. A wire touching a dark `subpatch` (a child
    /// this load could not resolve) is `Ok`, because the loader drops it rather than checks it.
    pub fn check(&self, from: &WireSource, to: &WireTarget) -> Result<(), LoadError> {
        self.check_ref(&from.wire_ref(), to)
    }

    /// [`check`](Self::check) for a source spelled as the document spells it — a wire-ref, which
    /// may be the sole-output sugar a [`WireSource`] cannot say.
    pub fn check_ref(&self, from: &str, to: &WireTarget) -> Result<(), LoadError> {
        match to {
            WireTarget::Input { node, port } => self.check_input(from, node, port),
            WireTarget::InterfaceOutput(name) => self.check_interface_output(from, name),
        }
    }

    /// Whether `from` names a source this document has — the half of [`check`](Self::check) that
    /// does not depend on the target, for a caller judging one source against many.
    pub fn check_source(&self, from: &WireSource) -> Result<(), LoadError> {
        self.resolve_source(&from.wire_ref()).map(|_| ())
    }

    /// Every place a wire could land in this document: each input of each document node that
    /// is not a Constant (a `subpatch` node's face inputs, for one), then each declared
    /// interface output. A dark `subpatch` has no face to offer, so it contributes nothing.
    pub fn targets(&self) -> Vec<WireTarget> {
        let r = &self.resolved;
        let mut out = Vec::new();
        for n in &self.doc.nodes {
            let ports: Vec<&str> = match r.faces.get(&n.address) {
                Some(face) => face.inputs.iter().map(|p| p.name.as_str()).collect(),
                None => match r.by_addr.get(n.address.as_str()) {
                    Some((_, desc)) => desc
                        .inputs
                        .iter()
                        .filter(|p| !desc.is_constant(p.name))
                        .map(|p| p.name)
                        .collect(),
                    None => Vec::new(),
                },
            };
            out.extend(ports.into_iter().map(|port| WireTarget::Input {
                node: n.address.clone(),
                port: port.to_string(),
            }));
        }
        if let Some(iface) = &self.doc.interface {
            out.extend(
                iface
                    .outputs
                    .keys()
                    .cloned()
                    .map(WireTarget::InterfaceOutput),
            );
        }
        out
    }

    fn check_input(&self, from: &str, node: &str, port: &str) -> Result<(), LoadError> {
        let r = &self.resolved;
        // A wire-ref is written into a document node's `inputs`; an input pipe's address is in
        // `by_addr` too, but it has no `inputs` entry to write into.
        let Some(n) = self.doc.nodes.iter().find(|n| n.address == node) else {
            return Err(LoadError::UnknownNode {
                address: node.to_string(),
                at: None,
            });
        };
        if r.dark.contains(node) {
            return Ok(());
        }
        // Pass 1 refuses a Constant's name in `inputs` before pass 2 could type the wire.
        if !r.faces.contains_key(node) {
            if let Some((_, desc)) = r.by_addr.get(node) {
                if desc.is_constant(port) {
                    return Err(LoadError::ConstantInInputs {
                        node: n.address.clone(),
                        name: port.to_string(),
                    });
                }
            }
        }
        let Some((_, _, to_ty)) = resolve_input(&r.faces, &r.by_addr, node, port)? else {
            return Ok(());
        };
        let Some((_, _, from_ty, from_label)) = self.resolve_source(from)? else {
            return Ok(());
        };
        if wire_type_compatible(&from_ty, &to_ty) {
            return Ok(());
        }
        Err(LoadError::TypeMismatch {
            from: from_label,
            from_type: Box::new(from_ty),
            to: format!("{node}.{port}"),
            to_type: Box::new(to_ty),
        })
    }

    fn check_interface_output(&self, from: &str, name: &str) -> Result<(), LoadError> {
        let r = &self.resolved;
        let entry = self
            .doc
            .interface
            .as_ref()
            .and_then(|iface| iface.outputs.get(name))
            .ok_or_else(|| LoadError::InterfacePipe {
                name: name.to_string(),
                reason: "no such interface output".to_string(),
            })?;
        // A normalized document holds only the feed form; the pipe loop refuses the other.
        let feed = entry.feed().ok_or_else(|| LoadError::InterfacePipe {
            name: name.to_string(),
            reason: "the target-pointing entry form is v1-only".to_string(),
        })?;
        let Some((key, idx, _, _)) = self.resolve_source(from)? else {
            return Ok(());
        };
        // Judged as if the pipe's `from` already named this source: that is the edit the wire is.
        check_output_feed(
            name,
            feed,
            from,
            &r.graph.nodes[key].descriptor.outputs[idx],
        )
    }

    /// Resolve `from` exactly as pass 2 resolves a wire-ref: `Ok(None)` is a source the loader
    /// drops unchecked (a dark nest, or a dark port of a live one).
    fn resolve_source(
        &self,
        reference: &str,
    ) -> Result<Option<(NodeKey, usize, PortType, String)>, LoadError> {
        let r = &self.resolved;
        let (addr, port) = parse_wire(reference);
        if r.dark.contains(addr) {
            return Ok(None);
        }
        resolve_output(&r.faces, &r.by_addr, addr, reference, port)
    }
}

/// One wire as a document writes it: where it lands, and the wire-ref it is fed from.
pub(crate) type Wire = (WireTarget, String);

/// Every wire `doc` writes: each wire-ref in a node's `inputs`, and each interface output's feed.
pub(crate) fn wires(doc: &NormalizedDoc) -> Vec<Wire> {
    let mut out = Vec::new();
    for n in &doc.nodes {
        for (name, value) in &n.inputs {
            if let InputValue::Wire { from } = value {
                let target = WireTarget::Input {
                    node: n.address.clone(),
                    port: name.clone(),
                };
                out.push((target, from.clone()));
            }
        }
    }
    if let Some(iface) = &doc.interface {
        for (name, entry) in &iface.outputs {
            if let Some(feed) = entry.feed() {
                out.push((WireTarget::InterfaceOutput(name.clone()), feed.from.clone()));
            }
        }
    }
    out
}

/// Each wire of `doc` a load passes over without checking it whole, with the address at either end
/// that made it: a node whose ports the load cannot know (an unknown operator type, a `subpatch`
/// whose child does not load or is missing, an address the rule refuses or another node holds), or
/// a dark port of a live child. No error can be reported for such a wire until that address loads.
pub(crate) fn unchecked_wires(
    doc: &NormalizedDoc,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> BTreeMap<Wire, String> {
    let r = doc.resolve_ports(
        registry,
        Some(resolver),
        &mut LoadCtx::default(),
        None,
        &mut Found::default(),
    );
    let source_dark = |from: &str| -> Option<String> {
        let (addr, port) = parse_wire(from);
        if r.dark.contains(addr) || r.dark.contains(from) {
            return Some(addr.to_string());
        }
        match resolve_output(&r.faces, &r.by_addr, addr, from, port) {
            Ok(None) => Some(addr.to_string()),
            Ok(Some(_)) | Err(_) => None,
        }
    };
    let mut out = BTreeMap::new();
    for (at, n) in doc.nodes.iter().enumerate() {
        let destination_dark = r.dark.contains(&n.address) || r.unchecked.contains(&at);
        for (name, value) in &n.inputs {
            let InputValue::Wire { from } = value else {
                continue;
            };
            let dark = if destination_dark
                || matches!(
                    resolve_input(&r.faces, &r.by_addr, &n.address, name),
                    Ok(None)
                ) {
                Some(n.address.clone())
            } else {
                source_dark(from)
            };
            if let Some(dark) = dark {
                let target = WireTarget::Input {
                    node: n.address.clone(),
                    port: name.clone(),
                };
                out.insert((target, from.clone()), dark);
            }
        }
    }
    if let Some(iface) = &doc.interface {
        for (name, entry) in &iface.outputs {
            let Some(feed) = entry.feed() else { continue };
            if let Some(dark) = source_dark(&feed.from) {
                out.insert(
                    (WireTarget::InterfaceOutput(name.clone()), feed.from.clone()),
                    dark,
                );
            }
        }
    }
    out
}
