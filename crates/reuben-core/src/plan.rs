//! Plan — the static execution image produced by Instantiate (ADR-0009, ADR-0010, ADR-0030).
//!
//! Instantiate consumes a [`Graph`], topologically orders its nodes, computes each node's
//! **Lane (Voice) count**, replicates Lane-expanded operators per Voice with independent
//! state, and assigns every Buffer (output port, Lane) a slot in the edge-buffer arena. The
//! result is immutable and is what [`crate::render`] executes per block.
//!
//! Lane counts: a node whose descriptor declares [`LaneRule::FromParam`] (the Voicer)
//! expands to that param's value; every other node inherits the max Lane count of its
//! inputs (1 if it has none). Because all fan-out shapes are fixed here, Render pays
//! nothing for them (ADR-0010).
//!
//! The seven former carriers collapse to one model (ADR-0030): every input port has a held
//! [`Arg`] **latch** (the ZOH value `io.last` reads), Buffer inputs additionally carry a dense
//! arena buffer, and every output port either owns arena buffers (a Buffer/signal output) or
//! routes emitted Messages to downstream input ports (a message output). The old context-arena /
//! enum-latch / param lanes and the separate `msg_targets` / `ctx_targets` routing are unified.

use slotmap::SecondaryMap;

use crate::config::AudioConfig;
use crate::descriptor::{Descriptor, LaneRule, Port, PortType};
use crate::graph::{Graph, NodeKey};
use crate::message::{Arg, Message};
use crate::operator::Operator;
use crate::vocab::harmony::Harmony;

/// How the engine treats an input port (ADR-0030), derived from its [`PortType`]:
///
/// - **Dense** — a [`Buffer`](PortType::Buffer) audio input, read per-sample via `io.signal`.
/// - **Held** — a scalar / enum / `Harmony` control whose last value is latched (ZOH) and read via
///   `io.last`; a mid-block change block-slices so the value is constant per `process` call.
/// - **Stream** — an event vocab (`Note`), delivered frame-stamped via `io.stream` and *not*
///   sliced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortKind {
    Dense,
    Held,
    Stream,
}

/// Classify an input/output port (ADR-0030). A `Buffer` or `F32` (float control) is Dense — both
/// present a per-sample buffer to `io.signal`, so a mid-block change writes into that buffer at its
/// frame (the F32 latch read by `io.last` is kept in sync from the same fill). A `Note` (struct
/// vocab that isn't `Harmony`) is a Stream event; everything else — enums, `Harmony` — is Held.
pub fn port_kind(p: &Port) -> PortKind {
    match &p.ty {
        PortType::Buffer | PortType::F32 => PortKind::Dense,
        PortType::Vocab {
            enum_meta: None,
            name,
        } if *name != "Harmony" => PortKind::Stream,
        _ => PortKind::Held,
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
        PortType::F32 => {
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
        // Note (stream) / Buffer (dense): no held value — a placeholder `io.last` never decodes.
        _ => Arg::F32(0.0),
    }
}

/// A node in execution order, with its Lane fan-out and arena buffer wiring resolved.
pub struct PlanNode {
    pub address: String,
    /// One operator instance per Lane (Voice); `ops.len() == lanes`. Lane 0 is the
    /// graph's original instance; the rest are fresh-state [`Operator::spawn`] copies.
    pub ops: Vec<Box<dyn Operator>>,
    pub descriptor: Descriptor,
    /// Lane (Voice) count at this node.
    pub lanes: usize,
    /// For each input port (full input-port order): the source's per-Lane arena buffer indices,
    /// or `None`. `Some` only for a [`Buffer`](PortType::Buffer) input — either wired to a Buffer
    /// source (zero-copy share; inner length = the source's Lane count, 1 broadcasts) or fed by a
    /// scalar source and so **materialized** (a dedicated single-Lane scratch buffer, see
    /// `materialize`). Held / Stream inputs carry no buffer (`None`).
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
    /// The held [`Arg`] latch per input port (ADR-0030) — the unified ZOH value `io.last` reads,
    /// collapsing the former Harmony / enum / param lanes into one. Length = input count; seeded
    /// from each input's default / author override, `Copy`-normalized, carried across blocks. Render
    /// block-slices Held ports at change frames and updates the slot there.
    pub latch: Vec<Arg>,
    /// Per-input `varying` hint (ADR-0030), in input-port order — preallocated here and reused every
    /// block (no audio-thread alloc). All-`true`; Render rewrites only materialized ports each block
    /// (`false` ⇒ held unchanged this block).
    pub varying: Vec<bool>,
    /// For each **signal (Buffer) output** port — in signal-output ordinal order, the index
    /// [`crate::operator::Io::signal_mut`] uses — this node's per-Lane arena buffer indices
    /// (length `lanes`).
    pub outputs: Vec<Vec<usize>>,
    /// Message-edge routing (ADR-0014, ADR-0030): for each **message output** port — in
    /// message-output ordinal order, the index [`crate::operator::Io::emit`] uses — the
    /// `(dst node, dst input port)` pairs its emissions are delivered to. Unifies the former
    /// `msg_targets` (Note edges) and `ctx_targets` (Harmony edges): a published Harmony is just a
    /// Message to a Held input. The dst input port's [`PortKind`] decides how it lands.
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

/// One master tap: a tapped port's per-Lane arena buffers, summed into the master output.
pub struct OutputTap {
    /// Logical master channel this tap feeds (ADR-0026), or `None` to broadcast to every
    /// channel (the historical mono fan).
    pub channel: Option<usize>,
    /// Per-Lane arena buffer indices of the tapped port; all summed.
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
}

/// Why Instantiate failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// The graph has a cycle (feedback needs an explicit unit-delay; deferred).
    Cycle,
}

impl Plan {
    /// Convert an inbound OSC datagram — an address plus a flat list of primitive `Arg`s — into the
    /// single typed [`Message`] it routes to, driven by the **destination port's Arg type**
    /// (ADR-0030, the boundary). Matches the address to a node by prefix and to an input port by
    /// name (the same rule the render path uses), then calls [`crate::boundary::osc_in_arg`] with
    /// that port's [`PortType`] to type the flat args. `None` if no node/port matches or the args
    /// don't fit the port. External OSC carries no timestamp, so the Message is stamped frame 0
    /// ("now").
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
                .and_then(|p| crate::boundary::osc_in_arg(&p.ty, args))?;
            return Some(Message::new(address, arg, 0));
        }
        None
    }

    /// Instantiate a Graph into an executable Plan (the construction sub-step of a Swap).
    pub fn instantiate(mut graph: Graph, mut config: AudioConfig) -> Result<Plan, PlanError> {
        let order = topo_order(&graph)?;

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

        // 1. Lane count per node, in topo order (sources resolved before dependents).
        let mut lanes: SecondaryMap<NodeKey, usize> = SecondaryMap::new();
        for key in &order {
            let node = &graph.nodes[*key];
            let count = match node.descriptor.lanes {
                LaneRule::FromParam(slot) => {
                    (node.params.get(slot).copied().unwrap_or(1.0).round() as usize).max(1)
                }
                LaneRule::Inherit => graph
                    .connections
                    .iter()
                    .filter(|c| c.dst == *key)
                    .map(|c| lanes.get(c.src).copied().unwrap_or(1))
                    .max()
                    .unwrap_or(1),
            };
            lanes.insert(*key, count);
        }

        // 2. Assign every (node, Buffer output port, Lane) a unique arena buffer index. A message
        // output (Note / Harmony / scalar control out) carries no Signal data — events arrive via
        // routing (ADR-0014) — so it gets an empty buffer list (its emptiness is the marker that an
        // edge into it must materialize rather than share).
        let mut next_buffer = 0usize;
        let mut out_buffers: SecondaryMap<NodeKey, Vec<Vec<usize>>> = SecondaryMap::new();
        for (key, node) in &graph.nodes {
            let n_lanes = lanes[key];
            let ports = node
                .descriptor
                .outputs
                .iter()
                .map(|p| {
                    if matches!(p.ty, PortType::Buffer) {
                        (0..n_lanes)
                            .map(|_| {
                                let i = next_buffer;
                                next_buffer += 1;
                                i
                            })
                            .collect()
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

        // 3. Build PlanNodes in execution order, replicating operators per Lane.
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
                // `io.signal` (ADR-0030): wired to a Buffer source they share it zero-copy;
                // otherwise (unwired, or fed by a scalar) the engine materializes a scratch filled
                // ZOH from the latch. Vocab inputs (enum / Note / Harmony) carry no buffer — they
                // are read via `io.last` / `io.stream`.
                if kind != PortKind::Dense {
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

            // Signal (Buffer) outputs, in signal-output ordinal order — the index `io.signal_mut`
            // uses.
            let outputs: Vec<Vec<usize>> = descriptor
                .outputs
                .iter()
                .enumerate()
                .filter(|(_, p)| matches!(p.ty, PortType::Buffer))
                .map(|(port, _)| out_buffers[*key][port].clone())
                .collect();

            // Message-edge targets, one entry per message output port (message-output ordinal
            // order — the index `io.emit` uses): the `(dst node, dst input port)` pairs wired to it.
            let out_targets: Vec<Vec<(usize, usize)>> = descriptor
                .outputs
                .iter()
                .enumerate()
                .filter(|(_, p)| !matches!(p.ty, PortType::Buffer))
                .map(|(port, _)| {
                    graph
                        .connections
                        .iter()
                        .filter(|c| c.src == *key && c.src_port == port)
                        .map(|c| (index_of[c.dst], c.dst_port))
                        .collect()
                })
                .collect();

            let n_lanes = lanes[*key];
            let node = graph.nodes.remove(*key).expect("key from topo order");
            let mut ops: Vec<Box<dyn Operator>> = Vec::with_capacity(n_lanes);
            ops.push(node.op);
            for _ in 1..n_lanes {
                ops.push(ops[0].spawn());
            }

            let materialize_clean = vec![false; materialize.len()];
            nodes.push(PlanNode {
                address: node.address,
                ops,
                descriptor: node.descriptor,
                lanes: n_lanes,
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

        Ok(Plan {
            config,
            nodes,
            num_buffers: next_buffer,
            materialize_scratch_mask,
            output_taps,
            outbound_taps,
        })
    }
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
