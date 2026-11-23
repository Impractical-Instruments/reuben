//! The parsed form of a mini-notation string, before compilation.
//!
//! Sugar that only rearranges steps (`!` replication, `_` elongation, `..` ranges, `.` grouping)
//! is already resolved here: a [`Step`] carries its weight and copy count, a range is its expanded
//! steps, and a `.`-grouped layer is a sequence of sub-sequences. Everything that changes time or
//! needs a structural random seed stays a node, so compilation is a straight translation.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ops::Range;

use crate::time::Rational;

/// A parsed pattern: the root expression of the whole string.
#[derive(Clone, Debug, PartialEq)]
pub struct Ast {
    /// The top-level body. An empty or whitespace-only string parses to [`ExprKind::Rest`].
    pub root: Expr,
}

/// One node of the parse tree, with the source bytes it came from.
#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    /// What the node is.
    pub kind: ExprKind,
    /// Byte range in the source text.
    pub span: Range<usize>,
}

/// One step of a sequence, alternation, or polymeter layer.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    /// The step's content.
    pub expr: Expr,
    /// Relative duration: 1 by default, set by `@w`, plus 1 for each following `_`.
    pub weight: Rational,
    /// How many consecutive copies the step occupies (`!`): 1 by default.
    pub copies: u32,
}

/// The kinds of parse-tree node.
#[derive(Clone, Debug, PartialEq)]
pub enum ExprKind {
    /// A number literal, exact (`3`, `-1.5`, `4%3`).
    Number(Rational),
    /// A note name resolved to a MIDI note number (`c4` → 60). Kept distinct from
    /// [`ExprKind::Number`] so a consumer can refuse note names where it expects plain values.
    Note(u8),
    /// `~` or `-`: a step with no event.
    Rest,
    /// Steps sharing one cycle in proportion to their weights (`a b c`, `[a b]`).
    Seq(Vec<Step>),
    /// Layers playing at once (`a, b`).
    Stack(Vec<Expr>),
    /// One layer per cycle, chosen by a deterministic hash (`a | b`).
    Choice(Vec<Expr>),
    /// One step per cycle, in turn (`<a b c>`). Weighted steps hold for `weight` cycles.
    Alt(Vec<Step>),
    /// Polymeter (`{a b c, d e}%n`): every layer advances `steps` steps per cycle; `None` means
    /// the first layer's total weight.
    Poly {
        /// The layers, each a step list.
        layers: Vec<Vec<Step>>,
        /// Steps per cycle, from `%n`.
        steps: Option<Rational>,
    },
    /// `*f`: the inner pattern plays `f` times per cycle.
    Fast(Box<Expr>, Rational),
    /// `/f`: the inner pattern stretches over `f` cycles.
    Slow(Box<Expr>, Rational),
    /// `?` / `?p`: each event is removed with probability `p` (default 1/2).
    Degrade(Box<Expr>, Rational),
    /// `(k,n,r)`: `k` pulses spread over `n` steps, rotated left by `r` steps.
    Euclid {
        /// The pattern played on each pulse.
        expr: Box<Expr>,
        /// `k`.
        pulses: u32,
        /// `n`.
        steps: u32,
        /// `r`; negative rotates right.
        rotation: i64,
    },
}
