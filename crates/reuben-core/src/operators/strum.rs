//! Strum — a drag-to-strum gesture op (V1.3 "The Toys", ADR-0022 §3).
//!
//! The strum-harp's one big fader streams its position as a Message (0..1). This op turns
//! that drag into a harp glissando: the 0..1 range is divided into `strings` equal bands,
//! and **each time the position crosses a string boundary a note is plucked** — a `degree`
//! Message, so the Voicer resolves it through the tonal context and the harp is always in key
//! (mirrors how [`Sequencer`](crate::operators::Sequencer) emits degrees without reading the
//! context itself). Both drag directions strum: dragging up plucks ascending strings, dragging
//! down plucks descending ones, in order, one note per boundary crossed (a fast drag across
//! several bands plucks each string between, like a thumb across a harp).
//!
//! - input 0: `position` (Message) — the fader's position events in 0..1. The first numeric
//!   arg is the position (matching how `m2s` reads a fader value; the address routes the edge,
//!   so any address the fader sends works). Position is held across blocks so a crossing that
//!   straddles a block boundary fires exactly once.
//! - output 0 (Message): `degrees` — `degree` Messages, arg 0 = **scale degree**, arg 1 =
//!   velocity (the `velocity` param on a pluck, 0 on the paired note-off). The Voicer resolves
//!   the degree through the tonal context (ADR-0008 amendment), so the harp re-spells live on a
//!   key/scale change.
//! - param 0: `strings` — number of strings the 0..1 range is divided into (1..=32, default 8
//!   = one diatonic octave).
//! - param 1: `octaves` — the degree span the strings cover (1..=4, default 1). String `k`
//!   plucks degree `round(k * octaves * 7 / (strings-1))`, so the bottom string is degree 0 and
//!   the top string is `octaves*7` (one full diatonic octave per `octaves`).
//! - param 2: `velocity` — pluck velocity (0..1, default 1).
//!
//! **Plucks, not held notes** (ADR-0022 "no held gate"): each crossing emits a note-on
//! immediately followed by a note-off `PLUCK_SAMPLES` later, so the downstream percussive
//! envelope opens and then rings out on its own decay/release. The pending note-offs are held
//! across blocks (a fixed-capacity queue, allocation-free) so a pluck near a block edge still
//! releases. Single-Lane: emission is pre-fan-out (Lane 0 only), exactly like the Sequencer.

use smallvec::SmallVec;

use crate::descriptor::{Curve, Descriptor, LaneRule, ParamMeta, Port};
use crate::message::Arg;
use crate::operator::{Io, Operator};

// Port + param indices — the wiring contract downstream nodes reference (ADR-0010).
pub const IN_POSITION: usize = 0;
/// Message output ordinal of the `degrees` port (the index [`Io::emit`] uses).
pub const OUT_DEGREES: usize = 0;
pub const P_STRINGS: usize = 0;
pub const P_OCTAVES: usize = 1;
pub const P_VELOCITY: usize = 2;

/// Diatonic degrees spanned per octave (0..6 then the octave at 7).
const DEGREES_PER_OCTAVE: f32 = 7.0;
/// How long after a pluck's note-on the paired note-off fires, in samples (~30 ms @ 48 kHz).
/// The percussive envelope's attack+decay ring within this window, then release takes over.
const PLUCK_SAMPLES: i64 = 1440;
/// Inline capacity for pending note-offs carried across blocks. A human drag emits at most a
/// handful of crossings per block; beyond this it spills to the heap (never on the audio path
/// for realistic input — the smallvec preallocates the inline slots).
const PENDING_CAP: usize = 32;

pub struct Strum {
    /// String index the position last sat in, or -1 before the first position event. Continuous
    /// across blocks; a crossing is `cur_string != prev_string`.
    prev_string: i64,
    /// Pending note-offs: (degree, samples_until_off). Decremented each sample; at 0 the off
    /// fires. Carried across blocks so a pluck near a block edge still rings out and releases.
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

impl Operator for Strum {
    fn descriptor() -> Descriptor {
        Descriptor {
            type_name: "strum",
            inputs: vec![Port::message("position")],
            outputs: vec![Port::message("degrees")],
            params: vec![
                ParamMeta {
                    name: "strings",
                    min: 1.0,
                    max: 32.0,
                    default: 8.0,
                    unit: "strings",
                    curve: Curve::Linear,
                },
                ParamMeta {
                    name: "octaves",
                    min: 1.0,
                    max: 4.0,
                    default: 1.0,
                    unit: "oct",
                    curve: Curve::Linear,
                },
                ParamMeta {
                    name: "velocity",
                    min: 0.0,
                    max: 1.0,
                    default: 1.0,
                    unit: "",
                    curve: Curve::Linear,
                },
            ],
            resources: vec![],
            lanes: LaneRule::Inherit,
        }
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let strings = (io.param(P_STRINGS).round() as i64).clamp(1, 32);
        let octaves = io.param(P_OCTAVES).max(1.0);
        let velocity = io.param(P_VELOCITY).clamp(0.0, 1.0);

        // Snapshot incoming position events (can't read events while emitting), sorted by frame.
        // First numeric arg is the position. Multiple events in a block are walked in order, so
        // a drag across several bands plucks each crossed string in sequence.
        let mut events: SmallVec<[(usize, f32); 16]> = SmallVec::new();
        for ev in io.events() {
            if let Some(v) = ev.args.first().and_then(Arg::as_f32) {
                events.push((ev.frame.min(n.saturating_sub(1)), v));
            }
        }
        events.sort_by_key(|(f, _)| *f);

        let mut prev_string = self.prev_string;
        let mut ei = 0usize;

        for i in 0..n {
            // Fire any pending note-offs due at this sample, then count down the rest.
            let mut k = 0;
            while k < self.pending.len() {
                if self.pending[k].1 <= 0 {
                    let deg = self.pending[k].0;
                    io.emit(OUT_DEGREES, "degree", [Arg::Float(deg), Arg::Float(0.0)], i);
                    self.pending.swap_remove(k);
                } else {
                    self.pending[k].1 -= 1;
                    k += 1;
                }
            }

            // Apply every position event landing at this sample, in order. Each band crossed
            // emits a pluck (note-on now + a scheduled note-off).
            while ei < events.len() && events[ei].0 == i {
                let cur_string = Self::string_at(events[ei].1, strings);
                if prev_string < 0 {
                    // First position seen: latch the band, no pluck (no crossing yet).
                    prev_string = cur_string;
                } else if cur_string != prev_string {
                    let step = if cur_string > prev_string { 1 } else { -1 };
                    let mut s = prev_string;
                    while s != cur_string {
                        s += step;
                        let deg = Self::degree_of(s, strings, octaves);
                        io.emit(
                            OUT_DEGREES,
                            "degree",
                            [Arg::Float(deg), Arg::Float(velocity)],
                            i,
                        );
                        if self.pending.len() < self.pending.capacity() {
                            self.pending.push((deg, PLUCK_SAMPLES));
                        }
                    }
                    prev_string = cur_string;
                }
                ei += 1;
            }
        }

        self.prev_string = prev_string;
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Emit, Event};
    use crate::operator::Io;

    const SR: f32 = 48_000.0;

    /// Run `strum` over one block with the given position events; returns the emitted Messages
    /// (block-absolute frames). Positions are `(frame, value)`.
    fn run(strum: &mut Strum, n: usize, params: &[f32], positions: &[(usize, f32)]) -> Vec<Emit> {
        let args: Vec<crate::message::Args> = positions
            .iter()
            .map(|(_, v)| {
                let mut a = crate::message::Args::new();
                a.push(Arg::Float(*v));
                a
            })
            .collect();
        let evs: Vec<Event> = positions
            .iter()
            .zip(&args)
            .map(|((frame, _), a)| Event {
                addr: "position",
                args: a,
                frame: *frame,
            })
            .collect();
        let mut emits: Vec<Emit> = Vec::new();
        {
            let outs: Vec<&mut [f32]> = vec![]; // Message port — no Signal buffer.
            let inputs: Vec<Option<&[f32]>> = vec![None];
            let mut io = Io::new(SR, n, inputs, outs, params, &evs).with_emit(&mut emits, 0);
            strum.process(&mut io);
        }
        emits
    }

    fn params(strings: f32, octaves: f32, velocity: f32) -> [f32; 3] {
        [strings, octaves, velocity]
    }

    fn deg(e: &Emit) -> f32 {
        e.args[0].as_f32().unwrap()
    }
    fn vel(e: &Emit) -> f32 {
        e.args[1].as_f32().unwrap()
    }

    /// Note-on degrees, in emission order.
    fn on_degrees(emits: &[Emit]) -> Vec<f32> {
        emits.iter().filter(|e| vel(e) > 0.5).map(deg).collect()
    }

    #[test]
    fn sweep_up_plucks_ascending_strings_in_order() {
        // 8 strings, 1 octave: a slow sweep 0 -> 1 crosses all 8 bands, plucking degrees 1..=7
        // plus the top (octave, degree 7). The first band (string 0) is latched on the opening
        // event, so the ascending plucks are strings 1..=7 -> degrees 1,2,3,4,5,6,7.
        let mut strum = Strum::new();
        // One event per band centre, ascending.
        let positions: Vec<(usize, f32)> =
            (0..8).map(|k| (k * 10, (k as f32 + 0.5) / 8.0)).collect();
        let emits = run(&mut strum, 200, &params(8.0, 1.0, 1.0), &positions);

        let ons = on_degrees(&emits);
        assert_eq!(ons, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]);
        assert!(emits.iter().all(|e| e.addr == "degree"));
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
        // First event latches band 0; a single jump to band 4 plucks every string between:
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
        // 8 strings over 2 octaves: top string is degree 14, bottom is 0. Ascending plucks land
        // on 2,4,6,8,10,12,14 (round(k * 14 / 7)).
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
        // Splitting the position stream across two blocks yields the same plucks as one whole
        // block: the band machine carries across the boundary (no spurious or missed crossing).
        let p = params(8.0, 1.0, 1.0);
        // Ascend then descend, crossing the mid-block boundary.
        let positions: Vec<(usize, f32)> = vec![
            (0, 0.01),
            (40, 2.5 / 8.0),
            (90, 5.5 / 8.0),
            (140, 1.5 / 8.0),
        ];
        let n = 200;

        let mut whole = Strum::new();
        let ew = run(&mut whole, n, &p, &positions);
        let ons_whole = on_degrees(&ew);

        let mid = 100;
        let mut split = Strum::new();
        let first: Vec<(usize, f32)> = positions
            .iter()
            .filter(|(f, _)| *f < mid)
            .copied()
            .collect();
        let second: Vec<(usize, f32)> = positions
            .iter()
            .filter(|(f, _)| *f >= mid)
            .map(|(f, v)| (f - mid, *v))
            .collect();
        let e1 = run(&mut split, mid, &p, &first);
        let e2 = run(&mut split, n - mid, &p, &second);
        let mut ons_split = on_degrees(&e1);
        ons_split.extend(on_degrees(&e2));

        assert_eq!(ons_whole, ons_split);
    }

    #[test]
    fn spawned_strum_resets_to_no_band() {
        // Drive `a` to some band, spawn `b`: `b`'s first event latches (no pluck) rather than
        // crossing from where `a` left off.
        let mut a = Strum::new();
        let _ = run(&mut a, 100, &params(8.0, 1.0, 1.0), &[(0, 0.9)]);
        let mut b = a.spawn();
        // b's first position is band 0; with a fresh -1 prev_string it latches and emits nothing.
        let mut emits: Vec<Emit> = Vec::new();
        let mut a0 = crate::message::Args::new();
        a0.push(Arg::Float(0.05));
        let evs = [Event {
            addr: "position",
            args: &a0,
            frame: 0,
        }];
        {
            let outs: Vec<&mut [f32]> = vec![];
            let inputs: Vec<Option<&[f32]>> = vec![None];
            let p = params(8.0, 1.0, 1.0);
            let mut io = Io::new(SR, 50, inputs, outs, &p, &evs).with_emit(&mut emits, 0);
            b.process(&mut io);
        }
        assert!(
            on_degrees(&emits).is_empty(),
            "fresh spawn latches its first position, no pluck: {emits:?}"
        );
    }
}
