//! Chord — tap-to-play diatonic harmony: a root degree → stacked-thirds note Messages.
//!
//! The gesture operator behind the V1.3 Chord-player Toy. Each button on the surface sends a `set`
//! `Note` carrying a **scale-degree** chord root ([`Degree`](crate::vocab::pitch::Pitch::Degree)) and a
//! velocity (> 0 = press / 0 = release). On a press the op emits the triad of **scale-relative
//! thirds** — degrees `d, d+2, d+4` — as degree [`Note`]s; on the matching release it emits the
//! note-offs for the same tones. `size` = 4 adds the seventh (`d+6`).
//!
//! Like the [`Sequencer`](crate::operators::Sequencer), it has **no Harmony input** and emits
//! plain degree [`Note`]s — the downstream [`Voicer`](crate::operators::Voicer) resolves each
//! degree through the tonal context, so a held or tapped chord **re-spells live** on a key/scale
//! change (the reason this Toy exists). The op only does degree arithmetic; harmony lives in the
//! context.
//!
//! It tracks the **set of held roots** so overlapping chords sound and release independently, and
//! captures each root's tone count at press time, so a mid-hold `size` change can't orphan a
//! note-off. A re-press of a held root releases its old chord before sounding the new one, so
//! every note-on is paired with exactly one note-off. Past twelve held roots, a press first
//! releases the least recently pressed root's tones, so no tone hangs and nothing grows on the
//! render thread. Emits one note stream, upstream of the
//! Voicer that fans it out to voices.
//!
//! - input 0: `set` (`Note`) — a degree note; velocity > 0 = note-on, else note-off. The degree
//!   is the chord root.
//! - input 1: `size` (`i32`, held) — chord tones: 3 = triad (default) / 4 = seventh. Its
//!   `3..=MAX_TONES` bound rides the port contract.
//! - output 0: `degrees` (`Note`) — a degree note per chord tone; wire to a Voicer.
//!
//! The third is **scale-relative** (`+2` degrees), so the same op spells a major, minor, or
//! diminished triad depending on where the root sits in the scale — the diatonic I–vii° set.

use alloc::boxed::Box;
use alloc::vec::Vec;
use smallvec::SmallVec;

use crate::descriptor::Descriptor;
use crate::engine::PENDING_CAP;
use crate::operator::{Io, Operator};
use crate::vocab::pitch::{Note, Pitch};

// `set` is a `Note` event port; `size` a held `i32` (3 = triad .. MAX_TONES = seventh).
crate::operator_contract!(Chord {
    inputs:  { set:  note,
               size: i32 { 3..=4, default 3 } },
    outputs: { degrees: note },
});

/// Max chord tones a single press can hold (seventh = 4; headroom for a future `size`).
const MAX_TONES: usize = 4;

// The `size` port's contract max is the literal `4`; keep it tied to `MAX_TONES` (the `tones`
// array bound `chord_tones` fills) so a change to one without the other fails to compile.
const _: () = assert!(MAX_TONES == 4);
/// Most simultaneously-held roots we track without spilling to the heap (10 fingers + slop).
const MAX_HELD: usize = 12;

/// One currently-held root: its root degree and the chord tones emitted for it, so the
/// matching release sends note-offs for *exactly* the same degrees even if `size` changed.
#[derive(Clone, Copy)]
struct Held {
    root: i32,
    tones: [i32; MAX_TONES],
    count: usize,
}

pub struct Chord {
    /// Roots currently pressed (note-on, not yet released), least recently pressed first, with
    /// their emitted tone sets. Never longer than [`MAX_HELD`], so it never spills.
    held: SmallVec<[Held; MAX_HELD]>,
    /// This (sub)block's `set` events `(frame, delivery index, root, on?)`, cleared per call. Holds
    /// a full control queue of them; a denser emitted burst grows it once and the capacity is kept.
    events: Vec<(usize, usize, i32, bool)>,
}

impl Default for Chord {
    fn default() -> Self {
        Self {
            held: SmallVec::new(),
            events: Vec::with_capacity(PENDING_CAP),
        }
    }
}

impl Chord {
    pub fn new() -> Self {
        Self::default()
    }

    /// Emit a note-off for each of `h`'s tones at `frame`.
    fn release(io: &mut Io, frame: usize, h: &Held) {
        for &t in h.tones.iter().take(h.count) {
            io.write(OUT_DEGREES)
                .emit(frame, Note::new(Pitch::Degree(t), 0.0));
        }
    }
}

/// The chord tones for root degree `d` at `size` tones: stacked scale-relative thirds
/// `d, d+2, d+4 [, d+6]`. Returns the filled tone array + its length (≤ [`MAX_TONES`]).
fn chord_tones(root: i32, size: usize) -> ([i32; MAX_TONES], usize) {
    let count = size.clamp(3, MAX_TONES);
    let mut tones = [0i32; MAX_TONES];
    for (k, t) in tones.iter_mut().enumerate().take(count) {
        *t = root + 2 * k as i32; // d, d+2, d+4, d+6 — scale-relative thirds.
    }
    (tones, count)
}

impl Operator for Chord {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        // `size` is a held `i32`; its `3..=MAX_TONES` bound rides the port contract, and
        // `chord_tones` floors/caps the count again as it fills the fixed tone array.
        let size = io.read(IN_SIZE) as usize;

        // Snapshot the set events for this (sub)block, sorted by frame — can't read the stream
        // while emitting. A non-degree note has no root → skip.
        self.events.clear();
        for (k, s) in io.read(IN_SET).enumerate() {
            let root = match s.payload.pitch.degree() {
                Some(d) => d,
                None => continue,
            };
            self.events
                .push((s.frame.min(n), k, root, s.payload.velocity > 0.0));
        }
        // Not `sort_by_key`: a stable sort allocates its merge buffer once a burst outgrows the
        // stack scratch. Keying on the delivery index keeps same-frame events in delivery order.
        self.events.sort_unstable_by_key(|e| (e.0, e.1));

        for k in 0..self.events.len() {
            let (frame, _, root, on) = self.events[k];
            if on {
                // Press past the held roots: release the least recently pressed first, so its tones
                // are off before this chord's sound.
                let held = self.held.iter().any(|h| h.root == root);
                if !held && self.held.len() == MAX_HELD {
                    let oldest = self.held.remove(0);
                    Self::release(io, frame, &oldest);
                }
                // A re-press re-sounds the root: it releases its whole old chord and goes last, as the
                // most recently pressed. Every old tone needs its note-off here, shared or not: the
                // Voicer gives each note-on its own voice, and the eventual release sends one
                // note-off per tone of the new record only.
                if let Some(idx) = self.held.iter().position(|h| h.root == root) {
                    let old = self.held.remove(idx);
                    Self::release(io, frame, &old);
                }
                // Emit a note-on per chord tone, and remember the tone set for release.
                let (tones, count) = chord_tones(root, size);
                for &t in tones.iter().take(count) {
                    io.write(OUT_DEGREES)
                        .emit(frame, Note::new(Pitch::Degree(t), 1.0));
                }
                self.held.push(Held { root, tones, count });
            } else {
                // Release: emit a note-off for each tone of the matching held root, if any.
                if let Some(idx) = self.held.iter().position(|h| h.root == root) {
                    let h = self.held.remove(idx);
                    Self::release(io, frame, &h);
                }
            }
        }
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Arg, Emit, Event};
    use crate::op_driver::OpDriver;

    const SR: f32 = 48_000.0;

    /// Drive a fresh Chord through the real engine: `size` is a held `Float` (`set` once, read via
    /// `io.read`); the `set` press/release notes are pushed as `Note` events at their global frames.
    /// Renders `n` frames (as real 128-frame blocks) and returns the emitted Messages.
    fn run(n: usize, size: f32, events: &[(usize, Note)]) -> Vec<Emit> {
        let mut d = OpDriver::for_type(Chord::new(), SR);
        d.set(IN_SIZE, size);
        for (frame, note) in events {
            d.push(IN_SET, *frame, *note);
        }
        d.render(n).emits().to_vec()
    }

    /// A `set` press/release as a degree note: `(frame, Note(Degree(root), gate))`.
    fn set(root: i32, gate: f32, frame: usize) -> (usize, Note) {
        (frame, Note::new(Pitch::Degree(root), gate))
    }

    fn deg(e: &Emit) -> i32 {
        match &e.arg {
            Arg::Note(n) => n.pitch.degree().unwrap(),
            other => panic!("expected a Note, got {other:?}"),
        }
    }
    fn vel(e: &Emit) -> f32 {
        match &e.arg {
            Arg::Note(n) => n.velocity,
            other => panic!("expected a Note, got {other:?}"),
        }
    }

    #[test]
    fn a_press_past_the_held_roots_releases_the_oldest_first() {
        // MAX_HELD roots pressed at frames 0.., then one more at frame 100: the oldest root (0)
        // releases its triad at that frame, before the new root's note-ons.
        let mut events: Vec<_> = (0..MAX_HELD as i32).map(|r| set(10 * r, 1.0, r as usize)).collect();
        events.push(set(500, 1.0, 100));
        let emits = run(128, 3.0, &events);
        let at_100: Vec<(i32, f32)> = emits
            .iter()
            .filter(|e| e.frame == 100)
            .map(|e| (deg(e), vel(e)))
            .collect();
        assert_eq!(
            at_100,
            vec![(0, 0.0), (2, 0.0), (4, 0.0), (500, 1.0), (502, 1.0), (504, 1.0)]
        );
        // Releasing the evicted root later emits nothing: it is no longer held.
        events.push(set(0, 0.0, 110));
        let emits = run(128, 3.0, &events);
        assert!(emits.iter().all(|e| e.frame != 110));
    }

    #[test]
    fn a_re_press_makes_a_root_the_last_to_be_released() {
        // Twelve roots held, root 0 re-pressed, then a thirteenth: root 10, now the least recently
        // pressed, releases, not root 0.
        let mut events: Vec<_> = (0..MAX_HELD as i32).map(|r| set(10 * r, 1.0, r as usize)).collect();
        events.push(set(0, 1.0, 50));
        events.push(set(500, 1.0, 100));
        let emits = run(128, 3.0, &events);
        let released: Vec<i32> = emits
            .iter()
            .filter(|e| e.frame == 100 && vel(e) == 0.0)
            .map(deg)
            .collect();
        assert_eq!(released, vec![10, 12, 14]);
    }

    #[test]
    fn triad_press_emits_three_note_ons_stacked_thirds() {
        // Root degree 0, default size (triad): degrees 0, 2, 4, all note-on.
        let emits = run(128, 3.0, &[set(0, 1.0, 0)]);
        assert_eq!(emits.len(), 3, "triad = 3 tones");
        for e in &emits {
            assert_eq!(e.frame, 0);
            approx::assert_relative_eq!(vel(e), 1.0);
        }
        let degs: Vec<i32> = emits.iter().map(deg).collect();
        assert_eq!(degs, vec![0, 2, 4]);
    }

    #[test]
    fn triad_on_arbitrary_root_stacks_relative_thirds() {
        // Root degree 4 (the V chord) → degrees 4, 6, 8.
        let emits = run(128, 3.0, &[set(4, 1.0, 0)]);
        let degs: Vec<i32> = emits.iter().map(deg).collect();
        assert_eq!(degs, vec![4, 6, 8]);
    }

    #[test]
    fn seventh_press_emits_four_note_ons() {
        // size = 4 (seventh): degrees d, d+2, d+4, d+6.
        let emits = run(128, 4.0, &[set(1, 1.0, 0)]);
        assert_eq!(emits.len(), 4, "seventh = 4 tones");
        let degs: Vec<i32> = emits.iter().map(deg).collect();
        assert_eq!(degs, vec![1, 3, 5, 7]);
    }

    #[test]
    fn release_emits_matching_note_offs() {
        // Press then release the same root in one block: 3 ons then 3 offs, same degrees, off at
        // the release frame.
        let emits = run(128, 3.0, &[set(0, 1.0, 0), set(0, 0.0, 64)]);
        assert_eq!(emits.len(), 6);
        let ons: Vec<i32> = emits.iter().filter(|e| vel(e) > 0.5).map(deg).collect();
        let offs: Vec<(i32, usize)> = emits
            .iter()
            .filter(|e| vel(e) < 0.5)
            .map(|e| (deg(e), e.frame))
            .collect();
        assert_eq!(ons, vec![0, 2, 4]);
        assert_eq!(offs, vec![(0, 64), (2, 64), (4, 64)]);
    }

    #[test]
    fn release_with_no_matching_root_is_silent() {
        // A note-off for a root that was never pressed emits nothing.
        let emits = run(128, 3.0, &[set(2, 0.0, 0)]);
        assert!(emits.is_empty());
    }

    #[test]
    fn two_overlapping_held_roots_sound_and_release_independently() {
        // Press root 0 (0,2,4) and root 1 (1,3,5); release root 0 at frame 128 — only its 3 tones
        // go off. Then release root 1 at frame 192: its tones (1,3,5) go off, proving it was held
        // independently across the intervening block boundary (the behavioral stand-in for the old
        // white-box `held` set inspection — `OpDriver` owns the operator).
        let emits = run(
            256,
            3.0,
            &[
                set(0, 1.0, 0),
                set(1, 1.0, 32),
                set(0, 0.0, 128),
                set(1, 0.0, 192),
            ],
        );
        let ons: Vec<i32> = emits.iter().filter(|e| vel(e) > 0.5).map(deg).collect();
        assert_eq!(ons, vec![0, 2, 4, 1, 3, 5]);
        // Root 0's release (frame 128) sends only its own tones; root 1's release (frame 192) sends
        // root 1's — independent hold/release.
        let offs_at = |f: usize| -> Vec<i32> {
            emits
                .iter()
                .filter(|e| vel(e) < 0.5 && e.frame == f)
                .map(deg)
                .collect()
        };
        assert_eq!(offs_at(128), vec![0, 2, 4], "only root 0 releases at 128");
        assert_eq!(offs_at(192), vec![1, 3, 5], "root 1 was still held");
    }

    #[test]
    fn hold_persists_across_blocks_then_releases() {
        // Press in block 1 (held carries across the real 128-frame boundary); release in block 2.
        // A second release at frame 192 emits nothing more — proof the release left no held state.
        let emits = run(
            256,
            3.0,
            &[set(3, 1.0, 0), set(3, 0.0, 128), set(3, 0.0, 192)],
        );
        let ons: Vec<&Emit> = emits.iter().filter(|e| vel(e) > 0.5).collect();
        let offs: Vec<&Emit> = emits.iter().filter(|e| vel(e) < 0.5).collect();
        assert_eq!(ons.len(), 3, "the press sounds across the block boundary");
        let on_degs: Vec<i32> = ons.iter().map(|e| deg(e)).collect();
        assert_eq!(on_degs, vec![3, 5, 7]);
        let off_degs: Vec<i32> = offs.iter().map(|e| deg(e)).collect();
        assert_eq!(off_degs, vec![3, 5, 7]);
        assert!(
            offs.iter().all(|e| e.frame == 128),
            "exactly one release batch (the second release is a no-op — held state was cleared)"
        );
    }

    #[test]
    fn release_matches_press_size_even_if_size_changed_mid_hold() {
        // Press as a seventh (4 tones), then release after `size` dropped to 3: the release must
        // still send 4 note-offs (the captured press set), or a voice would hang.
        //
        // NOT converted to `OpDriver`: this requires the held `size` to change *between* the press
        // block and the release block with no intervening re-press. `OpDriver::set` can change a
        // held control between `render` calls, but a pushed event re-fires on every `render` (each
        // restarts at frame 0), so a second `render` would re-press the root at the new size and
        // replace the captured tone set — defeating the test. Kept on the hand-rolled `Io` path
        // (the only operator surface that can feed block 1 then block 2 with no re-press).
        let mut c = Chord::new();
        let on = run_io(&mut c, 128, 4.0, &[set(0, 1.0, 0)]);
        assert_eq!(on.len(), 4);
        let off = run_io(&mut c, 128, 3.0, &[set(0, 0.0, 0)]);
        assert_eq!(off.len(), 4, "release covers all originally-pressed tones");
        let degs: Vec<i32> = off.iter().map(deg).collect();
        assert_eq!(degs, vec![0, 2, 4, 6]);
    }

    /// Sorted note-on degrees and sorted note-off degrees, duplicates kept: equal means every
    /// note-on has exactly one note-off, which is what the Voicer (one voice per note-on, one voice
    /// freed per note-off) needs for no voice to hang.
    fn ons_offs(emits: &[Emit]) -> (Vec<i32>, Vec<i32>) {
        let mut ons: Vec<i32> = emits.iter().filter(|e| vel(e) > 0.5).map(deg).collect();
        let mut offs: Vec<i32> = emits.iter().filter(|e| vel(e) < 0.5).map(deg).collect();
        ons.sort_unstable();
        offs.sort_unstable();
        (ons, offs)
    }

    #[test]
    fn a_re_press_at_a_smaller_size_releases_the_old_chord() {
        // Seventh pressed, re-pressed as a triad, then released. Three `run_io` blocks for the same
        // reason as the test above.
        let mut c = Chord::new();
        let mut emits = run_io(&mut c, 128, 4.0, &[set(0, 1.0, 0)]);
        let re = run_io(&mut c, 128, 3.0, &[set(0, 1.0, 0)]);
        // The whole old seventh goes off before the triad re-sounds.
        let re_pairs: Vec<(i32, f32)> = re.iter().map(|e| (deg(e), vel(e))).collect();
        assert_eq!(
            re_pairs,
            vec![(0, 0.0), (2, 0.0), (4, 0.0), (6, 0.0), (0, 1.0), (2, 1.0), (4, 1.0)]
        );
        emits.extend(re);
        emits.extend(run_io(&mut c, 128, 3.0, &[set(0, 0.0, 0)]));
        let (ons, offs) = ons_offs(&emits);
        assert_eq!(ons, offs, "every note-on has exactly one note-off");
    }

    #[test]
    fn an_unchanged_re_press_pairs_every_note_on_with_a_note_off() {
        // Press, re-press at the same size, release: the re-press releases the first triad, so the
        // release leaves no second voice per tone sounding.
        let emits = run(128, 3.0, &[set(0, 1.0, 0), set(0, 1.0, 64), set(0, 0.0, 96)]);
        let at_64: Vec<(i32, f32)> = emits
            .iter()
            .filter(|e| e.frame == 64)
            .map(|e| (deg(e), vel(e)))
            .collect();
        assert_eq!(
            at_64,
            vec![(0, 0.0), (2, 0.0), (4, 0.0), (0, 1.0), (2, 1.0), (4, 1.0)]
        );
        let (ons, offs) = ons_offs(&emits);
        assert_eq!(ons, offs, "every note-on has exactly one note-off");
    }

    /// Hand-rolled single-block runner for the tests that change the held `size` between blocks
    /// (see `release_matches_press_size_even_if_size_changed_mid_hold`): it threads the same `Chord`
    /// instance across calls with no re-fired events, which the `OpDriver` push model can't express.
    fn run_io(chord: &mut Chord, n: usize, size: f32, events: &[(usize, Note)]) -> Vec<Emit> {
        let args: Vec<Arg> = events.iter().map(|(_, nt)| Arg::Note(*nt)).collect();
        let evs: Vec<Event> = events
            .iter()
            .zip(&args)
            .map(|((frame, _), arg)| Event { arg, frame: *frame })
            .collect();
        let latched = [Arg::F32(0.0), Arg::F32(size)];
        let streams: [&[Event]; 2] = [&evs, &[]];
        let mut emits: Vec<Emit> = Vec::new();
        {
            let outs: Vec<&mut [f32]> = vec![];
            let inputs: Vec<Option<&[f32]>> = vec![None, None];
            let mut io = Io::new(SR, n, inputs, outs)
                .with_latched(&latched)
                .with_streams(&streams)
                .with_emit(&mut emits, 0);
            chord.process(&mut io);
        }
        emits
    }

    #[test]
    fn absolute_notes_are_ignored() {
        // A `set` carrying an absolute pitch has no chord-root degree → emits nothing.
        let other = (0, Note::new(Pitch::Absolute(60.0), 1.0));
        assert!(run(128, 3.0, &[other]).is_empty());
    }

    #[test]
    fn spawned_chord_starts_with_no_held_roots() {
        // Press a root on `a` (3 ons proves it has a held root), then spawn `b`: a fresh spawn
        // carries no held state, so its first release emits nothing.
        let mut a = OpDriver::for_type(Chord::new(), SR);
        a.set(IN_SIZE, 3.0)
            .push(IN_SET, 0, Note::new(Pitch::Degree(0), 1.0));
        assert_eq!(a.render(128).emits().len(), 3, "`a` holds a root");

        let mut b = a.spawn();
        b.set(IN_SIZE, 3.0)
            .push(IN_SET, 0, Note::new(Pitch::Degree(0), 0.0));
        assert!(b.render(128).emits().is_empty(), "spawn resets held roots");
    }
}
