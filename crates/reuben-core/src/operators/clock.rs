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
//!   **Unaffected by `division`** — it is always the once-per-beat phasor.
//! - output 1: `gate` (`Buffer`) — 1.0 for the first half of each **1/`division` sub-beat**, else
//!   0.0; its rising edge is a sample-accurate trigger. At the default `division` 1 this is the
//!   original once-per-beat gate, high for the first half of the beat. At `division` N the gate
//!   pulses N times per beat — a 16th-note grid is `division` 4.
//! - input 1: `tempo` (BPM).
//! - input 2: `division` (`i32`) — gate subdivisions per beat (1 = once per beat, default; 4 =
//!   16ths). Its `1` floor lives in the port contract (`I32Meta` min), so the gate can never
//!   collapse.
//!
//! `tempo`/`division` are Value inputs: read held, so a change block-slices and takes effect at the
//! exact sample of the change — and each can be *wired* and modulated (`division` by an `i32`
//! source).

use smallvec::SmallVec;

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};

// Single-source contract: one declaration -> IN_/OUT_ consts + Descriptor, no drift.
crate::operator_contract!(Clock {
    inputs:  { sync: note,
               tempo:    f32 { 1.0..=999.0, default 120.0, "BPM", lin },
               division: i32 { 1..=64,      default 1 } },
    outputs: { phase: f32_buffer, gate: f32 { 0.0..=1.0, default 0.0, "gate", lin } },
});

#[derive(Default)]
pub struct Clock {
    /// Beat phase in [0, 1), advanced per sample. Continuous across blocks/slices.
    /// Held in f64 so the beat grid doesn't drift off the sample timeline over a long
    /// session (f32 accumulation slips audibly within seconds).
    phase: f64,
    /// Last `gate` level emitted: `gate` is a sparse held Value (`MsgWriter`), so it
    /// emits one change per rising/falling edge instead of filling a dense buffer. Persists across
    /// blocks so the first frame of a block only re-emits if the level actually changed.
    gate_high: bool,
}

impl Clock {
    pub fn new() -> Self {
        Self::default()
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
        // times per beat. The `1` floor lives in the port's `I32Meta` (clamped before we read), so
        // the gate never collapses.
        let division = f64::from(io.read(IN_DIVISION));

        // Reset frames within this (sub)block, sorted. Any `sync` event re-zeroes the phase at its
        // exact sample (the port identifies it, payload ignored) — a sample-accurate
        // position locate.
        let mut resets: SmallVec<[usize; 4]> = SmallVec::new();
        for ev in io.read(IN_SYNC) {
            if ev.frame < n {
                resets.push(ev.frame);
            }
        }
        resets.sort_unstable();

        // Two passes over the block, one per output port (only one output borrow at a time).
        // Both replay the identical accumulator from `start` — applying resets — so they
        // stay in lock-step; a reset re-zeroes the phase before that sample is emitted.
        let start = self.phase;

        let end;
        {
            let out = io.write(OUT_PHASE);
            let mut phase = start;
            let mut ri = 0;
            for (i, s) in out.iter_mut().enumerate().take(n) {
                while ri < resets.len() && resets[ri] == i {
                    phase = 0.0;
                    ri += 1;
                }
                *s = phase as f32;
                phase += dt;
                // Wrap to [0,1). `dt` is beats/sample (≤ 999/60/sr ≪ 1), so after one increment
                // `phase < 2` and a single conditional subtraction is exactly `phase.floor()` here
                // — without the per-sample out-of-line libm `floor` call (hot path).
                if phase >= 1.0 {
                    phase -= 1.0;
                }
            }
            end = phase;
        }
        {
            // `gate` is a sparse held Value: replay the same accumulator, but emit a
            // `MsgWriter` change only at each rising/falling edge instead of writing every sample.
            // The held level carries across blocks (`self.gate_high`), so frame 0 only re-emits on a
            // genuine change. A downstream Value/materialize bridge ZOH-reconstructs the dense gate.
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
                let sub = (phase * division).fract();
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
        self.phase = end;
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(Clock);

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
        let mut buf = vec![0.0f32; n];
        let mut level = 0.0f32;
        let mut ei = 0;
        for (i, s) in buf.iter_mut().enumerate() {
            while ei < emits.len() && emits[ei].frame == i {
                if let Arg::F32(v) = emits[ei].arg {
                    level = v;
                }
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
}
