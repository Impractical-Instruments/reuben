//! Wire compatibility without wiring: may this source feed that target, and if not, why — asked
//! against a document the loader has resolved up to pass 2, through the loader's own endpoint
//! resolution and its own type rule.
//!
//! Only the type rule is answered. A wire that would close a cycle, or anything else a whole-
//! document validation rejects for reasons other than port types, is the edit's to report.

use std::fmt;

use super::{
    check_output_feed, parse_wire, resolve_input, resolve_output, wire_type_compatible, LoadCtx,
    LoadError, NormalizedDoc, Registry, Resolved,
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
    /// Fails with whatever a load would fail with before it reaches the first wire.
    pub fn resolve(
        doc: &'d NormalizedDoc,
        registry: &Registry,
        resolver: &dyn ResourceResolver,
    ) -> Result<Self, LoadError> {
        let resolved =
            doc.resolve_ports(registry, Some(resolver), &mut LoadCtx::default(), None)?;
        Ok(WirePorts { doc, resolved })
    }

    /// Whether the loader would accept a wire `from` → `to`, judged on port types: `Ok` when it
    /// would, else the error the load would fail with. A wire touching a dark `subpatch` (a child
    /// this load could not resolve) is `Ok`, because the loader drops it rather than checks it.
    pub fn check(&self, from: &WireSource, to: &WireTarget) -> Result<(), LoadError> {
        match to {
            WireTarget::Input { node, port } => self.check_input(from, node, port),
            WireTarget::InterfaceOutput(name) => self.check_interface_output(from, name),
        }
    }

    /// Whether `from` names a source this document has — the half of [`check`](Self::check) that
    /// does not depend on the target, for a caller judging one source against many.
    pub fn check_source(&self, from: &WireSource) -> Result<(), LoadError> {
        self.resolve_source(from).map(|_| ())
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

    fn check_input(&self, from: &WireSource, node: &str, port: &str) -> Result<(), LoadError> {
        let r = &self.resolved;
        // A wire-ref is written into a document node's `inputs`; an input pipe's address is in
        // `by_addr` too, but it has no `inputs` entry to write into.
        let Some(n) = self.doc.nodes.iter().find(|n| n.address == node) else {
            return Err(LoadError::UnknownNode(node.to_string()));
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

    fn check_interface_output(&self, from: &WireSource, name: &str) -> Result<(), LoadError> {
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
            &from.wire_ref(),
            &r.graph.nodes[key].descriptor.outputs[idx],
        )
    }

    /// Resolve `from` exactly as pass 2 resolves a wire-ref: `Ok(None)` is a source the loader
    /// drops unchecked (a dark nest, or a dark port of a live one).
    fn resolve_source(
        &self,
        from: &WireSource,
    ) -> Result<Option<(NodeKey, usize, PortType, String)>, LoadError> {
        let r = &self.resolved;
        let reference = from.wire_ref();
        let (addr, port) = parse_wire(&reference);
        if r.dark.contains(addr) {
            return Ok(None);
        }
        resolve_output(&r.faces, &r.by_addr, addr, &reference, port)
    }
}
