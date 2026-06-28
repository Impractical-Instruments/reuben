//! Plan — the static execution image produced by Instantiate (ADR-0009, ADR-0010, ADR-0030).
//!
//! Instantiate consumes a [`Graph`], topologically orders its nodes, instantiates each operator
//! (config-fixed, off the hot path), and assigns every Buffer output port a slot in the edge-buffer
//! arena. The result is immutable and is what [`crate::render`] executes per block. Polyphony is
//! hosted inside the Voicer (N voice sub-plans summed), not fanned out across engine Lanes — the
//! Lane model is gone (ADR-0032).
//!
//! The seven former carriers collapse to one model (ADR-0030): every input port has a held
//! [`Arg`] **latch** (the ZOH value `io.input::<T>` reads), Buffer inputs additionally carry a dense
//! arena buffer, and every output port either owns arena buffers (a Buffer/signal output) or
//! routes emitted Messages to downstream input ports (a message output). The old context-arena /
//! enum-latch / param lanes and the separate `msg_targets` / `ctx_targets` routing are unified.

use slotmap::SecondaryMap;

use crate::config::AudioConfig;
use crate::descriptor::{Descriptor, Port, PortType};
use crate::graph::{Graph, NodeKey};
use crate::message::{Arg, Message};
use crate::operator::Operator;
use crate::vocab::harmony::Harmony;

/// The **form** a wire carries (ADR-0031), *declared* by the port's [`PortType`] — not inferred
/// from the graph:
///
/// - **Signal** — a dense per-sample buffer ([`Buffer`](PortType::F32Buffer) audio), read via
///   `io.input::<&[f32]>`.
/// - **Value** — a latched single value (scalar / enum / `Harmony`): its last value is held (ZOH)
///   and read via `io.input::<T>`; a mid-block change block-slices so it is constant per `process` call.
/// - **Event** — an unlatched multi-valued stream (`Note`), delivered frame-stamped via `io.input::<Note>`
///   and *not* sliced.
///
/// The two sparse forms fall out of two axes — *latched?* and *single-valued?*: Value is latched ∧
/// single, Event is unlatched ∧ multi. The other two combinations are nonsense, so the set is
/// closed at three.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortKind {
    Signal,
    Value,
    Event,
}

/// Classify an input/output port into its declared [`PortKind`] form (ADR-0031). An `F32Buffer` is a
/// Signal (a dense per-sample carrier); a vocab declared `is_event` (a `Note` stream) is an Event;
/// everything latched — a bare `F32`, enums, `Harmony`, `I32`, `Str` — is a Value. The Phase-B flip
/// (ADR-0031): `F32 ⇒ Value`. A port that must carry a continuous signal with a scalar default
/// (`oscillator.freq`, `filter.cutoff`, `envelope.cv`) is declared `f32_buffer`-with-meta, so it
/// stays Signal and materializes from its default; every remaining bare `f32` is a held Value.
///
/// Event-ness reads the type's [`is_event`](PortType::Vocab) flag rather than the vocab name, so a
/// second held struct vocab is classified by its declaration, not silently treated as an Event.
pub fn port_kind(p: &Port) -> PortKind {
    match &p.ty {
        PortType::F32Buffer => PortKind::Signal,
        PortType::Vocab { is_event: true, .. } => PortKind::Event,
        _ => PortKind::Value,
    }
}

/// The seed [`Arg`] for an input port's latch at Instantiate (ADR-0030): an `F32` control's
/// (override-or-default) value, an enum's (override-or-default) variant, the default `Harmony`,
/// or a harmless placeholder for ports with no held value (`Note`, `Buffer`).
fn seed_latch(
    p: &Port,
    port: usize,
    input_overrides: &[(usize, f32)],
    enum_overrides: &[(usize, usize)],
) -> Arg {
    match &p.ty {
        // F32 (Value-bound scalar control) and an F32Buffer *carrying meta* (ADR-0031 decision (a):
        // a signal port with a scalar default, e.g. `oscillator.freq`) both seed from their
        // override-or-default. A bare F32Buffer (no meta — audio) has no held value and falls to the
        // placeholder arm below.
        PortType::F32 | PortType::F32Buffer if p.meta.is_some() => {
            let v = input_overrides
                .iter()
                .find(|(po, _)| *po == port)
                .map(|(_, v)| *v)
                .unwrap_or_else(|| p.meta.as_ref().map(|m| m.default).unwrap_or(0.0));
            Arg::F32(v)
        }
        PortType::Vocab {
            enum_meta: Some(e), ..
        } => {
            let idx = enum_overrides
                .iter()
                .find(|(po, _)| *po == port)
                .map(|(_, i)| *i)
                .unwrap_or(e.default);
            e.resolve_arg(&Arg::I32(idx as i32))
                .unwrap_or(Arg::I32(idx as i32))
        }
        PortType::Vocab { name, .. } if *name == "Harmony" => Arg::Harmony(Harmony::default()),
        PortType::I32 => Arg::I32(0),
        PortType::Str => Arg::Str(String::new()),
        // Note (stream) / Buffer (dense): no held value — a placeholder `io.input::<T>` never decodes.
        _ => Arg::F32(0.0),
    }
}

/// A node in execution order, with its arena buffer wiring resolved.
pub struct PlanNode {
    pub address: String,
    /// The operator instance (single-element `Vec`; the per-Lane fan-out is gone — ADR-0032).
    pub ops: Vec<Box<dyn Operator>>,
    pub descriptor: Descriptor,
    /// For each input port (full input-port order): the source's arena buffer index (a one-element
    /// `Vec`), or `None`. `Some` only for a [`Buffer`](PortType::F32Buffer) input — either wired to a
    /// Buffer source (zero-copy share) or fed by a scalar source and so **materialized** (a dedicated
    /// scratch buffer, see `materialize`). Held / Stream inputs carry no buffer (`None`).
    pub inputs: Vec<Option<Vec<usize>>>,
    /// Per input port (full input-port order): its [`PortKind`], precomputed at Instantiate so the
    /// hot message-routing path reads the bucket directly instead of re-deriving it from the port
    /// descriptor (ADR-0030 — `port_kind` does a `Vocab` name comparison that the audio thread
    /// should not repeat per routed message).
    pub input_kinds: Vec<PortKind>,
    /// Materialized inputs (ADR-0030): `(input port, scratch arena buffer)` for each Buffer input
    /// fed by a scalar source — the one implicit `F32`→`Buffer` ZOH bridge. The engine fills the
    /// buffer per block from `latch[port]` (decoded via `Arg::as_f32`), writing mid-block changes at
    /// their frame.
    pub materialize: Vec<(usize, usize)>,
    /// Per `materialize` entry (same index): `true` once the scratch buffer holds the latch
    /// uniformly across the block, so a held-unchanged input can skip its refill (ADR-0030).
    /// Carried across blocks. Starts `false` so the first block fills.
    pub materialize_clean: Vec<bool>,
    /// The held [`Arg`] latch per input port (ADR-0030) — the unified ZOH value `io.input::<T>` reads,
    /// collapsing the former Harmony / enum / param lanes into one. Length = input count; seeded
    /// from each input's default / author override, `Copy`-normalized, carried across blocks. Render
    /// block-slices Held ports at change frames and updates the slot there.
    pub latch: Vec<Arg>,
    /// Per-input `varying` hint (ADR-0030), in input-port order — preallocated here and reused every
    /// block (no audio-thread alloc). All-`true`; Render rewrites only materialized ports each block
    /// (`false` ⇒ held unchanged this block).
    pub varying: Vec<bool>,
    /// For each **signal (Buffer) output** port — in signal-output ordinal order — its arena
    /// buffer index (a one-element `Vec`). [`crate::operator::Io::output`] (`<&mut [f32]>`) indexes
    /// this by the all-outputs port index the contract macro emits, which equals the signal ordinal
    /// **only when signal outputs precede message outputs in the declaration** (the invariant every
    /// operator holds; e.g. `envelope` declares `cv` before `active`).
    pub outputs: Vec<Vec<usize>>,
    /// Message-edge routing (ADR-0014, ADR-0030, ADR-0032): indexed by **all-outputs port index**
    /// (the index [`crate::operator::Io::output`] passes; `emit.port` is that index). A signal output
    /// has an empty slot; a message output carries the `(dst node, dst input port)` pairs its
    /// emissions are delivered to. Full-index (not compacted to message ordinals) so an operator can
    /// interleave a signal output and a message output (`envelope.cv` + `envelope.active`). Unifies
    /// the former `msg_targets` (Note edges) and `ctx_targets` (Harmony edges): a published Harmony
    /// is just a Message to a Held input. The dst input port's [`PortKind`] decides how it lands.
    pub out_targets: Vec<Vec<(usize, usize)>>,
}

/// One outbound (OSC-out) sink (ADR-0026, ADR-0030): a node whose emitted Messages leave the
/// graph past the boundary. The engine drains the node's emissions each block into the outbound
/// list, stamping the node's `address` (one sink = one address) and the block-absolute frame;
/// native encodes + UDP-sends them. The marker is the operator type (`osc_out`) — the one
/// operator whose output is the external edge.
pub struct OutboundTap {
    /// The sink node's index in execution order.
    pub node: usize,
    /// The outbound OSC address — the node's address, stamped on every drained Message.
    pub address: String,
}

/// One resolved `interface` **output** (ADR-0032 §4): a voice patch's named boundary output, so a
/// host (`Voicer`) reads it by name + kind exactly as an operator reads a port — a Signal output
/// from its arena buffer, a Value output from a captured scalar. Resolved at instantiate from
/// [`Graph::interface`](crate::graph::Graph::interface); empty for a plan with no `interface`.
pub struct InterfaceOutput {
    /// External boundary name (e.g. `audio`, `active`).
    pub name: String,
    /// Producing node's index in execution order.
    pub node: usize,
    /// Producing output port (all-outputs index).
    pub port: usize,
    /// The port's form: `Signal` (read via `signal_buf`) or `Value` (read via `captured_slot`).
    /// `Event` interface outputs are not supported (nothing consumes them).
    pub kind: PortKind,
    /// `Some(arena buffer index)` for a Signal output — the host reads the rendered buffer there.
    pub signal_buf: Option<usize>,
    /// `Some(index into [`Plan::captured`])` for a Value output — `render_plan` writes the port's
    /// last-emitted scalar there each block (held ZOH across blocks).
    pub captured_slot: Option<usize>,
}

/// One master tap: a tapped port's arena buffers, summed into the master output.
pub struct OutputTap {
    /// Logical master channel this tap feeds (ADR-0026), or `None` to broadcast to every
    /// channel (the historical mono fan).
    pub channel: Option<usize>,
    /// Arena buffer indices of the tapped port; all summed.
    pub buffers: Vec<usize>,
}

/// The immutable execution image.
pub struct Plan {
    pub config: AudioConfig,
    /// Nodes in topological execution order.
    pub nodes: Vec<PlanNode>,
    /// Total number of edge buffers in the arena.
    pub num_buffers: usize,
    /// Length `num_buffers`: `true` at each arena slot that is a materialize **scratch** buffer
    /// (ADR-0030). Render skips these in its per-block "fresh edge buffers" clear, so a held input's
    /// buffer persists and need not be re-written every block (see `materialize_clean`).
    pub materialize_scratch_mask: Vec<bool>,
    /// Master taps, summed into the per-channel master output (ADR-0026).
    pub output_taps: Vec<OutputTap>,
    /// Outbound (OSC-out) sinks, drained past the boundary each block (ADR-0026, ADR-0030).
    pub outbound_taps: Vec<OutboundTap>,
    /// Resolved `interface` outputs (ADR-0032 §4), for a host operator to read this plan's boundary
    /// outputs by name + kind. Empty unless the document declared an `interface`.
    pub interface_outputs: Vec<InterfaceOutput>,
    /// One slot per Value `interface` output (parallel to the `captured_slot` indices in
    /// `interface_outputs`): the port's last-emitted scalar, held ZOH across blocks (seeded `0.0`).
    /// `render_plan` updates it when the port emits; the host reads it post-render.
    pub captured: Vec<f32>,
}

/// Why Instantiate failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// The graph has a cycle (feedback needs an explicit unit-delay; deferred).
    Cycle,
    /// A wire's two declared forms (ADR-0031) cannot connect: a Signal feeding a Value input (no
    /// implicit sample-and-hold), or an Event mismatched against a Signal/Value. `src`/`dst` name
    /// the offending `node.port`; `reason` says what is missing (e.g. the explicit converter op).
    FormMismatch {
        src: String,
        dst: String,
        reason: String,
    },
}

impl Plan {
    /// Convert an inbound OSC datagram — an address plus a flat list of primitive `Arg`s — into the
    /// single typed [`Message`] it routes to, driven by the **destination port's Arg type**
    /// (ADR-0030, the boundary). Matches the address to a node by prefix and to an input port by
    /// name (the same rule the render path uses), then calls [`crate::boundary::osc_in_arg`] with
    /// that [`Port`] to type the flat args (the port's `meta` is what lets a scalar-defaulted
    /// `f32_buffer` control like `djfilter.position` cross, while bare audio does not). `None` if no
    /// node/port matches or the args don't fit the port. External OSC carries no timestamp, so the
    /// Message is stamped frame 0 ("now").
    pub fn osc_in_message(&self, address: &str, args: &[Arg]) -> Option<Message> {
        for node in &self.nodes {
            let Some(local) = crate::render::local_address(address, &node.address) else {
                continue;
            };
            // The address targets this node; a message routes to at most one node, so this node
            // decides the outcome whether or not a port matches.
            let arg = node
                .descriptor
                .inputs
                .iter()
                .find(|p| p.name == local)
                .and_then(|p| crate::boundary::osc_in_arg(p, args))?;
            return Some(Message::new(address, arg, 0));
        }
        None
    }

    /// Instantiate a Graph into an executable Plan (the construction sub-step of a Swap).
    pub fn instantiate(mut graph: Graph, mut config: AudioConfig) -> Result<Plan, PlanError> {
        let order = topo_order(&graph)?;
        check_wire_forms(&graph)?;

        // Logical master width is derived from the instrument, not the device (ADR-0026):
        // the highest referenced channel index + 1, floored to stereo so a mono patch still
        // presents two channels. A broadcast tap (`None`) imposes no width on its own.
        config.channels = graph
            .outputs
            .iter()
            .filter_map(|(_, _, ch)| ch.map(|c| c + 1))
            .max()
            .unwrap_or(0)
            .max(AudioConfig::MIN_CHANNELS);

        // 1. Assign every (node, Buffer output port) a unique arena buffer index. A message output
        // (Note / Harmony / scalar control out) carries no Signal data — events arrive via routing
        // (ADR-0014) — so it gets an empty buffer list (its emptiness is the marker that an edge into
        // it must materialize rather than share). The inner `Vec` is a single buffer per signal port
        // (the per-Lane dimension is gone — polyphony is hosted inside the Voicer, ADR-0032).
        let mut next_buffer = 0usize;
        let mut out_buffers: SecondaryMap<NodeKey, Vec<Vec<usize>>> = SecondaryMap::new();
        for (key, node) in &graph.nodes {
            let ports = node
                .descriptor
                .outputs
                .iter()
                .map(|p| {
                    if matches!(p.ty, PortType::F32Buffer) {
                        let i = next_buffer;
                        next_buffer += 1;
                        vec![i]
                    } else {
                        Vec::new()
                    }
                })
                .collect();
            out_buffers.insert(key, ports);
        }

        let output_taps = graph
            .outputs
            .iter()
            .map(|(k, p, channel)| OutputTap {
                channel: *channel,
                buffers: out_buffers[*k][*p].clone(),
            })
            .collect();

        // Node index in execution order, for resolving Message-edge targets to Plan indices.
        let mut index_of: SecondaryMap<NodeKey, usize> = SecondaryMap::new();
        for (i, key) in order.iter().enumerate() {
            index_of.insert(*key, i);
        }

        // 2. Build PlanNodes in execution order.
        let mut nodes = Vec::with_capacity(order.len());
        // Arena slots that are materialize scratch (ADR-0030); Render skips them in its per-block
        // clear so held inputs persist. Collected as buffers are assigned below.
        let mut scratch_buffers: Vec<usize> = Vec::new();
        for key in &order {
            let descriptor = &graph.nodes[*key].descriptor;
            let overrides = &graph.nodes[*key].input_overrides;
            let enum_overrides = &graph.nodes[*key].enum_overrides;
            let n_inputs = descriptor.inputs.len();

            // The unified held latch per input port (ADR-0030): seeded from each input's default or
            // an author override; `Copy`-normalized; carried across blocks.
            let latch: Vec<Arg> = descriptor
                .inputs
                .iter()
                .enumerate()
                .map(|(port, p)| seed_latch(p, port, overrides, enum_overrides))
                .collect();

            // Input buffer wiring (ADR-0030): a Buffer input wired to a Buffer source shares its
            // arena buffers (zero-copy); a Buffer input wired to a scalar source materializes a
            // scratch buffer (the one implicit ZOH bridge); Held / Stream inputs carry no buffer.
            let mut inputs: Vec<Option<Vec<usize>>> = Vec::with_capacity(n_inputs);
            let mut materialize: Vec<(usize, usize)> = Vec::new();
            let varying: Vec<bool> = vec![true; n_inputs];
            // Classify every input port once (ADR-0030): the routing kind feeds both the buffer
            // wiring below and the per-node `input_kinds` the hot router reads each block.
            let input_kinds: Vec<PortKind> = descriptor.inputs.iter().map(port_kind).collect();
            for (port, &kind) in input_kinds.iter().enumerate() {
                // Buffer and F32 (float control) inputs both present a per-sample buffer to
                // `io.input::<&[f32]>` (ADR-0030): wired to a Buffer source they share it zero-copy;
                // otherwise (unwired, or fed by a scalar) the engine materializes a scratch filled
                // ZOH from the latch. Vocab inputs (enum / Note / Harmony) carry no buffer — they
                // are read via `io.input::<T>` / `io.input::<Note>`.
                if kind != PortKind::Signal {
                    inputs.push(None);
                    continue;
                }
                let wired = graph
                    .connections
                    .iter()
                    .find(|c| c.dst == *key && c.dst_port == port)
                    .map(|c| out_buffers[c.src][c.src_port].clone());
                match wired {
                    // Wired to a Buffer (audio) source: share its buffers zero-copy.
                    Some(bufs) if !bufs.is_empty() => inputs.push(Some(bufs)),
                    // Wired to a scalar (message) source, or unwired: materialize a scratch the
                    // engine fills ZOH from the latch + routed scalar messages.
                    _ => {
                        let buf = next_buffer;
                        next_buffer += 1;
                        materialize.push((port, buf));
                        scratch_buffers.push(buf);
                        inputs.push(Some(vec![buf]));
                    }
                }
            }

            // Signal (Buffer) outputs, in signal-output ordinal order — the index `io.output::<&mut [f32]>`
            // uses.
            let outputs: Vec<Vec<usize>> = descriptor
                .outputs
                .iter()
                .enumerate()
                .filter(|(_, p)| matches!(p.ty, PortType::F32Buffer))
                .map(|(port, _)| out_buffers[*key][port].clone())
                .collect();

            // Message-edge targets, indexed by **all-outputs port index** — the index
            // [`crate::operator::Io::output`] passes (the contract macro numbers outputs sequentially
            // across kinds, ADR-0030; `emit.port` is that index). A signal (`F32Buffer`) output never
            // emits Messages, so its slot is empty; a message output carries the `(dst node, dst input
            // port)` pairs wired to it. Indexing by the full output index (not a compacted
            // message-ordinal) is what lets an operator interleave a signal output and a message
            // output — e.g. `envelope` (`cv` signal + `active` message) for ADR-0032 voice-liveness.
            let out_targets: Vec<Vec<(usize, usize)>> = descriptor
                .outputs
                .iter()
                .enumerate()
                .map(|(port, p)| {
                    if matches!(p.ty, PortType::F32Buffer) {
                        return Vec::new();
                    }
                    graph
                        .connections
                        .iter()
                        .filter(|c| c.src == *key && c.src_port == port)
                        .map(|c| (index_of[c.dst], c.dst_port))
                        .collect()
                })
                .collect();

            let node = graph.nodes.remove(*key).expect("key from topo order");
            // Config-dependent runtime state (ADR-0032 §3): the Voicer instantiates its bound voice
            // graphs into per-voice sub-plans here, where `config` is fixed and we are off the hot
            // path.
            let mut op = node.op;
            op.on_instantiate(&config)?;
            let ops = vec![op];

            let materialize_clean = vec![false; materialize.len()];
            nodes.push(PlanNode {
                address: node.address,
                ops,
                descriptor: node.descriptor,
                inputs,
                input_kinds,
                materialize,
                materialize_clean,
                latch,
                varying,
                outputs,
                out_targets,
            });
        }

        let mut materialize_scratch_mask = vec![false; next_buffer];
        for b in scratch_buffers {
            materialize_scratch_mask[b] = true;
        }

        // Outbound sinks (ADR-0030): the `osc_out` operator is the one whose output is the external
        // edge, so its emissions drain past the boundary stamped with the node's address.
        let outbound_taps = nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.descriptor.type_name == "osc_out")
            .map(|(i, n)| OutboundTap {
                node: i,
                address: n.address.clone(),
            })
            .collect();

        // Resolve the `interface` outputs (ADR-0032 §4) so a host reads this plan's boundary by name
        // + kind: a Signal output from its arena buffer, a Value output from a captured scalar slot.
        // `graph.interface` survives the `graph.nodes.remove` drain above (separate field).
        let mut captured_len = 0usize;
        let interface_outputs: Vec<InterfaceOutput> = graph
            .interface
            .outputs
            .iter()
            .map(|(name, &(key, port))| {
                let node = index_of[key];
                let is_signal =
                    matches!(nodes[node].descriptor.outputs[port].ty, PortType::F32Buffer);
                let (kind, signal_buf, captured_slot) = if is_signal {
                    (PortKind::Signal, Some(out_buffers[key][port][0]), None)
                } else {
                    let slot = captured_len;
                    captured_len += 1;
                    (PortKind::Value, None, Some(slot))
                };
                InterfaceOutput {
                    name: name.clone(),
                    node,
                    port,
                    kind,
                    signal_buf,
                    captured_slot,
                }
            })
            .collect();

        Ok(Plan {
            config,
            nodes,
            num_buffers: next_buffer,
            materialize_scratch_mask,
            output_taps,
            outbound_taps,
            interface_outputs,
            captured: vec![0.0; captured_len],
        })
    }

    /// The arena buffer index of a Signal `interface` output (ADR-0032 §4), or `None` if there is no
    /// such named output or it is not a Signal. A host reads the rendered buffer at this index.
    pub fn interface_signal_buf(&self, name: &str) -> Option<usize> {
        self.interface_outputs
            .iter()
            .find(|o| o.name == name)
            .and_then(|o| o.signal_buf)
    }

    /// The [`captured`](Plan::captured) slot index of a Value `interface` output (ADR-0032 §4), or
    /// `None` if there is no such named output or it is not a Value. A host reads `captured[slot]`
    /// post-render for the port's held value.
    pub fn interface_value_slot(&self, name: &str) -> Option<usize> {
        self.interface_outputs
            .iter()
            .find(|o| o.name == name)
            .and_then(|o| o.captured_slot)
    }
}

/// The planner's only form job (ADR-0031): a **local per-wire check**. For each connection, compare
/// the source output's declared form against the destination input's and reject the illegal
/// crossings — there is no topological solver, no propagation. The legal combinations are
/// like→like (`Signal→Signal`, `Value→Value`, `Event→Event`) and the one implicit coercion
/// `Value→Signal` (materialized downstream). Everything else is a hard error: `Signal→Value` needs
/// an explicit sig→val converter, and any `Event` mismatch needs an explicit latch / change-detect.
fn check_wire_forms(graph: &Graph) -> Result<(), PlanError> {
    use PortKind::{Event, Signal, Value};
    for c in &graph.connections {
        let src_node = &graph.nodes[c.src];
        let dst_node = &graph.nodes[c.dst];
        let Some(src) = src_node.descriptor.outputs.get(c.src_port) else {
            continue;
        };
        let Some(dst) = dst_node.descriptor.inputs.get(c.dst_port) else {
            continue;
        };
        let reason = match (port_kind(src), port_kind(dst)) {
            // like→like, and the one implicit coercion Value→Signal (materialized at the sink).
            (Signal, Signal) | (Value, Value) | (Event, Event) | (Value, Signal) => continue,
            // An enum (Value-only) sink has no numeric converter — say so, rather than dangle the
            // envelope-follower/quantizer hint that only fits a numeric Value target.
            (Signal, Value) if dst.enum_meta().is_some() => format!(
                "Signal→enum '{}': an enum input takes a discrete choice, not a per-sample \
                signal — no converter exists",
                dst.name
            ),
            (Signal, Value) => "Signal→Value: no implicit sample-and-hold; wire an explicit \
                sig→val converter (envelope follower / quantizer)"
                .to_string(),
            (Event, Signal) | (Event, Value) => {
                "Event→Signal/Value: needs an explicit latch / change-detect / converter op"
                    .to_string()
            }
            (Signal, Event) | (Value, Event) => {
                "Signal/Value→Event: illegal; an Event port takes only an Event source".to_string()
            }
        };
        return Err(PlanError::FormMismatch {
            src: format!("{}.{}", src_node.address, src.name),
            dst: format!("{}.{}", dst_node.address, dst.name),
            reason,
        });
    }
    Ok(())
}

/// Kahn topological sort; deterministic given graph key order. Errors on cycle.
fn topo_order(graph: &Graph) -> Result<Vec<NodeKey>, PlanError> {
    let mut indegree: SecondaryMap<NodeKey, usize> =
        graph.nodes.keys().map(|k| (k, 0usize)).collect();
    for c in &graph.connections {
        indegree[c.dst] += 1;
    }

    // Seed with zero-indegree nodes in stable key order.
    let mut queue: Vec<NodeKey> = graph.nodes.keys().filter(|k| indegree[*k] == 0).collect();
    let mut order = Vec::with_capacity(graph.nodes.len());

    while let Some(k) = queue.pop() {
        order.push(k);
        for c in graph.connections.iter().filter(|c| c.src == k) {
            let d = &mut indegree[c.dst];
            *d -= 1;
            if *d == 0 {
                queue.push(c.dst);
            }
        }
    }

    if order.len() != graph.nodes.len() {
        return Err(PlanError::Cycle);
    }
    Ok(order)
}

#[cfg(test)]
mod port_kind_tests {
    use super::{port_kind, PortKind};
    use crate::descriptor::Port;
    use crate::vocab::SnapDir;

    // The fix for #107: event-ness is read from the type's `is_event` flag, not a `name == "Harmony"`
    // check. A held struct vocab is a Value; only a declared-event vocab (`Note`) is an Event — so a
    // second held struct vocab would be classified by its declaration, never silently as an Event.
    #[test]
    fn struct_vocab_event_ness_follows_the_flag_not_the_name() {
        assert_eq!(port_kind(&Port::note("in")), PortKind::Event);
        assert_eq!(port_kind(&Port::harmony("in")), PortKind::Value);
        // A hypothetical second held struct vocab (declared not-an-event) is a Value, not an Event.
        assert_eq!(
            port_kind(&Port::vocab("ctx", "SomeHeldVocab", false)),
            PortKind::Value
        );
        // An enum vocab is always a latched Value.
        assert_eq!(
            port_kind(&Port::enumerated(SnapDir::enum_meta("dir"))),
            PortKind::Value
        );
    }
}
