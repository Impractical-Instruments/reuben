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
//! - input 0: `position` (`f32`, held Value) — the fader's position in 0..1, read **once per
//!   block-slice** via [`Io::input`]. The engine block-slices at every position change (ADR-0031),
//!   so a moved fader re-spells the strum at the change frame — the slice's frame 0 *is* the change
//!   frame, so the pluck is sample-accurate — and a crossing straddling a block boundary fires
//!   exactly once (`prev_string` carries the band across).
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
//! fixed-capacity queue, allocation-free). Emits one note stream, upstream of the Voicer.

use smallvec::SmallVec;

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};
use crate::vocab::pitch::{Note, Pitch};

// Single-source contract (ADR-0025/0030/0031). All inputs are held `f32` Values: `position` is read
// once per block-slice (the engine slices at its changes), `strings`/`octaves`/`velocity` block-rate.
crate::operator_contract!(Strum {
    inputs:  { position: f32 { 0.0..=1.0,  default 0.0, "",        lin },
               strings:  f32 { 1.0..=32.0, default 8.0, "strings", lin },
               octaves:  f32 { 1.0..=4.0,  default 1.0, "oct",     lin },
               velocity: f32 { 0.0..=1.0,  default 1.0, "",        lin } },
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
        let strings = (io.input::<f32>(IN_STRINGS).unwrap_or(8.0).round() as i64).clamp(1, 32);
        let octaves = io.input::<f32>(IN_OCTAVES).unwrap_or(1.0).max(1.0);
        let velocity = io.input::<f32>(IN_VELOCITY).unwrap_or(1.0).clamp(0.0, 1.0);

        // `position` is a held Value (ADR-0031): the engine block-slices at every position change,
        // so this call sees one constant fader position. Read it once (the immutable borrow ends
        // with this `let`, so `io.output` can borrow mutably below) and resolve the band. Any band
        // crossed since the last slice emits a pluck **at frame 0** — the slice's start *is* the
        // change frame (block-absolute), so the pluck is sample-accurate — plus a scheduled
        // note-off. `prev_string` carries the band across blocks/slices.
        let pos = io.input::<f32>(IN_POSITION).unwrap_or(0.0);
        let cur_string = Self::string_at(pos, strings);
        if self.prev_string < 0 {
            // First position seen: latch the band, no pluck (no crossing yet).
            self.prev_string = cur_string;
        } else if cur_string != self.prev_string {
            let step = if cur_string > self.prev_string { 1 } else { -1 };
            let mut s = self.prev_string;
            while s != cur_string {
                s += step;
                let deg = Self::degree_of(s, strings, octaves);
                io.output::<Note>(OUT_DEGREES)
                    .emit(0, degree_note(deg, velocity));
                if self.pending.len() < self.pending.capacity() {
                    self.pending.push((deg, PLUCK_SAMPLES));
                }
            }
            self.prev_string = cur_string;
        }

        // Pending note-offs count down per-sample across the slice (1440 samples > a block, so they
        // thread across boundaries); each fires its paired note-off at its due frame.
        for i in 0..n {
            let mut k = 0;
            while k < self.pending.len() {
                if self.pending[k].1 <= 0 {
                    let deg = self.pending[k].0;
                    io.output::<Note>(OUT_DEGREES)
                        .emit(i, degree_note(deg, 0.0));
                    self.pending.swap_remove(k);
                } else {
                    self.pending[k].1 -= 1;
                    k += 1;
                }
            }
        }
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
    use crate::op_driver::OpDriver;

    const SR: f32 = 48_000.0;

    /// Drive a fresh Strum through the real engine over `n` frames. `position` is a held Value
    /// (ADR-0031): each discrete `(frame, value)` is `push`ed as a change, so the engine block-slices
    /// at it and re-spells the strum at that frame (zero-order-held between changes — exactly the
    /// fader's semantics). `strings`/`octaves`/`velocity` are held `Float` controls (`set` once).
    /// Returns the emitted Messages, frames block-absolute.
    fn run(n: usize, params: &[f32; 3], positions: &[(usize, f32)]) -> Vec<Emit> {
        let mut d = OpDriver::for_type(Strum::new(), SR);
        d.set(IN_STRINGS, params[0])
            .set(IN_OCTAVES, params[1])
            .set(IN_VELOCITY, params[2]);
        for &(frame, value) in positions {
            d.push(IN_POSITION, frame, value);
        }
        d.render(n).emits().to_vec()
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
        let positions: Vec<(usize, f32)> =
            (0..8).map(|k| (k * 10, (k as f32 + 0.5) / 8.0)).collect();
        let emits = run(200, &params(8.0, 1.0, 1.0), &positions);

        let ons = on_degrees(&emits);
        assert_eq!(ons, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]);
    }

    #[test]
    fn sweep_down_plucks_descending_strings_in_order() {
        // Start at the top, sweep to the bottom: descending plucks 6,5,4,3,2,1,0.
        let positions: Vec<(usize, f32)> =
            (0..8).map(|k| (k * 10, (7.5 - k as f32) / 8.0)).collect();
        let emits = run(200, &params(8.0, 1.0, 1.0), &positions);

        let ons = on_degrees(&emits);
        assert_eq!(ons, vec![6.0, 5.0, 4.0, 3.0, 2.0, 1.0, 0.0]);
    }

    #[test]
    fn movement_within_one_band_emits_nothing() {
        // Two positions both inside band 3 (no boundary crossed) -> no notes.
        let positions = [(0, 3.2 / 8.0), (50, 3.8 / 8.0)];
        let emits = run(100, &params(8.0, 1.0, 1.0), &positions);
        assert!(
            on_degrees(&emits).is_empty(),
            "no crossing -> no plucks, got {emits:?}"
        );
    }

    #[test]
    fn a_fast_drag_across_several_bands_plucks_each_in_between() {
        // First sample latches band 0; a single jump to band 4 plucks every string between:
        // degrees 1,2,3,4 (a glissando, not just the destination).
        let positions = [(0, 0.01), (20, 4.5 / 8.0)];
        let emits = run(100, &params(8.0, 1.0, 1.0), &positions);
        assert_eq!(on_degrees(&emits), vec![1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn each_pluck_pairs_a_note_off_so_it_rings_then_releases() {
        // A single crossing emits a note-on now and a note-off PLUCK_SAMPLES later (same degree).
        // PLUCK_SAMPLES (1440) is > one 128-frame block, so the paired note-off is proof the
        // pending queue threads across the real block boundaries.
        let n = (PLUCK_SAMPLES as usize) + 100;
        let positions = [(0, 0.01), (10, 1.5 / 8.0)]; // latch band 0, cross to band 1
        let emits = run(n, &params(8.0, 1.0, 1.0), &positions);

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
        let positions: Vec<(usize, f32)> =
            (0..8).map(|k| (k * 10, (k as f32 + 0.5) / 8.0)).collect();
        let emits = run(200, &params(8.0, 2.0, 1.0), &positions);
        assert_eq!(
            on_degrees(&emits),
            vec![2.0, 4.0, 6.0, 8.0, 10.0, 12.0, 14.0]
        );
    }

    #[test]
    fn velocity_param_sets_the_pluck_velocity() {
        let positions = [(0, 0.01), (10, 1.5 / 8.0)];
        let emits = run(100, &params(8.0, 1.0, 0.7), &positions);
        let on = emits.iter().find(|e| vel(e) > 0.01).expect("a note-on");
        approx::assert_relative_eq!(vel(on), 0.7);
    }

    #[test]
    fn crossing_state_is_continuous_across_block_slices() {
        // `OpDriver::render` steps the operator as real 128-frame blocks, so a single render over a
        // 200-sample buffer already crosses a block boundary at frame 128. The position jumps from
        // band 5 (frame 90, block 0) to band 1 (frame 140, block 1): the descending gliss it plucks
        // (4,3,2,1) spans the boundary, proving the band machine (`prev_string`) carries across it.
        let positions: Vec<(usize, f32)> = vec![
            (0, 0.01),
            (40, 2.5 / 8.0),
            (90, 5.5 / 8.0),
            (140, 1.5 / 8.0),
        ];
        let emits = run(200, &params(8.0, 1.0, 1.0), &positions);
        // 0->2: plucks 1,2; 2->5: plucks 3,4,5; 5->1: plucks 4,3,2,1 (across the block boundary).
        assert_eq!(
            on_degrees(&emits),
            vec![1.0, 2.0, 3.0, 4.0, 5.0, 4.0, 3.0, 2.0, 1.0]
        );
    }

    #[test]
    fn spawned_strum_resets_to_no_band() {
        // Drive `a` to some band, spawn `b`: `b`'s first sample latches (no pluck) rather than
        // crossing from where `a` left off.
        let mut a = OpDriver::for_type(Strum::new(), SR);
        let p = params(8.0, 1.0, 1.0);
        a.set(IN_STRINGS, p[0])
            .set(IN_OCTAVES, p[1])
            .set(IN_VELOCITY, p[2])
            .push(IN_POSITION, 0, 0.9);
        a.render(100);

        let mut b = a.spawn();
        b.set(IN_STRINGS, p[0])
            .set(IN_OCTAVES, p[1])
            .set(IN_VELOCITY, p[2])
            .push(IN_POSITION, 0, 0.05);
        let emits = b.render(50).emits().to_vec();
        assert!(
            on_degrees(&emits).is_empty(),
            "fresh spawn latches its first position, no pluck: {emits:?}"
        );
    }
}
