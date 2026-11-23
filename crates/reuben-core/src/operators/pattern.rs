//! pattern — play a compiled mini-notation pattern against the Clock.
//!
//! `pat` is a pattern in the `reuben-pattern` mini-notation (`"0 [3 5] <7 ~> 2*2"`), compiled off
//! the render thread by [`compile`]; the render thread only ever holds the compiled
//! [`Pattern`](reuben_pattern::Pattern). The Operator turns the Clock's `beat` and `phase` into a
//! position in cycles and fires each note at the exact frame the position reaches it.
//!
//! - input `pat` (`str`): the pattern. Unset, it is empty: silence.
//! - input `phase` (`f32_buffer`): the Clock's `phase`, a [0, 1) ramp per beat.
//! - input `beat` (`i32`): the Clock's `beat`, whole beats since its last `sync`.
//! - input `mode` ([`PatternMode`]): `Degree` (a number is a scale degree, resolved downstream
//!   through the harmony context), `Midi` (a number is a MIDI note, and a note name such as `c4`
//!   or `eb3` is read too), or `Value` (a number drives `value`; no notes).
//! - input `beats_per_cycle` (`i32`, default 4): how many beats one pattern cycle lasts.
//! - output `notes` (`Note` events): a note-on (velocity 1) at each event's onset and its note-off
//!   (velocity 0) at its release, each at its exact frame. A note releases at the earlier of its own
//!   end and the end of its step, so a step never sounds into the next (`[0 1]/1.5 2`: the `1`
//!   releases where the `2` starts).
//! - output `value` (`f32`, held): in `Value` mode the atom's number at each onset. A rest holds the
//!   last value, and so does every other mode.
//!
//! An atom the mode cannot read — a note name outside `Midi` mode, or a degree that is not a whole
//! number — is skipped and counted (`pattern_atoms_skipped` in the engine diagnostics).
//!
//! # Position
//!
//! The cycle is `floor(beat / beats_per_cycle)` and the position within it
//! `(beat mod beats_per_cycle + phase) / beats_per_cycle`, in f64. `beat` is held, so a change
//! slices the block at its frame: within one `process` call the cycle is constant and only `phase`
//! moves. The position is compared against each note's onset converted to f64 once; the phase is
//! never converted to a fraction.
//!
//! # Firing
//!
//! This follows the protocol in the `reuben-pattern` crate docs ("Playing a pattern from a clock"):
//!
//! - On entering a cycle the Operator queries it once into a buffer allocated at construction,
//!   sorts it by onset, and caches it. A query that does not fit is truncated and counted
//!   (`pattern_queries_truncated`).
//! - Each frame it first releases every held note whose release the position is at or past, then
//!   fires every cached note whose onset it is at or past. Releases go first because a release can
//!   equal the next onset (`0 0`, `0*4`).
//! - A fired note moves to a fixed-capacity held-notes list with its absolute release, which can
//!   lie in a later cycle (`0/1.5`). When the list is full, firing first releases the oldest held
//!   note, counted (`pattern_notes_evicted`).
//! - The previous frame's position is kept across calls, so each note fires exactly once however
//!   blocks fall.
//!
//! **Jumps.** A position that goes backwards — a Clock `sync`, `beat` wrapping, `phase` dropping
//! without `beat` stepping — or that skips a whole cycle is a locate: every held note is released,
//! then the cycle is re-queried from the new position, where a note whose onset lies exactly there
//! fires and earlier ones do not. A change of `mode` or `beats_per_cycle` releases every held note
//! and re-seeks the same way. So does the Operator's first call, which is how a node added mid-set
//! joins in time.
//!
//! **A new pattern** waits for the next cycle start and replaces the current one there, by swapping
//! a reference: the old pattern plays out its cycle, and its held notes release on schedule. A
//! pattern arriving while the current one is empty text, or before any has played, takes effect at
//! once from the current position. Every re-seek (a locate, a `mode` or `beats_per_cycle` change)
//! also applies a waiting pattern, since it starts the cycle afresh. The value the Operator lets go of goes to the retire queue ([`Io::retire`]), so the
//! render thread frees nothing.
//!
//! **Swap.** The cached cycle, the current and waiting patterns, the held notes and the previous
//! position all live in the box, so a surviving node carries them across a Swap. `value` is a held
//! output published on change, so [`on_transplant`](Operator::on_transplant) re-announces it.
//!
//! # Randomness
//!
//! `?` and `|` draw from a hash of the cycle, the structure, and a seed. The seed is the same fixed
//! value for every node, so the same pattern text plays the same notes every run: a performance
//! replays identically, and two nodes playing the same text agree.
//!
//! # Stack
//!
//! [`Pattern::cycle`](reuben_pattern::Pattern::cycle) recurses once per tree level and documents a
//! need of up to 40 KiB of stack in a release build (160 KiB unoptimised). The native engine calls
//! it on its audio thread: cpal's own stream thread on ALSA and WASAPI, and the headless render
//! thread, are std threads with Rust's 2 MiB default; on CoreAudio it is the system's I/O thread,
//! whose stack is 512 KiB. Each leaves more than 470 KiB of headroom in release, and over 350 KiB
//! unoptimised, above a render path that adds a few KiB, and a Voicer's nested render a few more.
//! An embedder driving the render on its own thread (the bare-metal target) must give that thread
//! at least 48 KiB for this Operator.

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

use reuben_pattern::{Pattern as Compiled, Rational, Value};

use crate::descriptor::Descriptor;
use crate::message::Arg;
use crate::operator::{Io, Operator, OperatorCounts};
use crate::prepare::{PrepareError, Prepared};
use crate::vocab::pitch::{Note, Pitch};
use crate::vocab::PatternMode;

crate::operator_contract!(Pattern {
    type_name: "pattern",
    inputs:  { pat: str(crate::operators::pattern::compile),
               phase: f32_buffer,
               beat: i32 { 0..=2147483647, default 0 },
               mode: enum(PatternMode),
               beats_per_cycle: i32 { 1..=256, default 4 } },
    outputs: { notes: note,
               value: f32 { min..=max, default 0.0 } },
});

/// Compile pattern text into the payload the render thread plays. Off the render thread: it
/// allocates. A refusal carries the byte span at fault.
pub fn compile(source: &str) -> Result<Prepared, PrepareError> {
    Compiled::new(source)
        .map(Prepared::new)
        .map_err(|e| PrepareError::new(e.span, e.message))
}

/// Notes one cycle query can return. Above it the query truncates and counts. 64 is sixteen notes
/// per beat at the default four beats per cycle; `0*128` (128 notes, still inside the compiler's
/// work bound) is the kind of pattern that truncates.
const CYCLE_CAP: usize = 64;

/// Notes sounding at once. Firing one more first releases the oldest.
const HELD_CAP: usize = 32;

/// The seed for `?` and `|`: fixed, so a performance replays identically.
const SEED: u64 = 0;

/// The largest f32 below `1.0`, the ceiling on a `phase` read: a Clock never emits more, and a
/// phase of `1.0` from another source would otherwise equal the next beat's start.
const PHASE_CEIL: f32 = 1.0 - f32::EPSILON / 2.0;

/// A position in cycles: the integer cycle and the fraction through it, in [0, 1). Ordered
/// cycle-first, so a position in a later cycle compares greater whatever the fractions, and two
/// fractions are only ever compared within one cycle.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
struct Pos {
    cycle: i64,
    frac: f64,
}

impl Pos {
    /// Later than every position a clock can reach.
    const NEVER: Pos = Pos {
        cycle: i64::MAX,
        frac: 0.0,
    };

    /// `cycle + r`, with `r` (a release, at or above 0) split into its whole cycles and fraction.
    fn offset(cycle: i64, r: Rational) -> Pos {
        let whole = r.floor();
        let frac = r
            .checked_sub(Rational::int(whole))
            .map_or(0.0, Rational::to_f64);
        Pos {
            cycle: cycle.saturating_add(whole),
            frac,
        }
    }
}

/// What a cached note does when it fires.
#[derive(Clone, Copy, Debug)]
enum Fire {
    /// Emit a note-on with this pitch, and its note-off at the release.
    Note(Pitch),
    /// Set `value`.
    Value(f32),
}

#[derive(Clone, Copy, Debug)]
struct Cached {
    onset: f64,
    release: Pos,
    fire: Fire,
}

#[derive(Clone, Copy, Debug)]
struct Held {
    release: Pos,
    pitch: Pitch,
}

pub struct Pattern {
    /// The pattern playing. `None` until the first call adopts the input's value.
    current: Option<Prepared>,
    /// A pattern waiting for the next cycle start.
    next: Option<Prepared>,
    /// The query's output buffer, [`CYCLE_CAP`] long.
    query: Box<[reuben_pattern::Note]>,
    /// The cached cycle's playable notes, sorted by onset; capacity [`CYCLE_CAP`].
    cache: Vec<Cached>,
    /// The first cached note not yet fired.
    cursor: usize,
    /// The cycle `cache` holds, or `None` before the first query and after a re-seek is asked for.
    cached_cycle: Option<i64>,
    /// Sounding notes in firing order, oldest first; capacity [`HELD_CAP`].
    held: Vec<Held>,
    /// The earliest release in `held`, or [`Pos::NEVER`].
    next_release: Pos,
    /// The previous frame's position, `None` before the first frame.
    prev: Option<Pos>,
    /// `mode` and `beats_per_cycle` as the previous call read them.
    mode: Option<PatternMode>,
    beats_per_cycle: i32,
    /// The last `value` set, re-announced after a transplant.
    value: Option<f32>,
    announce: bool,
}

impl Default for Pattern {
    fn default() -> Self {
        Self::new()
    }
}

impl Pattern {
    pub fn new() -> Self {
        Self {
            current: None,
            next: None,
            query: vec![reuben_pattern::Note::EMPTY; CYCLE_CAP].into_boxed_slice(),
            cache: Vec::with_capacity(CYCLE_CAP),
            cursor: 0,
            cached_cycle: None,
            held: Vec::with_capacity(HELD_CAP),
            next_release: Pos::NEVER,
            prev: None,
            mode: None,
            beats_per_cycle: 0,
            value: None,
            announce: false,
        }
    }

    /// Take the input's pattern if it is new: at once when nothing plays (returning `true`, so the
    /// caller re-seeks), else as the waiting pattern.
    fn adopt(&mut self, io: &Io, latched: Option<&Prepared>) -> bool {
        let Some(latched) = latched else {
            return false;
        };
        let held_already = |p: &Option<Prepared>| p.as_ref().is_some_and(|p| p.ptr_eq(latched));
        if held_already(&self.current) || held_already(&self.next) {
            return false;
        }
        // An equal value under a new reference (a Swap re-prepared the same text) changes nothing
        // musically; holding the new reference keeps the identity test above cheap.
        let target = if self.next.is_some() {
            &mut self.next
        } else {
            &mut self.current
        };
        if target.as_ref() == Some(latched) {
            retire(io, target.replace(latched.clone()));
            return false;
        }
        let playing = self
            .current
            .as_ref()
            .is_some_and(|p| !p.source().trim().is_empty());
        if playing {
            retire(io, self.next.replace(latched.clone()));
            false
        } else {
            retire(io, self.current.replace(latched.clone()));
            retire(io, self.next.take());
            true
        }
    }

    /// Make the waiting pattern current, retiring the one it replaces.
    fn promote(&mut self, io: &Io) {
        if let Some(next) = self.next.take() {
            retire(io, self.current.replace(next));
        }
    }

    /// Query `cycle` of the current pattern into the cache, sorted by onset, with the cursor at the
    /// first note at or after `from` within the cycle.
    fn load(&mut self, cycle: i64, from: f64, mode: PatternMode, counts: &mut OperatorCounts) {
        self.cache.clear();
        self.cursor = 0;
        self.cached_cycle = Some(cycle);
        let Some(pattern) = self.current.as_ref().and_then(|p| p.payload::<Compiled>()) else {
            return;
        };
        let res = pattern.cycle(cycle, SEED, &mut self.query);
        if res.dropped > 0 || res.overflowed > 0 || res.exhausted {
            counts.pattern_queries_truncated += 1;
        }
        for note in self.query.iter().take(res.written) {
            let Some(fire) = read_atom(note.value, mode) else {
                counts.pattern_atoms_skipped += 1;
                continue;
            };
            let cached = Cached {
                onset: note.onset.to_f64(),
                release: Pos::offset(cycle, note.release),
                fire,
            };
            // Insertion sort: stable, so simultaneous notes keep the pattern's order, and in place.
            let at = self.cache.partition_point(|c| c.onset <= cached.onset);
            self.cache.insert(at, cached);
        }
        self.cursor = self.cache.partition_point(|c| c.onset < from);
    }

    /// Release every held note whose release `pos` is at or past, in firing order.
    fn release_due(&mut self, io: &mut Io, f: usize, pos: Pos) {
        if pos < self.next_release {
            return;
        }
        let mut i = 0;
        while i < self.held.len() {
            if self.held[i].release <= pos {
                let h = self.held.remove(i);
                io.write(OUT_NOTES).emit(f, Note::new(h.pitch, 0.0));
            } else {
                i += 1;
            }
        }
        self.refresh_next_release();
    }

    fn release_all(&mut self, io: &mut Io, f: usize) {
        for h in self.held.drain(..) {
            io.write(OUT_NOTES).emit(f, Note::new(h.pitch, 0.0));
        }
        self.next_release = Pos::NEVER;
    }

    fn refresh_next_release(&mut self) {
        self.next_release = self
            .held
            .iter()
            .map(|h| h.release)
            .fold(Pos::NEVER, |a, b| if b < a { b } else { a });
    }

    /// Fire cached notes from the cursor while their onset is at or before `upto`.
    fn fire_upto(&mut self, io: &mut Io, f: usize, upto: f64, counts: &mut OperatorCounts) {
        while let Some(&c) = self.cache.get(self.cursor) {
            if c.onset > upto {
                break;
            }
            self.cursor += 1;
            match c.fire {
                Fire::Value(v) => {
                    io.write(OUT_VALUE).set(f, v);
                    self.value = Some(v);
                }
                Fire::Note(pitch) => {
                    if self.held.len() == HELD_CAP {
                        let oldest = self.held.remove(0);
                        io.write(OUT_NOTES).emit(f, Note::new(oldest.pitch, 0.0));
                        counts.pattern_notes_evicted += 1;
                    }
                    io.write(OUT_NOTES).emit(f, Note::new(pitch, 1.0));
                    self.held.push(Held {
                        release: c.release,
                        pitch,
                    });
                    if c.release < self.next_release {
                        self.next_release = c.release;
                    } else {
                        self.refresh_next_release();
                    }
                }
            }
        }
    }
}

/// Read one atom the way `mode` does, or `None` if it cannot.
fn read_atom(value: Value, mode: PatternMode) -> Option<Fire> {
    match (mode, value) {
        (PatternMode::Degree, Value::Number(r)) if r.denom() == 1 => i32::try_from(r.numer())
            .ok()
            .map(|d| Fire::Note(Pitch::Degree(d))),
        (PatternMode::Midi, Value::Number(r)) => {
            Some(Fire::Note(Pitch::Absolute(r.to_f64() as f32)))
        }
        (PatternMode::Midi, Value::Note(m)) => Some(Fire::Note(Pitch::Absolute(f32::from(m)))),
        (PatternMode::Value, Value::Number(r)) => Some(Fire::Value(r.to_f64() as f32)),
        _ => None,
    }
}

/// Hand a pattern the Operator lets go of to the retire queue, so the render thread frees nothing.
fn retire(io: &Io, p: Option<Prepared>) {
    if let Some(p) = p {
        io.retire(Arg::Prepared(p));
    }
}

impl Operator for Pattern {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let mut counts = OperatorCounts::default();
        let mode = io.read(IN_MODE);
        let bpc = io.read(IN_BEATS_PER_CYCLE).max(1);
        let beat = io.read(IN_BEAT);

        if self.announce {
            if let Some(v) = self.value {
                io.write(OUT_VALUE).set(0, v);
            }
            self.announce = false;
        }

        let mut seek = self.adopt(io, io.read(IN_PAT));
        if self.mode.is_some_and(|m| m != mode)
            || (self.mode.is_some() && self.beats_per_cycle != bpc)
        {
            self.release_all(io, 0);
            seek = true;
        }
        self.mode = Some(mode);
        self.beats_per_cycle = bpc;

        let cycle = i64::from(beat.div_euclid(bpc));
        let base = f64::from(beat.rem_euclid(bpc));
        let per_cycle = f64::from(bpc);
        let phase = io.read(IN_PHASE);
        for f in 0..n {
            let p = phase.get(f).copied().unwrap_or(0.0);
            // NaN fails `>=` and reads as 0, where `clamp` would pass it through.
            let p = if p >= 0.0 { p.min(PHASE_CEIL) } else { 0.0 };
            let pos = Pos {
                cycle,
                frac: (base + f64::from(p)) / per_cycle,
            };
            let jumped = self
                .prev
                .is_some_and(|prev| pos < prev || pos.cycle > prev.cycle.saturating_add(1));
            if jumped {
                self.release_all(io, f);
                seek = true;
            }
            if seek || self.cached_cycle.is_none() {
                self.promote(io);
                self.load(pos.cycle, pos.frac, mode, &mut counts);
                seek = false;
            } else if self.cached_cycle != Some(pos.cycle) {
                // The previous cycle's notes the clock passed without a frame landing on them fire
                // here, the first frame at or past their onset.
                self.release_due(io, f, pos);
                self.fire_upto(io, f, f64::INFINITY, &mut counts);
                self.promote(io);
                self.load(pos.cycle, 0.0, mode, &mut counts);
            }
            self.release_due(io, f, pos);
            self.fire_upto(io, f, pos.frac, &mut counts);
            self.prev = Some(pos);
        }
        *io.counts() += counts;
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }

    fn on_transplant(&mut self) {
        self.announce = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AudioConfig;
    use crate::graph::Graph;
    use crate::message::Message;
    use crate::operators::clock::{OUT_BEAT, OUT_PHASE};
    use crate::operators::{Clock, OscOut};
    use crate::plan::Plan;
    use crate::render::Renderer;
    use alloc::string::String;
    use alloc::sync::Arc;

    const SR: f32 = 48_000.0;
    /// A tempo whose beat advance per frame, `3 / 2^16`, is exact in binary, so the Clock's phase
    /// is exact and a position `b` beats in lands on frame `ceil(b · 2^16 / 3)`: off every block
    /// boundary the tests use.
    const TEMPO: f32 = 8_640_000.0 / 65_536.0;
    /// A tempo of exactly 2^14 frames per beat: every beat starts on a block edge.
    const EDGE_TEMPO: f32 = 2_880_000.0 / 16_384.0;

    /// The first frame at or past `n / d` beats at [`TEMPO`]: `ceil(n/d · 65536 / 3)`.
    fn at(n: i64, d: i64) -> usize {
        let num = i128::from(n) * 65_536;
        let den = i128::from(d) * 3;
        usize::try_from((num + den - 1).div_euclid(den)).unwrap()
    }

    /// The first frame of beat `n` at [`TEMPO`].
    fn b(n: i64) -> usize {
        at(n, 1)
    }

    fn prepared(text: &str) -> Arg {
        let desc = Pattern::contract();
        let preparer = desc.inputs[IN_PAT.index()]
            .preparer()
            .expect("`pat` prepares its text");
        Arg::Prepared(preparer.prepare(&Arc::<str>::from(text)).expect("compiles"))
    }

    fn sync() -> Arg {
        Arg::Note(Note::new(Pitch::Degree(0), 1.0))
    }

    /// Clock → pattern → two `osc_out` sinks (`/notes`, `/value`), rendered in `block`-frame
    /// blocks.
    struct Rig {
        plan: Plan,
        renderer: Renderer,
        block: usize,
        pattern: usize,
        at: usize,
    }

    /// One emitted event: (global frame, sink address, payload).
    type Ev = (usize, String, Arg);

    fn rig(text: &str, tempo: f32, bpc: i32, mode: PatternMode, block: usize) -> Rig {
        let mut g = Graph::new();
        let clk = g.add("/clk", Clock::new());
        g.set_value(clk, "tempo", &Arg::F32(tempo));
        let pat = g.add("/pat", Pattern::new());
        g.set_value(pat, "pat", &Arg::Str(text.into()));
        g.set_value(pat, "beats_per_cycle", &Arg::I32(bpc));
        g.set_value(pat, "mode", &mode.into());
        g.connect(clk, OUT_PHASE, pat, IN_PHASE);
        g.connect(clk, OUT_BEAT, pat, IN_BEAT);
        let notes = g.add("/notes", OscOut::new());
        g.connect(pat, OUT_NOTES, notes, 0usize);
        let value = g.add("/value", OscOut::new());
        g.connect(pat, OUT_VALUE, value, 0usize);
        let plan = Plan::instantiate(g, AudioConfig::new(SR, block)).expect("instantiates");
        let pattern = (plan.nodes.iter())
            .position(|n| &*n.address == "/pat")
            .expect("the pattern node");
        let renderer = Renderer::new(&plan);
        Rig {
            plan,
            renderer,
            block,
            pattern,
            at: 0,
        }
    }

    impl Rig {
        /// Render whole blocks until global frame `end` is reached, delivering each `(global
        /// frame, address, arg)` in `sends` at its frame. Returns every event the blocks emitted.
        fn run_to(&mut self, end: usize, sends: &[(usize, &str, Arg)]) -> Vec<Ev> {
            let mut out = Vec::new();
            let mut master: Vec<Vec<f32>> =
                vec![vec![0.0; self.block]; self.plan.config.channels];
            while self.at < end {
                let start = self.at;
                let msgs: Vec<Message> = (sends.iter())
                    .filter(|(f, _, _)| *f >= start && *f < start + self.block)
                    .map(|(f, a, arg)| Message::new(*a, arg.clone(), f - start))
                    .collect();
                let mut outbound = Vec::new();
                self.renderer.render_block_multi(
                    &mut self.plan,
                    &msgs,
                    &[],
                    &mut master,
                    &mut outbound,
                );
                out.extend(
                    (outbound.into_iter())
                        .map(|m| (start + m.frame, String::from(&*m.address), m.arg)),
                );
                self.at += self.block;
            }
            out
        }

        fn counts(&mut self) -> OperatorCounts {
            self.plan.take_operator_counts()
        }
    }

    type NoteEv = (usize, Pitch, bool);

    /// The `/notes` events before frame `end`.
    fn notes_before(evs: &[Ev], end: usize) -> Vec<NoteEv> {
        (evs.iter())
            .filter_map(|(f, a, arg)| match (a.as_str(), arg) {
                ("/notes", Arg::Note(n)) if *f < end => Some((*f, n.pitch, !n.is_off())),
                _ => None,
            })
            .collect()
    }

    /// The `/value` events before frame `end`.
    fn values_before(evs: &[Ev], end: usize) -> Vec<(usize, f32)> {
        (evs.iter())
            .filter_map(|(f, a, arg)| match (a.as_str(), arg) {
                ("/value", Arg::F32(v)) if *f < end => Some((*f, *v)),
                _ => None,
            })
            .collect()
    }

    fn deg(d: i32) -> Pitch {
        Pitch::Degree(d)
    }

    fn on(f: usize, d: i32) -> NoteEv {
        (f, deg(d), true)
    }

    fn off(f: usize, d: i32) -> NoteEv {
        (f, deg(d), false)
    }

    /// Every note-on before `cutoff` is matched by a later note-off of its pitch, and no note-off
    /// releases a note that is not sounding.
    fn assert_none_hang(evs: &[NoteEv], cutoff: usize) {
        let mut sounding: Vec<(usize, Pitch)> = Vec::new();
        for &(f, p, is_on) in evs {
            if is_on {
                sounding.push((f, p));
            } else {
                let i = (sounding.iter())
                    .position(|&(_, s)| s == p)
                    .unwrap_or_else(|| panic!("a note-off at {f} for {p:?}, which is not sounding"));
                sounding.remove(i);
            }
        }
        let hung: Vec<_> = sounding.iter().filter(|(f, _)| *f < cutoff).collect();
        assert!(hung.is_empty(), "hung notes: {hung:?}");
    }

    #[test]
    fn two_cycles_fire_onsets_and_releases_at_exact_frames() {
        let mut r = rig("0 3 <7 5> ~", TEMPO, 4, PatternMode::Degree, 128);
        let end = b(8) + 1;
        let got = notes_before(&r.run_to(end, &[]), end);
        assert_eq!(
            got,
            vec![
                on(0, 0),
                off(b(1), 0),
                on(b(1), 3),
                off(b(2), 3),
                on(b(2), 7),
                off(b(3), 7),
                on(b(4), 0),
                off(b(5), 0),
                on(b(5), 3),
                off(b(6), 3),
                on(b(6), 5),
                off(b(7), 5),
                on(b(8), 0),
            ]
        );
        assert!(!b(1).is_multiple_of(32), "the onsets fall off block boundaries");
    }

    fn performance(block: usize) -> Vec<Ev> {
        let mut r = rig("0 [3 5?] <7 [~ 2]> 1|4", TEMPO, 4, PatternMode::Degree, block);
        let mut evs = r.run_to(
            b(26),
            &[
                (at(5, 2), "/pat/pat", prepared("0/1.5 [3 5]*2")),
                (at(29, 2) + 5, "/clk/sync", sync()),
                (b(17) + 3, "/pat/mode", PatternMode::Midi.into()),
            ],
        );
        evs.retain(|e| e.0 < b(26));
        evs
    }

    #[test]
    fn output_is_identical_for_block_sizes_32_128_and_1024() {
        let a = performance(32);
        assert!(notes_before(&a, usize::MAX).len() > 20, "it plays: {a:?}");
        assert_eq!(a, performance(128));
        assert_eq!(a, performance(1024));
    }

    #[test]
    fn an_onset_exactly_on_a_block_edge_fires_once() {
        let mut r = rig("0 1 2 3", EDGE_TEMPO, 4, PatternMode::Degree, 128);
        let end = 4 * 16_384 + 1;
        let ons: Vec<_> = notes_before(&r.run_to(end, &[]), end)
            .into_iter()
            .filter(|e| e.2)
            .collect();
        assert_eq!(
            ons,
            vec![
                on(0, 0),
                on(16_384, 1),
                on(32_768, 2),
                on(49_152, 3),
                on(65_536, 0)
            ]
        );
        assert!(16_384usize.is_multiple_of(128), "every onset is a block's first frame");
    }

    #[test]
    fn a_note_releases_at_the_end_of_its_step_not_its_own_end() {
        // `[0 1]/1.5` fills the first half of the cycle: `1` starts at 3/8 and would hold to 3/4,
        // but its step ends at 1/2, where `2` starts.
        let mut r = rig("[0 1]/1.5 2", TEMPO, 4, PatternMode::Degree, 128);
        let end = b(4);
        let got = notes_before(&r.run_to(end, &[]), end);
        assert_eq!(
            got,
            vec![
                on(0, 0),
                off(at(3, 2), 0),
                on(at(3, 2), 1),
                off(b(2), 1),
                on(b(2), 2),
            ]
        );
    }

    #[test]
    fn a_release_past_the_cycle_end_fires_in_a_later_cycle() {
        // `0/1.5`: events at cycles 0, 1.5 and 3, each 1.5 cycles long. The one starting half way
        // through cycle 1 releases in cycle 3, where the next starts.
        let mut r = rig("0/1.5", TEMPO, 4, PatternMode::Degree, 128);
        let end = b(12) + 1;
        let got = notes_before(&r.run_to(end, &[]), end);
        assert_eq!(
            got,
            vec![
                on(0, 0),
                off(b(6), 0),
                on(b(6), 0),
                off(b(12), 0),
                on(b(12), 0),
            ]
        );
    }

    #[test]
    fn a_new_pattern_starts_at_the_next_cycle_and_the_old_notes_release_on_schedule() {
        let mut r = rig("0/1.5", TEMPO, 4, PatternMode::Degree, 128);
        let end = b(8) + 1;
        let got = notes_before(
            &r.run_to(end, &[(b(2) + 7, "/pat/pat", prepared("5 6"))]),
            end,
        );
        assert_eq!(
            got,
            vec![
                // The old pattern plays out cycle 0; its note holds to 1.5 cycles.
                on(0, 0),
                // Cycle 1 is the new pattern's.
                on(b(4), 5),
                // At beat 6 the new pattern's `5` ends and the old note reaches its own release,
                // both before the onset at the same frame.
                off(b(6), 0),
                off(b(6), 5),
                on(b(6), 6),
                off(b(8), 6),
                on(b(8), 5),
            ]
        );
    }

    #[test]
    fn a_pattern_sent_while_nothing_plays_starts_at_once() {
        let mut r = rig("", TEMPO, 4, PatternMode::Degree, 128);
        let end = b(4);
        let got = notes_before(
            &r.run_to(end, &[(at(3, 2), "/pat/pat", prepared("0 1 2 3"))]),
            end,
        );
        assert_eq!(got, vec![on(b(2), 2), off(b(3), 2), on(b(3), 3)]);
    }

    #[test]
    fn a_clock_sync_restarts_from_cycle_zero_with_no_hung_notes() {
        let mut r = rig("0 1 2 3", TEMPO, 4, PatternMode::Degree, 128);
        let s = at(11, 2) + 3; // mid `1`, in cycle 1
        let after = |n| s + b(n);
        let end = after(4) + 1;
        let got = notes_before(&r.run_to(end, &[(s, "/clk/sync", sync())]), end);
        let before: Vec<_> = got.iter().filter(|e| e.0 < s).copied().collect();
        assert_eq!(before.last(), Some(&on(b(5), 1)));
        let since: Vec<_> = got.iter().filter(|e| e.0 >= s).copied().collect();
        assert_eq!(
            since,
            vec![
                off(s, 1),
                on(s, 0),
                off(after(1), 0),
                on(after(1), 1),
                off(after(2), 1),
                on(after(2), 2),
                off(after(3), 2),
                on(after(3), 3),
                off(after(4), 3),
                on(after(4), 0),
            ]
        );
        assert_none_hang(&got, after(4));
    }

    #[test]
    fn value_mode_drives_a_held_output_that_changes_at_the_cycle_boundary_frame() {
        let mut r = rig("<300 2000>", TEMPO, 4, PatternMode::Value, 128);
        let end = b(12) + 1;
        let evs = r.run_to(end, &[]);
        assert!(notes_before(&evs, end).is_empty(), "value mode emits no notes");
        assert_eq!(
            values_before(&evs, end),
            vec![(0, 300.0), (b(4), 2000.0), (b(8), 300.0), (b(12), 2000.0)]
        );
    }

    #[test]
    fn a_rest_holds_the_last_value() {
        let mut r = rig("5 ~ 7 ~", TEMPO, 4, PatternMode::Value, 128);
        let end = b(4);
        let evs = r.run_to(end, &[]);
        assert_eq!(values_before(&evs, end), vec![(0, 5.0), (b(2), 7.0)]);
    }

    #[test]
    fn a_query_overflow_truncates_counts_and_hangs_nothing() {
        let mut r = rig("0*128", TEMPO, 4, PatternMode::Degree, 128);
        // Short of cycle 2, whose first block would query it.
        let end = b(8) - 128;
        let got = notes_before(&r.run_to(end, &[]), end);
        let ons = got.iter().filter(|e| e.2).count();
        assert_eq!(ons, 2 * CYCLE_CAP, "each cycle plays its first {CYCLE_CAP}");
        assert_eq!(r.counts().pattern_queries_truncated, 2);
        assert_none_hang(&got, end);
    }

    #[test]
    fn a_held_list_overflow_evicts_the_oldest_counts_and_hangs_nothing() {
        let layers: Vec<String> = (0..40).map(|d| alloc::format!("{d}")).collect();
        let mut r = rig(&layers.join(", "), TEMPO, 4, PatternMode::Degree, 128);
        let end = b(4) + 1;
        let got = notes_before(&r.run_to(end, &[]), end);
        let first: Vec<_> = got.iter().filter(|e| e.0 == 0).copied().collect();
        assert_eq!(first.iter().filter(|e| e.2).count(), 40);
        let evicted: Vec<_> = first.iter().filter(|e| !e.2).map(|e| e.1).collect();
        assert_eq!(
            evicted,
            (0..8).map(deg).collect::<Vec<_>>(),
            "the oldest go first"
        );
        // Cycle 1 evicts eight more at its first frame.
        assert_eq!(r.counts().pattern_notes_evicted, 16);
        assert_none_hang(&got, b(4));
    }

    #[test]
    fn an_atom_the_mode_cannot_read_is_skipped_and_counted() {
        let mut r = rig("c4 1.5 2", TEMPO, 3, PatternMode::Degree, 128);
        let end = b(3);
        let got = notes_before(&r.run_to(end, &[]), end);
        assert_eq!(got, vec![on(b(2), 2)]);
        assert_eq!(r.counts().pattern_atoms_skipped, 2);

        let mut r = rig("c4 61.5", TEMPO, 2, PatternMode::Midi, 128);
        let end = b(1) + 1;
        let got = notes_before(&r.run_to(end, &[]), end);
        assert_eq!(
            got,
            vec![
                (0, Pitch::Absolute(60.0), true),
                (b(1), Pitch::Absolute(60.0), false),
                (b(1), Pitch::Absolute(61.5), true),
            ]
        );
        assert_eq!(r.counts().pattern_atoms_skipped, 0);
    }

    #[test]
    fn a_mode_change_releases_held_notes() {
        let mut r = rig("0@4", TEMPO, 4, PatternMode::Degree, 128);
        let change = b(1) + 9;
        let end = b(4) + 1;
        let got = notes_before(
            &r.run_to(end, &[(change, "/pat/mode", PatternMode::Midi.into())]),
            end,
        );
        assert_eq!(
            got,
            vec![
                on(0, 0),
                off(change, 0),
                (b(4), Pitch::Absolute(0.0), true),
            ]
        );
    }

    #[test]
    fn value_is_reannounced_after_a_transplant() {
        let mut r = rig("<300 2000>", TEMPO, 4, PatternMode::Value, 128);
        r.run_to(256, &[]);
        let p = r.pattern;
        r.plan.nodes[p].op.on_transplant();
        let start = r.at;
        let evs = r.run_to(start + 128, &[]);
        assert_eq!(values_before(&evs, usize::MAX), vec![(start, 300.0)]);
        let evs = r.run_to(start + 256, &[]);
        assert!(values_before(&evs, usize::MAX).is_empty(), "then nothing repeats");
    }

    #[test]
    fn a_bad_pattern_is_refused_with_its_span() {
        let err = compile("0 [3").unwrap_err();
        assert!(err.span.end <= 4, "{err}");
        assert!(compile("").is_ok(), "empty text is silence");
    }

    #[test]
    fn a_spawned_copy_starts_fresh() {
        let mut r = rig("0 1 2 3", TEMPO, 4, PatternMode::Degree, 128);
        r.run_to(b(1) + 1, &[]);
        let p = r.pattern;
        let fresh = r.plan.nodes[p].op.spawn();
        let mut d = crate::op_driver::OpDriver::from_boxed(fresh, Arc::new(Pattern::contract()), SR);
        d.set(IN_PAT, prepared("4"));
        let emits = d.render(128).emits().to_vec();
        assert_eq!(emits.len(), 1, "the copy seeks from its own first frame: {emits:?}");
        assert_eq!(emits[0].frame, 0);
    }
}
