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
//!
//! **Input injection**: at the single-node level, [`drive`](OpDriver::drive) is
//! the known-buffer seam — it is how a loader-built signal `Pipe` (an interface input pipe's
//! runtime node) is driven with deterministic audio in tests. At the *graph* level, the same
//! carve-out is [`Renderer::render_block_multi`]'s `inputs` parameter: the offline render path
//! injects known buffers per logical input channel, so a render with injected input stays
//! bit-reproducible while live device input remains the sanctioned nondeterministic boundary.
//!
//! see rules: execution-runtime

use std::sync::Arc;

use crate::config::AudioConfig;
use crate::descriptor::Descriptor;
use crate::graph::Graph;
use crate::message::{Arg, Emit, Message};
use crate::operator::{Operator, PortIndex};
use crate::plan::Plan;
use crate::render::Renderer;
use crate::resources::{ResolvedRefs, ResourceStore, SampleBuffer};
use crate::signal::{AudioSample, BlockView};

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
    driven: Vec<(usize, usize, Vec<AudioSample>)>,
    /// Transient events: `(global frame, input port, payload)`. Rebased per block into messages.
    pushes: Vec<(usize, usize, Arg)>,
    /// Per Buffer-output (signal-output ordinal order): the rendered `n` frames after [`render`].
    outputs: Vec<Vec<AudioSample>>,
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

    /// Set a held control (read via `io.read`) — scalar, enum, or `Harmony` — **or** a constant
    /// audio-in (the materialized buffer ZOH-fills from it). Sticky across blocks (the latch
    /// persists), so call it once. For a numeric value on a materialized port, seeds the
    /// materialize fill too, so the dense read and the held latch agree.
    pub fn set(&mut self, port: impl PortIndex, value: impl Into<Arg>) -> &mut Self {
        let port = port.index();
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
    /// `io.read` event in the block that contains `frame`.
    pub fn push(
        &mut self,
        port: impl PortIndex,
        frame: usize,
        payload: impl Into<Arg>,
    ) -> &mut Self {
        self.pushes.push((frame, port.index(), payload.into()));
        self
    }

    /// Drive a time-varying audio-in: write `samples` into the input's scratch buffer per block.
    /// Detaches the port from materialize (so `process_node` won't overwrite our data) and marks it
    /// `varying`, so a const-folding op (the filter) takes its modulated path.
    pub fn drive(&mut self, port: impl PortIndex, samples: BlockView<'_>) -> &mut Self {
        let port = port.index();
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

            // Capture this block's output buffers and emissions.
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
    pub fn output(&self, port: impl PortIndex) -> BlockView<'_> {
        &self.outputs[self.signal_ordinal(port.index())]
    }

    /// The Messages the operator emitted across the last [`render`](OpDriver::render), block-absolute.
    pub fn emits(&self) -> &[Emit] {
        &self.emits
    }

    /// All Buffer outputs (signal-output ordinal order), each `n` frames after the last
    /// [`render`](OpDriver::render). For callers that want every output without naming each port
    /// (the micro-bench accumulator).
    pub fn outputs(&self) -> &[Vec<AudioSample>] {
        &self.outputs
    }

    /// Invoke the operator's [`Operator::on_transplant`] hook — the survivor-side seam a Swap runs
    /// after box-transplant. Lets an operator's unit test assert its post-swap
    /// re-assertion behavior (an on-change held publisher must re-emit its current value the next
    /// block, even with no input change).
    pub fn on_transplant(&mut self) -> &mut Self {
        self.plan.nodes[0].ops[0].on_transplant();
        self
    }

    /// A driver over a fresh [`Operator::spawn`] of this one: carries resource bindings forward (the
    /// op's spawn clones them) while resetting playback state. Configure it independently.
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
            .filter(|p| p.ty.is_buffer())
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::load;
    use crate::operators::{oscillator, Oscillator};
    use crate::registry::Registry;

    /// **Behavioral / end-to-end fidelity pin** (issue #89), the output-equivalence complement to the
    /// *structural* wire-form pin in `tests/wire_forms.rs`.
    ///
    /// The structural pin proves `OpDriver`'s wiring arrays (`latched` / `varying` / input-buffer
    /// presence) match a one-node `Plan`'s `PlanNode` — it pins the *wiring* drift surface but does
    /// **not** prove `process` produces identical *samples* through the driver's `step_node` seam vs.
    /// the production `render_block` → `render_plan` → `process_node` stepping path. This test closes
    /// that gap: it drives the same operator both ways and asserts sample-exact output equivalence.
    ///
    /// Representative operator: `oscillator` — a deterministic, no-event, no-resource signal generator
    /// (a non-trivial varying waveform, so block-threaded phase state would expose any drift).
    ///
    /// Master-bus confound (the issue's open question): the rendered buffer is the master-tap *sum*,
    /// not the node's raw output. For a single broadcast tap into a mono master, that sum is an exact
    /// copy, and `output` is a verified sample-exact unity passthrough (`operators/output.rs`) — so a
    /// one-node `oscillator → output` instrument's master buffer *is* the operator's raw output, with
    /// no gain stage to confound it. We render the real side through the full public `load` →
    /// `Plan::instantiate` → `render_block` path (JSON instrument included), matching `OpDriver`'s
    /// 48 kHz / 128-frame block geometry so the comparison is sample-for-sample.
    #[test]
    fn op_driver_output_matches_the_real_render_path_sample_exact() {
        const SR: f32 = 48_000.0;
        const FREQ: f32 = 440.0;
        const BLOCKS: usize = 4;
        const N: usize = BLOCKS * BLOCK_SIZE;

        // --- Driver side: oscillator through OpDriver's `step_node` seam. ---
        let mut d = OpDriver::for_type(Oscillator::new(), SR);
        d.set(oscillator::IN_FREQ, FREQ);
        d.render(N);
        let driver_out = d.output(oscillator::OUT_AUDIO).to_vec();

        // --- Real side: the same oscillator as a one-node unity-passthrough instrument, driven
        // through the production `load` → `instantiate` → `render_block` path. `/out` is `output`,
        // a sample-exact unity passthrough, so the master tap equals the oscillator's raw output. ---
        let json = r#"{
            "instrument": "behavioral_pin_osc",
            "nodes": [
                { "type": "oscillator", "address": "/osc", "inputs": { "freq": 440.0 } },
                { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/osc" } } }
            ],
            "outputs": [ { "node": "/out", "port": "audio" } ]
        }"#;
        let graph = load(json, &Registry::builtin()).expect("instrument loads");
        let config = AudioConfig::new(SR, BLOCK_SIZE);
        let mut plan = Plan::instantiate(graph, config).expect("instrument instantiates");
        let mut renderer = Renderer::new(&plan);
        let mut block = vec![0.0f32; BLOCK_SIZE];
        let mut real_out = Vec::with_capacity(N);
        for _ in 0..BLOCKS {
            // No messages: `freq` is a held Value, materialized identically on both paths.
            renderer.render_block(&mut plan, &[], &mut block);
            real_out.extend_from_slice(&block);
        }

        assert_eq!(driver_out.len(), N);
        assert_eq!(real_out.len(), N);
        // Bit-exact: both paths run identical DSP on identical per-node seeding at the same block
        // geometry. Any divergence between the driver's `step_node` and production `process_node`
        // stepping — the drift this pin guards — shows up as a sample mismatch.
        assert_eq!(
            driver_out, real_out,
            "OpDriver output diverged from the real render path"
        );
        // Guard against the degenerate pass where both sides are silent (e.g. a future change that
        // zeroes the oscillator): a 440 Hz tone must actually be present.
        let peak = real_out.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        assert!(
            peak > 0.05,
            "expected an audible tone, got near-silence (peak {peak})"
        );
    }

    /// Block-boundary zero-order-hold on a materialized `Float` port. A held value that
    /// changes mid-block must (a) write sample-accurately from its frame within that block, and
    /// (b) carry its end-of-block value into the *next* block's materialize fill **and** into the
    /// held read — the single-source-of-truth contract the former `input_latches` f32 shadow
    /// hand-synced against `latch`. Pinned so a future re-split of the two lanes is caught loudly.
    #[test]
    fn materialized_float_zoh_holds_across_the_block_boundary() {
        let reg = Registry::builtin();
        let entry = reg
            .get("add_f32_signal")
            .expect("add_f32_signal is a builtin operator");
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
        // ...and the held read (the `latch`) reflects the same end-of-block value: one source, no drift.
        assert_eq!(
            d.plan.nodes[0].latch[port_a].as_f32(),
            Some(7.0),
            "the held read sees the carried ZOH value"
        );
    }

    /// **Buffer-presence invariant** (the engine half of the typed-handle contract):
    /// every declared `f32_buffer` input handed to `process` is a dense buffer of **exactly
    /// `frames` samples** — an unwired *bare* buffer input (no meta, no source) materializes
    /// **silence** (zeros), never `&[]` or a short slice. This is what lets `io.read(SIG)[i]`
    /// index directly with no `.get(i).unwrap_or(..)` guard in every operator.
    ///
    /// The probe encodes the check into its output: `out[i] = in[i] + 10·(in.len() == frames)`,
    /// so a uniform `10.0` output proves the input read as length-n zeros through the real
    /// engine seeding + stepping (a `&[]` read would yield `0.0`s — and, first, trip the
    /// `debug_assert` inside the Signal handle read).
    #[test]
    fn unwired_bare_buffer_input_reads_length_n_zeros() {
        crate::operator_contract!(BarePresenceProbe {
            inputs:  { audio: f32_buffer },
            outputs: { audio: f32_buffer },
        });
        struct BarePresenceProbe;
        impl Operator for BarePresenceProbe {
            fn descriptor() -> Descriptor {
                Self::contract()
            }
            fn process(&mut self, io: &mut crate::operator::Io) {
                let n = io.frames();
                let input = io.read(IN_AUDIO);
                let ok = if input.len() == n { 10.0 } else { 0.0 };
                for (i, slot) in io.write(OUT_AUDIO)[..n].iter_mut().enumerate() {
                    *slot = input[i] + ok;
                }
            }
            fn spawn(&self) -> Box<dyn Operator> {
                Box::new(BarePresenceProbe)
            }
        }

        // Nothing wired, nothing set: the bare `audio` input must still present n zeros.
        let mut d = OpDriver::for_type(BarePresenceProbe, 48_000.0);
        d.render(2 * BLOCK_SIZE + 32); // partial final block: the invariant holds per sub-block
        assert!(
            d.output(OUT_AUDIO).iter().all(|&s| s == 10.0),
            "unwired bare buffer input must read as length-n silence"
        );
    }

    /// The loader-built interface pipe through the real engine substrate: a signal
    /// `Pipe` driven with a known buffer reproduces it verbatim — the single-node face of the
    /// P3 injection seam. `Pipe` is deliberately unregistered, so the registry smoke test
    /// below never covers it; this drives it through `from_boxed` like the loader does.
    #[test]
    fn signal_pipe_driven_with_a_known_buffer_reproduces_it() {
        use crate::operators::pipe::Pipe;
        use crate::plan::PortKind;

        let n = 2 * BLOCK_SIZE + 17; // partial final block: the copy holds per sub-block
        let samples: Vec<AudioSample> =
            (0..n).map(|i| ((i * 7) % 31) as f32 / 31.0 - 0.5).collect();
        let mut d = OpDriver::from_boxed(
            Box::new(Pipe::new(PortKind::Signal)),
            Pipe::descriptor(),
            48_000.0,
        );
        d.drive(0, &samples);
        d.render(n);
        assert_eq!(
            d.output(0),
            &samples[..],
            "a signal pipe must pass a driven buffer through bit-exact"
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
