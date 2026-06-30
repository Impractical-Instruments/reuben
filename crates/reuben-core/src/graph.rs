//! Graph — the author-facing description of a patch (ADR-0003).
//!
//! A Graph is plain data: operator instances (nodes) plus connections between their
//! ports. It carries no execution order — that is produced by Instantiate
//! ([`crate::plan::Plan::instantiate`]). Node identity is a stable slotmap key, so a
//! future Swap can match surviving operators across re-Instantiate.

use std::collections::BTreeMap;

use slotmap::{new_key_type, SlotMap};

use crate::descriptor::Descriptor;
use crate::message::Arg;
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
    /// Author value-overrides for settable inputs (ADR-0035), as `(input port, coerced `Arg`)` — the
    /// unwired-default a `/node/<input> v` literal sets, seeding the input's latch at Instantiate.
    /// One generic channel: an `F32` control's clamped value and an enum's concrete variant share it,
    /// replacing the former type-split `input_overrides` (`f32`) / `enum_overrides` (variant index).
    /// Sparse — empty unless an author overrides an input's default; the value is
    /// [`Port::coerce`](crate::descriptor::Port::coerce)-normalized at set time.
    pub value_overrides: Vec<(usize, Arg)>,
    /// The logical `sample` resource id (ADR-0016) this node referenced in its document, retained so
    /// [`InstrumentDoc::from_graph`](crate::format::InstrumentDoc::from_graph) can round-trip it on
    /// save. `None` unless the node declared a `sample` slot and named an id. The *decoded bytes* are
    /// bound out-of-band and do not round-trip; only this id does.
    pub sample_id: Option<String>,
    /// The logical `voice` instrument-resource id (ADR-0032) this node referenced, retained for the
    /// same save round-trip as [`sample_id`](Self::sample_id). `None` unless the node declared a
    /// `voice` slot and named an id.
    pub voice_id: Option<String>,
}

/// A directed connection from one node's output port to another's input port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Connection {
    pub src: NodeKey,
    pub src_port: usize,
    pub dst: NodeKey,
    pub dst_port: usize,
}

/// A patch's engine-honored I/O boundary (ADR-0032 §1) — the resolved form of a document's
/// `interface` block. Each external name maps to one internal `(node, port)`: an **input** name to
/// a node's input port (the boundary feeds it), an **output** name to a node's output port (the
/// boundary reads it). Empty unless the document declares an `interface`. Distinct from `control`
/// (ADR-0018), which is engine-ignored: this is real wiring the engine binds and type-checks (the
/// Voicer reads it to drive each voice sub-patch's `freq`/`gate` and tap its `audio`/`active`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Interface {
    /// External input name → the internal `(node, input port)` it drives.
    pub inputs: BTreeMap<String, (NodeKey, usize)>,
    /// External output name → the internal `(node, output port)` it exposes.
    pub outputs: BTreeMap<String, (NodeKey, usize)>,
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
    /// The resolved `interface` boundary (ADR-0032), empty unless declared. Set by the loader's
    /// [`build`](crate::format::InstrumentDoc::build) after nodes/wires resolve.
    pub interface: Interface,
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
            value_overrides: Vec::new(),
            sample_id: None,
            voice_id: None,
        })
    }

    /// Override a single value by name on a node (clamped to its range). Sets the param slot when
    /// `name` is a param; otherwise, when `name` is a materialized `F32` input
    /// (ADR-0030), records an input override that seeds that input's latch at Instantiate. Unknown
    /// names are ignored (the loader validates names up front).
    pub fn set_param(&mut self, node: NodeKey, name: &str, value: f32) {
        let n = &mut self.nodes[node];
        if let Some(i) = n.descriptor.param_index(name) {
            n.params[i] = n.descriptor.params[i].clamp(value);
            return;
        }
        // Not a param: route the numeric literal through the generic value channel. `coerce`
        // clamps it for an `F32` control, or reads it as the variant index for an enum input.
        self.set_value(node, name, &Arg::F32(value));
    }

    /// Override a settable input's unwired default by name (ADR-0035), coercing the raw author
    /// literal to the input's latch [`Arg`]: an `F32` control clamps to its range, an enum resolves a
    /// symbol / index / concrete variant. No-op if `name` is not a settable input or `raw` does not
    /// resolve (the loader validates names + values up front). Upserts the `(port, Arg)` override
    /// consumed by [`Plan::instantiate`](crate::plan::Plan::instantiate).
    pub fn set_value(&mut self, node: NodeKey, name: &str, raw: &Arg) {
        let n = &mut self.nodes[node];
        let Some((port, arg)) = n.descriptor.coerce_input(name, raw) else {
            return;
        };
        match n.value_overrides.iter_mut().find(|(p, _)| *p == port) {
            Some(slot) => slot.1 = arg,
            None => n.value_overrides.push((port, arg)),
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
