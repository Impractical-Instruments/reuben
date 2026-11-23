//! reuben-pattern — the mini-notation pattern language.
//!
//! A text pattern such as `"0 [3 5] <7 ~> 2*2"` is [`parse`]d to an [`Ast`], [`compile`]d to a
//! [`Pattern`], and then asked, one integer cycle at a time, for the notes that start in that cycle
//! ([`Pattern::cycle`]). Parse and compile allocate and belong off the Render thread; the cycle
//! query allocates nothing and writes into a buffer the caller owns.
//!
//! ```
//! use reuben_pattern::{Note, Pattern, Rational};
//!
//! let pat = Pattern::new("0 [3 5]").unwrap();
//! let mut buf = [Note::EMPTY; 16];
//! let res = pat.cycle(0, 0, &mut buf);
//! assert_eq!(res.written, 3);
//! assert_eq!(buf[1].onset, Rational::new(1, 2).unwrap());
//! assert_eq!(buf[1].release, Rational::new(3, 4).unwrap());
//! ```
//!
//! The text syntax follows Tidal's and Strudel's mini-notation. The engine underneath does not:
//! it is shaped around reuben's Clock, and matching their internals is not a goal.
//!
//! # Playing a pattern from a clock
//!
//! The Clock gives a `phase` that wraps every beat and an integer `beat` count. `beat` is the
//! Clock's `i32` output added by the companion change for cycle position; it is not on `dev`
//! until that lands. The quantity that only rises between syncs is `beat + phase`, and a consumer
//! (the `pattern` Operator) turns it into a cycle position, `(beat + phase) / beats_per_cycle`.
//! Then:
//!
//! 1. When the position enters a new cycle `n`, it calls [`Pattern::cycle`] for `n` once and
//!    caches the notes it returns.
//! 2. It converts each cached note's [`onset`](Note::onset) and [`release`](Note::release) to
//!    absolute positions once: `n + onset` and `n + release`, in the clock's units.
//! 3. Each frame, it first releases every held note whose release position the clock is at or
//!    past, then fires every cached note whose onset it is at or past. Both use the same "at or
//!    past" test, and releases go first because a release can equal the next onset: in `0 0` or
//!    `0*4`, each note releases exactly where the next starts.
//! 4. A fired note moves from the cache to a held-notes list with its absolute release
//!    position, because a release can lie beyond the cycle and outlive the cache: in `0/1.5`, the
//!    note starting half way through cycle 1 releases two cycles later, at `1 + 2`. The held list
//!    has a fixed capacity; when it is full, firing a note first releases the oldest held note.
//!    Replacing the cache for the next cycle leaves the held list alone.
//! 5. It keeps the previous frame's position across blocks, so each note fires exactly once
//!    however blocks fall. A backward jump (`sync`) is a locate: release every held note, then
//!    re-query from the new position, where a note whose onset lies exactly at the new position
//!    fires.
//!
//! # Time
//!
//! Inside a cycle, time is always exact: fractions of the cycle as [`Rational`] (`i64` over `i64`,
//! in lowest terms), never floating point. Integer cycle numbers are what `< >`, `?` and `|` key
//! on, so the cycle is an input to every query, not state the pattern keeps.
//!
//! The query carries time as an integer cycle plus a fraction. The cycle number only meets integers
//! and the pattern's own ratios, through 128-bit arithmetic; fractions stay over the pattern's
//! denominators, bounded at compile time by [`MAX_SCALE`], [`MAX_GROWTH`] and [`MAX_GRID`]. So any
//! cycle within [`MAX_CYCLE`] (2^31) of zero gives exact results; a property test checks cycles
//! across that range. Further out a time may not fit, and the query counts it in
//! [`CycleResult::overflowed`] rather than wrapping or panicking.
//!
//! # Determinism
//!
//! There is no random state. `?` removes a note when a hash of (seed, the `?` node's structural
//! id, the note's structural path, its onset) falls below its probability. `|` picks a branch by
//! a hash of (seed, the `|` node's structural id, its structural path, the cycle number).
//! Structural ids are assigned in source order at compile time. The path names which sequence slot
//! and stack layer led to the node, so the copies made by `!`, the pulses of a euclid and the
//! layers of a stack draw independently: `[0|1]!2` behaves like `[0|1] [0|1]`. The same text,
//! cycle and seed give the same notes on every run and platform. The *distribution* matches the
//! reference docs (probability `p`, equal-weight choice), but the particular outcomes differ from
//! Tidal's and Strudel's.
//!
//! # Notes
//!
//! A note belongs to the cycle its onset lies in, and only that cycle reports it, even when a
//! slowed pattern makes it sound into the next. Its release is its event's end cut to the end of
//! its enclosing sequence step, so a step never sounds into the next one (unlike the Tidal
//! reference, where an event holds for its whole extent). See [`Note`].
//!
//! # Syntax (v1)
//!
//! | Syntax | Meaning |
//! | --- | --- |
//! | `3`, `-1.5`, `4%3` | number atom, exact; `%` writes a ratio |
//! | `c4`, `eb3`, `f#2`, `cs` | note atom ([`Value::Note`]), a MIDI note number |
//! | `~`, `-` | rest |
//! | `a b c` | sequence: steps share the cycle |
//! | `[a b]` | subsequence in one step |
//! | `a b . c d e` | `.` groups: same as `[a b] [c d e]` |
//! | `<a b c>` | one step per cycle, in turn |
//! | `a*2`, `a/2` | play the step 2× faster / over 2 cycles |
//! | `a!3`, `a ! !` | replicate the step: `a a a` |
//! | `a@3`, `a _ _` | elongate: weight 3 relative to its neighbours |
//! | `a, b` | stack: play together |
//! | `{a b c, d e}`, `{a b c}%4` | polymeter: every layer steps at the first layer's rate, or `%n` steps per cycle |
//! | `a?`, `a?0.3` | remove each event with probability 1/2, or 0.3 |
//! | `a \| b \| c` | pick one per cycle |
//! | `a(3,8)`, `a(3,8,2)` | euclidean rhythm: 3 pulses in 8 steps, rotated left 2 |
//! | `0 .. 3` | inclusive integer range: `0 1 2 3` |
//!
//! Not in v1, each a clear error rather than silent acceptance: `:` sample index, patterned
//! modifier arguments (`a*<2 3>`, `a(3,8,<0 2>)`), and any word that is not a note name.
//!
//! Steps are separated by whitespace: `1-2`, `0~1` and `[0][1]` are errors rather than two steps.
//! A decimal needs its leading zero (`?0.5`, not `?.5`).
//!
//! Limits, each an error with a span:
//! - bracket nesting beyond [`MAX_NESTING`];
//! - more than [`MAX_MODIFIERS`] wrapping modifiers on one step;
//! - `!`, `..` and euclid expansions beyond [`MAX_EXPANSION`] steps;
//! - compiled depth beyond [`MAX_DEPTH`];
//! - time bounds beyond [`MAX_SCALE`], [`MAX_GROWTH`] or [`MAX_GRID`];
//! - a worst-case cycle query cost beyond [`MAX_WORK`];
//! - text longer than [`MAX_INPUT`];
//! - a compiled size beyond [`MAX_SIZE`].
//!
//! # Where the references disagree, and what this crate does
//!
//! Written clean-room from the prose and examples of the Tidal mini-notation reference
//! (tidalcycles.org/docs/reference/mini_notation) and Strudel's mini-notation guide
//! (strudel.cc/learn/mini-notation). Neither is a formal grammar; these are the calls made.
//!
//! - **`-` as a rest**: Strudel only. Adopted; `-` immediately followed by a digit is a negative
//!   number instead.
//! - **Weights inside `< >`**: only Strudel shows them (`<a@2 b c>`, `<a _ b c>`). Adopted with its
//!   stated meaning, that `< >` fixes the length to the number of (weighted) steps: `<a@2 b>` is
//!   `[a@2 b]/3`, so `a` holds for two cycles. Nested `< >` alternate on their own count:
//!   `<a <b c>>` plays `a b a c`.
//! - **Top-level `,` and `|`**: Tidal shows both inside `[ ]`; Strudel shows both bare. Both are
//!   accepted at any level. Mixing `,` and `|` in one group is an error, since neither reference
//!   gives a precedence; bracket one side.
//! - **`.` grouping, `{ }` polymeter, `%`**: Tidal only. Tidal's meaning is used: the first layer
//!   of `{ }` sets the step rate, and `%n` overrides it.
//! - **Euclid rotation**: Tidal says the third argument shifts the pattern left; Strudel calls it
//!   the starting position. These agree; `(3,8,1)` is `..x..x.x`, matching Tidal's printed grid.
//!   The pulse placement is Bjorklund's algorithm, which reproduces every grid Tidal prints.
//!   Negative pulse counts (which neither page documents) are an error.
//! - **`..` ranges**: documented on neither page. Implemented as an inclusive range between two
//!   integer literals that expands into separate steps, ascending or descending.
//! - **Bare `!`**: neither page documents it. A `!` with no count, attached or standalone, adds one
//!   more copy of the previous step, so `a!`, `a !` and `a!2` are all `a a`.
//! - **Note names and octaves**: neither page fixes an octave numbering. This crate uses scientific
//!   pitch notation: [`Value::Note`] is the MIDI note number, `c4` = 60, and `c-1` = 0 is the
//!   lowest. A name with no octave takes [`DEFAULT_OCTAVE`] (4), so a bare `c` is middle C.
//! - **Modifier order**: neither page says. Speed, `?` and euclid wrap the step in the order
//!   written (`a*2?` degrades the sped-up step); `@` and `!` are properties of the step and apply
//!   wherever they appear among its modifiers. A modifier must touch its step: `a *2` is an error.
//!
//! # Room for a function layer
//!
//! v1 has no functions (`fast`, `every`, ...). The plan for adding a closed set later without
//! breaking any mini-notation string:
//!
//! - **`"` is reserved.** No v1 pattern contains a double quote; one is a parse error. A future
//!   expression grammar quotes its mini-notation (`fast 2 "0 3"`, `every 3 (fast 2) "0 3"`), so an
//!   input containing `"` is an expression and anything else is plain mini-notation, unchanged.
//! - **Functions compile to the same nodes.** The compiled tree already has the time-transforming
//!   nodes such functions need (`fast`/`slow` are the speed node, `degradeBy` the `?` node, `stack`
//!   and `cat` the stack and sequence nodes), so the query does not change shape; new functions
//!   that need new behaviour add node kinds to the same walk.
//! - **The query is the contract.** A function layer produces a [`Pattern`]; the `pattern`
//!   Operator only ever calls [`Pattern::cycle`].
//!
//! # Cost
//!
//! The query is a tree walk over a flat node arena: no string hashing, no trait objects, no
//! per-note allocation. Compilation bounds the work of one cycle query
//! ([`Pattern::max_cycle_work`]), counting every node visit (rests, empty sequences, wrappers),
//! every cycle and step the walk iterates, and every `?` filter each onset is tested against. A
//! pattern whose bound exceeds [`MAX_WORK`] does not compile, so a compiled pattern's cycle query
//! never sets [`CycleResult::exhausted`]; [`QUERY_BUDGET`] is only a backstop.
//!
//! The cost to plan for is one cycle query each time the clock enters a new cycle. Block size is
//! not bounded, but under the Clock's cycle-position design `beat` is a held input that slices a
//! block at each beat change, and with an integer `beats_per_cycle` of at least 1 each slice enters
//! at most one new cycle. So a slice makes at most one cycle query.
//!
//! In time: `examples/query_cost.rs`, pinned to one core of an i7-13620H in release, measured the
//! worst unit at about 115 ns (145 ns in the noisiest run), over patterns near the cap that include
//! deep `?` chains and fractional speeds, at cycles across the guaranteed range. One cycle query is
//! then at most about 19 us (23 us): 1.4% (1.7%) of a 64-frame block at 48 kHz. As an
//! illustration, four queries in one block would be about 74 us (93 us), 5.6% (7%) of it. 128-bit
//! division on a Cortex-M is not measured.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod ast;
mod compile;
mod cost;
mod error;
mod parse;
mod query;
mod time;

use alloc::vec::Vec;

pub use ast::{Ast, Expr, ExprKind, Step};
pub use compile::{compile, MAX_DEPTH, MAX_GRID, MAX_GROWTH, MAX_SCALE, MAX_SIZE, MAX_WORK};
pub use error::Error;
pub use parse::{parse, DEFAULT_OCTAVE, MAX_EXPANSION, MAX_INPUT, MAX_MODIFIERS, MAX_NESTING};
pub use query::{CycleResult, Note, MAX_CYCLE, QUERY_BUDGET};
pub use time::Rational;

/// The value a note carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Value {
    /// A number atom, exact as written.
    Number(Rational),
    /// A note-name atom as a MIDI note number (`c4` = 60). Distinct from [`Value::Number`] so a
    /// consumer can accept note names only where it means pitches.
    Note(u8),
}

/// A compiled pattern, ready to query one [`cycle`](Pattern::cycle) at a time.
///
/// Immutable after compilation; queries take `&self`, so one pattern can be queried from any
/// thread. Dropping it frees its arena, so drop it off the Render thread.
#[derive(Clone, Debug)]
pub struct Pattern {
    nodes: Vec<compile::Node>,
    steps: Vec<compile::SeqStep>,
    children: Vec<u32>,
    root: u32,
    /// The compile-time bound on one cycle query's work.
    max_work: u32,
}

impl Pattern {
    /// Parse and compile `src` in one step.
    pub fn new(src: &str) -> Result<Pattern, Error> {
        compile(&parse(src)?)
    }

    /// The pattern with no events.
    pub fn silence() -> Pattern {
        Pattern {
            nodes: alloc::vec![compile::Node::Silence],
            steps: Vec::new(),
            children: Vec::new(),
            root: 0,
            max_work: 1,
        }
    }

    /// The most units of work [`Pattern::cycle`] can spend, for any cycle and seed: a bound
    /// proved at compile time. At most [`MAX_WORK`], below [`QUERY_BUDGET`].
    pub fn max_cycle_work(&self) -> u32 {
        self.max_work
    }
}
