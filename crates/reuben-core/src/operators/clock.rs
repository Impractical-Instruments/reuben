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
//! - output 1: `gate` (Signal) — 1.0 for the first half of each beat, else 0.0; its rising
//!   edge is a sample-accurate beat trigger (drive an envelope's gate with it).
//! - param 0: `tempo` (BPM).
//!
//! Tempo is an ordinary param, so the engine block-slices on tempo changes and a new tempo
//! takes effect at the exact sample of the change.

use smallvec::SmallVec;

use crate::descriptor::{Curve, Descriptor, LaneRule, ParamMeta, Port};
use crate::operator::{Io, Operator};

pub const IN_SYNC: usize = 0;
pub const OUT_PHASE: usize = 0;
pub const OUT_GATE: usize = 1;
pub const P_TEMPO: usize = 0;

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
        Descriptor {
            type_name: "clock",
            inputs: vec![Port::message("sync")],
            outputs: vec![Port::signal("phase"), Port::signal("gate")],
            params: vec![ParamMeta {
                name: "tempo",
                min: 1.0,
                max: 999.0,
                default: 120.0,
                unit: "BPM",
                curve: Curve::Linear,
            }],
            resources: vec![],
            lanes: LaneRule::Inherit,
        }
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let sample_rate = io.sample_rate();

        // Beats advanced per sample. Tempo is constant for this (sub)block (block-sliced).
        let dt: f64 = if sample_rate > 0.0 {
            (io.param(P_TEMPO).max(0.0) as f64 / 60.0) / sample_rate as f64
        } else {
            0.0
        };

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
                phase -= phase.floor();
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
                *s = if phase < 0.5 { 1.0 } else { 0.0 };
                phase += dt;
                phase -= phase.floor();
            }
        }
        self.phase = end;
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Arg, Event, Message};
    use crate::operator::Io;

    const SR: f32 = 48_000.0;

    /// Run `clock` over one block of `n` frames at `tempo`, with optional reset events.
    /// Returns the (phase, gate) buffers.
    fn run(clock: &mut Clock, n: usize, tempo: f32, events: &[Message]) -> (Vec<f32>, Vec<f32>) {
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
        {
            let outs: Vec<&mut [f32]> = vec![&mut phase[..], &mut gate[..]];
            let inputs: Vec<Option<&[f32]>> = vec![None];
            let params = [tempo];
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
            let inputs: Vec<Option<&[f32]>> = vec![None];
            let params = [120.0f32];
            let mut io = Io::new(SR, 1, inputs, outs, &params, &[]);
            b.process(&mut io);
        }
        assert_eq!(phase[0], 0.0, "spawned clock starts fresh at phase 0");
    }
}
