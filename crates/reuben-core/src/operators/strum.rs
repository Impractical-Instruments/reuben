//! Strum — a drag-to-strum gesture op (V1.3 "The Toys", ADR-0022 §3; unified model, ADR-0030).
//!
//! The strum-harp's one big fader streams its position (0..1). This op turns that drag into a harp
//! glissando: the 0..1 range is divided into `strings` equal bands, and **each time the position
//! crosses a string boundary a note is plucked** — a degree [`Note`], so the Voicer resolves it
//! through the tonal context and the harp is always in key (mirrors how
//! [`Sequencer`](crate::operators::Sequencer) emits degrees without reading the context itself).
//! Both drag directions strum: dragging up plucks ascending strings, dragging down plucks
//! descending ones, in order, one note per boundary crossed (a fast drag across several bands
//! plucks each string between, like a thumb across a harp).
//!
//! - input 0: `position` (`Float`) — the fader's position in 0..1, read **per-sample** via
//!   [`Io::signal`] (a materialized control), so a crossing is detected at its exact frame and a
//!   crossing straddling a block boundary fires exactly once (the held value carries across).
//! - input 1: `strings` (`Float`, held) — strings the 0..1 range is divided into (1..=32, default
//!   8 = one diatonic octave).
//! - input 2: `octaves` (`Float`, held) — the degree span the strings cover (1..=4, default 1).
//!   String `k` plucks degree `round(k * octaves * 7 / (strings-1))`.
//! - input 3: `velocity` (`Float`, held) — pluck velocity (0..1, default 1).
//! - output 0: `degrees` (`Note`) — a degree note (velocity on the pluck, 0 on the paired off).
//!
//! **Plucks, not held notes** (ADR-0022 "no held gate"): each crossing emits a note-on immediately
//! followed by a note-off `PLUCK_SAMPLES` later, so the downstream percussive envelope opens and
//! then rings out on its own decay/release. The pending note-offs are held across blocks (a
//! fixed-capacity queue, allocation-free). Single-Lane: emission is pre-fan-out (Lane 0 only).

use smallvec::SmallVec;

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};
use crate::vocab::pitch::{Note, Pitch};

// Single-source contract (ADR-0025/0030). `position` is a materialized `Float` (read per-sample);
// `strings`/`octaves`/`velocity` are held `Float`s.
crate::operator_contract!(Strum {
    inputs:  { position: float { 0.0..=1.0,  default 0.0, "",        lin },
               strings:  float { 1.0..=32.0, default 8.0, "strings", lin },
               octaves:  float { 1.0..=4.0,  default 1.0, "oct",     lin },
               velocity: float { 0.0..=1.0,  default 1.0, "",        lin } },
    outputs: { degrees: note },
});

/// Diatonic degrees spanned per octave (0..6 then the octave at 7).
const DEGREES_PER_OCTAVE: f32 = 7.0;
/// How long after a pluck's note-on the paired note-off fires, in samples (~30 ms @ 48 kHz).
const PLUCK_SAMPLES: i64 = 1440;
/// Inline capacity for pending note-offs carried across blocks.
const PENDING_CAP: usize = 32;

pub struct Strum {
    /// String index the position last sat in, or -1 before the first position sample. Continuous
    /// across blocks; a crossing is `cur_string != prev_string`.
    prev_string: i64,
    /// Pending note-offs: (degree, samples_until_off). Decremented each sample; at 0 the off fires.
    pending: SmallVec<[(f32, i64); PENDING_CAP]>,
}

impl Default for Strum {
    fn default() -> Self {
        Self {
            prev_string: -1,
            pending: SmallVec::new(),
        }
    }
}

impl Strum {
    pub fn new() -> Self {
        Self::default()
    }

    /// The string index for a position in 0..1, clamped to `0..strings-1` (so a position of
    /// exactly 1.0 sits on the top string, not one past it).
    fn string_at(position: f32, strings: i64) -> i64 {
        let p = position.clamp(0.0, 1.0);
        ((p * strings as f32).floor() as i64).clamp(0, strings - 1)
    }

    /// The scale degree plucked by string `k` given the octave span.
    fn degree_of(k: i64, strings: i64, octaves: f32) -> f32 {
        if strings <= 1 {
            return 0.0;
        }
        let span = octaves * DEGREES_PER_OCTAVE;
        (k as f32 * span / (strings as f32 - 1.0)).round()
    }
}

/// A degree note from a (possibly fractional) degree value.
fn degree_note(degree: f32, velocity: f32) -> Note {
    Note::new(Pitch::Degree(degree.round() as i32), velocity)
}

impl Operator for Strum {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let strings = (io.last::<f32>(IN_STRINGS).unwrap_or(8.0).round() as i64).clamp(1, 32);
        let octaves = io.last::<f32>(IN_OCTAVES).unwrap_or(1.0).max(1.0);
        let velocity = io.last::<f32>(IN_VELOCITY).unwrap_or(1.0).clamp(0.0, 1.0);

        let mut prev_string = self.prev_string;

        for i in 0..n {
            // Fire any pending note-offs due at this sample, then count down the rest.
            let mut k = 0;
            while k < self.pending.len() {
                if self.pending[k].1 <= 0 {
                    let deg = self.pending[k].0;
                    io.emit(OUT_DEGREES, "notes", degree_note(deg, 0.0), i);
                    self.pending.swap_remove(k);
                } else {
                    self.pending[k].1 -= 1;
                    k += 1;
                }
            }

            // Read this sample's position (the immutable borrow ends with this `let`, so `io.emit`
            // can borrow mutably below). Each band crossed emits a pluck + a scheduled note-off.
            let pos = io.signal(IN_POSITION).get(i).copied().unwrap_or(0.0);
            let cur_string = Self::string_at(pos, strings);
            if prev_string < 0 {
                // First position seen: latch the band, no pluck (no crossing yet).
                prev_string = cur_string;
            } else if cur_string != prev_string {
                let step = if cur_string > prev_string { 1 } else { -1 };
                let mut s = prev_string;
                while s != cur_string {
                    s += step;
                    let deg = Self::degree_of(s, strings, octaves);
                    io.emit(OUT_DEGREES, "notes", degree_note(deg, velocity), i);
                    if self.pending.len() < self.pending.capacity() {
                        self.pending.push((deg, PLUCK_SAMPLES));
                    }
                }
                prev_string = cur_string;
            }
        }

        self.prev_string = prev_string;
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(Strum);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Arg, Emit};

    const SR: f32 = 48_000.0;

    /// Build a zero-order-held position buffer of `n` samples from discrete `(frame, value)`
    /// positions — the way the engine materializes a sparse control into a per-sample buffer.
    fn pos_buf(n: usize, positions: &[(usize, f32)]) -> Vec<f32> {
        let mut buf = vec![0.0f32; n];
        let mut cur = 0.0;
        let mut pi = 0;
        for (i, slot) in buf.iter_mut().enumerate() {
            while pi < positions.len() && positions[pi].0 == i {
                cur = positions[pi].1;
                pi += 1;
            }
            *slot = cur;
        }
        buf
    }

    /// Run `strum` over a prebuilt per-sample `position` buffer; returns the emitted Messages.
    fn run_buf(strum: &mut Strum, params: &[f32; 3], position: &[f32]) -> Vec<Emit> {
        let n = position.len();
        let latched = [
            Arg::F32(0.0), // position (read per-sample, not via last)
            Arg::F32(params[0]),
            Arg::F32(params[1]),
            Arg::F32(params[2]),
        ];
        let mut emits: Vec<Emit> = Vec::new();
        {
            let outs: Vec<&mut [f32]> = vec![]; // note port — no Signal buffer.
            let inputs: Vec<Option<&[f32]>> = vec![Some(position), None, None, None];
            let mut io = Io::new(SR, n, inputs, outs)
                .with_latched(&latched)
                .with_emit(&mut emits, 0);
            strum.process(&mut io);
        }
        emits
    }

    /// Convenience: run from discrete positions, materializing them to a ZOH buffer first.
    fn run(
        strum: &mut Strum,
        n: usize,
        params: &[f32; 3],
        positions: &[(usize, f32)],
    ) -> Vec<Emit> {
        let buf = pos_buf(n, positions);
        run_buf(strum, params, &buf)
    }

    fn params(strings: f32, octaves: f32, velocity: f32) -> [f32; 3] {
        [strings, octaves, velocity]
    }

    fn deg(e: &Emit) -> f32 {
        match &e.arg {
            Arg::Note(n) => n.pitch.degree().unwrap() as f32,
            other => panic!("expected a Note, got {other:?}"),
        }
    }
    fn vel(e: &Emit) -> f32 {
        match &e.arg {
            Arg::Note(n) => n.velocity,
            other => panic!("expected a Note, got {other:?}"),
        }
    }

    /// Note-on degrees, in emission order.
    fn on_degrees(emits: &[Emit]) -> Vec<f32> {
        emits.iter().filter(|e| vel(e) > 0.5).map(deg).collect()
    }

    #[test]
    fn sweep_up_plucks_ascending_strings_in_order() {
        // 8 strings, 1 octave: a slow sweep 0 -> 1 crosses all 8 bands. The first band is latched
        // on the opening sample, so the ascending plucks are strings 1..=7 -> degrees 1..7.
        let mut strum = Strum::new();
        let positions: Vec<(usize, f32)> =
            (0..8).map(|k| (k * 10, (k as f32 + 0.5) / 8.0)).collect();
        let emits = run(&mut strum, 200, &params(8.0, 1.0, 1.0), &positions);

        let ons = on_degrees(&emits);
        assert_eq!(ons, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]);
        assert!(emits.iter().all(|e| e.address == "notes"));
    }

    #[test]
    fn sweep_down_plucks_descending_strings_in_order() {
        // Start at the top, sweep to the bottom: descending plucks 6,5,4,3,2,1,0.
        let mut strum = Strum::new();
        let positions: Vec<(usize, f32)> =
            (0..8).map(|k| (k * 10, (7.5 - k as f32) / 8.0)).collect();
        let emits = run(&mut strum, 200, &params(8.0, 1.0, 1.0), &positions);

        let ons = on_degrees(&emits);
        assert_eq!(ons, vec![6.0, 5.0, 4.0, 3.0, 2.0, 1.0, 0.0]);
    }

    #[test]
    fn movement_within_one_band_emits_nothing() {
        // Two positions both inside band 3 (no boundary crossed) -> no notes.
        let mut strum = Strum::new();
        let positions = [(0, 3.2 / 8.0), (50, 3.8 / 8.0)];
        let emits = run(&mut strum, 100, &params(8.0, 1.0, 1.0), &positions);
        assert!(
            on_degrees(&emits).is_empty(),
            "no crossing -> no plucks, got {emits:?}"
        );
    }

    #[test]
    fn a_fast_drag_across_several_bands_plucks_each_in_between() {
        // First sample latches band 0; a single jump to band 4 plucks every string between:
        // degrees 1,2,3,4 (a glissando, not just the destination).
        let mut strum = Strum::new();
        let positions = [(0, 0.01), (20, 4.5 / 8.0)];
        let emits = run(&mut strum, 100, &params(8.0, 1.0, 1.0), &positions);
        assert_eq!(on_degrees(&emits), vec![1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn each_pluck_pairs_a_note_off_so_it_rings_then_releases() {
        // A single crossing emits a note-on now and a note-off PLUCK_SAMPLES later (same degree).
        let mut strum = Strum::new();
        let n = (PLUCK_SAMPLES as usize) + 100;
        let positions = [(0, 0.01), (10, 1.5 / 8.0)]; // latch band 0, cross to band 1
        let emits = run(&mut strum, n, &params(8.0, 1.0, 1.0), &positions);

        let ons: Vec<&Emit> = emits.iter().filter(|e| vel(e) > 0.5).collect();
        let offs: Vec<&Emit> = emits.iter().filter(|e| vel(e) < 0.5).collect();
        assert_eq!(ons.len(), 1, "one pluck");
        assert_eq!(offs.len(), 1, "one paired note-off");
        approx::assert_relative_eq!(deg(ons[0]), 1.0);
        approx::assert_relative_eq!(deg(offs[0]), 1.0);
        assert!(
            offs[0].frame > ons[0].frame,
            "note-off comes after the note-on (the ring)"
        );
    }

    #[test]
    fn octaves_param_widens_the_string_span() {
        // 8 strings over 2 octaves: ascending plucks land on 2,4,6,8,10,12,14 (round(k * 14 / 7)).
        let mut strum = Strum::new();
        let positions: Vec<(usize, f32)> =
            (0..8).map(|k| (k * 10, (k as f32 + 0.5) / 8.0)).collect();
        let emits = run(&mut strum, 200, &params(8.0, 2.0, 1.0), &positions);
        assert_eq!(
            on_degrees(&emits),
            vec![2.0, 4.0, 6.0, 8.0, 10.0, 12.0, 14.0]
        );
    }

    #[test]
    fn velocity_param_sets_the_pluck_velocity() {
        let mut strum = Strum::new();
        let positions = [(0, 0.01), (10, 1.5 / 8.0)];
        let emits = run(&mut strum, 100, &params(8.0, 1.0, 0.7), &positions);
        let on = emits.iter().find(|e| vel(e) > 0.01).expect("a note-on");
        approx::assert_relative_eq!(vel(on), 0.7);
    }

    #[test]
    fn crossing_state_is_continuous_across_block_slices() {
        // Splitting the position buffer across two blocks yields the same plucks as one whole
        // block: the band machine carries across the boundary.
        let p = params(8.0, 1.0, 1.0);
        let positions: Vec<(usize, f32)> = vec![
            (0, 0.01),
            (40, 2.5 / 8.0),
            (90, 5.5 / 8.0),
            (140, 1.5 / 8.0),
        ];
        let n = 200;
        let full = pos_buf(n, &positions);

        let mut whole = Strum::new();
        let ew = run_buf(&mut whole, &p, &full);
        let ons_whole = on_degrees(&ew);

        let mid = 100;
        let mut split = Strum::new();
        let e1 = run_buf(&mut split, &p, &full[..mid]);
        let e2 = run_buf(&mut split, &p, &full[mid..]);
        let mut ons_split = on_degrees(&e1);
        ons_split.extend(on_degrees(&e2));

        assert_eq!(ons_whole, ons_split);
    }

    #[test]
    fn spawned_strum_resets_to_no_band() {
        // Drive `a` to some band, spawn `b`: `b`'s first sample latches (no pluck) rather than
        // crossing from where `a` left off.
        let mut a = Strum::new();
        let _ = run(&mut a, 100, &params(8.0, 1.0, 1.0), &[(0, 0.9)]);
        let mut b = a.spawn();
        let p = params(8.0, 1.0, 1.0);
        let buf = pos_buf(50, &[(0, 0.05)]);
        let latched = [
            Arg::F32(0.0),
            Arg::F32(p[0]),
            Arg::F32(p[1]),
            Arg::F32(p[2]),
        ];
        let mut emits: Vec<Emit> = Vec::new();
        {
            let outs: Vec<&mut [f32]> = vec![];
            let inputs: Vec<Option<&[f32]>> = vec![Some(&buf[..]), None, None, None];
            let mut io = Io::new(SR, buf.len(), inputs, outs)
                .with_latched(&latched)
                .with_emit(&mut emits, 0);
            b.process(&mut io);
        }
        assert!(
            on_degrees(&emits).is_empty(),
            "fresh spawn latches its first position, no pluck: {emits:?}"
        );
    }
}
