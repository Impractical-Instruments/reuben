//! Instrument format — the JSON canonical document (ADR-0004).
//!
//! An instrument is plain data: a list of operator `nodes` (type + address + params),
//! `connections` between named ports, and master `outputs`. Ports are referenced by
//! **name** (from the operator's [`Descriptor`](crate::descriptor::Descriptor)), not by
//! brittle index. Optional `doc` fields carry human/agent notes. The schema that
//! validates these documents is generated from the operator descriptors ([`crate::schema`]).
//!
//! [`load`] turns JSON into a [`Graph`] (resolving types via a [`Registry`]); [`InstrumentDoc::from_graph`]
//! goes the other way. Loading is an authoring step, not a realtime path — it lives in the
//! portable core but never runs on the audio thread.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::descriptor::{Descriptor, PortKind};
use crate::graph::Graph;
use crate::registry::Registry;

/// A complete instrument document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstrumentDoc {
    /// Human-facing name / id of this instrument.
    pub instrument: String,
    /// Optional note for humans and agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    pub nodes: Vec<NodeDoc>,
    #[serde(default)]
    pub connections: Vec<ConnectionDoc>,
    #[serde(default)]
    pub outputs: Vec<PortRef>,
}

/// One operator instance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeDoc {
    /// Operator type name (must be registered, e.g. `"oscillator"`).
    #[serde(rename = "type")]
    pub type_name: String,
    /// OSC address / routing prefix, e.g. `"/osc"`. Unique within the instrument.
    pub address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// Param overrides by name; omitted params use the descriptor default.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, f32>,
}

/// A reference to one node's port, by names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortRef {
    pub node: String,
    pub port: String,
}

/// A connection from one node's output port to another's input port.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectionDoc {
    pub from: PortRef,
    pub to: PortRef,
}

/// Why loading an instrument document failed. Messages are written for an author
/// (human or agent) to act on.
#[derive(Debug)]
pub enum LoadError {
    /// The JSON itself was malformed.
    Json(serde_json::Error),
    /// A node names an operator type that isn't registered.
    UnknownType { address: String, type_name: String },
    /// Two nodes share an address.
    DuplicateAddress(String),
    /// A connection or output references a node that doesn't exist.
    UnknownNode(String),
    /// A node has no port with that name (in the required direction).
    UnknownPort { node: String, port: String },
    /// A node has no param with that name.
    UnknownParam { node: String, param: String },
    /// A connection joins ports of different kinds (Signal vs Message).
    PortKindMismatch { from: String, to: String },
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Json(e) => write!(f, "invalid JSON: {e}"),
            LoadError::UnknownType { address, type_name } => {
                write!(f, "node {address}: unknown operator type {type_name:?}")
            }
            LoadError::DuplicateAddress(a) => write!(f, "duplicate node address {a:?}"),
            LoadError::UnknownNode(n) => write!(f, "reference to unknown node {n:?}"),
            LoadError::UnknownPort { node, port } => {
                write!(f, "node {node:?} has no port {port:?}")
            }
            LoadError::UnknownParam { node, param } => {
                write!(f, "node {node:?} has no param {param:?}")
            }
            LoadError::PortKindMismatch { from, to } => {
                write!(
                    f,
                    "connection {from} -> {to} joins a Signal and a Message port"
                )
            }
        }
    }
}

impl std::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LoadError::Json(e) => Some(e),
            _ => None,
        }
    }
}

/// Parse JSON and build the [`Graph`], resolving operator types via `registry`.
pub fn load(json: &str, registry: &Registry) -> Result<Graph, LoadError> {
    InstrumentDoc::from_json(json)?.build(registry)
}

impl InstrumentDoc {
    /// Parse a document from JSON (no operator resolution yet).
    pub fn from_json(json: &str) -> Result<Self, LoadError> {
        serde_json::from_str(json).map_err(LoadError::Json)
    }

    /// Serialize to pretty JSON (the canonical on-disk form).
    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(self).expect("InstrumentDoc serializes")
    }

    /// Build the [`Graph`] this document describes.
    pub fn build(&self, registry: &Registry) -> Result<Graph, LoadError> {
        let mut graph = Graph::new();
        // address -> (key, descriptor) for resolving connections and outputs.
        let mut by_addr: BTreeMap<&str, (crate::graph::NodeKey, Descriptor)> = BTreeMap::new();

        for n in &self.nodes {
            let entry = registry
                .get(&n.type_name)
                .ok_or_else(|| LoadError::UnknownType {
                    address: n.address.clone(),
                    type_name: n.type_name.clone(),
                })?;
            if by_addr.contains_key(n.address.as_str()) {
                return Err(LoadError::DuplicateAddress(n.address.clone()));
            }
            let descriptor = entry.descriptor.clone();
            let key = graph.add_boxed(&n.address, (entry.make)(), descriptor.clone());
            for (name, value) in &n.params {
                if descriptor.param_index(name).is_none() {
                    return Err(LoadError::UnknownParam {
                        node: n.address.clone(),
                        param: name.clone(),
                    });
                }
                graph.set_param(key, name, *value);
            }
            by_addr.insert(&n.address, (key, descriptor));
        }

        for c in &self.connections {
            let (src_key, src_desc) = lookup(&by_addr, &c.from.node)?;
            let (dst_key, dst_desc) = lookup(&by_addr, &c.to.node)?;
            let (src_port, src_kind) = out_port(src_desc, &c.from)?;
            let (dst_port, dst_kind) = in_port(dst_desc, &c.to)?;
            if src_kind != dst_kind {
                return Err(LoadError::PortKindMismatch {
                    from: format!("{}:{}", c.from.node, c.from.port),
                    to: format!("{}:{}", c.to.node, c.to.port),
                });
            }
            graph.connect(src_key, src_port, dst_key, dst_port);
        }

        for o in &self.outputs {
            let (key, desc) = lookup(&by_addr, &o.node)?;
            let (port, _) = out_port(desc, o)?;
            graph.tap_output(key, port);
        }

        Ok(graph)
    }

    /// Derive a document from a built [`Graph`] (the canonical "save" path). Nodes and
    /// connections are emitted in a stable order so output is deterministic.
    pub fn from_graph(graph: &Graph, instrument: impl Into<String>) -> Self {
        let mut nodes: Vec<NodeDoc> = graph
            .nodes
            .values()
            .map(|node| {
                let params = node
                    .descriptor
                    .params
                    .iter()
                    .enumerate()
                    .map(|(i, p)| (p.name.to_string(), node.params[i]))
                    .collect();
                NodeDoc {
                    type_name: node.descriptor.type_name.to_string(),
                    address: node.address.clone(),
                    doc: None,
                    params,
                }
            })
            .collect();
        nodes.sort_by(|a, b| a.address.cmp(&b.address));

        let mut connections: Vec<ConnectionDoc> = graph
            .connections
            .iter()
            .map(|c| ConnectionDoc {
                from: PortRef {
                    node: graph.nodes[c.src].address.clone(),
                    port: graph.nodes[c.src].descriptor.outputs[c.src_port]
                        .name
                        .to_string(),
                },
                to: PortRef {
                    node: graph.nodes[c.dst].address.clone(),
                    port: graph.nodes[c.dst].descriptor.inputs[c.dst_port]
                        .name
                        .to_string(),
                },
            })
            .collect();
        connections.sort_by(|a, b| {
            (&a.from.node, &a.from.port, &a.to.node, &a.to.port).cmp(&(
                &b.from.node,
                &b.from.port,
                &b.to.node,
                &b.to.port,
            ))
        });

        let outputs = graph
            .outputs
            .iter()
            .map(|(key, port)| PortRef {
                node: graph.nodes[*key].address.clone(),
                port: graph.nodes[*key].descriptor.outputs[*port].name.to_string(),
            })
            .collect();

        Self {
            instrument: instrument.into(),
            doc: None,
            nodes,
            connections,
            outputs,
        }
    }
}

fn lookup<'a>(
    by_addr: &'a BTreeMap<&str, (crate::graph::NodeKey, Descriptor)>,
    node: &str,
) -> Result<(crate::graph::NodeKey, &'a Descriptor), LoadError> {
    by_addr
        .get(node)
        .map(|(k, d)| (*k, d))
        .ok_or_else(|| LoadError::UnknownNode(node.to_string()))
}

fn out_port(desc: &Descriptor, r: &PortRef) -> Result<(usize, PortKind), LoadError> {
    desc.outputs
        .iter()
        .position(|p| p.name == r.port)
        .map(|i| (i, desc.outputs[i].kind))
        .ok_or_else(|| LoadError::UnknownPort {
            node: r.node.clone(),
            port: r.port.clone(),
        })
}

fn in_port(desc: &Descriptor, r: &PortRef) -> Result<(usize, PortKind), LoadError> {
    desc.inputs
        .iter()
        .position(|p| p.name == r.port)
        .map(|i| (i, desc.inputs[i].kind))
        .ok_or_else(|| LoadError::UnknownPort {
            node: r.node.clone(),
            port: r.port.clone(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO_NODE: &str = r#"
    {
      "instrument": "test",
      "nodes": [
        { "type": "oscillator", "address": "/osc", "params": { "freq": 220.0 } },
        { "type": "output", "address": "/out" }
      ],
      "connections": [
        { "from": {"node":"/osc","port":"audio"}, "to": {"node":"/out","port":"audio"} }
      ],
      "outputs": [ {"node":"/out","port":"audio"} ]
    }"#;

    fn reg() -> Registry {
        Registry::builtin()
    }

    #[test]
    fn loads_a_simple_instrument() {
        let g = load(TWO_NODE, &reg()).expect("load");
        assert_eq!(g.nodes.len(), 2);
        assert_eq!(g.connections.len(), 1);
        assert_eq!(g.outputs.len(), 1);
    }

    #[test]
    fn unknown_type_errors() {
        let json = r#"{"instrument":"t","nodes":[{"type":"nope","address":"/x"}]}"#;
        assert!(matches!(
            load(json, &reg()),
            Err(LoadError::UnknownType { .. })
        ));
    }

    #[test]
    fn duplicate_address_errors() {
        let json = r#"{"instrument":"t","nodes":[
            {"type":"output","address":"/x"},
            {"type":"output","address":"/x"}]}"#;
        assert!(matches!(
            load(json, &reg()),
            Err(LoadError::DuplicateAddress(_))
        ));
    }

    #[test]
    fn unknown_port_errors() {
        let json = r#"{"instrument":"t",
            "nodes":[{"type":"output","address":"/a"},{"type":"output","address":"/b"}],
            "connections":[{"from":{"node":"/a","port":"nope"},"to":{"node":"/b","port":"audio"}}]}"#;
        assert!(matches!(
            load(json, &reg()),
            Err(LoadError::UnknownPort { .. })
        ));
    }

    #[test]
    fn unknown_param_errors() {
        let json = r#"{"instrument":"t",
            "nodes":[{"type":"filter","address":"/f","params":{"nope":1.0}}]}"#;
        assert!(matches!(
            load(json, &reg()),
            Err(LoadError::UnknownParam { .. })
        ));
    }

    #[test]
    fn port_kind_mismatch_errors() {
        // osc.audio is a Signal output; voicer.notes is a Message input.
        let json = r#"{"instrument":"t",
            "nodes":[{"type":"oscillator","address":"/osc"},{"type":"voicer","address":"/v"}],
            "connections":[{"from":{"node":"/osc","port":"audio"},"to":{"node":"/v","port":"notes"}}]}"#;
        assert!(matches!(
            load(json, &reg()),
            Err(LoadError::PortKindMismatch { .. })
        ));
    }

    #[test]
    fn doc_json_round_trips() {
        let doc = InstrumentDoc::from_json(TWO_NODE).expect("parse");
        let reparsed = InstrumentDoc::from_json(&doc.to_json_pretty()).expect("reparse");
        assert_eq!(doc, reparsed);
    }

    #[test]
    fn from_graph_then_build_is_stable() {
        // load -> save -> reparse -> save again: the two saved docs are identical.
        let g1 = load(TWO_NODE, &reg()).expect("load");
        let saved1 = InstrumentDoc::from_graph(&g1, "test");
        let g2 = saved1.build(&reg()).expect("rebuild");
        let saved2 = InstrumentDoc::from_graph(&g2, "test");
        assert_eq!(saved1, saved2);
        assert_eq!(saved1.nodes.len(), 2);
    }
}
