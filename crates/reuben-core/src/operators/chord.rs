//! Chord — tap-to-play diatonic harmony: a root degree → stacked-thirds note Messages (ADR-0022).
//!
//! The gesture operator behind the V1.3 Chord-player Toy. Each button on the surface sends a
//! `set [degree, gate]` Message carrying a **scale-degree** chord root (arg0) and a gate (arg1,
//! 1 = press / 0 = release). On a press the op emits the triad of **scale-relative thirds** —
//! degrees `d, d+2, d+4` — as `degree` Messages; on the matching release it emits the note-offs
//! for the same tones. `size` = 4 adds the seventh (`d+6`).
//!
//! Like the [`Sequencer`](crate::operators::Sequencer), it has **no Context input** and emits
//! plain `degree` Messages — the downstream [`Voicer`](crate::operators::Voicer) resolves each
//! degree through the tonal context, so a held or tapped chord **re-spells live** on a key/scale
//! change (the reason this Toy exists). The op only does degree arithmetic; harmony lives in the
//! context.
//!
//! It tracks the **set of held roots** so overlapping chords sound and release independently, and
//! captures each root's tone count at press time, so a mid-hold `size` change can't orphan a
//! note-off. Single-Lane (ADR-0014): emission is pre-fan-out; the Voicer expands to Voices.
//!
//! - input 0: `set` (Message) — `set [degree, gate]`; arg0 = chord-root scale degree, arg1 = gate
//!   (> 0 = note-on, else note-off). One input port/address (degree rides the arg, ADR-0022).
//! - input 1: `size` (`Float`) — chord tones: 3 = triad (default) / 4 = seventh; read block-rate.
//! - output 0 (Message): `degrees` — `degree [degree, vel]` per chord tone; wire to a Voicer.
//!
//! The third is **scale-relative** (`+2` degrees), so the same op spells a major, minor, or
//! diminished triad depending on where the root sits in the scale — the diatonic I–vii° set.

use smallvec::SmallVec;

use crate::descriptor::Descriptor;
use crate::message::Arg;
use crate::operator::{Io, Operator};

// Single-source contract (ADR-0025): one declaration -> IN_/OUT_/P_ consts + Descriptor, no drift.
crate::operator_contract!(Chord {
    inputs:  { set: message,
               size: float { 3.0..=4.0, default 3.0, "tones", lin } },
    outputs: { degrees: message },
});

/// Max chord tones a single press can hold (seventh = 4; headroom for a future `size`).
const MAX_TONES: usize = 4;
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

#[derive(Default)]
pub struct Chord {
    /// Roots currently pressed (note-on, not yet released), with their emitted tone sets.
    /// A SmallVec so the steady-state hold path touches no allocator.
    held: SmallVec<[Held; MAX_HELD]>,
}

impl Chord {
    pub fn new() -> Self {
        Self::default()
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
        let size = (io.value(IN_SIZE).round() as usize).clamp(3, MAX_TONES);

        // Snapshot the set events for this (sub)block, sorted by frame — can't read events
        // while emitting. Each: (frame, root degree, on?).
        let mut events: SmallVec<[(usize, i32, bool); 8]> = SmallVec::new();
        for ev in io.events() {
            if ev.addr != "set" {
                continue;
            }
            let root = match ev.args.first().and_then(Arg::as_f32) {
                Some(v) => v.round() as i32,
                None => continue,
            };
            let on = ev.args.get(1).and_then(Arg::as_f32).unwrap_or(0.0) > 0.0;
            events.push((ev.frame.min(n), root, on));
        }
        events.sort_by_key(|e| e.0);

        for (frame, root, on) in events {
            if on {
                // Press: emit a note-on per chord tone, and remember the tone set for release.
                let (tones, count) = chord_tones(root, size);
                for &t in tones.iter().take(count) {
                    io.emit(
                        OUT_DEGREES,
                        "degree",
                        [Arg::Float(t as f32), Arg::Float(1.0)],
                        frame,
                    );
                }
                // Re-press of an already-held root: replace its record (it re-sounds).
                if let Some(h) = self.held.iter_mut().find(|h| h.root == root) {
                    *h = Held { root, tones, count };
                } else {
                    self.held.push(Held { root, tones, count });
                }
            } else {
                // Release: emit a note-off for each tone of the matching held root, if any.
                if let Some(idx) = self.held.iter().position(|h| h.root == root) {
                    let h = self.held[idx];
                    for &t in h.tones.iter().take(h.count) {
                        io.emit(
                            OUT_DEGREES,
                            "degree",
                            [Arg::Float(t as f32), Arg::Float(0.0)],
                            frame,
                        );
                    }
                    self.held.swap_remove(idx);
                }
            }
        }
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(Chord);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Emit, Event, Message};
    use crate::operator::Io;

    const SR: f32 = 48_000.0;

    /// Run `chord` over one block with the given `set` events; returns the emitted Messages
    /// (segment-relative frames).
    fn run(chord: &mut Chord, n: usize, size: f32, events: &[Message]) -> Vec<Emit> {
        let evs: Vec<Event> = events
            .iter()
            .map(|m| Event {
                addr: &m.addr,
                args: &m.args,
                frame: m.frame,
            })
            .collect();
        // `size` is a `Float` input now (ADR-0028) — supply the per-sample buffer the engine
        // would materialize. Port order: set (Message, via events), size (Float).
        let size_buf = vec![size; n];
        let mut emits: Vec<Emit> = Vec::new();
        {
            let outs: Vec<&mut [f32]> = vec![]; // `degrees` is a Message port — no Signal buffer.
            let inputs: Vec<Option<&[f32]>> = vec![None, Some(&size_buf[..])];
            let params: [f32; 0] = [];
            let mut io = Io::new(SR, n, inputs, outs, &params, &evs).with_emit(&mut emits, 0);
            chord.process(&mut io);
        }
        emits
    }

    fn set(root: f32, gate: f32, frame: usize) -> Message {
        Message::new("set", [Arg::Float(root), Arg::Float(gate)], frame)
    }

    fn deg(e: &Emit) -> f32 {
        e.args[0].as_f32().unwrap()
    }
    fn vel(e: &Emit) -> f32 {
        e.args[1].as_f32().unwrap()
    }

    #[test]
    fn triad_press_emits_three_note_ons_stacked_thirds() {
        // Root degree 0, default size (triad): degrees 0, 2, 4, all note-on, all `degree`.
        let mut c = Chord::new();
        let emits = run(&mut c, 128, 3.0, &[set(0.0, 1.0, 0)]);
        assert_eq!(emits.len(), 3, "triad = 3 tones");
        for e in &emits {
            assert_eq!(e.addr, "degree");
            assert_eq!(e.frame, 0);
            assert_eq!(vel(e), 1.0);
        }
        let degs: Vec<f32> = emits.iter().map(deg).collect();
        assert_eq!(degs, vec![0.0, 2.0, 4.0]);
    }

    #[test]
    fn triad_on_arbitrary_root_stacks_relative_thirds() {
        // Root degree 4 (the V chord) → degrees 4, 6, 8.
        let mut c = Chord::new();
        let emits = run(&mut c, 128, 3.0, &[set(4.0, 1.0, 0)]);
        let degs: Vec<f32> = emits.iter().map(deg).collect();
        assert_eq!(degs, vec![4.0, 6.0, 8.0]);
    }

    #[test]
    fn seventh_press_emits_four_note_ons() {
        // size = 4 (seventh): degrees d, d+2, d+4, d+6.
        let mut c = Chord::new();
        let emits = run(&mut c, 128, 4.0, &[set(1.0, 1.0, 0)]);
        assert_eq!(emits.len(), 4, "seventh = 4 tones");
        let degs: Vec<f32> = emits.iter().map(deg).collect();
        assert_eq!(degs, vec![1.0, 3.0, 5.0, 7.0]);
    }

    #[test]
    fn release_emits_matching_note_offs() {
        // Press then release the same root in one block: 3 ons (gate 1) then 3 offs (gate 0),
        // same degrees, off at the release frame.
        let mut c = Chord::new();
        let emits = run(&mut c, 128, 3.0, &[set(0.0, 1.0, 0), set(0.0, 0.0, 64)]);
        assert_eq!(emits.len(), 6);
        let ons: Vec<f32> = emits.iter().filter(|e| vel(e) > 0.5).map(deg).collect();
        let offs: Vec<(f32, usize)> = emits
            .iter()
            .filter(|e| vel(e) < 0.5)
            .map(|e| (deg(e), e.frame))
            .collect();
        assert_eq!(ons, vec![0.0, 2.0, 4.0]);
        assert_eq!(offs, vec![(0.0, 64), (2.0, 64), (4.0, 64)]);
    }

    #[test]
    fn release_with_no_matching_root_is_silent() {
        // A gate-0 for a root that was never pressed emits nothing.
        let mut c = Chord::new();
        let emits = run(&mut c, 128, 3.0, &[set(2.0, 0.0, 0)]);
        assert!(emits.is_empty());
    }

    #[test]
    fn two_overlapping_held_roots_sound_and_release_independently() {
        // Press root 0 (0,2,4) and root 1 (1,3,5); release only root 0 — its 3 tones go off,
        // root 1's stay held (no off for 1,3,5).
        let mut c = Chord::new();
        let emits = run(
            &mut c,
            256,
            3.0,
            &[set(0.0, 1.0, 0), set(1.0, 1.0, 32), set(0.0, 0.0, 128)],
        );
        // 3 + 3 ons, then 3 offs for root 0 only.
        let ons: Vec<f32> = emits.iter().filter(|e| vel(e) > 0.5).map(deg).collect();
        assert_eq!(ons, vec![0.0, 2.0, 4.0, 1.0, 3.0, 5.0]);
        let offs: Vec<f32> = emits.iter().filter(|e| vel(e) < 0.5).map(deg).collect();
        assert_eq!(offs, vec![0.0, 2.0, 4.0], "only root 0 releases");
        // Root 1 is still held in state.
        assert_eq!(c.held.len(), 1);
        assert_eq!(c.held[0].root, 1);
    }

    #[test]
    fn hold_persists_across_blocks_then_releases() {
        // Press in block 1 (held carries forward); release in block 2 with no new press.
        let mut c = Chord::new();
        let on = run(&mut c, 128, 3.0, &[set(3.0, 1.0, 0)]);
        assert_eq!(on.len(), 3);
        assert_eq!(c.held.len(), 1);
        let off = run(&mut c, 128, 3.0, &[set(3.0, 0.0, 0)]);
        let degs: Vec<f32> = off.iter().map(deg).collect();
        assert_eq!(degs, vec![3.0, 5.0, 7.0]);
        assert!(off.iter().all(|e| vel(e) < 0.5));
        assert!(c.held.is_empty(), "released root leaves state");
    }

    #[test]
    fn release_matches_press_size_even_if_size_changed_mid_hold() {
        // Press as a seventh (4 tones), then release after `size` dropped to 3: the release
        // must still send 4 note-offs (the captured press set), or a voice would hang.
        let mut c = Chord::new();
        let on = run(&mut c, 128, 4.0, &[set(0.0, 1.0, 0)]);
        assert_eq!(on.len(), 4);
        let off = run(&mut c, 128, 3.0, &[set(0.0, 0.0, 0)]);
        assert_eq!(off.len(), 4, "release covers all originally-pressed tones");
        let degs: Vec<f32> = off.iter().map(deg).collect();
        assert_eq!(degs, vec![0.0, 2.0, 4.0, 6.0]);
    }

    #[test]
    fn non_set_events_are_ignored() {
        let mut c = Chord::new();
        let other = Message::new("note", [Arg::Float(0.0), Arg::Float(1.0)], 0);
        assert!(run(&mut c, 128, 3.0, &[other]).is_empty());
    }

    #[test]
    fn spawned_chord_starts_with_no_held_roots() {
        let mut a = Chord::new();
        let _ = run(&mut a, 128, 3.0, &[set(0.0, 1.0, 0)]);
        assert_eq!(a.held.len(), 1);
        let b = a.spawn();
        // A fresh spawn carries no held state — its first release emits nothing.
        let mut bb = b;
        let mut emits: Vec<Emit> = Vec::new();
        {
            let evs = [Event {
                addr: "set",
                args: &Message::new("set", [Arg::Float(0.0), Arg::Float(0.0)], 0).args,
                frame: 0,
            }];
            let outs: Vec<&mut [f32]> = vec![];
            let size_buf = vec![3.0f32; 128];
            let inputs: Vec<Option<&[f32]>> = vec![None, Some(&size_buf[..])];
            let params: [f32; 0] = [];
            let mut io = Io::new(SR, 128, inputs, outs, &params, &evs).with_emit(&mut emits, 0);
            bb.process(&mut io);
        }
        assert!(emits.is_empty(), "spawn resets held roots");
    }
}
