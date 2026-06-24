//! Clock — base musical timing (ADR-0006): a sample-accurate beat phasor at `tempo`.
//!
//! The Clock is where sample-accuracy actually lives. It free-runs on the deterministic
//! sample timeline, advancing a beat phase by `tempo / 60 / sample_rate` beats per sample,
//! so beat boundaries land on exact samples regardless of block size — the thing external
//! OSC arrival times can't honestly give us. It provides *timing only* (tempo + beat grid +
//! position); groove, swing, and meter are separate concerns (ADR-0006).
//!
//! - input 0: `sync` (Message) — control events by address routing, read via
//!   [`crate::operator::Io::events`]; this port is documentary (no Signal edge). The
//!   `reset` event re-zeroes the phase at its (sample-accurate) frame, locating position 1.
//! - output 0: `phase` (Signal) — beat phasor, a [0, 1) sawtooth that wraps once per beat.
//!   **Unaffected by `division`** — it is always the once-per-beat phasor.
//! - output 1: `gate` (Signal) — 1.0 for the first half of each **1/`division` sub-beat**, else
//!   0.0; its rising edge is a sample-accurate trigger. At the default `division` 1 this is the
//!   original once-per-beat gate, high for the first half of the beat. At `division` N the gate
//!   pulses N times per beat — a 16th-note grid is `division` 4 (ADR-0022, the thin slice of
//!   ADR-0006's deferred subdivision).
//! - input 1: `tempo` (`Float`, BPM) — read block-rate via `io.value`.
//! - input 2: `division` (`Float`) — gate subdivisions per beat (1 = once per beat, default;
//!   4 = 16ths), read block-rate via `io.value`.
//!
//! `tempo`/`division` are `Float` inputs (ADR-0028): read block-rate, so a change block-slices and
//! takes effect at the exact sample of the change — and each can now be *wired* and modulated.

use smallvec::SmallVec;

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};

// Single-source contract (ADR-0025): one declaration -> IN_/OUT_/P_ consts + Descriptor, no drift.
crate::operator_contract!(Clock {
    inputs:  { sync: message,
               tempo:    float { 1.0..=999.0, default 120.0, "BPM", lin },
               division: float { 1.0..=64.0,  default 1.0,   "",    lin } },
    outputs: { phase: float, gate: float },
});

#[derive(Default)]
pub struct Clock {
    /// Beat phase in [0, 1), advanced per sample. Continuous across blocks/slices.
    /// Held in f64 so the beat grid doesn't drift off the sample timeline over a long
    /// session (f32 accumulation slips audibly within seconds).
    phase: f64,
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
            (io.value(IN_TEMPO).max(0.0) as f64 / 60.0) / sample_rate as f64
        } else {
            0.0
        };
        // Gate subdivisions per beat. division 1 = the original once-per-beat gate; N pulses N
        // times per beat. Rounded and floored at 1 so it never collapses the gate.
        let division = (io.value(IN_DIVISION).round() as f64).max(1.0);

        // Reset frames within this (sub)block, sorted. A `reset` event re-zeroes the phase
        // at its exact sample — sample-accurate position locate.
        let mut resets: SmallVec<[usize; 4]> = SmallVec::new();
        for ev in io.events() {
            if ev.addr == "reset" && ev.frame < n {
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
            let out = io.output(OUT_PHASE);
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
                // — without the per-sample out-of-line libm `floor` call (hot path, ADR-0019).
                if phase >= 1.0 {
                    phase -= 1.0;
                }
            }
            end = phase;
        }
        {
            let out = io.output(OUT_GATE);
            let mut phase = start;
            let mut ri = 0;
            for (i, s) in out.iter_mut().enumerate().take(n) {
                while ri < resets.len() && resets[ri] == i {
                    phase = 0.0;
                    ri += 1;
                }
                // Sub-beat phasor: phase·division wrapped to [0,1); gate high for its first
                // half. division 1 reduces to `phase < 0.5` exactly (bit-identical default).
                let sub = (phase * division).fract();
                *s = if sub < 0.5 { 1.0 } else { 0.0 };
                phase += dt;
                // Wrap to [0,1). `dt` is beats/sample (≤ 999/60/sr ≪ 1), so after one increment
                // `phase < 2` and a single conditional subtraction is exactly `phase.floor()` here
                // — without the per-sample out-of-line libm `floor` call (hot path, ADR-0019).
                if phase >= 1.0 {
                    phase -= 1.0;
                }
            }
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
    use crate::message::{Arg, Event, Message};
    use crate::operator::Io;

    const SR: f32 = 48_000.0;

    /// Run `clock` over one block of `n` frames at `tempo` (division 1), with optional reset
    /// events. Returns the (phase, gate) buffers.
    fn run(clock: &mut Clock, n: usize, tempo: f32, events: &[Message]) -> (Vec<f32>, Vec<f32>) {
        run_div(clock, n, tempo, 1.0, events)
    }

    /// Like [`run`] but with an explicit gate `division`.
    fn run_div(
        clock: &mut Clock,
        n: usize,
        tempo: f32,
        division: f32,
        events: &[Message],
    ) -> (Vec<f32>, Vec<f32>) {
        let mut phase = vec![0.0f32; n];
        let mut gate = vec![0.0f32; n];
        let evs: Vec<Event> = events
            .iter()
            .map(|m| Event {
                addr: &m.addr,
                args: &m.args,
                frame: m.frame,
            })
            .collect();
        // tempo/division are `Float` inputs now (ADR-0028) — supply the per-sample buffers the
        // engine would materialize, in port order (sync, tempo, division). `sync` is a message
        // input (no Signal buffer); events are delivered via the events arg.
        let tempo_buf = vec![tempo; n];
        let division_buf = vec![division; n];
        {
            let outs: Vec<&mut [f32]> = vec![&mut phase[..], &mut gate[..]];
            let inputs: Vec<Option<&[f32]>> =
                vec![None, Some(&tempo_buf[..]), Some(&division_buf[..])];
            let params: [f32; 0] = [];
            let mut io = Io::new(SR, n, inputs, outs, &params, &evs);
            clock.process(&mut io);
        }
        (phase, gate)
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
        let mut c = Clock::new();
        let (phase, gate) = run(&mut c, 96_000, 120.0, &[]);

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
        let mut c = Clock::new();
        let (_p, gate) = run(&mut c, 48_000, 240.0, &[]);
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
        // The phase at the start of a second block equals where the first block left off.
        let n = 1000;
        let mut whole = Clock::new();
        let (p_whole, _) = run(&mut whole, 2 * n, 120.0, &[]);

        let mut split = Clock::new();
        let (p1, _) = run(&mut split, n, 120.0, &[]);
        let (p2, _) = run(&mut split, n, 120.0, &[]);

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
        let mut c = Clock::new();
        let reset = vec![Message::new("reset", [Arg::Float(0.0)], r)];
        let (phase, gate) = run(&mut c, n, 120.0, &reset);

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
        let mut a = Clock::new();
        let _ = run(&mut a, 5_000, 120.0, &[]);
        let mut b = a.spawn();
        // The fresh spawn starts at the downbeat regardless of `a`'s advanced phase.
        let mut phase = [0.0f32; 1];
        let mut gate = [0.0f32; 1];
        {
            let outs: Vec<&mut [f32]> = vec![&mut phase[..], &mut gate[..]];
            let (tempo, division) = ([120.0f32], [1.0f32]);
            let inputs: Vec<Option<&[f32]>> = vec![None, Some(&tempo[..]), Some(&division[..])];
            let params: [f32; 0] = [];
            let mut io = Io::new(SR, 1, inputs, outs, &params, &[]);
            b.process(&mut io);
        }
        assert_eq!(phase[0], 0.0, "spawned clock starts fresh at phase 0");
    }

    // --- V1.3 `division` param: subdivide the gate (ADR-0022) ---

    #[test]
    fn division_one_is_bit_identical_to_the_default_gate() {
        // division 1 must reproduce the original once-per-beat gate exactly. The original logic
        // was `gate = if phase < 0.5 { 1 } else { 0 }` on the f64 beat phasor; replay that exact
        // accumulator here as the reference and compare bit-for-bit.
        let n = 96_000;
        let mut c = Clock::new();
        let (_phase, gate) = run_div(&mut c, n, 120.0, 1.0, &[]);

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
        let mut c1 = Clock::new();
        let (_p1, g1) = run_div(&mut c1, n, 120.0, 1.0, &[]);
        let mut c4 = Clock::new();
        let (p4, g4) = run_div(&mut c4, n, 120.0, 4.0, &[]);

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
}
