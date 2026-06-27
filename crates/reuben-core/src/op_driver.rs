//! OpDriver — drive a single operator through the **real engine** for tests and benches
//! (candidate #1, the OpDriver reframe).
//!
//! An operator's unit tests need to feed it inputs and read its outputs. The temptation is a
//! hand-rolled `run()` per operator that builds an [`Io`](crate::operator::Io) directly — but that
//! is a *third* independent implementation of "descriptor → wired `Io`", alongside
//! [`Plan::instantiate`](crate::plan::Plan::instantiate) (the real seeding) and
//! [`process_node`](crate::render) (the real per-node step). Three impls drift.
//!
//! `OpDriver` removes the duplication: it builds a one-node [`Graph`], instantiates a real [`Plan`],
//! and steps it with a real [`Renderer`] via [`Renderer::step_node`]. It is purely an
//! **injection + observation** harness over the production substrate — drift is impossible by
//! construction. Ports are addressed by the operator's generated `IN_*` / `OUT_*` consts.
//!
//! Surface (all by-const port addressing):
//! - [`set`](OpDriver::set) — a held control (scalar / enum / `Harmony`) **or** a constant audio-in;
//!   sticky (ZOH) across blocks.
//! - [`push`](OpDriver::push) — a transient event (`Note`) at a global frame.
//! - [`drive`](OpDriver::drive) — a time-varying audio-in buffer (repoints the input's scratch).
//! - [`bind`](OpDriver::bind) — a decoded [`SampleBuffer`] for a resource slot.
//! - [`render`](OpDriver::render) — run `n` frames as ⌈n/128⌉ real production-size blocks, threading
//!   operator state + latch across them.
//! - [`output`](OpDriver::output) / [`emits`](OpDriver::emits) — read a Buffer output / the emitted
//!   Messages.
//! - [`spawn`](OpDriver::spawn) — a driver over a fresh [`Operator::spawn`] copy (carries resource
//!   bindings, resets playback state).
//!
//! Gated to test/bench builds: it reaches `Renderer`'s `pub(crate)` `step_node` seam, which is not
//! part of the public render API.

use std::sync::Arc;

use crate::config::AudioConfig;
use crate::descriptor::{Descriptor, PortType};
use crate::graph::Graph;
use crate::message::{Arg, Emit, Message};
use crate::operator::Operator;
use crate::plan::Plan;
use crate::render::Renderer;
use crate::resources::{ResolvedRefs, ResourceStore, SampleBuffer};

/// Production-size render block — the shipped default (matches `bench_support::BLOCK_SIZE`).
pub const BLOCK_SIZE: usize = 128;

/// The single node's address inside the harness graph. Arbitrary but fixed: `push` builds
/// `"<ADDR>/<port name>"` messages the real router matches back to this node's input ports.
const ADDR: &str = "op";

/// A single operator wired into a real one-node engine, driven block by block.
pub struct OpDriver {
    plan: Plan,
    renderer: Renderer,
    descriptor: Descriptor,
    sample_rate: f32,
    /// Time-varying audio-in buffers: `(input port, scratch arena buffer, samples)`. Written into
    /// the arena each block (the slot is materialize scratch, so the per-block clear skips it).
    driven: Vec<(usize, usize, Vec<f32>)>,
    /// Transient events: `(global frame, input port, payload)`. Rebased per block into messages.
    pushes: Vec<(usize, usize, Arg)>,
    /// Per Buffer-output (signal-output ordinal order): the rendered `n` frames after [`render`].
    outputs: Vec<Vec<f32>>,
    /// Emissions across the whole render, frames rebased block-absolute.
    emits: Vec<Emit>,
    /// Kept alive for the operator's resource binding (the op clones the `Arc`).
    _store: Option<Arc<ResourceStore>>,
}

impl OpDriver {
    /// Build a driver for a concrete operator `op`, addressed by its `T::descriptor()`. The op is
    /// instantiated through the real [`Plan`] path, so it sees exactly the per-node seeding the
    /// engine builds.
    pub fn for_type<T: Operator + 'static>(op: T, sample_rate: f32) -> Self {
        let descriptor = T::descriptor();
        Self::from_boxed(Box::new(op), descriptor, sample_rate)
    }

    /// Build a driver from an already-boxed operator + its descriptor — the registry-driven path
    /// (`Registry::get(kind)`), and the target of [`spawn`](OpDriver::spawn).
    pub fn from_boxed(op: Box<dyn Operator>, descriptor: Descriptor, sample_rate: f32) -> Self {
        let mut graph = Graph::new();
        graph.add_boxed(ADDR, op, descriptor.clone());
        let config = AudioConfig::new(sample_rate, BLOCK_SIZE);
        let plan = Plan::instantiate(graph, config).expect("single-node graph always instantiates");
        let renderer = Renderer::new(&plan);
        Self {
            plan,
            renderer,
            descriptor,
            sample_rate,
            driven: Vec::new(),
            pushes: Vec::new(),
            outputs: Vec::new(),
            emits: Vec::new(),
            _store: None,
        }
    }

    /// Set a held control (read via `io.last`) — scalar, enum, or `Harmony` — **or** a constant
    /// audio-in (the materialized buffer ZOH-fills from it). Sticky across blocks (the latch
    /// persists), so call it once. For a numeric value on a materialized port, seeds the
    /// materialize fill too, so `io.signal` and `io.last` agree.
    pub fn set(&mut self, port: usize, value: impl Into<Arg>) -> &mut Self {
        let node = &mut self.plan.nodes[0];
        // Force the materialized scratch to refill from the new latch on the next block.
        if let Some(k) = node.materialize.iter().position(|(p, _)| *p == port) {
            node.materialize_clean[k] = false;
        }
        node.latch[port] = value.into();
        self
    }

    /// Queue a transient event (a `Note`) on a Stream input at a **global** frame. Delivered via the
    /// real message router (built into a `"<addr>/<port>"` Message), so it lands as a zero-copy
    /// `io.stream` event in the block that contains `frame`.
    pub fn push(&mut self, port: usize, frame: usize, payload: impl Into<Arg>) -> &mut Self {
        self.pushes.push((frame, port, payload.into()));
        self
    }

    /// Drive a time-varying audio-in: write `samples` into the input's scratch buffer per block.
    /// Detaches the port from materialize (so `process_node` won't overwrite our data) and marks it
    /// `varying`, so a const-folding op (the filter) takes its modulated path.
    pub fn drive(&mut self, port: usize, samples: &[f32]) -> &mut Self {
        let node = &mut self.plan.nodes[0];
        let bi = node.inputs[port]
            .as_ref()
            .and_then(|b| b.first().copied())
            .expect("drive() target must be a Buffer/Float input with a scratch buffer");
        // Take it out of the materialize loop; the slot stays scratch (skips the per-block clear),
        // so the buffer we write survives and `varying` stays as we set it.
        if let Some(k) = node.materialize.iter().position(|(p, _)| *p == port) {
            node.materialize.remove(k);
            node.materialize_clean.remove(k);
        }
        node.varying[port] = true;
        self.driven.push((port, bi, samples.to_vec()));
        self
    }

    /// Bind a decoded sample to a resource `slot` (e.g. `"sample"`), the way the loader does — the
    /// op's `bind_resources` receives a real store + resolved ref. Keeps the store alive.
    pub fn bind(&mut self, slot: &'static str, buffer: SampleBuffer) -> &mut Self {
        let mut store = ResourceStore::new();
        let id = store.insert(slot, buffer);
        let store = Arc::new(store);
        let mut refs = ResolvedRefs::new();
        refs.set(slot, id);
        self.plan.nodes[0].ops[0].bind_resources(&store, &refs);
        self._store = Some(store);
        self
    }

    /// Render `n` frames as ⌈n/128⌉ real `step_node` blocks, threading operator state + latch (and
    /// the materialize ZOH) across them. Output/emits are captured for [`output`](OpDriver::output)
    /// / [`emits`](OpDriver::emits). The final block is partial when `n` is not a multiple of 128.
    pub fn render(&mut self, n: usize) -> &mut Self {
        let n_sig_outs = self.plan.nodes[0].outputs.len();
        self.outputs = vec![vec![0.0; n]; n_sig_outs];
        self.emits.clear();

        let mut start = 0;
        while start < n {
            let frames = (n - start).min(BLOCK_SIZE);

            // Messages for events firing in this block, frames rebased to block-local.
            let msgs: Vec<Message> = self
                .pushes
                .iter()
                .filter(|(gf, _, _)| *gf >= start && *gf < start + frames)
                .map(|(gf, port, arg)| {
                    let name = self.descriptor.inputs[*port].name;
                    Message::new(format!("{ADDR}/{name}"), arg.clone(), gf - start)
                })
                .collect();

            // Refresh each driven audio-in buffer with this block's slice.
            for (_, bi, samples) in &self.driven {
                let dst = self.renderer.arena_buffer_mut(*bi);
                for (f, slot) in dst.iter_mut().enumerate().take(frames) {
                    *slot = samples.get(start + f).copied().unwrap_or(0.0);
                }
            }

            self.renderer.step_node(&mut self.plan, 0, frames, &msgs);

            // Capture this block's output buffers (lane 0) and emissions.
            for (ord, bufs) in self.plan.nodes[0].outputs.iter().enumerate() {
                let src = self.renderer.arena_buffer(bufs[0]);
                self.outputs[ord][start..start + frames].copy_from_slice(&src[..frames]);
            }
            for e in self.renderer.last_emits() {
                self.emits.push(Emit {
                    frame: e.frame + start,
                    ..e.clone()
                });
            }

            start += frames;
        }
        self
    }

    /// The rendered samples on a Buffer output `port` (its `OUT_*` const) — `n` frames after the
    /// last [`render`](OpDriver::render).
    pub fn output(&self, port: usize) -> &[f32] {
        &self.outputs[self.signal_ordinal(port)]
    }

    /// The Messages the operator emitted across the last [`render`](OpDriver::render), block-absolute.
    pub fn emits(&self) -> &[Emit] {
        &self.emits
    }

    /// All Buffer outputs (signal-output ordinal order), each `n` frames after the last
    /// [`render`](OpDriver::render). For callers that want every output without naming each port
    /// (the micro-bench accumulator).
    pub fn outputs(&self) -> &[Vec<f32>] {
        &self.outputs
    }

    /// A driver over a fresh [`Operator::spawn`] of this one: carries resource bindings forward (the
    /// op's spawn clones them) while resetting per-Lane playback state. Configure it independently.
    pub fn spawn(&self) -> OpDriver {
        let op = self.plan.nodes[0].ops[0].spawn();
        let mut d = OpDriver::from_boxed(op, self.descriptor.clone(), self.sample_rate);
        d._store = self._store.clone();
        d
    }

    /// Map a full-declaration output port index (`OUT_*`) to its signal-output ordinal — the index
    /// [`Plan`] keys node outputs by (Buffer outputs only, in declaration order).
    fn signal_ordinal(&self, port: usize) -> usize {
        self.descriptor.outputs[..port]
            .iter()
            .filter(|p| matches!(p.ty, PortType::F32Buffer))
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Registry;

    /// Block-boundary zero-order-hold on a materialized `Float` port (ADR-0030). A held value that
    /// changes mid-block must (a) write sample-accurately from its frame within that block, and
    /// (b) carry its end-of-block value into the *next* block's materialize fill **and** into the
    /// `io.last` read — the single-source-of-truth contract the former `input_latches` f32 shadow
    /// hand-synced against `latch`. Pinned so a future re-split of the two lanes is caught loudly.
    #[test]
    fn materialized_float_zoh_holds_across_the_block_boundary() {
        let reg = Registry::builtin();
        let entry = reg.get("add").expect("add is a builtin operator");
        let mut d = OpDriver::from_boxed((entry.make)(), entry.descriptor.clone(), 48_000.0);
        let port_a = entry
            .descriptor
            .inputs
            .iter()
            .position(|p| p.name == "a")
            .expect("add has a Float input `a`");
        let out = entry
            .descriptor
            .outputs
            .iter()
            .position(|p| p.name == "out")
            .expect("add has an output `out`");

        // Block 0: held at 1.0, changes to 7.0 at frame 64 (block size 128).
        d.set(port_a, 1.0_f32);
        d.push(port_a, 64, 7.0_f32);
        d.render(2 * BLOCK_SIZE);

        let signal = d.output(out);
        // (a) sample-accurate within block 0: 1.0 before the change frame, 7.0 from it onward.
        assert!(signal[..64].iter().all(|&s| s == 1.0), "pre-change segment");
        assert!(
            signal[64..BLOCK_SIZE].iter().all(|&s| s == 7.0),
            "post-change segment"
        );
        // (b) ZOH across the boundary: block 1 holds 7.0 with no further change.
        assert!(
            signal[BLOCK_SIZE..].iter().all(|&s| s == 7.0),
            "next block holds the carried value"
        );
        // ...and `io.last` (the `latch`) reflects the same end-of-block value: one source, no drift.
        assert_eq!(
            d.plan.nodes[0].latch[port_a].as_f32(),
            Some(7.0),
            "io.last reads the carried ZOH value"
        );
    }

    /// Every registered operator builds a driver and renders a few blocks without panicking — the
    /// fidelity smoke test (the harness wires each operator's real `Io` regardless of port shape).
    /// Idle drives are fine: an un-triggered operator producing silence is still a valid render.
    #[test]
    fn every_operator_builds_a_driver_and_renders() {
        let reg = Registry::builtin();
        for name in reg.type_names() {
            let entry = reg.get(name).expect("type_names yields registered keys");
            let mut d = OpDriver::from_boxed((entry.make)(), entry.descriptor.clone(), 48_000.0);
            d.render(3 * BLOCK_SIZE);
            for (ord, buf) in d.outputs.iter().enumerate() {
                assert!(
                    buf.iter().all(|s| s.is_finite()),
                    "{name} output {ord} produced a non-finite sample"
                );
            }
        }
    }
}
