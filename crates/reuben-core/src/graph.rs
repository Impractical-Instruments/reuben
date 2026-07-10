//! Graph — the author-facing description of a patch (ADR-0003).
//!
//! A Graph is plain data: operator instances (nodes) plus connections between their
//! ports. It carries no execution order — that is produced by Instantiate
//! ([`crate::plan::Plan::instantiate`]). Node identity is a stable slotmap key, so a
//! future Swap can match surviving operators across re-Instantiate.

use std::collections::{BTreeMap, BTreeSet};

use slotmap::{new_key_type, SlotMap};

use crate::descriptor::Descriptor;
use crate::message::Arg;
use crate::operator::{Operator, PortIndex};

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
    /// Author value-overrides for settable inputs (ADR-0035), as `(input port, coerced `Arg`)` — the
    /// unwired-default a `/node/<input> v` literal sets, seeding the input's latch at Instantiate.
    /// One generic channel: an `F32` control's clamped value and an enum's concrete variant share it,
    /// replacing the former type-split `input_overrides` (`f32`) / `enum_overrides` (variant index).
    /// Sparse — empty unless an author overrides an input's default; the value is
    /// [`Port::coerce`](crate::descriptor::Port::coerce)-normalized at set time.
    pub value_overrides: Vec<(usize, Arg)>,
    /// Author overrides for the operator's plan-time **`Constant`** ports (ADR-0035), as
    /// `(constant slot, coerced `Arg`)` — the value the patch's `config` block sets (e.g. the
    /// voicer's `voices`). The sibling of [`value_overrides`](Self::value_overrides) for the
    /// plan-time surface: sparse, descriptor-default fallback, `Arg`-valued. Routed to `config` on
    /// save, never `inputs`.
    pub constant_overrides: Vec<(usize, Arg)>,
    /// The logical `sample` resource id (ADR-0016) this node referenced in its document, retained so
    /// [`NormalizedDoc::from_graph`](crate::format::NormalizedDoc::from_graph) can round-trip it on
    /// save. `None` unless the node declared a `sample` slot and named an id. The *decoded bytes* are
    /// bound out-of-band and do not round-trip; only this id does.
    pub sample_id: Option<String>,
    /// The logical `voice` instrument-resource id (ADR-0032) this node referenced, retained for the
    /// same save round-trip as [`sample_id`](Self::sample_id). `None` unless the node declared a
    /// `voice` slot and named an id.
    ///
    /// A `subpatch` node's `patch` id has no counterpart here: the node **dissolves** at build
    /// (ADR-0034 §2, nesting P4) — its child's nodes are spliced in with prefixed addresses and no
    /// node survives to carry the reference. A built graph is the *flattened* instrument;
    /// reference-preserving save is the library thread (P7,
    /// [#122](https://github.com/Impractical-Instruments/reuben/issues/122)).
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

/// A patch's engine-honored I/O boundary — the resolved form of a document's `interface` block
/// (ADR-0032 §1, reshaped by ADR-0038 §2 into **pipes**). Each external **input** name maps to
/// the `in` port of the loader-built pipe node it minted (`in` → `/in`); whatever feeds the
/// boundary lands there, and internal consumers wire from the pipe's output. Each **output**
/// name maps to the internal `(node, output port)` that feeds it. Empty unless the document
/// declares an `interface`. Distinct from `control` (ADR-0018), which is engine-ignored: this
/// is real wiring the engine binds and type-checks (the Voicer reads it to drive each voice
/// sub-patch's `freq`/`gate` pipes and tap its `audio`/`active`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Interface {
    /// External input name → the pipe node's `(node, input port)` the boundary feeds.
    pub inputs: BTreeMap<String, (NodeKey, usize)>,
    /// External output name → the internal `(node, output port)` that feeds it.
    pub outputs: BTreeMap<String, (NodeKey, usize)>,
    /// Logical **input** channel bindings (ADR-0038 §3): input pipe name → the logical input
    /// channel it reads when this graph is played at top level. Inert when nested/hosted (a
    /// splice discards the child's `Interface`, so a binding never reaches hardware from inside
    /// a nest). Consumed by the core input master (P3).
    pub input_channels: BTreeMap<String, usize>,
    /// Logical **output** channel bindings (ADR-0038 §3): output pipe name → the logical master
    /// channel it feeds (already applied to [`Graph::outputs`] taps at build; retained here so
    /// save/introspection can reconstruct the entry). Omitted = broadcast.
    pub output_channels: BTreeMap<String, usize>,
    /// Declared **input** names whose internal target went dark — an unavailable nested child
    /// (ADR-0016/0034). The port is real in the document but resolves to nothing this load; a
    /// consumer referencing it degrades (drops the wire with a warning) instead of failing, so
    /// dark degradation stays transitive through re-exports rather than escalating to a
    /// structural error one level up.
    pub dark_inputs: BTreeSet<String>,
    /// Declared **output** names whose internal target went dark (see `dark_inputs`).
    pub dark_outputs: BTreeSet<String>,
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
    /// [`build`](crate::format::NormalizedDoc::build) after nodes/wires resolve.
    pub interface: Interface,
    /// Derived **logical input width** (ADR-0038 §3): max bound input channel + 1 across this
    /// graph's own input pipes, `0` when none binds a channel — a patch that uses no inputs pays
    /// nothing. Honored only when this graph is played at top level (the core input master, P3);
    /// a nested/hosted graph's value is inert.
    pub input_channels_width: usize,
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

    /// Add an already-boxed operator with its descriptor. Used by the instrument loader, which
    /// builds operators from a [`crate::registry`]. Inputs and constants default from the descriptor;
    /// only author overrides are stored on the node.
    pub fn add_boxed(
        &mut self,
        address: &str,
        op: Box<dyn Operator>,
        descriptor: Descriptor,
    ) -> NodeKey {
        self.nodes.insert(Node {
            address: address.to_string(),
            op,
            descriptor,
            value_overrides: Vec::new(),
            constant_overrides: Vec::new(),
            sample_id: None,
            voice_id: None,
        })
    }

    /// Override a plan-time **`Constant`** by name (ADR-0035), coercing the raw author literal to the
    /// constant's stored [`Arg`] (an `i32` count clamps to its range). No-op if `name` is not a
    /// constant or `raw` does not resolve. Upserts the `(slot, Arg)` override the patch's `config`
    /// block sets and `from_graph` saves back.
    pub fn set_constant(&mut self, node: NodeKey, name: &str, raw: &Arg) {
        let n = &mut self.nodes[node];
        let Some((slot, arg)) = n.descriptor.coerce_constant(name, raw) else {
            return;
        };
        match n.constant_overrides.iter_mut().find(|(s, _)| *s == slot) {
            Some(entry) => entry.1 = arg,
            None => n.constant_overrides.push((slot, arg)),
        }
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

    /// Connect `src` output port to `dst` input port. Ports are typed contract handles
    /// (`OUT_*`/`IN_*`, ADR-0037) or bare indices (the loader's resolved ordinals).
    pub fn connect(
        &mut self,
        src: NodeKey,
        src_port: impl PortIndex,
        dst: NodeKey,
        dst_port: impl PortIndex,
    ) {
        self.connections.push(Connection {
            src,
            src_port: src_port.index(),
            dst,
            dst_port: dst_port.index(),
        });
    }

    /// Designate a master output tap broadcast to every logical channel (the mono fan).
    pub fn tap_output(&mut self, node: NodeKey, port: impl PortIndex) {
        self.outputs.push((node, port.index(), None));
    }

    /// Designate a master output tap feeding a single logical master `channel` (ADR-0026) —
    /// e.g. a `pan` op's `left`/`right` tapped as channel 0 / 1.
    pub fn tap_output_channel(&mut self, node: NodeKey, port: impl PortIndex, channel: usize) {
        self.outputs.push((node, port.index(), Some(channel)));
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
