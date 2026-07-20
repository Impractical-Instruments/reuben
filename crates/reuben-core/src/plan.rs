//! Plan — the static execution image produced by Instantiate.
//!
//! Instantiate consumes a [`Graph`], topologically orders its nodes, instantiates each operator
//! (config-fixed, off the hot path), and assigns every Buffer output port a slot in the edge-buffer
//! arena. The result is immutable and is what [`crate::render`] executes per block. Polyphony is
//! hosted inside the Voicer (N voice sub-plans summed), not fanned out across engine Lanes — the
//! Lane model is gone.
//!
//! The seven former carriers collapse to one model: every input port has a held
//! [`Arg`] **latch** (the ZOH value a held-handle `io.read` sees), Buffer inputs additionally carry a dense
//! arena buffer, and every output port either owns arena buffers (a Buffer/signal output) or
//! routes emitted Messages to downstream input ports (a message output). The old context-arena /
//! enum-latch / param lanes and the separate `msg_targets` / `ctx_targets` routing are unified.
//!
//! see rules: execution-runtime

use slotmap::SecondaryMap;

use crate::config::AudioConfig;
use crate::descriptor::{Descriptor, Port, PortType};
use crate::graph::{Connection, Graph, NodeKey};
use crate::message::{Arg, Message};
use crate::operator::Operator;
use crate::vocab::harmony::Harmony;
use crate::vocab::pitch::Pitch;

/// The **form** a wire carries, *declared* by the port's [`PortType`] — not inferred
/// from the graph:
///
/// - **Signal** — a dense per-sample buffer ([`Buffer`](PortType::F32Buffer) audio), read via
///   `io.read` on a `SignalF32` handle.
/// - **Value** — a latched single value (scalar / enum / `Harmony`): its last value is held (ZOH)
///   and read via a `Held<T>` handle; a mid-block change block-slices so it is constant per `process` call.
/// - **Event** — an unlatched multi-valued stream (`Note`), delivered frame-stamped via an `Event<Note>`
///   handle and *not* sliced.
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

/// Classify an input/output port into its declared [`PortKind`] form. An `F32Buffer` is a
/// Signal (a dense per-sample carrier); a vocab declared `is_event` (a `Note` stream) is an Event;
/// everything latched — a bare `F32`, enums, `Harmony`, `I32`, `Str` — is a Value. The Phase-B flip
/// is `F32 ⇒ Value`. A port that must carry a continuous signal with a scalar default
/// (`oscillator.freq`, `filter.cutoff`, `envelope.cv`) is declared `f32_buffer`-with-meta, so it
/// stays Signal and materializes from its default; every remaining bare `f32` is a held Value.
///
/// Event-ness reads the type's [`is_event`](PortType::Vocab) flag rather than the vocab name, so a
/// second held struct vocab is classified by its declaration, not silently treated as an Event.
pub(crate) fn port_kind(p: &Port) -> PortKind {
    match &p.ty {
        PortType::F32Buffer => PortKind::Signal,
        PortType::Vocab { is_event: true, .. } => PortKind::Event,
        // A type-agnostic pass-through (issue #141) is an Event stream: routing then delivers the
        // raw `Arg` unlatched and uncoerced, so the sink can re-emit it verbatim.
        PortType::Arg => PortKind::Event,
        _ => PortKind::Value,
    }
}

/// The seed [`Arg`] for an input port's latch at Instantiate: an `F32` control's
/// (override-or-default) value, an enum's (override-or-default) variant, the default `Harmony`,
/// or a harmless placeholder for ports with no held value (`Note`, `Buffer`).
fn seed_latch(p: &Port, port: usize, value_overrides: &[(usize, Arg)]) -> Arg {
    // An author override is already `Port::coerce`-normalized to this port's latch value (an
    // `F32` control's clamped scalar, an enum's concrete variant) — use it verbatim.
    if let Some((_, arg)) = value_overrides.iter().find(|(po, _)| *po == port) {
        return arg.clone();
    }
    match &p.ty {
        // F32 (Value-bound scalar control) and an F32Buffer *carrying meta*
        // (a signal port with a scalar default, e.g. `oscillator.freq`) both seed from their default.
        // A bare F32Buffer (no meta — audio) has no held value and falls to the placeholder arm below.
        PortType::F32 | PortType::F32Buffer if p.meta.is_some() => {
            Arg::F32(p.meta.as_ref().map(|m| m.default).unwrap_or(0.0))
        }
        PortType::Vocab {
            enum_meta: Some(e), ..
        } => e
            .resolve_arg(&Arg::I32(e.default as i32))
            .unwrap_or(Arg::I32(e.default as i32)),
        PortType::Vocab { name, .. } if *name == "Harmony" => Arg::Harmony(Harmony::default()),
        // A held `Pitch` leaf — its own named `Arg`, parallel to `Harmony`. Without this the
        // placeholder arm below would seed `F32(0.0)`, which decodes as a `Pitch` only by the
        // handle read's default-fallback; seed the real tonic so `latch_arg` is correct for any
        // consumer that inspects it directly (an interface-pipe forward, `resolve`).
        PortType::Vocab { name, .. } if *name == "Pitch" => Arg::Pitch(Pitch::default()),
        PortType::I32 { meta } => Arg::I32(meta.as_ref().map(|m| m.default).unwrap_or(0)),
        PortType::Str => Arg::Str("".into()),
        // Note (stream) / Buffer (dense): no held value — a placeholder a held-handle read never decodes.
        _ => Arg::F32(0.0),
    }
}

/// A node in execution order, with its arena buffer wiring resolved.
pub struct PlanNode {
    pub address: String,
    /// The operator instance (single-element `Vec`; the per-Lane fan-out is gone).
    /// `pub(crate)`: the survivor transplant ([`Plan::transplant_survivors`]) is the only writer
    /// that moves these boxes, and it lives on `Plan` — no caller reaches in to swap them (#495).
    pub(crate) ops: Vec<Box<dyn Operator>>,
    pub descriptor: Descriptor,
    /// For each input port (full input-port order): the source's arena buffer index (a one-element
    /// `Vec`), or `None`. `Some` for **every** [`Buffer`](PortType::F32Buffer) input — wired to a
    /// Buffer source (zero-copy share) or **materialized** (a dedicated scratch buffer, see
    /// `materialize`) when fed by a scalar source *or unwired* (an unwired bare buffer fills with
    /// silence from its zero-seeded latch). That totality is the **buffer-presence invariant**:
    /// `process` always sees a dense length-n buffer on a Signal input, so a typed
    /// Signal read indexes directly. Held / Stream inputs carry no buffer (`None`).
    pub inputs: Vec<Option<Vec<usize>>>,
    /// Per input port (full input-port order): its [`PortKind`], precomputed at Instantiate so the
    /// hot message-routing path reads the bucket directly instead of re-deriving it from the port
    /// descriptor (`port_kind` does a `Vocab` name comparison that the audio thread
    /// should not repeat per routed message).
    pub input_kinds: Vec<PortKind>,
    /// Materialized inputs: `(input port, scratch arena buffer)` for each Buffer input
    /// fed by a scalar source — the one implicit `F32`→`Buffer` ZOH bridge. The engine fills the
    /// buffer per block from `latch[port]` (decoded via `Arg::as_f32`), writing mid-block changes at
    /// their frame.
    pub materialize: Vec<(usize, usize)>,
    /// Per `materialize` entry (same index): `true` once the scratch buffer holds the latch
    /// uniformly across the block, so a held-unchanged input can skip its refill.
    /// Carried across blocks. Starts `false` so the first block fills.
    pub materialize_clean: Vec<bool>,
    /// Per `materialize` entry (same index): `true` when the input master wrote device audio
    /// into the scratch **this block** ([`InputTap`]) — the ZOH fill must skip it
    /// (device audio wins over the latch and over routed messages for the block). Set by
    /// [`crate::render::render_plan`]'s tap copy, consumed (reset) by `process_node`. Always
    /// all-`false` for a plan with no channel-bound pipes — the common case pays one branch.
    pub materialize_device_fed: Vec<bool>,
    /// The held [`Arg`] latch per input port — the unified ZOH value a `Held<T>` handle's `io.read` sees,
    /// collapsing the former Harmony / enum / param lanes into one. Length = input count; seeded
    /// from each input's default / author override, `Copy`-normalized, carried across blocks. Render
    /// block-slices Held ports at change frames and updates the slot there.
    pub latch: Vec<Arg>,
    /// Per-input `varying` hint, in input-port order — preallocated here and reused every
    /// block (no audio-thread alloc). All-`true`; Render rewrites only materialized ports each block
    /// (`false` ⇒ held unchanged this block).
    pub varying: Vec<bool>,
    /// For each **signal (Buffer) output** port — in signal-output ordinal order — its arena
    /// buffer index (a one-element `Vec`). [`crate::operator::Io::write`] on a Signal handle indexes
    /// this by the all-outputs port index the contract macro emits, which equals the signal ordinal
    /// **only when signal outputs precede message outputs in the declaration** (the invariant every
    /// operator holds; e.g. `envelope` declares `cv` before `active`).
    pub outputs: Vec<Vec<usize>>,
    /// Message-edge routing: indexed by **all-outputs port index**
    /// (the index an `Out` handle carries into [`crate::operator::Io::write`]; `emit.port` is that index). A signal output
    /// has an empty slot; a message output carries the `(dst node, dst input port)` pairs its
    /// emissions are delivered to. Full-index (not compacted to message ordinals) so an operator can
    /// interleave a signal output and a message output (`envelope.cv` + `envelope.active`). Unifies
    /// the former `msg_targets` (Note edges) and `ctx_targets` (Harmony edges): a published Harmony
    /// is just a Message to a Held input. The dst input port's [`PortKind`] decides how it lands.
    pub out_targets: Vec<Vec<(usize, usize)>>,
}

/// One outbound (OSC-out) sink: a node whose emitted Messages leave the
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

/// One resolved `interface` **output**: a voice patch's named boundary output, so a
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

/// One **dissolved interface pipe**'s live external address. Instantiate collapses
/// a single-consumer pass-through pipe node out of the schedule (see
/// [`dissolve_interface_pipes`]) — the pipe stays an authoring/format concept, not a rendered
/// node — but its minted address (`in` → `/in`) is real boundary surface: the Voicer drives a
/// voice's `freq`/`gate` by message there, and external OSC lands there. This alias keeps that
/// address routable: a message to `<address>/<port.name>` delivers to the rewired consumer
/// `(node, dst_port)` with exactly the normalization the rendered pipe applied (the pipe port
/// types/clamps first, then the consumer port — the same two hops the node made).
pub(crate) struct InputAlias {
    /// The dissolved pipe node's minted address (e.g. `/freq`).
    pub address: String,
    /// The pipe's synthesized `in` port: types inbound OSC args ([`Plan::osc_in_message`]) and
    /// clamps/resolves Value messages, exactly as routing to the rendered pipe node did.
    pub port: Port,
    /// The pipe's declared form — decides delivery, as the pipe's `process` arm did.
    pub kind: PortKind,
    /// The consumer's node index in execution order.
    pub node: usize,
    /// The consumer's input port index.
    pub dst_port: usize,
}

/// A pipe collapsed by [`dissolve_interface_pipes`], keyed by its (pre-instantiate) consumer.
struct DissolvedPipe {
    address: String,
    port: Port,
    kind: PortKind,
    consumer: NodeKey,
    consumer_port: usize,
}

/// One input-master tap — the dual of [`OutputTap`]: a **top-level** signal
/// input pipe bound to a logical input channel. Each block, [`crate::render::render_plan`]
/// copies the caller-supplied channel buffer into `buffer` (the pipe's `in` scratch, excluded
/// from the per-block arena clear) *before* any node runs. A channel the caller does not
/// supply leaves the pipe on its ordinary materialize path, so the pipe's **declared default
/// materializes** (a bare pipe's zero-seeded latch fills silence) and routed messages still
/// drive it — dark-degrade, never fatal, and a `channel` + `default` pipe stays
/// the sweepable control `describe` advertises. Distinct taps may share a channel (fan-out at
/// the master, like output broadcast); each pipe still owns its own buffer. Built only from
/// the played graph's **own** channel bindings: a subpatch-inlined child's bindings are
/// discarded at splice and a Voicer-hosted voice's are cleared in the loader's voice pass,
/// so nested/hosted bindings stay inert.
#[derive(Clone, Copy)]
pub struct InputTap {
    /// The logical input channel this pipe reads.
    pub channel: usize,
    /// Arena buffer index of the pipe's `in` materialize scratch, overwritten from the
    /// caller's input each supplied block. The entry **stays** in the node's `materialize`
    /// list: when the channel is unsupplied, the ZOH latch fill (declared default + routed
    /// messages) owns the buffer exactly as for an unbound pipe.
    pub buffer: usize,
    /// The pipe node's execution index, for flagging `materialize_device_fed`.
    pub node: usize,
    /// Index of `buffer`'s entry in the node's `materialize`/`materialize_device_fed` lists.
    pub mat_index: usize,
}

/// One master tap: a tapped port's arena buffers, summed into the master output.
pub struct OutputTap {
    /// Logical master channel this tap feeds, or `None` to broadcast to every
    /// channel (the historical mono fan).
    pub channel: Option<usize>,
    /// Arena buffer indices of the tapped port; all summed.
    pub buffers: Vec<usize>,
}

/// The immutable execution image.
pub struct Plan {
    pub config: AudioConfig,
    /// Nodes in topological execution order. `pub(crate)`: the survivor migration seam
    /// ([`Plan::transplant_survivors`]) is the one interface that mutates node state across a Swap;
    /// no caller indexes `.nodes[..].ops` directly (#495).
    pub(crate) nodes: Vec<PlanNode>,
    /// Total number of edge buffers in the arena.
    pub num_buffers: usize,
    /// Length `num_buffers`: `true` at each arena slot that is a materialize **scratch** buffer.
    /// Render skips these in its per-block "fresh edge buffers" clear, so a held input's
    /// buffer persists and need not be re-written every block (see `materialize_clean`).
    pub materialize_scratch_mask: Vec<bool>,
    /// Master taps, summed into the per-channel master output.
    pub output_taps: Vec<OutputTap>,
    /// Input-master taps: each channel-bound top-level signal input pipe, fed
    /// from the caller's logical input buffers before nodes run. Empty for a patch that binds
    /// no input channel (the common case pays nothing) and for hosted voice plans.
    pub input_taps: Vec<InputTap>,
    /// Outbound (OSC-out) sinks, drained past the boundary each block.
    pub outbound_taps: Vec<OutboundTap>,
    /// Resolved `interface` outputs, for a host operator to read this plan's boundary
    /// outputs by name + kind. Empty unless the document declared an `interface`.
    pub interface_outputs: Vec<InterfaceOutput>,
    /// One slot per Value `interface` output (parallel to the `captured_slot` indices in
    /// `interface_outputs`): the port's last-emitted scalar, held ZOH across blocks (seeded `0.0`).
    /// `render_plan` updates it when the port emits; the host reads it post-render.
    pub captured: Vec<f32>,
    /// Live addresses of interface pipes dissolved out of the schedule: message
    /// routing ([`crate::render`]) and [`Plan::osc_in_message`] consult these before the node
    /// scan, so a collapsed pipe's minted address keeps feeding its rewired consumer. Empty for
    /// a graph with no dissolvable pipes.
    pub(crate) input_aliases: Vec<InputAlias>,
}

/// Why Instantiate failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// The graph has a cycle (feedback needs an explicit unit-delay; deferred).
    Cycle,
    /// A wire's two declared forms cannot connect: a Signal feeding a Value input (no
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
    /// (the boundary). Resolves the address to a node + input port via
    /// [`crate::render::resolve_port`] — the *same* resolver the render routing path uses, so a
    /// nested node behind a prefix-matching ancestor stays reachable on both paths (issue #165) —
    /// then calls [`crate::boundary::osc_in_arg`] with that [`Port`] to type the flat args (the
    /// port's `meta` is what lets a scalar-defaulted `f32_buffer` control like `djfilter.position`
    /// cross, while bare audio does not). `None` if no node/port matches or the args don't fit the
    /// port. External OSC carries no timestamp, so the Message is stamped frame 0 ("now").
    pub fn osc_in_message(&self, address: &str, args: &[Arg]) -> Option<Message> {
        // A dissolved pipe's minted address stays live: type the args by the
        // pipe's own synthesized port, exactly as when the pipe was a rendered node.
        let port = self
            .input_alias(address)
            .map(|a| &a.port)
            .or_else(|| crate::render::resolve_port(&self.nodes, address).map(|(_, _, p)| p))?;
        let arg = crate::boundary::osc_in_arg(port, args)?;
        Some(Message::new(address, arg, 0))
    }

    /// The dissolved-pipe alias `address` targets, if any — the pipe-form `/name/in` address of
    /// an interface pipe [`dissolve_interface_pipes`] collapsed out of the schedule.
    pub(crate) fn input_alias(&self, address: &str) -> Option<&InputAlias> {
        self.input_aliases
            .iter()
            .find(|a| crate::render::local_address(address, &a.address) == Some(a.port.name))
    }

    /// Instantiate a Graph into an executable Plan (the construction sub-step of a Swap).
    pub fn instantiate(mut graph: Graph, mut config: AudioConfig) -> Result<Plan, PlanError> {
        // Validate the authored wires first (same error surface as before), then collapse
        // pass-through interface pipes out of the schedule (the pipe is a format
        // concept, not a mandatory rendered node) before ordering what actually executes.
        check_wire_forms(&graph)?;
        let dissolved = dissolve_interface_pipes(&mut graph);
        let order = topo_order(&graph)?;

        // Logical master width is derived from the instrument, not the device:
        // the highest referenced channel index + 1, floored to stereo so a mono patch still
        // presents two channels. A broadcast tap (`None`) imposes no width on its own.
        config.channels = graph
            .outputs
            .iter()
            .filter_map(|(_, _, ch)| ch.map(|c| c + 1))
            .max()
            .unwrap_or(0)
            .max(AudioConfig::MIN_CHANNELS);

        // Logical **input** width, the dual: max bound input channel + 1 across
        // the graph's own input pipes, 0 when none binds — a patch that uses no inputs pays
        // nothing (no floor). Derived here from the one binding source of truth
        // (`interface.input_channels`, the same map the taps below are built from), so no
        // writer can desynchronize width from taps. Nested bindings never reach here (a
        // splice discards the child's `Interface`; the loader's voice pass clears a hosted
        // voice graph's bindings), so a hosted plan derives 0 and gets no input-master
        // plumbing.
        config.input_channels = graph
            .interface
            .input_channels
            .values()
            .map(|&c| c + 1)
            .max()
            .unwrap_or(0);

        // 1. Assign every (node, Buffer output port) a unique arena buffer index. A message output
        // (Note / Harmony / scalar control out) carries no Signal data — events arrive via routing
        // — so it gets an empty buffer list (its emptiness is the marker that an edge into
        // it must materialize rather than share). The inner `Vec` is a single buffer per signal port
        // (the per-Lane dimension is gone — polyphony is hosted inside the Voicer).
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
        // Arena slots that are materialize scratch; Render skips them in its per-block
        // clear so held inputs persist. Collected as buffers are assigned below.
        let mut scratch_buffers: Vec<usize> = Vec::new();
        for key in &order {
            let descriptor = &graph.nodes[*key].descriptor;
            let overrides = &graph.nodes[*key].value_overrides;
            let n_inputs = descriptor.inputs.len();

            // The unified held latch per input port: seeded from each input's default or
            // an author override; `Copy`-normalized; carried across blocks.
            let latch: Vec<Arg> = descriptor
                .inputs
                .iter()
                .enumerate()
                .map(|(port, p)| seed_latch(p, port, overrides))
                .collect();

            // Input buffer wiring: a Buffer input wired to a Buffer source shares its
            // arena buffers (zero-copy); a Buffer input wired to a scalar source materializes a
            // scratch buffer (the one implicit ZOH bridge); Held / Stream inputs carry no buffer.
            let mut inputs: Vec<Option<Vec<usize>>> = Vec::with_capacity(n_inputs);
            let mut materialize: Vec<(usize, usize)> = Vec::new();
            let varying: Vec<bool> = vec![true; n_inputs];
            // Classify every input port once: the routing kind feeds both the buffer
            // wiring below and the per-node `input_kinds` the hot router reads each block.
            let input_kinds: Vec<PortKind> = descriptor.inputs.iter().map(port_kind).collect();
            for (port, &kind) in input_kinds.iter().enumerate() {
                // Every Signal (Buffer) input presents a per-sample buffer to the operator's
                // Signal read: wired to a Buffer source it shares it zero-copy;
                // otherwise (unwired, or fed by a scalar) the engine materializes a scratch
                // filled ZOH from the latch — an unwired *bare* buffer's latch seeds 0.0, so it
                // fills with silence. No Signal input is ever `None`: the buffer-presence
                // invariant. Held / Event inputs carry no buffer — they are read held
                // / as a stream.
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

            // Signal (Buffer) outputs, in signal-output ordinal order — the index a Signal write
            // handle (`io.write` on an `Out<SignalF32>`) uses.
            let outputs: Vec<Vec<usize>> = descriptor
                .outputs
                .iter()
                .enumerate()
                .filter(|(_, p)| matches!(p.ty, PortType::F32Buffer))
                .map(|(port, _)| out_buffers[*key][port].clone())
                .collect();

            // Message-edge targets, indexed by **all-outputs port index** — the index an `Out`
            // handle carries into [`crate::operator::Io::write`] (the contract macro numbers outputs
            // sequentially across kinds; `emit.port` is that index). A signal (`F32Buffer`) output never
            // emits Messages, so its slot is empty; a message output carries the `(dst node, dst input
            // port)` pairs wired to it. Indexing by the full output index (not a compacted
            // message-ordinal) is what lets an operator interleave a signal output and a message
            // output — e.g. `envelope` (`cv` signal + `active` message) for voice-liveness.
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
            // Config-dependent runtime state: the Voicer instantiates its bound voice
            // graphs into per-voice sub-plans here, where `config` is fixed and we are off the hot
            // path.
            let mut op = node.op;
            op.on_instantiate(&config)?;
            let ops = vec![op];

            let materialize_clean = vec![false; materialize.len()];
            let materialize_device_fed = vec![false; materialize.len()];
            nodes.push(PlanNode {
                address: node.address,
                ops,
                descriptor: node.descriptor,
                inputs,
                input_kinds,
                materialize,
                materialize_clean,
                materialize_device_fed,
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

        // Input-master taps: bind each channel-bound signal input pipe to its
        // logical input channel. The pipe's `in` port is unwired at its own level (an input
        // pipe is a source; only a parent face ever wires into it, and then this graph is not
        // the one being played), so the node loop above materialized it a scratch buffer.
        // The entry **stays** on the ZOH materialize path: each block the render tap copy
        // overwrites the scratch with the caller's channel and flags `materialize_device_fed`
        // so the ZOH fill yields; an **unsupplied** channel leaves the ordinary materialize
        // fill in charge, so the pipe's declared default (and routed messages) drive it — a
        // `channel` + `default` pipe degrades to the control it advertises, not to zeros.
        // Both loader invariants (a channel binding names a real pipe; a pipe's `in` is
        // unwired at top level, hence materialized) are asserted loudly in dev and skipped
        // dark in release — a broken invariant must not become a silent dead mic. BTreeMap
        // iteration makes tap order (and so behavior) deterministic.
        let mut input_taps: Vec<InputTap> = Vec::new();
        for (name, &channel) in &graph.interface.input_channels {
            let entry = graph.interface.inputs.get(name);
            debug_assert!(
                entry.is_some(),
                "channel binding {name:?} names no interface input pipe"
            );
            let Some(&(key, port)) = entry else { continue };
            let node_idx = index_of[key];
            let mat_index = nodes[node_idx]
                .materialize
                .iter()
                .position(|(p, _)| *p == port);
            debug_assert!(
                mat_index.is_some(),
                "channel-bound pipe {name:?} has no materialized `in` scratch — wired or \
                 non-signal, which the loader forbids"
            );
            let Some(mat_index) = mat_index else { continue };
            let (_, buffer) = nodes[node_idx].materialize[mat_index];
            input_taps.push(InputTap {
                channel,
                buffer,
                node: node_idx,
                mat_index,
            });
        }

        // Outbound sinks: the `osc_out` operator is the one whose output is the external
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

        // Resolve the `interface` outputs so a host reads this plan's boundary by name
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

        // Dissolved pipes' addresses, re-keyed from NodeKey to execution index now that the
        // order is fixed. The consumer key is always live: chain dissolution re-pointed any
        // alias whose consumer was itself dissolved.
        let input_aliases: Vec<InputAlias> = dissolved
            .into_iter()
            .map(|d| InputAlias {
                address: d.address,
                port: d.port,
                kind: d.kind,
                node: index_of[d.consumer],
                dst_port: d.consumer_port,
            })
            .collect();

        Ok(Plan {
            config,
            nodes,
            num_buffers: next_buffer,
            materialize_scratch_mask,
            output_taps,
            input_taps,
            outbound_taps,
            interface_outputs,
            captured: vec![0.0; captured_len],
            input_aliases,
        })
    }

    /// The arena buffer index of a Signal `interface` output, or `None` if there is no
    /// such named output or it is not a Signal. A host reads the rendered buffer at this index.
    pub fn interface_signal_buf(&self, name: &str) -> Option<usize> {
        self.interface_outputs
            .iter()
            .find(|o| o.name == name)
            .and_then(|o| o.signal_buf)
    }

    /// The [`captured`](Plan::captured) slot index of a Value `interface` output, or
    /// `None` if there is no such named output or it is not a Value. A host reads `captured[slot]`
    /// post-render for the port's held value.
    pub fn interface_value_slot(&self, name: &str) -> Option<usize> {
        self.interface_outputs
            .iter()
            .find(|o| o.name == name)
            .and_then(|o| o.captured_slot)
    }

    /// Transplant survivor operator boxes from a `from` Plan into this (freshly built) one, per a
    /// precomputed migration table. Each `(old_index, new_index)` pair moves the
    /// surviving box — the operator instance *is* its state, including a voicer's
    /// hosted voice sub-plans — from `from.nodes[old_index]` into `self.nodes[new_index]`; the
    /// displaced cold box (the fresh Plan's node for that slot) lands back in `from` and frees
    /// off-thread with it. The new Plan's wiring and latches (which live in the [`PlanNode`], not
    /// the box) stay this Plan's, so a survivor re-reads its inputs from the *new* document.
    ///
    /// This is the single seam that mutates survivor state across a Swap, so the **pairing
    /// invariant** concentrates here: each pair must share operator type + instantiate-time
    /// identity, guaranteed by the survivor key the
    /// [`MigrationTable`](crate::coordinator::manifest::MigrationTable) carries, so the
    /// transplanted box's internal layout matches its new Plan node. A wrong-but-in-bounds pairing
    /// is a caller bug the bounds `debug_assert!` cannot catch (strengthening it to a per-pair type
    /// check is deferred, #495).
    ///
    /// The bare `&[(usize, usize)]` signature keeps this primitive from importing the coordinator
    /// (preserving the one-way `coordinator → plan/engine` layering): the migration *table* (which
    /// pairs, computed how) is a Coordinator concept. [`crate::engine::Engine`] forwards straight to
    /// here; the coordinator call sites unwrap the survivor slice from their
    /// [`MigrationTable`](crate::coordinator::manifest::MigrationTable) at the seam where the table
    /// already lives.
    ///
    /// **RT-safe:** a bounded loop of [`std::mem::swap`] over `Vec<Box<dyn Operator>>` — pointer
    /// swaps only, no allocation, no drop, no lock. Runs at the render-callback top (ticket #321).
    pub(crate) fn transplant_survivors(&mut self, from: &mut Plan, pairs: &[(usize, usize)]) {
        for &(old_index, new_index) in pairs {
            debug_assert!(
                old_index < from.nodes.len() && new_index < self.nodes.len(),
                "migration table index out of range — mispaired table/engine"
            );
            std::mem::swap(
                &mut from.nodes[old_index].ops,
                &mut self.nodes[new_index].ops,
            );
            // The survivor's box carried its emit-on-change dedup baselines, but the new
            // Plan reset every downstream consumer latch to its declared default. Let each
            // transplanted op re-assert its on-change held outputs on the first post-swap block, so a
            // consumer is not stranded on that default (default no-op; only publishers like `harmony`
            // act). RT-safe: a bounded loop of small baseline resets, no allocation.
            for op in &mut self.nodes[new_index].ops {
                op.on_transplant();
            }
        }
    }
}

/// Collapse pass-through **interface pipes** out of the execution schedule
/// (issue #189). A pipe is an authoring/format concept — a named boundary entry that mints an
/// address — and rendering one as a real node costs a full per-node engine pass every block
/// (routing, segmenting, an arena buffer + copy for a signal pipe), multiplied by the Voicer's
/// per-voice plans. This pass removes each dissolvable pipe node and rewires around it so the
/// rendered schedule is what a hand-flattened patch would have been:
///
/// - its single consumer takes the pipe's **feeder wire** directly (zero-copy for signals), or —
///   when the boundary feeds the pipe by message (a Voicer, external OSC) — the consumer's own
///   materialize/latch path, with the pipe's rest **seed transferred** as a value-override so an
///   unfed boundary renders exactly what the rendered pipe did (declared default, or silence);
/// - its minted address stays live through [`InputAlias`] — Instantiate's caller-visible
///   surface (message routing, `osc_in_message`) is unchanged.
///
/// A pipe is kept as a rendered node when collapsing could change behavior or lose surface:
/// fan-out (≥ 2 consumers share the one materialized feed), no consumer (the address must stay
/// addressable), a master tap or `interface` output reading the pipe's `out` port, or a Value
/// pipe feeding an Event/`Arg` pass-through input (its frame-0 seed emission is observable
/// there). Chains (a pipe feeding a pipe, e.g. through a spliced nest) collapse to fixpoint;
/// an alias whose consumer dissolves is re-pointed to that pipe's own consumer.
fn dissolve_interface_pipes(graph: &mut Graph) -> Vec<DissolvedPipe> {
    let mut dissolved: Vec<DissolvedPipe> = Vec::new();
    loop {
        // One dissolve per scan, to fixpoint: rewiring can make another pipe dissolvable
        // (chains), and mutating while iterating the slotmap is not on.
        let mut found: Option<(NodeKey, NodeKey, usize)> = None;
        for (key, node) in &graph.nodes {
            // Loader-built pipes only: a document cannot name `"type": "pipe"` on a node.
            if node.descriptor.type_name != "pipe" {
                continue;
            }
            // A channel-bound pipe stays a rendered node: the input master
            // writes the caller's logical channel into the pipe's materialized scratch each
            // block, so the buffer — and the interface entry that finds it — must survive.
            // Hosted/nested bindings are cleared/discarded before instantiate, so their pipes
            // still dissolve.
            if graph.interface.input_channels.keys().any(|n| {
                graph
                    .interface
                    .inputs
                    .get(n)
                    .is_some_and(|(k, _)| *k == key)
            }) {
                continue;
            }
            // Exactly one wire consumer — its input port absorbs the pipe's role wholesale.
            let mut consumers = graph.connections.iter().filter(|c| c.src == key);
            let Some(c0) = consumers.next() else { continue };
            if consumers.next().is_some() {
                continue;
            }
            // The pipe's `out` must not be read by name elsewhere: a master tap or an
            // `interface` output on it needs the rendered buffer/port to exist.
            if graph.outputs.iter().any(|(k, _, _)| *k == key)
                || graph.interface.outputs.values().any(|(k, _)| *k == key)
            {
                continue;
            }
            // A Value pipe into an Event/`Arg` pass-through input: the pipe's frame-0 seed
            // emission lands there as a real observable event — keep the node.
            let kind = port_kind(&node.descriptor.inputs[0]);
            let dst_kind = port_kind(&graph.nodes[c0.dst].descriptor.inputs[c0.dst_port]);
            if kind == PortKind::Value && dst_kind == PortKind::Event {
                continue;
            }
            found = Some((key, c0.dst, c0.dst_port));
            break;
        }
        let Some((key, dst, dst_port)) = found else {
            return dissolved;
        };

        let node = &graph.nodes[key];
        let pipe_port = node.descriptor.inputs[0].clone();
        let kind = port_kind(&pipe_port);
        let address = node.address.clone();
        // What the pipe held at rest: its declared default (or an author/boundary override),
        // via the same seeding rule Instantiate applies below.
        let seed = seed_latch(&pipe_port, 0, &node.value_overrides);

        // Splice the pipe out of the wiring: its feeder (if any) takes over the consumer edge.
        // Legality is transitive — the pipe's `in`/`out` share one declared type, so every
        // rewired pair is a combination `check_wire_forms` already admitted (the one crossing it
        // would not, Value-source→Arg-input, is excluded by the Event-consumer guard above).
        let feeder = graph
            .connections
            .iter()
            .find(|c| c.dst == key)
            .map(|c| (c.src, c.src_port));
        graph.connections.retain(|c| c.src != key && c.dst != key);
        if let Some((src, src_port)) = feeder {
            graph.connections.push(Connection {
                src,
                src_port,
                dst,
                dst_port,
            });
        }

        // Transfer the pipe's rest seed to the consumer's latch, normalized exactly as the
        // rendered pipe's frame-0 seed emission would have landed (routing): raw f32
        // onto a materialized Signal input, `held_arg`-resolved onto a Value input. An Event
        // pipe holds nothing. If the seed cannot land (as it could not by message), the
        // consumer keeps its own default — same as when the emission was dropped.
        if kind != PortKind::Event {
            let dst_p = &graph.nodes[dst].descriptor.inputs[dst_port];
            let transferred = match port_kind(dst_p) {
                PortKind::Signal => seed.as_f32().map(Arg::F32),
                _ => crate::render::held_arg(dst_p, &seed),
            };
            if let Some(arg) = transferred {
                let overrides = &mut graph.nodes[dst].value_overrides;
                overrides.retain(|(p, _)| *p != dst_port);
                overrides.push((dst_port, arg));
            }
        }

        graph.nodes.remove(key);
        graph.interface.inputs.retain(|_, (k, _)| *k != key);
        // A chained alias (this pipe was another dissolved pipe's consumer) follows through to
        // this pipe's own consumer. (Its normalization keeps the outermost pipe's port — for
        // in-range values, the only case authored chains produce, the hops agree.)
        for d in dissolved.iter_mut() {
            if d.consumer == key {
                d.consumer = dst;
                d.consumer_port = dst_port;
            }
        }
        dissolved.push(DissolvedPipe {
            address,
            port: pipe_port,
            kind,
            consumer: dst,
            consumer_port: dst_port,
        });
    }
}

/// The planner's only form job: a **local per-wire check**. For each connection, compare
/// the source output's declared form against the destination input's and reject the illegal
/// crossings — there is no topological solver, no propagation. The legal combinations are
/// like→like (`Signal→Signal`, `Value→Value`, `Event→Event`) and the one implicit coercion
/// `Value→Signal` (materialized downstream). Everything else is a hard error: `Signal→Value` needs
/// an explicit sig→val converter, and any `Event` mismatch needs an explicit latch / change-detect.
/// One destination-side exception: a type-agnostic [`Arg`](PortType::Arg) pass-through input
/// (issue #141) accepts any Event *or* Value source **whose type has an external OSC form**
/// ([`has_osc_form`](crate::boundary::has_osc_form)); a Signal or no-form source is rejected.
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
            // A type-agnostic pass-through input (issue #141) spans the Event/Value split:
            // any Message-domain source whose type has an external OSC form wires in, both
            // delivered as raw Events (capability-keyed via `boundary::has_osc_form`, the
            // single statement shared with the load-time check). A no-form type (`Harmony`)
            // would make a wire that can never send anything — hard error, same philosophy
            // as the Signal arm below.
            (Event | Value, Event) if matches!(dst.ty, PortType::Arg) => {
                if crate::boundary::has_osc_form(&src.ty) {
                    continue;
                }
                let ty = match &src.ty {
                    PortType::Vocab { name, .. } => name,
                    _ => "the source type",
                };
                format!(
                    "{ty}→Arg: {ty} has no external OSC form (the boundary opt-out), \
                     so a pass-through wire could never send anything; \
                     the type registers no boundary converter — Harmony's wire form \
                     is tracked in issue #209"
                )
            }
            // A Signal source never emits Messages (its data lives in arena buffers), so
            // wiring one into the pass-through would silently send nothing — and audio stays
            // off the wire by construction: hard error.
            (Signal, Event) if matches!(dst.ty, PortType::Arg) => {
                "Signal→Arg: a pass-through input takes Message-domain sources only; \
                    audio never crosses the boundary (a live Signal needs the deferred \
                    Signal→Message sampler)"
                    .to_string()
            }
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

/// Wire-form oracle + per-wire checker fixtures.
///
/// Built test-first as the spine's substrate (impl-prep §1). A port's **form** is *declared* by its
/// [`PortType`] — `f32` = Value, `f32_buffer` = Signal, a struct vocab (`Note`) = Event — and the
/// planner's only form job is a **local per-wire check**: Value→Signal materializes, Signal→Value is
/// a hard error, like→like is direct. These fixtures pin that check.
///
/// The fixtures wire **synthetic single-port operators** (one declared form each) so a plan's
/// buffer count isolates the wire under test: [`signal_buffer_count`] == declared-Signal ports +
/// materialized Value→Signal edges. Real operators carry their forms after the step-4 sweep; until
/// then these probes are the oracle.
///
/// Relocated from `tests/wire_forms.rs` into a unit module (#495): the fixtures reach into
/// [`Plan::nodes`] (now `pub(crate)`), which they always did — they are unit checks of
/// `instantiate`'s wiring, not black-box integration tests, so they live where they can see the
/// crate internals they assert on.
#[cfg(test)]
mod wire_forms {
    use super::{port_kind, Plan, PlanError, PortKind};
    use crate::config::AudioConfig;
    use crate::descriptor::{Descriptor, Port, PortType};
    use crate::graph::Graph;
    use crate::operator::{Io, Operator};
    use crate::vocab::FilterMode;

    // ------------------------------------------------------------------------------------------
    // Synthetic single-port operators. `add_boxed` takes the descriptor explicitly, so one no-op
    // `Probe` body backs every form — the descriptor is what carries the declared form under test.
    // ------------------------------------------------------------------------------------------

    struct Probe;

    impl Operator for Probe {
        fn descriptor() -> Descriptor {
            // Never called: every Probe is added via `add_boxed` with an explicit descriptor.
            desc("probe", vec![], vec![])
        }
        fn process(&mut self, _io: &mut Io) {}
        fn spawn(&self) -> Box<dyn Operator> {
            Box::new(Probe)
        }
    }

    fn desc(type_name: &'static str, inputs: Vec<Port>, outputs: Vec<Port>) -> Descriptor {
        Descriptor {
            type_name,
            inputs,
            outputs,
            constants: vec![],
            resources: vec![],
        }
    }

    /// A Signal port — a dense per-sample buffer (`f32_buffer` audio: an LFO out, `filter.cutoff`).
    fn signal(name: &'static str) -> Port {
        Port::f32_buffer(name)
    }

    /// A Value port — a latched single value. Modelled with `I32` so it classifies Value *now*; until
    /// the step-4 sweep `F32` still classifies Signal (decision A), so a genuine numeric Value source
    /// is `I32` here. The real `f32`-Value fixtures (C/E/F: `tempo`, gate spine) arrive at step 4.
    fn value(name: &'static str) -> Port {
        Port {
            name,
            ty: PortType::I32 { meta: None },
            meta: None,
        }
    }

    /// A Value port carrying an enum (`filter.mode`) — a Value-only type with no buffer form.
    fn value_enum(name: &'static str) -> Port {
        Port::enumerated(FilterMode::enum_meta(name))
    }

    /// An Event port — a sparse frame-stamped stream (`Note`: a sequencer's `degrees` out).
    fn event(name: &'static str) -> Port {
        Port::note(name)
    }

    /// A type-agnostic pass-through port — any `Arg`, delivered as a raw Event stream (issue #141:
    /// `osc_out.in`).
    fn passthrough(name: &'static str) -> Port {
        Port::arg(name)
    }

    // ------------------------------------------------------------------------------------------
    // Oracle probes (impl-prep §1).
    // ------------------------------------------------------------------------------------------

    /// The declared form of an input port, read from the plan's precomputed classification.
    fn port_form(plan: &Plan, node: usize, port: usize) -> PortKind {
        plan.nodes[node].input_kinds[port]
    }

    /// Buffer cost of a plan: declared-Signal ports + materialized Value→Signal edges. With
    /// single-port synthetic operators this isolates the wire under test.
    fn signal_buffer_count(plan: &Plan) -> usize {
        plan.num_buffers
    }

    /// Wire one source-output form to one sink-input form through the real planner.
    fn wire(src: Port, dst: Port) -> Result<Plan, PlanError> {
        let mut g = Graph::new();
        let s = g.add_boxed("/src", Box::new(Probe), desc("src", vec![], vec![src]));
        let d = g.add_boxed("/dst", Box::new(Probe), desc("dst", vec![dst], vec![]));
        g.connect(s, 0, d, 0);
        Plan::instantiate(g, AudioConfig::new(48_000.0, 128))
    }

    // ------------------------------------------------------------------------------------------
    // Step 0 — oracle substrate (tracer bullets).
    // ------------------------------------------------------------------------------------------

    #[test]
    fn graph_helper_wires_two_nodes_and_instantiates() {
        // Tracer bullet: the thin Graph helper builds a real Plan from two wired nodes.
        let plan =
            wire(Port::f32_buffer("o"), Port::f32_buffer("i")).expect("a valid wire instantiates");
        assert_eq!(plan.nodes.len(), 2);
    }

    #[test]
    fn port_form_reads_a_declared_input_form() {
        let plan = wire(Port::f32_buffer("o"), Port::f32_buffer("i")).expect("instantiate");
        // The sink node (index varies with topo order); find it by address.
        let dst = plan.nodes.iter().position(|n| n.address == "/dst").unwrap();
        assert_eq!(port_form(&plan, dst, 0), port_kind(&Port::f32_buffer("i")));
    }

    #[test]
    fn signal_buffer_count_counts_the_signal_edge() {
        // One Signal source feeding a Signal sink: a single shared edge buffer.
        let plan = wire(Port::f32_buffer("o"), Port::f32_buffer("i")).expect("instantiate");
        assert_eq!(signal_buffer_count(&plan), 1);
    }

    #[test]
    fn helper_surfaces_plan_errors_as_err_not_panic() {
        // A two-node cycle is the error the planner already rejects; the helper returns it as `Err`
        // (the coercion fixtures G/H/I will assert `Err(FormMismatch)` over the same surface).
        let mut g = Graph::new();
        let a = g.add_boxed(
            "/a",
            Box::new(Probe),
            desc("a", vec![value("i")], vec![value("o")]),
        );
        let b = g.add_boxed(
            "/b",
            Box::new(Probe),
            desc("b", vec![value("i")], vec![value("o")]),
        );
        g.connect(a, 0, b, 0);
        g.connect(b, 0, a, 0);
        match Plan::instantiate(g, AudioConfig::new(48_000.0, 128)) {
            Err(e) => assert_eq!(e, PlanError::Cycle),
            Ok(_) => panic!("a cycle must not instantiate"),
        }
    }

    // ------------------------------------------------------------------------------------------
    // Step 1 — per-wire form checker (impl-prep §2). Synthetic ports isolate each form crossing; the
    // real-port versions (C/E/F numeric Value spine) light up at step 4 as operators migrate.
    // ------------------------------------------------------------------------------------------

    fn dst_idx(plan: &Plan) -> usize {
        plan.nodes.iter().position(|n| n.address == "/dst").unwrap()
    }

    /// A — Value→Signal is the one implicit coercion: the Value source materializes a (constant)
    /// buffer at the Signal input. One buffer, the materialized edge.
    #[test]
    fn value_into_signal_input_materializes_one_buffer() {
        let plan = wire(value("o"), signal("i")).expect("Value→Signal is legal");
        let dst = dst_idx(&plan);
        assert_eq!(port_form(&plan, dst, 0), PortKind::Signal);
        assert!(
            !plan.nodes[dst].materialize.is_empty(),
            "the Signal input is fed by a Value source, so it materializes"
        );
        assert_eq!(signal_buffer_count(&plan), 1);
    }

    /// B — Signal→Signal is a plain wire: the sink shares the source's edge buffer, no coercion.
    #[test]
    fn signal_into_signal_input_is_a_direct_shared_edge() {
        let plan = wire(signal("o"), signal("i")).expect("Signal→Signal is legal");
        let dst = dst_idx(&plan);
        assert_eq!(port_form(&plan, dst, 0), PortKind::Signal);
        assert!(
            plan.nodes[dst].materialize.is_empty(),
            "a Signal source shares its buffer; nothing materializes"
        );
        assert_eq!(signal_buffer_count(&plan), 1);
    }

    /// C — Value→Value is direct and costs no buffer: a held knob never materializes.
    #[test]
    fn value_into_value_input_is_direct_and_bufferless() {
        let plan = wire(value("o"), value("i")).expect("Value→Value is legal");
        let dst = dst_idx(&plan);
        assert_eq!(port_form(&plan, dst, 0), PortKind::Value);
        assert_eq!(signal_buffer_count(&plan), 0);
    }

    /// G — Signal→Value is the headline hard error: there is no implicit sample-and-hold, and the
    /// message must name the missing converter (a user *will* try this wire). Deliberate gap.
    #[test]
    fn signal_into_value_input_is_a_hard_error_naming_the_converter() {
        match wire(signal("o"), value("i")).err() {
            Some(PlanError::FormMismatch { src, dst, reason }) => {
                assert_eq!(src, "/src.o");
                assert_eq!(dst, "/dst.i");
                assert!(
                    reason.contains("envelope follower") || reason.contains("quantizer"),
                    "Signal→Value error must name the converter op: {reason}"
                );
            }
            other => panic!("expected FormMismatch, got {other:?}"),
        }
    }

    /// H — Signal into a Value-only type (an enum) is equally illegal, and the message must explain the
    /// enum case (a discrete choice, not a per-sample signal) — *not* dangle the numeric-Value converter
    /// hint, since no envelope-follower/quantizer produces an enum.
    #[test]
    fn signal_into_enum_value_input_is_a_hard_error_explaining_the_enum() {
        match wire(signal("o"), value_enum("mode")).err() {
            Some(PlanError::FormMismatch { dst, reason, .. }) => {
                assert_eq!(dst, "/dst.mode");
                assert!(
                    reason.contains("enum") && reason.contains("discrete choice"),
                    "Signal→enum error must explain the enum target: {reason}"
                );
                assert!(
                    !reason.contains("envelope follower"),
                    "must not dangle the numeric converter hint for an enum: {reason}"
                );
            }
            other => panic!("expected FormMismatch, got {other:?}"),
        }
    }

    /// I — Event→Signal is illegal: a note stream cannot feed a per-sample input without an explicit op,
    /// and the message must name the missing latch / change-detect.
    #[test]
    fn event_into_signal_input_is_a_hard_error_naming_the_latch() {
        match wire(event("o"), signal("i")).err() {
            Some(PlanError::FormMismatch { reason, .. }) => assert!(
                reason.contains("latch") || reason.contains("change-detect"),
                "Event→Signal error must name the latch / change-detect op: {reason}"
            ),
            other => panic!("expected FormMismatch, got {other:?}"),
        }
    }

    /// J — Value→Event is a hard error: a held scalar (a gate, a knob echo) is not a note stream, and
    /// wiring one into a vocab Event input (`voicer.notes`) must be rejected, not silently latched.
    /// The sink is `Port::note` — a *vocab* Event, not the `Arg` pass-through — so the issue-#141
    /// passthrough exception must not swallow this wire.
    #[test]
    fn value_into_event_input_is_a_hard_error() {
        match wire(value("o"), event("i")).err() {
            Some(PlanError::FormMismatch { src, dst, reason }) => {
                assert_eq!(src, "/src.o");
                assert_eq!(dst, "/dst.i");
                assert!(
                    reason.contains("Event port takes only an Event source"),
                    "Value→Event error must state the Event-only rule: {reason}"
                );
            }
            other => panic!("expected FormMismatch, got {other:?}"),
        }
    }

    /// K — Signal→Event is equally illegal: a dense audio buffer never becomes a note stream. Distinct
    /// from the Signal→Arg pass-through rejection (whose message explains the *boundary* opt-out) —
    /// this arm's message states the Event-only rule, so the two rejections can't be conflated.
    #[test]
    fn signal_into_event_input_is_a_hard_error() {
        match wire(signal("o"), event("i")).err() {
            Some(PlanError::FormMismatch { src, dst, reason }) => {
                assert_eq!(src, "/src.o");
                assert_eq!(dst, "/dst.i");
                assert!(
                    reason.contains("Event port takes only an Event source"),
                    "Signal→Event error must state the Event-only rule: {reason}"
                );
            }
            other => panic!("expected FormMismatch, got {other:?}"),
        }
    }

    /// L — Event→Value is the other half of I: a note stream cannot drive a held knob without an
    /// explicit op, and the message must name the missing latch / change-detect. Pinned separately
    /// from I even though the checker shares one arm today, so the halves can split independently.
    #[test]
    fn event_into_value_input_is_a_hard_error_naming_the_latch() {
        match wire(event("o"), value("i")).err() {
            Some(PlanError::FormMismatch { src, dst, reason }) => {
                assert_eq!(src, "/src.o");
                assert_eq!(dst, "/dst.i");
                assert!(
                    reason.contains("latch") || reason.contains("change-detect"),
                    "Event→Value error must name the latch / change-detect op: {reason}"
                );
            }
            other => panic!("expected FormMismatch, got {other:?}"),
        }
    }

    // ------------------------------------------------------------------------------------------
    // The type-agnostic pass-through (issue #141) — `osc_out.in`. It classifies Event (raw, unlatched
    // delivery) and is the one destination that spans the Event/Value split: any Message-domain
    // source wires in; only a Signal source is rejected (audio never crosses the boundary).
    // ------------------------------------------------------------------------------------------

    /// An Event source (a `Note` stream) wires into the pass-through like→like, costing no buffer.
    #[test]
    fn event_into_passthrough_is_legal_and_bufferless() {
        let plan = wire(event("o"), passthrough("in")).expect("Event→Arg is legal");
        let dst = dst_idx(&plan);
        assert_eq!(port_form(&plan, dst, 0), PortKind::Event);
        assert_eq!(signal_buffer_count(&plan), 0);
    }

    /// A Value source (a held scalar — a Good Button `map` echo) wires into the pass-through too:
    /// its emissions deliver as raw Events, so a control value can reach the outbound boundary.
    #[test]
    fn value_into_passthrough_is_legal_and_bufferless() {
        let plan = wire(value("o"), passthrough("in")).expect("Value→Arg is legal");
        let dst = dst_idx(&plan);
        assert_eq!(port_form(&plan, dst, 0), PortKind::Event);
        assert_eq!(signal_buffer_count(&plan), 0);
    }

    /// A vocab-enum Value source wires in as well — the wire that makes the boundary's outbound enum
    /// arm reachable at all (issue #141: enums could never flow outbound before).
    #[test]
    fn enum_value_into_passthrough_is_legal() {
        let plan = wire(value_enum("mode"), passthrough("in")).expect("enum Value→Arg is legal");
        let dst = dst_idx(&plan);
        assert_eq!(port_form(&plan, dst, 0), PortKind::Event);
    }

    /// A no-OSC-form Value source (`Harmony`, the documented boundary opt-out) is equally a hard
    /// error: legality into the pass-through is capability-keyed (`boundary::has_osc_form`), so a
    /// wire that could never send anything is rejected at plan, not left silently dead. Struct
    /// converters landed with the boundary registry (`register_osc_form!`, epic #146); `Harmony`
    /// registers none — its wire form is deferred to issue #209.
    #[test]
    fn harmony_into_passthrough_is_a_hard_error_naming_the_opt_out() {
        let harmony = Port {
            name: "harmony",
            ty: PortType::Vocab {
                name: "Harmony",
                is_event: false,
                enum_meta: None,
            },
            meta: None,
        };
        match wire(harmony, passthrough("in")).err() {
            Some(PlanError::FormMismatch { src, dst, reason }) => {
                assert_eq!(src, "/src.harmony");
                assert_eq!(dst, "/dst.in");
                assert!(
                    reason.contains("no external OSC form"),
                    "Harmony→Arg error must name the missing OSC form: {reason}"
                );
            }
            other => panic!("expected FormMismatch, got {other:?}"),
        }
    }

    /// A Signal source stays a hard error: a dense buffer never emits Messages (the wire would
    /// silently send nothing), and audio is kept off the OSC wire by construction.
    #[test]
    fn signal_into_passthrough_is_a_hard_error_keeping_audio_off_the_wire() {
        match wire(signal("o"), passthrough("in")).err() {
            Some(PlanError::FormMismatch { src, dst, reason }) => {
                assert_eq!(src, "/src.o");
                assert_eq!(dst, "/dst.in");
                assert!(
                    reason.contains("pass-through") && reason.contains("audio"),
                    "Signal→Arg error must explain the boundary opt-out: {reason}"
                );
            }
            other => panic!("expected FormMismatch, got {other:?}"),
        }
    }
}

/// Unit tests for the survivor-migration seam (#495). The transplant is the one primitive that
/// mutates survivor state across a Swap; these prove the box (whose identity *is* the operator's
/// state) moves, and that a mispaired-out-of-bounds table trips the debug guard.
#[cfg(test)]
mod transplant_tests {
    use super::Plan;
    use crate::config::AudioConfig;
    use crate::descriptor::Descriptor;
    use crate::graph::Graph;
    use crate::operator::{Io, Operator};

    // Carries a byte of state so it is **not** a ZST: a `Box<ZST>` is a shared dangling pointer,
    // which would give every box the same address and defeat the identity assertions below. One
    // field makes each box a real, distinct heap allocation.
    struct Probe {
        _state: u8,
    }

    impl Operator for Probe {
        fn descriptor() -> Descriptor {
            Descriptor {
                type_name: "probe",
                inputs: vec![],
                outputs: vec![],
                constants: vec![],
                resources: vec![],
            }
        }
        fn process(&mut self, _io: &mut Io) {}
        fn spawn(&self) -> Box<dyn Operator> {
            Box::new(Probe { _state: 0 })
        }
    }

    /// A one-node plan whose single node's operator box is the transplant subject.
    fn one_node_plan() -> Plan {
        let mut g = Graph::new();
        g.add_boxed("/n", Box::new(Probe { _state: 0 }), Probe::descriptor());
        Plan::instantiate(g, AudioConfig::new(48_000.0, 128)).expect("one-node plan instantiates")
    }

    /// The heap address of an operator box, discarding the vtable — a stable identity for the box
    /// (and so for the operator state it carries) across a `mem::swap`.
    fn box_addr(op: &dyn Operator) -> *const () {
        op as *const dyn Operator as *const ()
    }

    /// The seam's payoff: transplanting a survivor moves the *exact* live box into the fresh Plan
    /// (its internal state crosses), and the fresh node's cold box lands back in
    /// `from` to free off-thread. Proven by box identity, not by a value comparison, so it holds
    /// for any operator regardless of what state the box carries.
    #[test]
    fn transplant_moves_the_survivor_box_and_lands_the_cold_box_in_from() {
        let mut fresh = one_node_plan();
        let mut retiring = one_node_plan();

        let survivor = box_addr(&*retiring.nodes[0].ops[0]);
        let cold = box_addr(&*fresh.nodes[0].ops[0]);
        assert_ne!(
            survivor, cold,
            "the two plans must own distinct boxes to start"
        );

        fresh.transplant_survivors(&mut retiring, &[(0, 0)]);

        assert_eq!(
            box_addr(&*fresh.nodes[0].ops[0]),
            survivor,
            "the survivor's live box crosses into the fresh Plan"
        );
        assert_eq!(
            box_addr(&*retiring.nodes[0].ops[0]),
            cold,
            "the displaced cold box lands in `from`, to free off-thread with the retiree"
        );
    }

    /// An empty table is a no-op: every node resets, nothing moves.
    #[test]
    fn empty_table_transplants_nothing() {
        let mut fresh = one_node_plan();
        let mut retiring = one_node_plan();
        let before = box_addr(&*fresh.nodes[0].ops[0]);
        fresh.transplant_survivors(&mut retiring, &[]);
        assert_eq!(box_addr(&*fresh.nodes[0].ops[0]), before);
    }

    /// A mispaired, out-of-bounds table trips the bounds guard in debug builds — the one thing the
    /// (bounds-only) `debug_assert!` can catch. (A wrong-but-in-bounds pairing is not caught; a
    /// per-pair type check is the deferred follow-up, #495.)
    #[test]
    #[should_panic(expected = "out of range")]
    #[cfg(debug_assertions)]
    fn out_of_bounds_pair_trips_the_bounds_guard_in_debug() {
        let mut fresh = one_node_plan();
        let mut retiring = one_node_plan();
        fresh.transplant_survivors(&mut retiring, &[(0, 5)]);
    }
}
