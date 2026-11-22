//! reuben-pattern — the mini-notation pattern language.
//!
//! A text pattern such as `"0 [3 5] <7 ~> 2*2"` is [`parse`]d to an [`Ast`], [`compile`]d to a
//! [`Pattern`], and then [`Pattern::query`]'d for the events in a span of time. Parse and compile
//! allocate and belong off the Render thread; the query allocates nothing and writes into a buffer
//! the caller owns.
//!
//! ```
//! use reuben_pattern::{Event, Pattern, Rational, Span};
//!
//! let pat = Pattern::new("0 [3 5]").unwrap();
//! let mut buf = [Event::EMPTY; 16];
//! let res = pat.query(Span::new(Rational::ZERO, Rational::ONE), 0, &mut buf);
//! assert_eq!((res.written, res.dropped), (3, 0));
//! ```
//!
//! # Time
//!
//! Time is measured in cycles and is always an exact [`Rational`] (`i64` over `i64`, kept in
//! lowest terms), never floating point. The query's span is in cycles; the caller converts its
//! clock (beats, frames) to cycles exactly. Integer cycle numbers are what `< >`, `?` and `|` key
//! on, so the cycle position is an input to every query, not state the pattern keeps.
//!
//! Overflow: arithmetic widens to `i128` and fails cleanly when a reduced result does not fit
//! `i64`. In the query a failure abandons that sub-query and counts it in
//! [`QueryResult::overflowed`]; nothing wraps and nothing panics. Compilation keeps every node's
//! cumulative time scale within [`MAX_SCALE`], which keeps realistic spans far from the limit: a
//! year of cycles at frame resolution (denominators up to 176 400) through patterns at the scale
//! cap reports no overflow, and a test holds that.
//!
//! # Determinism
//!
//! There is no random state. `?` removes an event when a hash of (seed, the `?` node's structural
//! id, the event's onset) falls below its probability; `|` picks a branch by a hash of (seed, the
//! `|` node's structural id, the cycle number). Structural ids are assigned in source order at
//! compile time, so the same text, span and seed give the same events on every run and platform.
//! The *distribution* matches the reference docs (probability `p`, equal-weight choice), but the
//! particular outcomes differ from Tidal's and Strudel's.
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
//! Limits, each an error with a span: bracket nesting beyond [`MAX_NESTING`], more than
//! [`MAX_MODIFIERS`] wrapping modifiers on one step, `!`, `..` and euclid expansions beyond
//! [`MAX_EXPANSION`] steps, compiled depth beyond [`MAX_DEPTH`], and time scale beyond
//! [`MAX_SCALE`].
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
//! - **Note names and octaves**: Tidal numbers octaves so that `c5` is middle C; Strudel uses
//!   scientific pitch, `c4` is middle C. This crate uses scientific pitch: [`Value::Note`] is the
//!   MIDI note number, `c4` = 60. A name with no octave takes [`DEFAULT_OCTAVE`] (4), so a bare
//!   `c` is middle C, which is also where an octave-less note lands in Tidal.
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
//!   Operator only ever calls [`Pattern::query`].
//!
//! # Cost
//!
//! The query is a tree walk over a flat node arena: no string hashing, no trait objects, no
//! per-event allocation. Its cost is linear in the events produced plus the cycles the span
//! touches at each node, so query block-sized spans, not hours.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod ast;
mod compile;
mod error;
mod parse;
mod query;
mod time;

use alloc::vec::Vec;

pub use ast::{Ast, Expr, ExprKind, Step};
pub use compile::{compile, MAX_DEPTH, MAX_SCALE};
pub use error::Error;
pub use parse::{parse, DEFAULT_OCTAVE, MAX_EXPANSION, MAX_MODIFIERS, MAX_NESTING};
pub use query::{Event, QueryResult};
pub use time::{Rational, Span};

/// The value an event carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Value {
    /// A number atom, exact as written.
    Number(Rational),
    /// A note-name atom as a MIDI note number (`c4` = 60). Distinct from [`Value::Number`] so a
    /// consumer can accept note names only where it means pitches.
    Note(u8),
}

/// A compiled pattern, ready to [`query`](Pattern::query).
///
/// Immutable after compilation; queries take `&self`, so one pattern can be queried from any
/// thread. Dropping it frees its arena, so drop it off the Render thread.
#[derive(Clone, Debug)]
pub struct Pattern {
    nodes: Vec<compile::Node>,
    steps: Vec<compile::SeqStep>,
    children: Vec<u32>,
    root: u32,
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
        }
    }
}
