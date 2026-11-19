//! A document as data: its nodes, wires, interface pipes, resources and canvas positions, with
//! every node's ports resolved the way a load resolves them — for a program that draws and
//! inspects a document, where the [`projection`](crate::projection) renders one for a model.
//!
//! Answered for any document that mints. A document that does not **load** still reads in full,
//! with the load report saying why; what a failed load cannot give is the ports of a `subpatch`
//! whose child did not load, and that is reported against the node rather than failing the read.

use std::collections::BTreeMap;

use crate::contract::{content_hash, Diag, Report};
use crate::format::{
    parse_wire, ConfigValue, DocValue, InputValue, Layout, LoadWarning, NodeDoc, NormalizedDoc,
    WirePorts, WireTarget,
};
use crate::introspect::{curve, describe, describe_patch, validate_doc_with, PortInfo};
use crate::projection::Rebased;
use crate::resources::ResourceResolver;
use reuben_core::registry::Registry;

/// Everything [`read`] answers about one document.
#[derive(Debug, Clone)]
pub struct Structure {
    pub instrument: String,
    pub description: Option<String>,
    /// [`content_hash`] of the document read.
    pub hash: String,
    /// In document order.
    pub nodes: Vec<Node>,
    pub inputs: Vec<InputPipe>,
    pub outputs: Vec<OutputPipe>,
    pub resources: Vec<Resource>,
    /// What [`validate_doc`](crate::introspect::validate_doc) reports, with three additions that
    /// localize a failure on a node of *this* document: every wire the loader's type rule refuses
    /// is an error on the node and input holding it (the loader stops at the first, and names the
    /// source), a nested child that did not load is a diagnostic on the node referencing it, and a
    /// sample that did not resolve is a warning on every node reading it (the loader resolves each
    /// id once, for no node in particular).
    pub report: Report,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub address: String,
    pub type_name: String,
    pub description: Option<String>,
    pub layout: Option<Layout>,
    /// The `config` entries as written.
    pub constants: Vec<(String, DocValue)>,
    /// The `inputs` entries as written.
    pub inputs: Vec<(String, Input)>,
    /// `(slot, resource id)` for each resource reference the node carries.
    pub resources: Vec<(&'static str, String)>,
    /// The ports a wire can name on this node: the registry's for an operator, the child's
    /// interface for a `subpatch`. Empty for a type the registry lacks and for a `subpatch` whose
    /// child did not load.
    pub ports: Ports,
    /// The interface of the document this node references through `voice` or `patch`, when it
    /// loaded. For a `subpatch` it is `ports` again; for a Voicer it is the voice patch's
    /// interface, which the Voicer drives itself and no wire in this document can name.
    pub child: Option<Ports>,
}

#[derive(Debug, Clone, Default)]
pub struct Ports {
    /// Runtime inputs, then `Constant`s.
    pub inputs: Vec<PortInfo>,
    pub outputs: Vec<PortInfo>,
}

#[derive(Debug, Clone)]
pub enum Input {
    Literal(DocValue),
    Wire(Source),
}

/// Where a wire-ref points, as the loader reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// `port` is `None` only for the sole-output sugar on a source that does not have exactly one
    /// output to resolve it to — a wire the load report refuses.
    Output {
        node: String,
        port: Option<String>,
    },
    InterfaceInput(String),
}

#[derive(Debug, Clone)]
pub struct InputPipe {
    /// The pipe as the port it mints, carrying the *effective* range and curve.
    pub port: PortInfo,
    pub layout: Option<Layout>,
    pub declared: Declared,
}

/// An input pipe's range and curve as written, each `None` where the pipe declares none. Not
/// recoverable from the port: an undeclared range reads there as the loader's unbounded sentinel,
/// and an undeclared curve as linear, though the pipe takes the curve of the port it feeds.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Declared {
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// `linear` or `exponential`, the words [`PortInfo::curve`] uses.
    pub curve: Option<&'static str>,
}

/// An output pipe as declared: its type is the feeding port's, found through `from`.
#[derive(Debug, Clone)]
pub struct OutputPipe {
    pub name: String,
    pub from: Source,
    pub channel: Option<usize>,
    pub unit: Option<String>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub layout: Option<Layout>,
}

#[derive(Debug, Clone)]
pub struct Resource {
    pub id: String,
    pub source: String,
    /// [`ResourceResolver::canonical`] of `source` with no referrer — the call a load of this
    /// document makes, so this is the source the loader reads. It is the right file only when the
    /// resolver's top level is this document, which is true of a resolver scoped to it and not of
    /// one shared with the document that nests it.
    pub canonical: String,
}

/// Read `doc` as data. One load of the document, plus one load of each distinct child a node
/// references; a document that does not load pays a second port resolution to localize its wires.
pub fn read(
    doc: &NormalizedDoc,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Structure {
    let mut report = validate_doc_with(doc, registry, resolver, |w| sample_readers(doc, w));
    let mut children = Children {
        doc,
        registry,
        resolver,
        described: BTreeMap::new(),
    };
    let faces: Vec<Face> = doc
        .nodes
        .iter()
        .map(|n| children.face(n, &mut report))
        .collect();
    localize_wires(doc, registry, resolver, &mut report);

    let index: BTreeMap<&str, usize> = doc
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.address.as_str(), i))
        .collect();
    let source = |reference: &str| wire_source(doc, &faces, &index, reference);
    let nodes = doc
        .nodes
        .iter()
        .zip(&faces)
        .map(|(n, face)| Node {
            address: n.address.clone(),
            type_name: n.type_name.clone(),
            description: n.doc.clone(),
            layout: n.layout,
            constants: n
                .config
                .iter()
                .map(|(name, v)| {
                    let value = match v {
                        ConfigValue::Number(x) => DocValue::Number(*x),
                        ConfigValue::Symbol(s) => DocValue::Symbol(s.clone()),
                    };
                    (name.clone(), value)
                })
                .collect(),
            inputs: n
                .inputs
                .iter()
                .map(|(name, v)| {
                    let input = match v {
                        InputValue::Wire { from } => Input::Wire(source(from)),
                        InputValue::Number(x) => Input::Literal(DocValue::Number(*x)),
                        InputValue::Symbol(s) => Input::Literal(DocValue::Symbol(s.clone())),
                    };
                    (name.clone(), input)
                })
                .collect(),
            resources: n
                .resource_refs()
                .into_iter()
                .filter_map(|(slot, id)| id.clone().map(|id| (slot, id)))
                .collect(),
            ports: face.ports.clone(),
            child: face.child.clone(),
        })
        .collect();

    // An entry in the form the mint does not leave behind has no pipe to describe; the load
    // report refuses it by name.
    let interface = doc.interface.as_ref();
    let inputs = interface
        .into_iter()
        .flat_map(|i| &i.inputs)
        .filter_map(|(name, entry)| {
            let pipe = entry.pipe()?;
            Some(InputPipe {
                port: PortInfo::from_pipe(name, pipe),
                layout: pipe.layout,
                declared: Declared {
                    min: pipe.min,
                    max: pipe.max,
                    curve: pipe.curve.map(|c| curve(c.into())),
                },
            })
        })
        .collect();
    let outputs = interface
        .into_iter()
        .flat_map(|i| &i.outputs)
        .filter_map(|(name, entry)| {
            let feed = entry.feed()?;
            Some(OutputPipe {
                name: name.clone(),
                from: source(&feed.from),
                channel: feed.channel,
                unit: feed.unit.clone(),
                min: feed.min,
                max: feed.max,
                layout: feed.layout,
            })
        })
        .collect();

    Structure {
        instrument: doc.instrument.clone(),
        description: doc.doc.clone(),
        hash: content_hash(doc),
        nodes,
        inputs,
        outputs,
        resources: doc
            .resources
            .iter()
            .map(|(id, source)| Resource {
                id: id.clone(),
                source: source.clone(),
                canonical: resolver.canonical(source, None),
            })
            .collect(),
        report,
    }
}

/// One node's resolved ports, and what the sole-output sugar needs to know beyond them.
#[derive(Default)]
struct Face {
    ports: Ports,
    child: Option<Ports>,
    /// Outputs the child declares but this load could not type. They are absent from `ports`, yet
    /// the loader counts them when it decides whether a bare `/node` reference is ambiguous.
    dark_outputs: usize,
}

/// Why a referenced child has no interface to report.
enum ChildFailure {
    /// The reference names nothing readable. A load degrades this to silence and warns.
    Unavailable(String),
    /// The child was read and is not a loadable document. A load of the parent fails on it.
    Broken(String),
}

/// The children one read describes, each once however many nodes reference it.
struct Children<'a> {
    doc: &'a NormalizedDoc,
    registry: &'a Registry,
    resolver: &'a dyn ResourceResolver,
    /// Canonical child source → its interface and dark output count.
    described: BTreeMap<String, Result<(Ports, usize), String>>,
}

impl Children<'_> {
    fn face(&mut self, n: &NodeDoc, report: &mut Report) -> Face {
        let Some(entry) = self.registry.get(&n.type_name) else {
            return Face::default();
        };
        let descriptor = &entry.descriptor;
        // The two slots that name a document; a node declares at most one of them.
        let nested = ["patch", "voice"].into_iter().find_map(|slot| {
            let (_, id) = n.resource_refs().into_iter().find(|(s, _)| *s == slot)?;
            id.as_ref().filter(|_| descriptor.has_resource(slot))
        });
        let child = nested.and_then(|id| match self.describe(id) {
            Ok(described) => Some(described),
            Err(failure) => {
                let (unavailable, message) = match failure {
                    ChildFailure::Unavailable(why) => (true, why),
                    ChildFailure::Broken(why) => (false, why),
                };
                let diag = Diag {
                    node: Some(n.address.clone()),
                    port: None,
                    message,
                };
                // A load that succeeded has already warned about an unavailable child — but once
                // per source, on the first node referencing it. One that failed reports no
                // warnings at all.
                let said =
                    report.ok && unavailable && report.warnings.iter().any(|w| w.node == diag.node);
                if !report.ok && !unavailable {
                    // The loader's own error for this failure is localized on a node of the
                    // *child*, an address that names nothing in this document or, worse, one of
                    // its unrelated nodes. `diag` carries the same text on the node that holds
                    // the child, so the loader's copy goes.
                    report.errors.retain(|d| {
                        d.message == diag.message || !diag.message.contains(&d.message)
                    });
                    report.errors.push(diag);
                } else if !said {
                    report.warnings.push(diag);
                }
                None
            }
        });
        if descriptor.has_resource("patch") {
            return match child {
                Some((ports, dark_outputs)) => Face {
                    child: Some(ports.clone()),
                    ports,
                    dark_outputs,
                },
                None => Face::default(),
            };
        }
        let operator = describe(self.registry, Some(&n.type_name))
            .ok()
            .and_then(|mut found| found.pop());
        Face {
            ports: operator
                .map(|o| Ports {
                    inputs: o.inputs,
                    outputs: o.outputs,
                })
                .unwrap_or_default(),
            child: child.map(|(ports, _)| ports),
            dark_outputs: 0,
        }
    }

    fn describe(&mut self, id: &str) -> Result<(Ports, usize), ChildFailure> {
        let Some(source) = self.doc.resources.get(id) else {
            return Err(ChildFailure::Unavailable(format!(
                "{id:?} is not in the resources table"
            )));
        };
        let canonical = self.resolver.canonical(source, None);
        if let Some(hit) = self.described.get(&canonical) {
            return hit.clone().map_err(ChildFailure::Broken);
        }
        // An unreadable source is not cached: the read is the whole cost of finding that out.
        let text = self
            .resolver
            .resolve_text(&canonical)
            .map_err(|e| ChildFailure::Unavailable(format!("{source:?} did not resolve: {e}")))?;
        let rebased = Rebased {
            inner: self.resolver,
            referrer: canonical.clone(),
        };
        let described = describe_patch(&text, self.registry, &rebased)
            .map(|b| {
                let ports = Ports {
                    inputs: b.inputs,
                    outputs: b.outputs,
                };
                (ports, b.dark_outputs.len())
            })
            .map_err(|e| format!("{source:?} did not load: {e}"));
        self.described.insert(canonical, described.clone());
        described.map_err(ChildFailure::Broken)
    }
}

/// `w` as a diagnostic on each node of `doc` reading the sample it failed to resolve, or as the
/// loader's own when it is no such failure or no node reads it.
fn sample_readers(doc: &NormalizedDoc, w: &LoadWarning) -> Vec<Diag> {
    if let LoadWarning::ResolveFailed { slot, id, .. } = w {
        let readers: Vec<Diag> = (doc.nodes.iter())
            .filter(|n| *slot == "sample" && n.sample.as_ref() == Some(id))
            .map(|n| Diag {
                node: Some(n.address.clone()),
                port: None,
                message: w.to_string(),
            })
            .collect();
        if !readers.is_empty() {
            return readers;
        }
    }
    vec![Diag::from_warning(w)]
}

/// Replace the loader's one wire error with an error per refused wire, each on the node and input
/// that hold it. Only a document that failed to load has any, and only one whose endpoints still
/// resolve can be asked.
fn localize_wires(
    doc: &NormalizedDoc,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
    report: &mut Report,
) {
    if report.ok {
        return;
    }
    let Ok(ports) = WirePorts::resolve(doc, registry, resolver) else {
        return;
    };
    let mut refused = Vec::new();
    for n in &doc.nodes {
        for (name, value) in &n.inputs {
            let InputValue::Wire { from } = value else {
                continue;
            };
            let to = WireTarget::Input {
                node: n.address.clone(),
                port: name.clone(),
            };
            if let Err(e) = ports.check_ref(from, &to) {
                refused.push(Diag {
                    node: Some(n.address.clone()),
                    port: Some(name.clone()),
                    message: e.to_string(),
                });
            }
        }
    }
    for (name, entry) in doc.interface.iter().flat_map(|i| &i.outputs) {
        let Some(feed) = entry.feed() else { continue };
        let to = WireTarget::InterfaceOutput(name.clone());
        if let Err(e) = ports.check_ref(&feed.from, &to) {
            // A boundary-named problem localizes on `port`, as the loader's own do.
            refused.push(Diag {
                node: None,
                port: Some(name.clone()),
                message: e.to_string(),
            });
        }
    }
    // The loader's error for the first refused wire is one of these, worded identically because
    // both come from the same check; it goes, so the wire is reported once.
    report
        .errors
        .retain(|d| !refused.iter().any(|r| r.message == d.message));
    report.errors.extend(refused);
}

/// Read a wire-ref the way pass 2 does: a reference that names a whole address is that address,
/// before the last `.` is taken for a port separator.
fn wire_source(
    doc: &NormalizedDoc,
    faces: &[Face],
    index: &BTreeMap<&str, usize>,
    reference: &str,
) -> Source {
    let pipe = |address: &str| {
        let name = address.strip_prefix('/')?;
        doc.interface
            .as_ref()
            .is_some_and(|i| i.inputs.contains_key(name))
            .then(|| name.to_string())
    };
    let node = |address: &str| index.get(address).copied();
    let (address, port) = if node(reference).is_some() || pipe(reference).is_some() {
        (reference, None)
    } else {
        parse_wire(reference)
    };
    if let Some(name) = pipe(address) {
        return Source::InterfaceInput(name);
    }
    let sole_output = || {
        let face = &faces[node(address)?];
        match face.ports.outputs.as_slice() {
            [only] if face.dark_outputs == 0 => Some(only.name.clone()),
            _ => None,
        }
    };
    Source::Output {
        node: address.to_string(),
        port: port.map(str::to_string).or_else(sole_output),
    }
}
