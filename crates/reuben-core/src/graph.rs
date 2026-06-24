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
    /// Initial value overrides for **materialized [`Shape::Float`](crate::descriptor::Shape)
    /// inputs** (ADR-0028), as `(input port, value)` — the unwired-default a `/node/<input> v`
    /// literal sets, seeding the input's latch at Instantiate. Empty unless an author overrides
    /// a Float input's default. The successor to a legacy "unwired-default param".
    pub input_overrides: Vec<(usize, f32)>,
    /// Initial choice overrides for **[`Shape::Enum`](crate::descriptor::Shape) inputs** (ADR-0028),
    /// as `(input port, variant index)` — the unwired default a `/node/<input> "Hp"` literal sets,
    /// seeding the input's enum latch at Instantiate. Empty unless an author overrides an enum's
    /// default. Sibling of `input_overrides`, for the discrete (non-numeric) settable surface.
    pub enum_overrides: Vec<(usize, usize)>,
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
            input_overrides: Vec::new(),
            enum_overrides: Vec::new(),
        })
    }

    /// Override a single value by name on a node (clamped to its range). Sets the param slot when
    /// `name` is a param; otherwise, when `name` is a materialized [`Shape::Float`] input
    /// (ADR-0028), records an input override that seeds that input's latch at Instantiate. Unknown
    /// names are ignored (the loader validates names up front).
    pub fn set_param(&mut self, node: NodeKey, name: &str, value: f32) {
        let n = &mut self.nodes[node];
        if let Some(i) = n.descriptor.param_index(name) {
            n.params[i] = n.descriptor.params[i].clamp(value);
            return;
        }
        if n.descriptor.materialized_input(name).is_some() {
            self.set_input(node, name, value);
            return;
        }
        // An [`Shape::Enum`] input set as a numeric literal: the value is the variant **index**
        // fallback (ADR-0028). A string symbol (`"Hp"`) arrives via the loader's typed path; this
        // f32 surface carries the index. No-op if `name` is not an enum input.
        self.set_enum(node, name, &(value.round() as i64).to_string());
    }

    /// Override a materialized [`Shape::Float`] input's unwired default by name (ADR-0028),
    /// clamped to its range. No-op if `name` is not such an input. Upserts the `(port, value)`
    /// override consumed by [`Plan::instantiate`](crate::plan::Plan::instantiate).
    pub fn set_input(&mut self, node: NodeKey, name: &str, value: f32) {
        let n = &mut self.nodes[node];
        let Some((port, v)) = n
            .descriptor
            .materialized_input(name)
            .map(|(p, m)| (p, m.clamp(value)))
        else {
            return;
        };
        match n.input_overrides.iter_mut().find(|(p, _)| *p == port) {
            Some(slot) => slot.1 = v,
            None => n.input_overrides.push((port, v)),
        }
    }

    /// Override an [`Shape::Enum`] input's unwired default by name (ADR-0028), resolving a wire
    /// **token** (symbol `"Hp"` or fallback index `"1"`) against the input's variants. No-op if
    /// `name` is not an enum input or `token` resolves to no variant. Upserts the `(port, index)`
    /// override consumed by [`Plan::instantiate`](crate::plan::Plan::instantiate).
    pub fn set_enum(&mut self, node: NodeKey, name: &str, token: &str) {
        let n = &mut self.nodes[node];
        let Some((port, idx)) = n
            .descriptor
            .enum_input(name)
            .and_then(|(p, e)| e.resolve(token).map(|i| (p, i)))
        else {
            return;
        };
        match n.enum_overrides.iter_mut().find(|(p, _)| *p == port) {
            Some(slot) => slot.1 = idx,
            None => n.enum_overrides.push((port, idx)),
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
