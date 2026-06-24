//! Graph — the author-facing description of a patch (ADR-0003).
//!
//! A Graph is plain data: operator instances (nodes) plus connections between their
//! ports. It carries no execution order — that is produced by Instantiate
//! ([`crate::plan::Plan::instantiate`]). Node identity is a stable slotmap key, so a
//! future Swap can match surviving operators across re-Instantiate.

use slotmap::{new_key_type, SlotMap};

use crate::descriptor::Descriptor;
use crate::operator::Operator;

new_key_type! {
    /// Stable identity of a node within a Graph.
    pub struct NodeKey;
}

/// One operator instance in the Graph.
pub struct Node {
    /// OSC address of this node (its public name; message routing prefix).
    pub address: String,
    pub op: Box<dyn Operator>,
    pub descriptor: Descriptor,
    /// Initial param values, in descriptor slot order.
    pub params: Vec<f32>,
}

/// A directed connection from one node's output port to another's input port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Connection {
    pub src: NodeKey,
    pub src_port: usize,
    pub dst: NodeKey,
    pub dst_port: usize,
}

/// A patch under construction.
#[derive(Default)]
pub struct Graph {
    pub nodes: SlotMap<NodeKey, Node>,
    pub connections: Vec<Connection>,
    /// Master output taps: `(node, output port, channel)`. `channel` is the logical master
    /// channel index this tap feeds (ADR-0026); `None` broadcasts to every channel (the
    /// historical mono fan). Summed into the rendered output.
    pub outputs: Vec<(NodeKey, usize, Option<usize>)>,
}

impl Graph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an operator instance with default params. Returns its stable key.
    pub fn add<T: Operator + 'static>(&mut self, address: &str, op: T) -> NodeKey {
        let descriptor = T::descriptor();
        self.add_boxed(address, Box::new(op), descriptor)
    }

    /// Add an already-boxed operator with its descriptor (params defaulted from it).
    /// Used by the instrument loader, which builds operators from a [`crate::registry`].
    pub fn add_boxed(
        &mut self,
        address: &str,
        op: Box<dyn Operator>,
        descriptor: Descriptor,
    ) -> NodeKey {
        let params = descriptor.default_params();
        self.nodes.insert(Node {
            address: address.to_string(),
            op,
            descriptor,
            params,
        })
    }

    /// Override a single param by name on a node (clamped to its range).
    pub fn set_param(&mut self, node: NodeKey, name: &str, value: f32) {
        let n = &mut self.nodes[node];
        if let Some(i) = n.descriptor.param_index(name) {
            n.params[i] = n.descriptor.params[i].clamp(value);
        }
    }

    /// Connect `src` output port to `dst` input port.
    pub fn connect(&mut self, src: NodeKey, src_port: usize, dst: NodeKey, dst_port: usize) {
        self.connections.push(Connection {
            src,
            src_port,
            dst,
            dst_port,
        });
    }

    /// Designate a master output tap broadcast to every logical channel (the mono fan).
    pub fn tap_output(&mut self, node: NodeKey, port: usize) {
        self.outputs.push((node, port, None));
    }

    /// Designate a master output tap feeding a single logical master `channel` (ADR-0026) —
    /// e.g. a `pan` op's `left`/`right` tapped as channel 0 / 1.
    pub fn tap_output_channel(&mut self, node: NodeKey, port: usize, channel: usize) {
        self.outputs.push((node, port, Some(channel)));
    }

    /// Find a node by its OSC address. Used by the loader to bind resources to the right
    /// node after the graph is built (ADR-0016).
    pub fn find(&self, address: &str) -> Option<NodeKey> {
        self.nodes
            .iter()
            .find(|(_, n)| n.address == address)
            .map(|(k, _)| k)
    }
}
