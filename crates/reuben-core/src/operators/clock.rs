//! Clock — base musical timing: a sample-accurate beat phasor at `tempo`.
//!
//! The Clock is where sample-accuracy actually lives. It free-runs on the deterministic
//! sample timeline, advancing a beat phase by `tempo / 60 / sample_rate` beats per sample,
//! so beat boundaries land on exact samples regardless of block size — the thing external
//! OSC arrival times can't honestly give us. It provides *timing only* (tempo + beat grid +
//! position); groove, swing, and meter are separate concerns.
//!
//! - input 0: `sync` (`Note` event) — a trigger port: **any** event re-zeroes the
//!   phase at its (sample-accurate) frame, locating position 1. Read via `io.read(IN_SYNC)` — the port,
//!   not the address, identifies it, so there is no address-filtering.
//! - output 0: `phase` (`Buffer`) — beat phasor, a [0, 1) sawtooth that wraps once per beat.
//!   **Unaffected by `division`** — it is always the once-per-beat phasor. The f64 accumulator is
//!   narrowed to f32 with a ceiling one ulp below `1.0`: a phase just under 1 would otherwise round
//!   up to `1.0f32` on the frame before the wrap, where `beat + phase` would equal the next beat's
//!   start one frame early.
//! - output 1: `gate` (`f32`, held Value) — 1.0 for the first half of each **1/`division` sub-beat**, else
//!   0.0; its rising edge is a sample-accurate trigger. At the default `division` 1 this is the
//!   original once-per-beat gate, high for the first half of the beat. At `division` N the gate
//!   pulses N times per beat — a 16th-note grid is `division` 4. Emitted only on change (at each
//!   edge), and never announced: a consumer added in a Swap starts with its edge flag low, so an
//!   announced high level would be a rising edge at the Swap frame, off the beat grid. Known
//!   limitation: a Swap resets each consumer's held `gate` input to that port's own default, which
//!   holds until the next edge. It matters when that default differs from the live level. For an
//!   edge consumer (default 0.0) that means a Swap landing while `gate` is high: a new one waits for
//!   the next rising edge, and a surviving one sees a falling edge at the Swap. For example,
//!   `envelope` releases early, a looping `sample` starts its release early, `euclid` sends its
//!   gate-off early if the current step is a pulse, and `sequencer` sends the held step's note-off
//!   early. A plain Value consumer whose default is nonzero (say a `gain` of 1.0) latches that
//!   default after a Swap even while `gate` is low, until the next edge.
//! - input 1: `tempo` (BPM).
//! - input 2: `division` (`i32`) — gate subdivisions per beat (1 = once per beat, default; 4 =
//!   16ths). Its `1` floor lives in the port contract (`I32Meta` min), so the gate can never
//!   collapse.
//! - output 2: `beat` (`i32`, held Value) — whole beats since the last `sync` (or since the Clock
//!   started). `beat + phase` is the musical position. It is driven by the same accumulator as
//!   `phase`, so it steps at exactly the frame `phase` wraps, and resets to 0 at a `sync` frame.
//!   Emitted at those changes, and announced (see below).
//!   After `i32::MAX` it wraps to 0, exactly as a `sync` would (about 34 years at 120 BPM): the
//!   value stays inside its declared range, and a consumer that already treats a backward jump as a
//!   locate needs no second case. A held `i32` input latches it exactly, but clamped to that
//!   input's declared range: the `*_i32_value` arithmetic operators declare ±1,000,000, so a `beat`
//!   routed through one pins at 1,000,000 (about 5.8 days at 120 BPM).
//!
//! **Announcing `beat`.** Every Swap builds a Plan whose wired held latches start at their
//! defaults: a new consumer's, and a surviving consumer's too. So the Clock *announces* `beat`'s current value
//! at frame 0 of its first `process` call after construction and of its first call after
//! `on_transplant` (a surviving Clock is transplanted into the new Plan). The Clock renders before
//! its consumers, so each reads the right beat in its first block after the Swap, and nothing
//! repeats in steady state. A beat count is a position, not a trigger, so announcing it is safe
//! where announcing `gate` is not. Every internal consumer receives the announcement. An external
//! listener on an `osc_out` can miss it: the new Engine emits it in an up-ramp segment, and
//! `RenderSlot` drops that segment's outbound when the same callback continues into the steady
//! segment, which loses any `beat` step or `gate` edge emitted there too. Separately, the install
//! discards the retiring Engine's outbound, which loses a `beat` step or `gate` edge it emitted
//! in the installing callback's down segment. The KNOWN LIMITATION in `coordinator/slot.rs` gives
//! the exact conditions.
//!
//! `tempo`/`division` are Value inputs: read held, so a change block-slices and takes effect at the
//! exact sample of the change — and each can be *wired* and modulated (`division` by an `i32`
//! source).

use alloc::boxed::Box;
use alloc::vec::Vec;
use num_traits::Float;

use crate::descriptor::Descriptor;
use crate::engine::PENDING_CAP;
use crate::operator::{Io, Operator};

// Single-source contract: one declaration -> IN_/OUT_ consts + Descriptor, no drift.
crate::operator_contract!(Clock {
    inputs:  { sync: note,
               tempo:    f32 { 1.0..=999.0, default 120.0, "BPM", lin },
               division: i32 { 1..=64,      default 1 } },
    outputs: { phase: f32_buffer,
               gate: f32 { 0.0..=1.0, default 0.0, "gate", lin },
               beat: i32 { 0..=2147483647, default 0 } },
});

#[derive(Default)]
pub struct Clock {
    /// Beat phase in [0, 1), advanced per sample. Continuous across blocks/slices.
    /// Held in f64 so the beat grid doesn't drift off the sample timeline over a long
    /// session (f32 accumulation slips audibly within seconds).
    phase: f64,
    /// The `gate` level at the end of the previous call; edges within a call are found against it.
    gate_high: bool,
    /// This (sub)block's `sync` frames, cleared per call. Holds a full control queue of syncs; a
    /// denser emitted burst grows it once and the capacity is kept.
    resets: Vec<usize>,
    /// Whole beats since the last `sync`, as of the next call's frame 0 (a wrap after the previous
    /// call's last frame included).
    beat: i32,
    /// A wrap after the previous call's last frame changed `beat` with no frame left to emit it on;
    /// the next call's frame 0 emits it.
    beat_pending: bool,
    /// Whether `beat` has been announced since construction or the last `on_transplant`. False in
    /// a fresh Clock, so the first call announces.
    announced: bool,
    /// Forces [`Clock::replay_beat`], so tests can hold the fast path against it.
    #[cfg(test)]
    replay: bool,
}

/// How many wraps one call records for `beat`. A call of `n` frames wraps at most `1 + n·dt`
/// times, and `dt` is at most 999/60/sample_rate: 16 covers a 43000-frame block at 48 kHz. A call
/// that wraps more falls back to [`Clock::replay_beat`], so the cap bounds only the fast path.
const WRAP_CAP: usize = 16;

/// The largest f32 below `1.0`: the ceiling on the emitted `phase`.
const PHASE_CEIL: f32 = 1.0 - f32::EPSILON / 2.0;

/// The beat after `beat`: wraps to 0 past `i32::MAX`, the same locate a `sync` performs.
fn next_beat(beat: i32) -> i32 {
    if beat == i32::MAX {
        0
    } else {
        beat + 1
    }
}

impl Clock {
    pub fn new() -> Self {
        Self {
            resets: Vec::with_capacity(PENDING_CAP),
            ..Self::default()
        }
    }

    /// Emit `beat` for one call from the sorted `resets` and the `phase` pass's `wraps` (each the
    /// frame after which the phase wrapped, so the beat changes at the next frame). At a frame with
    /// both, the wrap applies before the reset, as in the accumulator.
    fn emit_beat(&mut self, io: &mut Io, n: usize, resets: &[usize], wraps: &[usize]) {
        let mut out = io.write(OUT_BEAT);
        let mut beat = self.beat;
        // A reset at frame 0 emits there anyway; otherwise frame 0 carries the announcement or a
        // wrap that fell after the previous call's last frame.
        if n > 0 && (self.beat_pending || !self.announced) && resets.first() != Some(&0) {
            out.set(0, beat);
        }
        let (mut ri, mut wi) = (0, 0);
        let mut pending = false;
        loop {
            let f = match (resets.get(ri), wraps.get(wi)) {
                (None, None) => break,
                (Some(&r), None) => r,
                (None, Some(&w)) => w + 1,
                (Some(&r), Some(&w)) => r.min(w + 1),
            };
            while wi < wraps.len() && wraps[wi] + 1 == f {
                beat = next_beat(beat);
                wi += 1;
            }
            while ri < resets.len() && resets[ri] == f {
                beat = 0;
                ri += 1;
            }
            if f < n {
                out.set(f, beat);
            } else {
                // Only a wrap after the last frame lands here.
                pending = true;
            }
        }
        self.beat = beat;
        // The renderer never calls with zero frames; if one did, nothing was emitted, so the
        // pending wrap and the announcement must carry to the next call.
        if n > 0 {
            self.beat_pending = pending;
            self.announced = true;
        }
    }

    /// `beat` by a per-frame replay of the accumulator: the fallback when one call wraps more
    /// than [`WRAP_CAP`] times, and the reference [`Clock::emit_beat`] is tested against.
    fn replay_beat(&mut self, io: &mut Io, n: usize, start: f64, dt: f64, resets: &[usize]) {
        let mut out = io.write(OUT_BEAT);
        let mut phase = start;
        let mut ri = 0;
        let mut beat = self.beat;
        let mut changed = self.beat_pending || !self.announced;
        for i in 0..n {
            while ri < resets.len() && resets[ri] == i {
                phase = 0.0;
                beat = 0;
                changed = true;
                ri += 1;
            }
            if changed {
                out.set(i, beat);
                changed = false;
            }
            phase += dt;
            if phase >= 1.0 {
                phase -= 1.0;
                beat = next_beat(beat);
                changed = true;
            }
        }
        self.beat = beat;
        if n > 0 {
            self.beat_pending = changed;
            self.announced = true;
        }
    }
}

impl Operator for Clock {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let sample_rate = io.sample_rate();

        // Beats advanced per sample. Tempo is constant for this (sub)block (block-sliced).
        let dt: f64 = if sample_rate > 0.0 {
            (io.read(IN_TEMPO).max(0.0) as f64 / 60.0) / sample_rate as f64
        } else {
            0.0
        };
        // Gate subdivisions per beat. division 1 = the original once-per-beat gate; N pulses N
        // times per beat. The port's `I32Meta` carries the `1..=64` range; we keep the `1` floor
        // here too so a raw latch (the `OpDriver`/bench path) can never collapse the gate to a
        // stuck level — `phase * 0` would never toggle (the hot-path-totality rule).
        let division = f64::from(io.read(IN_DIVISION)).max(1.0);

        // Reset frames within this (sub)block, sorted. Any `sync` event re-zeroes the phase at its
        // exact sample (the port identifies it, payload ignored) — a sample-accurate
        // position locate.
        let mut resets = core::mem::take(&mut self.resets);
        resets.clear();
        for ev in io.read(IN_SYNC) {
            if ev.frame < n {
                resets.push(ev.frame);
            }
        }
        resets.sort_unstable();

        // `phase` and `gate` each replay the identical accumulator from `start` — applying resets —
        // so they stay in lock-step; a reset re-zeroes the phase before that sample is emitted.
        // `beat` is built from the wrap frames the `phase` pass records, not a third replay.
        let start = self.phase;

        let end;
        let mut wraps = [0usize; WRAP_CAP];
        let mut n_wraps = 0usize;
        {
            let out = io.write(OUT_PHASE);
            let mut phase = start;
            let mut ri = 0;
            for (i, s) in out.iter_mut().enumerate().take(n) {
                while ri < resets.len() && resets[ri] == i {
                    phase = 0.0;
                    ri += 1;
                }
                *s = (phase as f32).min(PHASE_CEIL);
                phase += dt;
                // Wrap to [0,1). `dt` is beats/sample (≤ 999/60/sr ≪ 1), so after one increment
                // `phase < 2` and a single conditional subtraction is exactly `phase.floor()` here
                // — without the per-sample out-of-line libm `floor` call (hot path).
                if phase >= 1.0 {
                    phase -= 1.0;
                    if n_wraps < WRAP_CAP {
                        wraps[n_wraps] = i;
                    }
                    n_wraps += 1;
                }
            }
            end = phase;
        }
        {
            // `gate`: replay the same accumulator, emitting only at each edge. `self.gate_high`
            // carries the level across calls, so a block boundary is not an edge. A downstream
            // Value/materialize bridge ZOH-reconstructs the dense gate.
            let mut out = io.write(OUT_GATE);
            let mut phase = start;
            let mut ri = 0;
            let mut high = self.gate_high;
            for i in 0..n {
                while ri < resets.len() && resets[ri] == i {
                    phase = 0.0;
                    ri += 1;
                }
                // Sub-beat phasor: phase·division wrapped to [0,1); gate high for its first
                // half. division 1 reduces to `phase < 0.5` exactly (bit-identical default).
                let sub = Float::fract(phase * division);
                let now_high = sub < 0.5;
                if now_high != high {
                    out.set(i, if now_high { 1.0 } else { 0.0 });
                    high = now_high;
                }
                phase += dt;
                // Wrap to [0,1). `dt` is beats/sample (≤ 999/60/sr ≪ 1), so after one increment
                // `phase < 2` and a single conditional subtraction is exactly `phase.floor()` here
                // — without the per-sample out-of-line libm `floor` call (hot path).
                if phase >= 1.0 {
                    phase -= 1.0;
                }
            }
            self.gate_high = high;
        }
        #[cfg(test)]
        let n_wraps = if self.replay { usize::MAX } else { n_wraps };
        if n_wraps <= WRAP_CAP {
            self.emit_beat(io, n, &resets, &wraps[..n_wraps]);
        } else {
            self.replay_beat(io, n, start, dt, &resets);
        }
        self.phase = end;
        self.resets = resets;
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }

    fn on_transplant(&mut self) {
        // The new Plan's consumers latch their defaults; announce the current beat to them.
        self.announced = false;
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::op_driver::OpDriver;
    use crate::vocab::pitch::Note;
    use crate::vocab::pitch::Pitch;

    const SR: f32 = 48_000.0;

    /// Drive a fresh Clock for `n` frames at `tempo` (division 1) through the real engine, with
    /// optional `sync` resets at global frames. Returns the (phase, gate) buffers. `tempo`/
    /// `division` are held `Float` controls (`set` once); each reset is a `sync` `Note` event
    /// `push`ed at its global frame (the payload is ignored — the port identifies the trigger).
    fn run(n: usize, tempo: f32, resets: &[usize]) -> (Vec<f32>, Vec<f32>) {
        run_div(n, tempo, 1.0, resets)
    }

    /// Like [`run`] but with an explicit gate `division`.
    fn run_div(n: usize, tempo: f32, division: f32, resets: &[usize]) -> (Vec<f32>, Vec<f32>) {
        let mut d = OpDriver::for_type(Clock::new(), SR);
        d.set(IN_TEMPO, tempo).set(IN_DIVISION, division);
        for &r in resets {
            d.push(IN_SYNC, r, Note::new(Pitch::Degree(0), 1.0));
        }
        d.render(n);
        // `gate` is a sparse held Value: ZOH-reconstruct the dense gate from its
        // edge emits so the edge/bit-identity assertions below read it exactly as before.
        let gate = gate_buffer(d.emits(), n);
        (d.output(OUT_PHASE).to_vec(), gate)
    }

    /// Reconstruct the dense gate from the `gate` port's sparse edge emits (ZOH): each emit sets the
    /// held level at its frame; the level holds until the next edge. Equals the old dense gate buffer.
    fn gate_buffer(emits: &[crate::message::Emit], n: usize) -> Vec<f32> {
        use crate::message::Arg;
        // Only `gate` emits `F32`; `beat`'s `I32` emits interleave with them per block.
        let gate: Vec<(usize, f32)> = emits
            .iter()
            .filter_map(|e| match e.arg {
                Arg::F32(v) => Some((e.frame, v)),
                _ => None,
            })
            .collect();
        let mut buf = vec![0.0f32; n];
        let mut level = 0.0f32;
        let mut ei = 0;
        for (i, s) in buf.iter_mut().enumerate() {
            while ei < gate.len() && gate[ei].0 == i {
                level = gate[ei].1;
                ei += 1;
            }
            *s = level;
        }
        buf
    }

    /// Indices where `gate` rises 0 -> 1.
    fn rising_edges(gate: &[f32]) -> Vec<usize> {
        let mut edges = Vec::new();
        let mut prev = 0.0f32;
        for (i, &g) in gate.iter().enumerate() {
            if prev < 0.5 && g >= 0.5 {
                edges.push(i);
            }
            prev = g;
        }
        edges
    }

    #[test]
    fn beat_grid_is_sample_accurate() {
        // 120 BPM @ 48 kHz -> 24000 samples per beat. Over 2 s (96000 frames) expect beats
        // at 0, 24000, 48000, 72000.
        let (phase, gate) = run(96_000, 120.0, &[]);

        assert_eq!(phase[0], 0.0, "phase starts at the downbeat");
        assert_eq!(gate[0], 1.0, "gate is high on the downbeat");

        let edges = rising_edges(&gate);
        assert_eq!(edges.len(), 4, "expected 4 beat triggers, got {edges:?}");
        for (k, &e) in edges.iter().enumerate() {
            let expected = k * 24_000;
            assert!(
                e.abs_diff(expected) <= 1,
                "beat {k} edge at {e}, expected ~{expected}"
            );
        }
    }

    #[test]
    fn tempo_scales_the_beat_period() {
        // Double the tempo -> half the period (12000 samples at 240 BPM).
        let (_p, gate) = run(48_000, 240.0, &[]);
        let edges = rising_edges(&gate);
        assert!(edges.len() >= 2);
        let period = edges[1] - edges[0];
        assert!(
            period.abs_diff(12_000) <= 1,
            "expected ~12000-sample period at 240 BPM, got {period}"
        );
    }

    #[test]
    fn phase_is_continuous_across_calls() {
        // The phase at the start of a second render equals where the first render left off — the
        // f64 phase threads across the real 128-frame blocks and across separate `render` calls.
        let n = 1000;
        let (p_whole, _) = run(2 * n, 120.0, &[]);

        let mut split = OpDriver::for_type(Clock::new(), SR);
        split.set(IN_TEMPO, 120.0).set(IN_DIVISION, 1.0);
        let p1 = split.render(n).output(OUT_PHASE).to_vec();
        let p2 = split.render(n).output(OUT_PHASE).to_vec();

        assert_eq!(p1[0], p_whole[0]);
        for i in 0..n {
            assert!((p1[i] - p_whole[i]).abs() < 1e-6, "block 1 differs at {i}");
            assert!(
                (p2[i] - p_whole[n + i]).abs() < 1e-6,
                "block 2 differs at {i}"
            );
        }
    }

    #[test]
    fn reset_rezeroes_phase_at_its_frame() {
        // 120 BPM: beat is 24000 samples, gate high for the first 12000. A reset at 15000
        // (when the gate is low, mid-beat) restarts the beat: phase[15000] == 0 and the
        // gate retriggers (rising edge at 15000).
        let n = 16_000;
        let r = 15_000;
        let (phase, gate) = run(n, 120.0, &[r]);

        assert!(
            phase[r - 1] > 0.5,
            "phase should be mid-beat before the reset"
        );
        assert_eq!(gate[r - 1], 0.0, "gate is low mid-beat before the reset");
        assert_eq!(phase[r], 0.0, "phase re-zeroes at the reset frame");
        assert!(rising_edges(&gate).contains(&r), "gate retriggers at reset");
    }

    #[test]
    fn spawned_clock_has_independent_state() {
        let mut a = OpDriver::for_type(Clock::new(), SR);
        a.set(IN_TEMPO, 120.0).set(IN_DIVISION, 1.0);
        a.render(5_000);
        // The fresh spawn starts at the downbeat regardless of `a`'s advanced phase.
        let mut b = a.spawn();
        let phase = b
            .set(IN_TEMPO, 120.0)
            .set(IN_DIVISION, 1.0)
            .render(1)
            .output(OUT_PHASE)
            .to_vec();
        assert_eq!(phase[0], 0.0, "spawned clock starts fresh at phase 0");
    }

    // --- V1.3 `division` param: subdivide the gate ---

    #[test]
    fn division_one_is_bit_identical_to_the_default_gate() {
        // division 1 must reproduce the original once-per-beat gate exactly. The original logic
        // was `gate = if phase < 0.5 { 1 } else { 0 }` on the f64 beat phasor; replay that exact
        // accumulator here as the reference and compare bit-for-bit.
        let n = 96_000;
        let (_phase, gate) = run_div(n, 120.0, 1.0, &[]);

        let dt = (120.0_f64 / 60.0) / SR as f64;
        let mut ref_phase = 0.0f64;
        for (i, &g) in gate.iter().enumerate() {
            let expected: f32 = if ref_phase < 0.5 { 1.0 } else { 0.0 };
            assert_eq!(g.to_bits(), expected.to_bits(), "gate differs at {i}");
            ref_phase += dt;
            ref_phase -= ref_phase.floor();
        }
    }

    #[test]
    fn division_four_quadruples_the_rising_edges() {
        // 120 BPM @ 48 kHz over 1 beat (24000 frames): division 1 fires 1 rising edge, division
        // 4 fires 4 (one per 16th note). The phase phasor is unchanged either way.
        let n = 24_000;
        let (_p1, g1) = run_div(n, 120.0, 1.0, &[]);
        let (p4, g4) = run_div(n, 120.0, 4.0, &[]);

        assert_eq!(rising_edges(&g1).len(), 1, "division 1: one beat trigger");
        let edges4 = rising_edges(&g4);
        assert_eq!(
            edges4.len(),
            4,
            "division 4: four 16th triggers, got {edges4:?}"
        );
        // Edges land at the quarter-beat marks (0, 6000, 12000, 18000).
        for (k, &e) in edges4.iter().enumerate() {
            let expected = k * 6_000;
            assert!(
                e.abs_diff(expected) <= 1,
                "16th {k} edge at {e}, expected ~{expected}"
            );
        }

        // The phase output is still the once-per-beat phasor regardless of division.
        assert_eq!(p4[0], 0.0, "phase still starts at the downbeat");
        let phase_edges = {
            // phase wraps once per beat: it never resets within this single beat.
            let mut wraps = 0;
            for i in 1..n {
                if p4[i] < p4[i - 1] {
                    wraps += 1;
                }
            }
            wraps
        };
        assert_eq!(
            phase_edges, 0,
            "phase wraps once per beat, not per sub-beat"
        );
    }

    #[test]
    fn zero_division_does_not_stick_the_gate() {
        // A raw `division` latch of 0 — the `OpDriver`/bench path bypasses the port's `1..` floor
        // — would make `(phase * 0).fract()` a constant, sticking the gate at one level after the
        // first block (silence downstream, no error). `process` floors division at 1, so the beat
        // gate still toggles. 48000 frames @ 120 BPM = 2 beats → at least two rising edges.
        let n = 48_000;
        let (_p, gate) = run_div(n, 120.0, 0.0, &[]);
        assert!(
            rising_edges(&gate).len() >= 2,
            "a zero-division latch must not collapse the gate"
        );
    }

    #[test]
    fn tempo_change_mid_render_takes_effect_at_its_frame() {
        // The module-doc invariant for the held Value inputs: a `tempo` change block-slices
        // and takes effect at the *exact sample* of the change (not the next 128-block boundary),
        // with the beat phase continuous across the cut. The change lands mid-beat (phase 0.75)
        // so a phase reset at the slice would be visible — at a beat boundary the wrap masks it.
        let n = 48_000;
        let cut = 18_000; // 120 BPM @ 48 kHz: 0.75 beats in — mid-beat, gate low
        let mut d = OpDriver::for_type(Clock::new(), SR);
        d.set(IN_TEMPO, 120.0).set(IN_DIVISION, 1.0);
        d.push(IN_TEMPO, cut, 240.0);
        d.render(n);
        let phase = d.output(OUT_PHASE).to_vec();
        let gate = gate_buffer(d.emits(), n);

        let old_dt = 1.0 / 24_000.0f64; // 120 BPM beats/sample
        let new_dt = 1.0 / 12_000.0f64; // 240 BPM beats/sample
                                        // Phase continuity at the cut: the step *onto* the cut frame is still the old dt (the
                                        // accumulator carried across the slice, no re-zero)...
        let step_in = phase[cut] as f64 - phase[cut - 1] as f64;
        assert!(
            (step_in - old_dt).abs() < 1e-6,
            "step onto the cut should be the old dt: got {step_in}, want {old_dt}"
        );
        // ...and the very next step advances at the new dt — 240 BPM from exactly frame `cut`.
        let step_out = phase[cut + 1] as f64 - phase[cut] as f64;
        assert!(
            (step_out - new_dt).abs() < 1e-6,
            "step off the cut should be the new dt: got {step_out}, want {new_dt}"
        );

        // The beat grid downstream: the remaining 0.25 beat takes 3000 samples at 240 BPM (wrap
        // at 21000), then the 12000-sample 240 BPM grid. A tempo latched at block granularity or
        // a phase reset at the slice shifts every one of these trigger frames.
        let edges = rising_edges(&gate);
        let expected = [0usize, 21_000, 33_000, 45_000];
        assert_eq!(
            edges.len(),
            expected.len(),
            "expected {} beat triggers, got {edges:?}",
            expected.len()
        );
        for (k, (&e, &x)) in edges.iter().zip(expected.iter()).enumerate() {
            assert!(e.abs_diff(x) <= 1, "beat {k} edge at {e}, expected ~{x}");
        }
    }

    // --- `beat`: whole beats since `sync`, from the `phase` accumulator ---

    /// Drive `op` for `total` frames in `render` calls of `chunk` frames at `tempo`, with optional
    /// `sync` resets at global frames. Returns the dense `phase`, the ZOH-reconstructed `beat` per
    /// frame, and the global frames `beat` was emitted at.
    fn run_beat(
        op: Clock,
        total: usize,
        chunk: usize,
        tempo: f32,
        resets: &[usize],
    ) -> (Vec<f32>, Vec<i32>, Vec<usize>) {
        use crate::message::Arg;
        let mut d = OpDriver::for_type(op, SR);
        d.set(IN_TEMPO, tempo).set(IN_DIVISION, 1.0);
        let (mut phase, mut beat, mut emits) = (vec![], vec![], vec![]);
        let mut level = None;
        let mut at = 0;
        // `OpDriver::push` frames are relative to each `render` call and replay on every later
        // call, so a run with resets is one call (still split into `BLOCK_SIZE` blocks).
        assert!(resets.is_empty() || chunk >= total, "resets need a single render call");
        for &r in resets {
            d.push(IN_SYNC, r, Note::new(Pitch::Degree(0), 1.0));
        }
        while at < total {
            let n = chunk.min(total - at);
            d.render(n);
            phase.extend_from_slice(d.output(OUT_PHASE));
            // Only `beat` emits `I32`; they arrive in frame order among themselves.
            let mut ev = d.emits().iter().filter_map(|e| match e.arg {
                Arg::I32(v) => Some((e.frame, v)),
                _ => None,
            });
            let mut next = ev.next();
            for i in 0..n {
                while let Some((_, v)) = next.filter(|&(f, _)| f == i) {
                    level = Some(v);
                    emits.push(at + i);
                    next = ev.next();
                }
                beat.push(level.expect("beat is emitted at frame 0"));
            }
            assert!(next.is_none(), "beat emits are in frame order");
            at += n;
        }
        (phase, beat, emits)
    }

    /// The reference: replay the f64 accumulator, returning per frame the expected beat and
    /// whether that frame is a wrap (its phase is the wrapped value).
    fn reference_beats(total: usize, tempo: f32, resets: &[usize]) -> (Vec<i32>, Vec<bool>) {
        let dt = (f64::from(tempo) / 60.0) / f64::from(SR);
        let (mut phase, mut beat, mut wrapped) = (0.0f64, 0i32, false);
        let (mut beats, mut wraps) = (vec![], vec![]);
        for i in 0..total {
            if resets.contains(&i) {
                phase = 0.0;
                beat = 0;
            }
            beats.push(beat);
            wraps.push(wrapped);
            phase += dt;
            wrapped = phase >= 1.0;
            if wrapped {
                phase -= 1.0;
                beat += 1;
            }
        }
        (beats, wraps)
    }

    #[test]
    fn beat_steps_at_the_frame_phase_wraps() {
        for &tempo in &[60.0, 120.0, 173.3, 999.0] {
            for &chunk in &[1usize, 7, 64, 128, 1000] {
                let total = 60_000;
                let (phase, beat, emits) = run_beat(Clock::new(), total, chunk, tempo, &[]);
                let (want, wraps) = reference_beats(total, tempo, &[]);
                assert_eq!(beat, want, "beat drifted at {tempo} BPM, chunk {chunk}");
                assert!(*beat.last().unwrap() >= 1, "the run spans a beat");
                for i in 1..total {
                    let phase_wrapped = phase[i] < phase[i - 1];
                    assert_eq!(
                        phase_wrapped, wraps[i],
                        "phase wrap at {i} ({tempo} BPM, chunk {chunk})"
                    );
                    assert_eq!(
                        beat[i] != beat[i - 1],
                        phase_wrapped,
                        "beat and phase disagree at {i} ({tempo} BPM, chunk {chunk})"
                    );
                }
                // Announced at frame 0, then emitted exactly at every wrap: no block-start repeats.
                let want_emits: Vec<usize> = (0..total)
                    .filter(|&i| i == 0 || wraps[i])
                    .collect();
                assert_eq!(emits, want_emits, "{tempo} BPM, chunk {chunk}");
            }
        }
    }

    #[test]
    fn sync_resets_beat_and_phase_at_its_frame() {
        // 240 BPM: a beat is 12000 frames. A sync 2.5 beats in, and another 1.25 beats later.
        // 44_928 is a block start (351 * 128); 30_000 is mid-block.
        let resets = [30_000, 44_928];
        let total = 60_000;
        let (phase, beat, emits) = run_beat(Clock::new(), total, total, 240.0, &resets);
        let (want, _) = reference_beats(total, 240.0, &resets);
        assert_eq!(beat, want);
        assert_eq!(beat[29_999], 2, "two beats elapsed before the first sync");
        assert_eq!(beat[44_927], 1, "one beat since the first sync");
        assert!(beat[59_999] >= 1, "counting resumes after the second sync");
        for &r in &resets {
            assert_eq!(phase[r], 0.0, "phase re-zeroes at {r}");
            assert_eq!(beat[r], 0, "beat resets at {r}");
            assert!(emits.contains(&r), "beat is emitted at the sync frame {r}");
        }
    }

    #[test]
    fn beat_and_phase_are_chunk_size_independent() {
        let total = 50_000;
        let (p1, b1, _) = run_beat(Clock::new(), total, 1, 173.3, &[]);
        for &chunk in &[7usize, 64, 128, 333] {
            let (p, b, _) = run_beat(Clock::new(), total, chunk, 173.3, &[]);
            assert!(
                p.iter().zip(&p1).all(|(a, b)| a.to_bits() == b.to_bits()),
                "phase, chunk {chunk}"
            );
            assert_eq!(b, b1, "beat, chunk {chunk}");
        }
    }

    #[test]
    fn phase_never_emits_one() {
        // Just under 1 in f64 rounds to 1.0 in f32; the emitted phase must stay below it.
        let start = 1.0 - 1e-12;
        assert_eq!(start as f32, 1.0, "the case under test rounds up when cast");
        let clock = Clock { phase: start, ..Clock::new() };
        let mut d = OpDriver::for_type(clock, SR);
        d.set(IN_TEMPO, 1.0).set(IN_DIVISION, 1.0).render(2);
        let phase = d.output(OUT_PHASE).to_vec();
        assert!(phase[0] < 1.0, "phase {} reached 1.0", phase[0]);
        assert_eq!(PHASE_CEIL.to_bits(), 1.0f32.to_bits() - 1, "one ulp below 1.0");
        assert_eq!(phase[0], PHASE_CEIL);
        assert!(phase[1] < 0.01, "and wraps on the next frame: {}", phase[1]);
    }

    #[test]
    fn phase_stays_below_one_over_a_long_run() {
        for &tempo in &[120.0, 997.7, 999.0] {
            let mut d = OpDriver::for_type(Clock::new(), SR);
            d.set(IN_TEMPO, tempo).set(IN_DIVISION, 1.0);
            // Ten minutes of audio.
            for _ in 0..600 {
                let out = d.render(48_000).output(OUT_PHASE);
                let max = out.iter().copied().fold(0.0f32, f32::max);
                assert!(max < 1.0, "phase reached {max} at {tempo} BPM");
            }
        }
    }

    #[test]
    fn beat_wraps_to_zero_past_i32_max() {
        // Start one beat short of `i32::MAX`. At 999 BPM a beat is ~2883 frames, so 7000 frames
        // cross two wraps: MAX-1 -> MAX -> 0, the same locate a `sync` performs.
        let clock = Clock { beat: i32::MAX - 1, ..Clock::new() };
        let (_, beat, _) = run_beat(clock, 7_000, 128, 999.0, &[]);
        let mut seen = vec![beat[0]];
        for &b in &beat[1..] {
            if b != *seen.last().unwrap() {
                seen.push(b);
            }
        }
        assert_eq!(seen, vec![i32::MAX - 1, i32::MAX, 0]);
    }

    #[test]
    fn beat_is_announced_once_after_construction_and_after_transplant() {
        use crate::message::Arg;
        // 240 BPM: a beat every 12000 frames, the gate high for the first 6000. Frames 0..5120
        // hold no edge after frame 0 and no wrap, so only an announcement emits after frame 0.
        let mut d = OpDriver::for_type(Clock::new(), SR);
        d.set(IN_TEMPO, 240.0).set(IN_DIVISION, 1.0);
        let first = d.render(2048).emits().to_vec();
        let announced = |es: &[crate::message::Emit]| -> Vec<(usize, Arg)> {
            es.iter().map(|e| (e.frame, e.arg.clone())).collect()
        };
        assert_eq!(
            announced(&first),
            vec![(0, Arg::F32(1.0)), (0, Arg::I32(0))],
            "the gate's first rising edge, and beat's announcement; later blocks repeat nothing"
        );
        assert!(d.render(1024).emits().is_empty(), "steady state emits nothing");

        d.on_transplant();
        let after = d.render(1024).emits().to_vec();
        assert_eq!(
            announced(&after),
            vec![(0, Arg::I32(0))],
            "the first call after a transplant announces beat once, and not the unchanged gate"
        );
        assert!(d.render(1024).emits().is_empty(), "then nothing repeats");
    }

    /// Every emit (frame, port, value) and the dense `phase` of one Clock over `total` frames in
    /// `chunk`-frame renders, at `sr`.
    fn run_all(
        op: Clock,
        sr: f32,
        total: usize,
        chunk: usize,
        tempo: f32,
        resets: &[usize],
    ) -> (Vec<u32>, Vec<(usize, usize, crate::message::Arg)>) {
        let mut d = OpDriver::for_type(op, sr);
        d.set(IN_TEMPO, tempo).set(IN_DIVISION, 1.0);
        assert!(resets.is_empty() || chunk >= total, "resets need a single render call");
        for &r in resets {
            d.push(IN_SYNC, r, Note::new(Pitch::Degree(0), 1.0));
        }
        let (mut phase, mut emits) = (vec![], vec![]);
        let mut at = 0;
        while at < total {
            let n = chunk.min(total - at);
            d.render(n);
            phase.extend(d.output(OUT_PHASE).iter().map(|v| v.to_bits()));
            emits.extend(d.emits().iter().map(|e| (at + e.frame, e.port, e.arg.clone())));
            at += n;
        }
        (phase, emits)
    }

    fn replaying() -> Clock {
        Clock {
            replay: true,
            ..Clock::new()
        }
    }

    #[test]
    fn beat_from_recorded_wraps_matches_the_per_frame_replay() {
        // Chunked runs: tempo extremes, block sizes that split wraps across calls.
        for &tempo in &[1.0, 60.0, 173.3, 999.0] {
            for &chunk in &[1usize, 7, 64, 128, 1000] {
                let fast = run_all(Clock::new(), SR, 100_000, chunk, tempo, &[]);
                let slow = run_all(replaying(), SR, 100_000, chunk, tempo, &[]);
                assert_eq!(fast, slow, "{tempo} BPM, chunk {chunk}");
            }
        }
        // Resets: at frame 0, on block starts, mid-block, doubled, on consecutive frames, and next
        // to a wrap (999 BPM wraps about every 2883 frames).
        let resets = [
            0, 1, 128, 2_882, 2_883, 2_884, 5_000, 5_000, 6_400, 6_401, 9_999, 12_800,
        ];
        for &tempo in &[1.0, 120.0, 999.0] {
            let fast = run_all(Clock::new(), SR, 40_000, 40_000, tempo, &resets);
            let slow = run_all(replaying(), SR, 40_000, 40_000, tempo, &resets);
            assert_eq!(fast, slow, "{tempo} BPM with resets");
        }
        // A reset on the very frame a wrap lands: the wrap applies first, then the reset wins.
        let (_, plain) = run_all(Clock::new(), SR, 10_000, 10_000, 999.0, &[]);
        let wrap = plain
            .iter()
            .find_map(|(f, _, a)| matches!(a, crate::message::Arg::I32(1)).then_some(*f))
            .expect("a wrap within the run");
        let on_wrap = [wrap];
        let fast = run_all(Clock::new(), SR, 10_000, 10_000, 999.0, &on_wrap);
        let slow = run_all(replaying(), SR, 10_000, 10_000, 999.0, &on_wrap);
        assert_eq!(fast, slow, "a reset on the wrap frame {wrap}");
    }

    #[test]
    fn a_call_with_more_wraps_than_the_cap_falls_back_and_stays_exact() {
        // 999 BPM at 100 Hz wraps every ~6 frames: ~21 wraps a 128-frame block, past `WRAP_CAP`.
        let sr = 100.0;
        let (phase, emits) = run_all(Clock::new(), sr, 1_280, 128, 999.0, &[]);
        let (want_phase, want_emits) = run_all(replaying(), sr, 1_280, 128, 999.0, &[]);
        assert_eq!(phase, want_phase);
        assert_eq!(emits, want_emits);
        let dt = (999.0f64 / 60.0) / f64::from(sr);
        let (mut p, mut beat) = (0.0f64, 0i32);
        for _ in 0..1_280 {
            p += dt;
            if p >= 1.0 {
                p -= 1.0;
                beat += 1;
            }
        }
        let last = emits
            .iter()
            .rev()
            .find_map(|(_, _, a)| match a {
                crate::message::Arg::I32(v) => Some(*v),
                _ => None,
            })
            .unwrap();
        assert!(beat > 200, "the run wraps far past the cap");
        // A wrap after the last frame is still pending, not yet emitted.
        assert!(last == beat || last == beat - 1, "last beat {last}, want ~{beat}");
    }
}
