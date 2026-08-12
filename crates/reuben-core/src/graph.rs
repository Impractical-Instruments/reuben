//! Graph — the author-facing description of a patch.
//!
//! A Graph is plain data: operator instances (nodes) plus connections between their
//! ports. It carries no execution order — that is produced by Instantiate
//! ([`crate::plan::Plan::instantiate`]).
//!
//! see rules: composition-operators

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use slotmap::{new_key_type, SecondaryMap, SlotMap};

use crate::descriptor::Descriptor;
use crate::message::Arg;
use crate::operator::{Operator, PortIndex};

new_key_type! {
    /// Stable identity of a node within a Graph.
    pub struct NodeKey;
}

/// One operator instance in the Graph.
pub struct Node {
    /// OSC address of this node (its public name; message routing prefix), behind a shared
    /// handle — so [`spawn_copy`](Graph::spawn_copy) reproduces it with a refcount bump instead
    /// of a `String` per node per copy. An N-voice pool is N copies of one build, and it (with
    /// the Plans they instantiate into, which move the handle) holds one `/osc` rather than N.
    /// The loader additionally mints from one intern table per load, which extends the same
    /// sharing across the graphs it *builds* rather than copies.
    ///
    /// Never edited in place: the one writer after build (the subpatch splice, which prefixes the
    /// address with the reusing node's) replaces the whole handle with its own mint.
    pub address: Arc<str>,
    pub op: Box<dyn Operator>,
    /// This node's operator type's self-description, behind a shared handle — see
    /// [`Entry::descriptor`](crate::registry::Entry::descriptor). A node the loader built from a
    /// registered type points at that entry's copy, so a document naming one type N times costs one
    /// descriptor rather than N; a loader-minted interface pipe (no entry exists — the name is
    /// reserved) and a node added through [`Graph::add`] each mint their own.
    pub descriptor: Arc<Descriptor>,
    /// Author value-overrides for settable inputs, as `(input port, coerced `Arg`)` — the
    /// unwired-default a `/node/<input> v` literal sets, seeding the input's latch at Instantiate.
    /// One generic channel: an `F32` control's clamped value and an enum's concrete variant share it,
    /// replacing the former type-split `input_overrides` (`f32`) / `enum_overrides` (variant index).
    /// Sparse — empty unless an author overrides an input's default; the value is
    /// [`Port::coerce`](crate::descriptor::Port::coerce)-normalized at set time.
    pub value_overrides: Vec<(usize, Arg)>,
    /// Author overrides for the operator's plan-time **`Constant`** ports, as
    /// `(constant slot, coerced `Arg`)` — the value the patch's `config` block sets (e.g. the
    /// voicer's `voices`). The sibling of [`value_overrides`](Self::value_overrides) for the
    /// plan-time surface: sparse, descriptor-default fallback, `Arg`-valued. Routed to `config` on
    /// save, never `inputs`.
    pub constant_overrides: Vec<(usize, Arg)>,
    /// The logical `sample` resource id this node referenced in its document, retained so
    /// `reuben-document`'s `NormalizedDoc::from_graph` can round-trip it on
    /// save. `None` unless the node declared a `sample` slot and named an id. The *decoded bytes* are
    /// bound out-of-band and do not round-trip; only this id does.
    pub sample_id: Option<String>,
    /// The logical `voice` instrument-resource id this node referenced, retained for the
    /// same save round-trip as [`sample_id`](Self::sample_id). `None` unless the node declared a
    /// `voice` slot and named an id.
    ///
    /// A `subpatch` node's `patch` id has no counterpart here: the node **dissolves** at build
    /// (nesting P4) — its child's nodes are spliced in with prefixed addresses and no
    /// node survives to carry the reference. A built graph is the *flattened* instrument;
    /// reference-preserving save is deferred to the library thread (P7).
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
/// (reshaped into **pipes**). Each external **input** name maps to
/// the `in` port of the loader-built pipe node it minted (`in` → `/in`); whatever feeds the
/// boundary lands there, and internal consumers wire from the pipe's output. Each **output**
/// name maps to the internal `(node, output port)` that feeds it. Empty unless the document
/// declares an `interface`. Distinct from `control`, which is engine-ignored: this
/// is real wiring the engine binds and type-checks (the Voicer reads it to drive each voice
/// sub-patch's `freq`/`gate` pipes and tap its `audio`/`active`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Interface {
    /// External input name → the pipe node's `(node, input port)` the boundary feeds.
    pub inputs: BTreeMap<String, (NodeKey, usize)>,
    /// External output name → the internal `(node, output port)` that feeds it.
    pub outputs: BTreeMap<String, (NodeKey, usize)>,
    /// Logical **input** channel bindings: input pipe name → the logical input
    /// channel it reads when this graph is played at top level. Inert when nested/hosted (a
    /// splice discards the child's `Interface`, so a binding never reaches hardware from inside
    /// a nest). Consumed by the core input master (P3).
    pub input_channels: BTreeMap<String, usize>,
    /// Logical **output** channel bindings: output pipe name → the logical master
    /// channel it feeds (already applied to [`Graph::outputs`] taps at build; retained here so
    /// save/introspection can reconstruct the entry). Omitted = broadcast.
    pub output_channels: BTreeMap<String, usize>,
    /// Declared **input** names whose internal target went dark — an unavailable nested child.
    /// The port is real in the document but resolves to nothing this load; a
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
    /// channel index this tap feeds; `None` broadcasts to every channel (the
    /// historical mono fan). Summed into the rendered output.
    pub outputs: Vec<(NodeKey, usize, Option<usize>)>,
    /// The resolved `interface` boundary, empty unless declared. Set by the loader's
    /// `NormalizedDoc::build` (in `reuben-document`) after nodes/wires resolve.
    pub interface: Interface,
    /// Derived **logical input width**: max bound input channel + 1 across this
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
        self.add_boxed(address, Box::new(op), Arc::new(T::descriptor()))
    }

    /// Add an already-boxed operator with a handle to its type's descriptor. Used by the instrument
    /// loader, which builds operators from a [`crate::registry`] and hands over a clone of that
    /// entry's handle — so every node of one type points at the registry's single copy. Inputs and
    /// constants default from the descriptor; only author overrides are stored on the node.
    ///
    /// `address` takes an already-shared handle as readily as a `&str`: the loader hands over a
    /// clone from its per-load intern table (see [`Node::address`]), a caller building a graph by
    /// hand hands over a literal and mints one.
    pub fn add_boxed(
        &mut self,
        address: impl Into<Arc<str>>,
        op: Box<dyn Operator>,
        descriptor: Arc<Descriptor>,
    ) -> NodeKey {
        self.nodes.insert(Node {
            address: address.into(),
            op,
            descriptor,
            value_overrides: Vec::new(),
            constant_overrides: Vec::new(),
            sample_id: None,
            voice_id: None,
        })
    }

    /// Override a plan-time **`Constant`** by name, coercing the raw author literal to the
    /// constant's stored [`Arg`] (an `i32` count clamps to its range). Upserts the `(slot, Arg)`
    /// override the patch's `config` block sets and `from_graph` saves back.
    ///
    /// **Silent no-op** if `name` is not a constant or `raw` does not resolve — see
    /// [`set_value`](Self::set_value) for what that obliges a caller to do.
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

    /// Override a settable input's unwired default by name, coercing the raw author
    /// literal to the input's latch [`Arg`]: an `F32` control clamps to its range, an enum resolves a
    /// symbol / index / concrete variant. Upserts the `(port, Arg)` override consumed by
    /// [`Plan::instantiate`](crate::plan::Plan::instantiate).
    ///
    /// **Silent no-op** if `name` is not a settable input or `raw` does not resolve. That silence
    /// is why a caller must hand over the value its own validation produced, not the one the author
    /// wrote: validating one form and passing another leaves the document saying one thing and the
    /// graph playing another, with nothing raised anywhere.
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
    /// (`OUT_*`/`IN_*`) or bare indices (the loader's resolved ordinals).
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

    /// Designate a master output tap feeding a single logical master `channel` —
    /// e.g. a `pan` op's `left`/`right` tapped as channel 0 / 1.
    pub fn tap_output_channel(&mut self, node: NodeKey, port: impl PortIndex, channel: usize) {
        self.outputs.push((node, port.index(), Some(channel)));
    }

    /// A **fresh-state structural copy** of this patch: the same nodes, wires, master taps,
    /// `interface` boundary and author overrides, with every operator box taken through
    /// [`Operator::spawn`] — so each copy starts at zero running state while shared resource
    /// bindings (a decoded sample's `Arc`) ride along rather than being duplicated.
    ///
    /// This is what makes a *reused* child cheap: a document referenced by N `subpatch` nodes, or
    /// a voice patch hosted N times, is built once and copied for the rest.
    ///
    /// Keys are the copy's own. A [`NodeKey`] taken from the original is not merely stale here, it
    /// is *wrong-and-live*: SlotMap keys are per-map, so an original's key resolves against
    /// whatever the copy happens to hold in that slot. Every stored key is therefore remapped
    /// through the insertion rather than carried.
    pub fn spawn_copy(&self) -> Graph {
        let mut nodes = SlotMap::with_capacity_and_key(self.nodes.len());
        let mut remap: SecondaryMap<NodeKey, NodeKey> =
            SecondaryMap::with_capacity(self.nodes.len());
        for (key, n) in &self.nodes {
            let fresh = nodes.insert(Node {
                address: n.address.clone(),
                op: n.op.spawn(),
                descriptor: Arc::clone(&n.descriptor),
                value_overrides: n.value_overrides.clone(),
                constant_overrides: n.constant_overrides.clone(),
                sample_id: n.sample_id.clone(),
                voice_id: n.voice_id.clone(),
            });
            remap.insert(key, fresh);
        }
        let at = |k: NodeKey| remap[k];
        Graph {
            nodes,
            connections: self
                .connections
                .iter()
                .map(|c| Connection {
                    src: at(c.src),
                    src_port: c.src_port,
                    dst: at(c.dst),
                    dst_port: c.dst_port,
                })
                .collect(),
            outputs: self
                .outputs
                .iter()
                .map(|(k, port, channel)| (at(*k), *port, *channel))
                .collect(),
            interface: Interface {
                inputs: self
                    .interface
                    .inputs
                    .iter()
                    .map(|(name, (k, port))| (name.clone(), (at(*k), *port)))
                    .collect(),
                outputs: self
                    .interface
                    .outputs
                    .iter()
                    .map(|(name, (k, port))| (name.clone(), (at(*k), *port)))
                    .collect(),
                input_channels: self.interface.input_channels.clone(),
                output_channels: self.interface.output_channels.clone(),
                dark_inputs: self.interface.dark_inputs.clone(),
                dark_outputs: self.interface.dark_outputs.clone(),
            },
            input_channels_width: self.input_channels_width,
        }
    }

    /// Find a node by its OSC address. Used by the loader to bind resources to the right
    /// node after the graph is built.
    pub fn find(&self, address: &str) -> Option<NodeKey> {
        self.nodes
            .iter()
            .find(|(_, n)| &*n.address == address)
            .map(|(k, _)| k)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor::{Descriptor, Port};
    use crate::operator::{Io, Operator};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// An operator that counts its own `spawn` calls, so a test can tell a box that came through
    /// [`Operator::spawn`] from one built any other way. It carries a `binding` across the call
    /// the way a resource-holding operator must; that a copy also starts with *fresh* state is an
    /// audible claim, proven at the render seam (`tests/nesting.rs`), not here.
    struct Counted {
        binding: Arc<u32>,
        spawns: Arc<AtomicUsize>,
    }

    impl Operator for Counted {
        fn descriptor() -> Descriptor {
            Descriptor {
                type_name: "counted",
                inputs: vec![Port::f32_buffer("in")],
                outputs: vec![Port::f32_buffer("out")],
                constants: vec![],
                resources: vec![],
            }
        }
        fn process(&mut self, _io: &mut Io) {}
        fn spawn(&self) -> Box<dyn Operator> {
            self.spawns.fetch_add(1, Ordering::Relaxed);
            Box::new(Counted {
                binding: Arc::clone(&self.binding),
                spawns: Arc::clone(&self.spawns),
            })
        }
    }

    fn node(
        g: &mut Graph,
        desc: &Arc<Descriptor>,
        address: &str,
        spawns: &Arc<AtomicUsize>,
    ) -> NodeKey {
        g.nodes.insert(Node {
            address: Arc::from(address),
            op: Box::new(Counted {
                binding: Arc::new(1),
                spawns: Arc::clone(spawns),
            }),
            descriptor: Arc::clone(desc),
            value_overrides: vec![],
            constant_overrides: vec![],
            sample_id: None,
            voice_id: None,
        })
    }

    /// A two-node graph carrying one of everything [`Graph::spawn_copy`] has to reproduce.
    fn template(spawns: &Arc<AtomicUsize>) -> Graph {
        let mut g = Graph::new();
        let desc = Arc::new(Counted::descriptor());
        // Burn a slot before the real nodes: a removal bumps the slot's generation, so this
        // graph's keys are ones a freshly filled SlotMap never hands out. Without it the original
        // and its copy hand out *identical* keys, and a copy that carried its parent's keys
        // verbatim would satisfy every assertion below while being wrong by construction.
        let scratch = node(&mut g, &desc, "/scratch", spawns);
        g.nodes.remove(scratch);
        let a = node(&mut g, &desc, "/a", spawns);
        let b = node(&mut g, &desc, "/b", spawns);
        g.nodes[a].value_overrides.push((0, Arg::F32(0.25)));
        g.nodes[a].constant_overrides.push((0, Arg::I32(4)));
        g.nodes[a].sample_id = Some("kick".to_string());
        g.nodes[b].voice_id = Some("tone".to_string());
        g.connect(a, 0usize, b, 0usize);
        g.tap_output_channel(b, 0usize, 1);
        g.interface.inputs.insert("freq".to_string(), (a, 0));
        g.interface.outputs.insert("audio".to_string(), (b, 0));
        g.interface.input_channels.insert("freq".to_string(), 2);
        g.interface.output_channels.insert("audio".to_string(), 1);
        g.interface.dark_inputs.insert("gone".to_string());
        g.interface.dark_outputs.insert("also-gone".to_string());
        g.input_channels_width = 3;
        g
    }

    /// Every key-bearing part of a graph, written out in **address** terms — so two graphs that
    /// describe the same patch under different keys compare equal, and one that remapped a key
    /// onto the wrong node does not.
    fn shape(g: &Graph) -> Vec<String> {
        let addr = |k: NodeKey| &*g.nodes[k].address;
        let mut lines: Vec<String> = Vec::new();
        for c in &g.connections {
            lines.push(format!(
                "wire {}:{} -> {}:{}",
                addr(c.src),
                c.src_port,
                addr(c.dst),
                c.dst_port
            ));
        }
        for (k, port, channel) in &g.outputs {
            lines.push(format!("tap {}:{port} -> {channel:?}", addr(*k)));
        }
        for (name, (k, port)) in &g.interface.inputs {
            lines.push(format!("in {name} -> {}:{port}", addr(*k)));
        }
        for (name, (k, port)) in &g.interface.outputs {
            lines.push(format!("out {name} <- {}:{port}", addr(*k)));
        }
        lines
    }

    #[test]
    fn spawn_copy_reproduces_the_patch_under_its_own_keys() {
        let spawns = Arc::new(AtomicUsize::new(0));
        let original = template(&spawns);
        let copy = original.spawn_copy();

        assert_eq!(
            shape(&copy),
            shape(&original),
            "same patch, in address terms"
        );
        assert_eq!(copy.input_channels_width, original.input_channels_width);
        assert_eq!(
            copy.interface.input_channels,
            original.interface.input_channels
        );
        assert_eq!(
            copy.interface.output_channels,
            original.interface.output_channels
        );
        assert_eq!(copy.interface.dark_inputs, original.interface.dark_inputs);
        assert_eq!(copy.interface.dark_outputs, original.interface.dark_outputs);

        // The template burns a slot before `/a` (see there), so the original's `/a` key carries a
        // bumped generation the copy's freshly filled map never issues. That is the one key
        // provably remapped rather than coincidentally equal — and every structural claim above
        // rides on the remap, since a key carried over verbatim resolves to nothing here.
        assert_ne!(
            copy.find("/a").expect("/a"),
            original.find("/a").expect("/a"),
            "a copy's keys are its own"
        );

        // Author overrides ride along — the copy has to render what the original would.
        let a = copy.find("/a").expect("/a");
        let b = copy.find("/b").expect("/b");
        assert_eq!(copy.nodes[a].value_overrides, vec![(0, Arg::F32(0.25))]);
        assert_eq!(copy.nodes[a].constant_overrides, vec![(0, Arg::I32(4))]);
        assert_eq!(copy.nodes[a].sample_id.as_deref(), Some("kick"));
        assert_eq!(copy.nodes[b].voice_id.as_deref(), Some("tone"));
        assert!(Arc::ptr_eq(
            &copy.nodes[a].descriptor,
            &original.nodes[original.find("/a").unwrap()].descriptor
        ));
    }

    #[test]
    fn spawn_copy_takes_every_operator_box_through_spawn() {
        // The whole state story rests on this: `spawn` is the one contract that resets running
        // state while carrying a resource binding forward, so a copy that built its boxes any
        // other way would either inherit the template's charge or drop its sample.
        let spawns = Arc::new(AtomicUsize::new(0));
        let original = template(&spawns);
        let copy = original.spawn_copy();
        assert_eq!(
            spawns.load(Ordering::Relaxed),
            copy.nodes.len(),
            "one spawn per node, and no other route"
        );
    }
}
