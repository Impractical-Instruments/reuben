//! [`Ast`] → [`Pattern`]: a flat arena of nodes the query walks.
//!
//! Every construct lowers to one of six node kinds. `< >` is a sequence slowed by its total
//! weight; `{ }%n` is a stack of sequences each sped up to `n` steps per cycle; euclid is a
//! sequence holding only its pulse steps. Shared children (the copies from `!`, the pulses of a
//! euclid) are compiled once and referenced from several steps.
//!
//! Compilation is also where every bound the query relies on is proved: the input is a public
//! [`Ast`] that may be built by hand, so nothing here trusts the parser to have rejected it.

use alloc::vec::Vec;
use core::ops::Range;

use crate::ast::{Ast, Expr, ExprKind, Step};
use crate::error::Error;
use crate::parse::MAX_EXPANSION;
use crate::time::Rational;
use crate::{Pattern, Value};

/// Deepest node nesting a compiled pattern may have; bounds the query's recursion, and with it the
/// stack a query needs (see [`Pattern::cycle`]).
pub const MAX_DEPTH: u32 = 32;

/// Largest numerator or denominator any node's time scale may reach. A node's scale is how many
/// root cycles one of its own cycles spans (`[a b]` gives `a` the scale 1/2; `a/3` gives 3).
pub const MAX_SCALE: i64 = 1 << 20;

/// Largest factor by which a node may multiply the denominator of the query span on the way down
/// to its leaves: the product, along a path, of each step's start-offset denominator and length
/// numerator, and of each speed factor's denominator, tracked by divisibility. A cycle query starts
/// from whole cycles, so no node's local time has a denominator above this.
pub const MAX_GROWTH: i64 = 1 << 20;

/// Largest denominator of any onset or release, relative to its cycle. Bounds the lcm of every
/// scale and slot offset denominator along a path. With [`MAX_SCALE`], it keeps every time a cycle
/// query computes within an `i64` for any cycle up to [`MAX_CYCLE`](crate::MAX_CYCLE) from zero.
pub const MAX_GRID: i64 = 1 << 30;

/// Most units of work (see [`QUERY_BUDGET`](crate::QUERY_BUDGET)) one cycle query may need.
/// Compilation computes a worst-case bound for every pattern (see [`Pattern::max_cycle_work`]) and
/// refuses one above this. Set so one cycle query stays a small share of a 64-frame block at
/// 48 kHz (see the crate docs' Cost section). Cost depends on structure, not only note count:
/// `0*128` costs 130 for 128 notes; `[0,4,7,11]*16` costs 70 for 64; `[0 1 2 3 4 5 6 7]*4` costs
/// 106 for 32; and `[[0 1 2 3]*4]?0.3, [4 5]*8` costs 142 for at most 32, because every `?` test
/// is charged.
pub const MAX_WORK: u32 = 160;

/// Most entries (nodes, sequence steps, and stack children) a compiled pattern may hold: about
/// 2.5 MB at most.
pub const MAX_SIZE: usize = 1 << 16;

#[derive(Clone, Debug)]
pub(crate) enum Node {
    Atom(Value),
    Silence,
    /// Steps `steps.0..steps.1` of [`Pattern::steps`], sorted and non-overlapping.
    Seq {
        steps: (u32, u32),
    },
    /// Children `children.0..children.1` of [`Pattern::children`]; never empty.
    Stack {
        children: (u32, u32),
    },
    /// One child per cycle, picked by hashing `(seed, id, cycle, path)`; never empty.
    Choice {
        children: (u32, u32),
        id: u32,
    },
    /// The child's time runs `factor` (> 0) times as fast.
    Fast {
        child: u32,
        factor: Rational,
    },
    /// Removes each event whose `hash(seed, id, onset, path)` is below `threshold`, so an event is
    /// removed with probability `threshold / 2^64`.
    Degrade {
        child: u32,
        threshold: u128,
        id: u32,
    },
}

/// One slot of a sequence: the child plays in `[c + begin, c + end)` of every cycle `c`.
#[derive(Clone, Debug)]
pub(crate) struct SeqStep {
    pub(crate) node: u32,
    pub(crate) begin: Rational,
    pub(crate) end: Rational,
    pub(crate) len: Rational,
}

/// Compile a parsed pattern. Never panics, for any [`Ast`]; every limit is an [`Error`] with the
/// span of the expression that broke it.
pub fn compile(ast: &Ast) -> Result<Pattern, Error> {
    let mut c = Compiler {
        nodes: Vec::new(),
        steps: Vec::new(),
        children: Vec::new(),
        next_id: 0,
    };
    let root_ctx = Ctx {
        scale: Rational::ONE,
        growth: 1,
        grid: 1,
        blame: None,
    };
    let root = c.expr(&ast.root, root_ctx, 1)?;
    let mut pattern = Pattern {
        nodes: c.nodes,
        steps: c.steps,
        children: c.children,
        root,
        max_work: 0,
    };
    let work = crate::cost::cycle_work(&pattern)
        .filter(|w| *w <= Rational::int(i64::from(MAX_WORK)))
        .ok_or_else(|| too_costly(&ast.root.span))?;
    // At most MAX_WORK, so the rounded-up value fits.
    pattern.max_work = (work.numer() + work.denom() - 1).div_euclid(work.denom()) as u32;
    Ok(pattern)
}

fn too_costly(span: &Range<usize>) -> Error {
    Error::new(span.clone(), "pattern too dense to query in real time")
}

fn too_fine(span: &Range<usize>) -> Error {
    Error::new(
        span.clone(),
        "pattern subdivides or stretches too far for exact time",
    )
}

/// The bounds a node's position in the tree imposes; see [`MAX_SCALE`], [`MAX_GROWTH`],
/// [`MAX_GRID`].
///
/// `growth` is tracked by divisibility, not size: a query span with denominator `d` gives this
/// node local times whose denominators divide `d * growth`. That lets factors cancel exactly where
/// the arithmetic cancels them (`<a b c>` slows by 3 and its slots of 1/3 undo it).
#[derive(Clone, Copy)]
struct Ctx {
    scale: Rational,
    growth: i64,
    grid: i64,
    /// The source span of the innermost speed modifier above, if any: the place to point when a
    /// slot is only too fine because of the speed it inherits.
    blame: Option<(usize, usize)>,
}

fn gcd(mut a: i128, mut b: i128) -> i128 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a.abs()
}

/// `lcm(a, b)` for positive `a`, `b`; `None` past `2^62`, beyond any limit it is checked against.
fn lcm(a: i128, b: i128) -> Option<i128> {
    (a / gcd(a, b)).checked_mul(b).filter(|l| *l <= 1 << 62)
}

impl Ctx {
    fn checked(scale: Option<Rational>, growth: Option<i128>, grid: Option<i128>) -> Option<Ctx> {
        let scale = scale.filter(|s| s.numer() <= MAX_SCALE && s.denom() <= MAX_SCALE)?;
        let growth = growth.filter(|g| *g <= i128::from(MAX_GROWTH))?;
        let grid = grid
            .and_then(|g| lcm(g, i128::from(scale.denom())))
            .filter(|g| *g <= i128::from(MAX_GRID))?;
        Some(Ctx {
            scale,
            growth: growth as i64,
            grid: grid as i64,
            blame: None,
        })
    }

    /// The context of a child whose time runs `f` times as fast: local `t` becomes `t * p / q`.
    fn fast(self, f: Rational, span: &Range<usize>) -> Result<Ctx, Error> {
        let g = i128::from(self.growth);
        let growth = g / gcd(g, i128::from(f.numer())) * i128::from(f.denom());
        let ctx = Ctx::checked(
            self.scale.checked_div(f),
            Some(growth),
            Some(i128::from(self.grid)),
        )
        .ok_or_else(|| too_fine(span))?;
        Ok(Ctx {
            blame: Some((span.start, span.end)),
            ..ctx
        })
    }

    /// The context of a child filling slots `[b, b + len)` for each `b` in `begins`: local `t`
    /// becomes `(t - b) / len`, and the slot edges `a * b` and `a * (b + len)` join the grid.
    fn slot(self, begins: &[Rational], len: Rational, span: &Range<usize>) -> Result<Ctx, Error> {
        self.slot_inner(begins, len).ok_or_else(|| {
            // A slot that is fine on its own is only too fine because of the speed above it.
            let root = Ctx {
                scale: Rational::ONE,
                growth: 1,
                grid: 1,
                blame: None,
            };
            match self.blame {
                Some((a, b)) if root.slot_inner(begins, len).is_some() => too_fine(&(a..b)),
                _ => too_fine(span),
            }
        })
    }

    fn slot_inner(self, begins: &[Rational], len: Rational) -> Option<Ctx> {
        let mut offsets = i128::from(self.growth);
        let mut grid = i128::from(self.grid);
        for b in begins {
            offsets = lcm(offsets, i128::from(b.denom()))?;
            for edge in [*b, b.checked_add(len)?] {
                let at = self.scale.checked_mul(edge)?;
                grid = lcm(grid, i128::from(at.denom()))?;
            }
        }
        let growth = offsets / gcd(offsets, i128::from(len.denom())) * i128::from(len.numer());
        Some(Ctx {
            blame: self.blame,
            ..Ctx::checked(self.scale.checked_mul(len), Some(growth), Some(grid))?
        })
    }
}

/// Bjorklund's algorithm: `k` pulses as evenly as possible over `n` steps, first step a pulse.
/// Repeatedly pairs each leading group with one trailing group until at most one trailing group
/// is left, which yields the canonical rotation (`(3,8)` is `x..x..x.`).
fn bjorklund(k: u32, n: u32) -> Vec<bool> {
    let (k, n) = (k as usize, n as usize);
    if k == 0 {
        return alloc::vec![false; n];
    }
    let mut heads: Vec<Vec<bool>> = (0..k).map(|_| alloc::vec![true]).collect();
    let mut tails: Vec<Vec<bool>> = (0..n - k).map(|_| alloc::vec![false]).collect();
    while tails.len() > 1 {
        let m = heads.len().min(tails.len());
        let rest = if heads.len() > m {
            heads.split_off(m)
        } else {
            tails.split_off(m)
        };
        for (h, t) in heads.iter_mut().zip(tails.drain(..)) {
            h.extend(t);
        }
        tails = rest;
    }
    heads.into_iter().chain(tails).flatten().collect()
}

struct Compiler {
    nodes: Vec<Node>,
    steps: Vec<SeqStep>,
    children: Vec<u32>,
    next_id: u32,
}

impl Compiler {
    fn size(&self) -> usize {
        self.nodes.len() + self.steps.len() + self.children.len()
    }

    fn reserve(&self, extra: usize, span: &Range<usize>) -> Result<(), Error> {
        if self.size() + extra > MAX_SIZE {
            return Err(Error::new(span.clone(), "pattern too large to compile"));
        }
        Ok(())
    }

    fn push(&mut self, node: Node, span: &Range<usize>) -> Result<u32, Error> {
        self.reserve(1, span)?;
        self.nodes.push(node);
        Ok((self.nodes.len() - 1) as u32)
    }

    fn id(&mut self) -> u32 {
        self.next_id = self.next_id.wrapping_add(1);
        self.next_id.wrapping_sub(1)
    }

    fn list(&mut self, ids: Vec<u32>, span: &Range<usize>) -> Result<(u32, u32), Error> {
        self.reserve(ids.len(), span)?;
        let start = self.children.len() as u32;
        self.children.extend(ids);
        Ok((start, self.children.len() as u32))
    }

    fn expr(&mut self, e: &Expr, ctx: Ctx, depth: u32) -> Result<u32, Error> {
        let span = &e.span;
        if depth > MAX_DEPTH {
            return Err(Error::new(span.clone(), "pattern nested too deeply"));
        }
        self.expr_inner(e, ctx, depth + 1)
    }

    fn expr_inner(&mut self, e: &Expr, ctx: Ctx, d: u32) -> Result<u32, Error> {
        let span = &e.span;
        match &e.kind {
            ExprKind::Number(n) => self.push(Node::Atom(Value::Number(*n)), span),
            ExprKind::Note(m) => {
                if *m > 127 {
                    return Err(Error::new(
                        span.clone(),
                        "note outside the MIDI range 0-127",
                    ));
                }
                self.push(Node::Atom(Value::Note(*m)), span)
            }
            ExprKind::Rest => self.push(Node::Silence, span),
            ExprKind::Seq(steps) => self.seq(steps, ctx, d, span),
            ExprKind::Stack(layers) | ExprKind::Choice(layers) => {
                if layers.is_empty() {
                    return Err(Error::new(span.clone(), "empty stack or choice"));
                }
                let mut ids = Vec::with_capacity(layers.len());
                for layer in layers {
                    let id = self.expr(layer, ctx, d)?;
                    ids.push(id);
                }
                let children = self.list(ids, span)?;
                if matches!(e.kind, ExprKind::Stack(_)) {
                    self.push(Node::Stack { children }, span)
                } else {
                    let node = Node::Choice {
                        children,
                        id: self.id(),
                    };
                    self.push(node, span)
                }
            }
            ExprKind::Alt(steps) => {
                let total = total_weight(steps, span)?;
                let factor = Rational::ONE
                    .checked_div(total)
                    .ok_or_else(|| too_fine(span))?;
                self.fast_seq(steps, factor, ctx, d, span)
            }
            ExprKind::Poly { layers, steps } => {
                let Some(first) = layers.first() else {
                    return Err(Error::new(span.clone(), "empty polymeter"));
                };
                let base = match steps {
                    Some(n) if n.is_positive() => *n,
                    Some(_) => {
                        return Err(Error::new(
                            span.clone(),
                            "step count must be greater than zero",
                        ))
                    }
                    None => total_weight(first, span)?,
                };
                let mut ids = Vec::with_capacity(layers.len());
                for layer in layers {
                    let total = total_weight(layer, span)?;
                    let factor = base.checked_div(total).ok_or_else(|| too_fine(span))?;
                    let id = self.fast_seq(layer, factor, ctx, d, span)?;
                    ids.push(id);
                }
                if ids.len() == 1 {
                    return Ok(ids[0]);
                }
                let children = self.list(ids, span)?;
                self.push(Node::Stack { children }, span)
            }
            ExprKind::Fast(inner, f) | ExprKind::Slow(inner, f) => {
                if !f.is_positive() {
                    return Err(Error::new(span.clone(), "speed must be greater than zero"));
                }
                let factor = if matches!(e.kind, ExprKind::Fast(..)) {
                    *f
                } else {
                    Rational::ONE
                        .checked_div(*f)
                        .ok_or_else(|| too_fine(span))?
                };
                // The modifier's own text, `*f` or `/f`, after the term it applies to.
                let modifier = if inner.span.end < span.end {
                    inner.span.end..span.end
                } else {
                    span.clone()
                };
                let child = self.expr(inner, ctx.fast(factor, &modifier)?, d)?;
                self.push(Node::Fast { child, factor }, span)
            }
            ExprKind::Degrade(inner, p) => {
                if *p < Rational::ZERO || *p > Rational::ONE {
                    return Err(Error::new(
                        span.clone(),
                        "probability must be between 0 and 1",
                    ));
                }
                let child = self.expr(inner, ctx, d)?;
                let threshold = ((p.numer() as u128) << 64) / p.denom() as u128;
                let id = self.id();
                let node = Node::Degrade {
                    child,
                    threshold,
                    id,
                };
                self.push(node, span)
            }
            ExprKind::Euclid {
                expr,
                pulses,
                steps,
                rotation,
            } => {
                let n = *steps;
                if !(1..=MAX_EXPANSION).contains(&i64::from(n)) {
                    return Err(Error::new(
                        span.clone(),
                        "euclid steps must be between 1 and 1024",
                    ));
                }
                if *pulses > n {
                    return Err(Error::new(span.clone(), "more euclid pulses than steps"));
                }
                let n64 = i64::from(n);
                let len = Rational::new(1, n64).ok_or_else(|| too_fine(span))?;
                let pattern = bjorklund(*pulses, n);
                let r = rotation.rem_euclid(n64) as usize;
                let hits: Vec<i64> = (0..n as usize)
                    .filter(|i| pattern[(i + r) % n as usize])
                    .map(|i| i as i64)
                    .collect();
                let begins = hits
                    .iter()
                    .map(|i| Rational::new(*i, n64).ok_or_else(|| too_fine(span)))
                    .collect::<Result<Vec<_>, _>>()?;
                let child = self.expr(expr, ctx.slot(&begins, len, span)?, d + 1)?;
                self.reserve(begins.len() + 1, span)?;
                let start = self.steps.len() as u32;
                for begin in begins {
                    let end = begin.checked_add(len).ok_or_else(|| too_fine(span))?;
                    self.steps.push(SeqStep {
                        node: child,
                        begin,
                        end,
                        len,
                    });
                }
                let end = self.steps.len() as u32;
                self.push(
                    Node::Seq {
                        steps: (start, end),
                    },
                    span,
                )
            }
        }
    }

    /// A sequence of `steps` whose time runs `factor` times as fast: `< >` and polymeter layers.
    fn fast_seq(
        &mut self,
        steps: &[Step],
        factor: Rational,
        ctx: Ctx,
        depth: u32,
        span: &Range<usize>,
    ) -> Result<u32, Error> {
        let child = self.seq(steps, ctx.fast(factor, span)?, depth + 1, span)?;
        self.push(Node::Fast { child, factor }, span)
    }

    fn seq(
        &mut self,
        steps: &[Step],
        ctx: Ctx,
        depth: u32,
        span: &Range<usize>,
    ) -> Result<u32, Error> {
        if depth > MAX_DEPTH {
            return Err(Error::new(span.clone(), "pattern nested too deeply"));
        }
        let total = total_weight(steps, span)?;
        if let [only] = steps {
            if only.copies == 1 {
                return self.expr(&only.expr, ctx, depth);
            }
        }
        let mut slots = Vec::new();
        let mut offset = Rational::ZERO;
        for step in steps {
            let sspan = &step.expr.span;
            let len = step
                .weight
                .checked_div(total)
                .ok_or_else(|| too_fine(sspan))?;
            let mut begins = Vec::with_capacity(step.copies as usize);
            for _ in 0..step.copies {
                begins.push(offset.checked_div(total).ok_or_else(|| too_fine(sspan))?);
                offset = offset
                    .checked_add(step.weight)
                    .ok_or_else(|| too_fine(sspan))?;
            }
            let child = self.expr(&step.expr, ctx.slot(&begins, len, sspan)?, depth + 1)?;
            if matches!(self.nodes[child as usize], Node::Silence) {
                continue;
            }
            self.reserve(slots.len() + begins.len(), span)?;
            for begin in begins {
                let end = begin.checked_add(len).ok_or_else(|| too_fine(sspan))?;
                slots.push(SeqStep {
                    node: child,
                    begin,
                    end,
                    len,
                });
            }
        }
        self.reserve(slots.len() + 1, span)?;
        let start = self.steps.len() as u32;
        self.steps.extend(slots);
        let end = self.steps.len() as u32;
        self.push(
            Node::Seq {
                steps: (start, end),
            },
            span,
        )
    }
}

/// The sum of a step list's weights. Positive, or an error: an empty list, a non-positive weight,
/// or a copy count outside `1..=MAX_EXPANSION` is rejected here.
fn total_weight(steps: &[Step], span: &Range<usize>) -> Result<Rational, Error> {
    if steps.is_empty() {
        return Err(Error::new(span.clone(), "empty sequence"));
    }
    let mut total = Rational::ZERO;
    for s in steps {
        if !s.weight.is_positive() {
            return Err(Error::new(
                s.expr.span.clone(),
                "weight must be greater than zero",
            ));
        }
        if s.copies == 0 || i64::from(s.copies) > MAX_EXPANSION {
            return Err(Error::new(
                s.expr.span.clone(),
                "repeat count must be between 1 and 1024",
            ));
        }
        if s.weight.denom() > MAX_SCALE {
            return Err(Error::new(
                s.expr.span.clone(),
                "weight too finely divided for exact time",
            ));
        }
        let sum = s
            .weight
            .checked_mul(Rational::int(i64::from(s.copies)))
            .and_then(|w| total.checked_add(w));
        total = match sum {
            Some(t) if t.numer() <= MAX_SCALE && t.denom() <= MAX_SCALE => t,
            // A total that is not itself large has outgrown the bound through its denominator.
            Some(t) if t.floor() < MAX_SCALE => {
                let finest = steps.iter().max_by_key(|s| s.weight.denom()).unwrap_or(s);
                return Err(Error::new(
                    finest.expr.span.clone(),
                    "step weights too finely divided for exact time",
                ));
            }
            _ => return Err(Error::new(span.clone(), "step weights too large")),
        };
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(k: u32, n: u32) -> alloc::string::String {
        bjorklund(k, n)
            .into_iter()
            .map(|b| if b { 'x' } else { '.' })
            .collect()
    }

    #[test]
    fn bjorklund_matches_published_rhythms() {
        assert_eq!(grid(3, 8), "x..x..x.");
        assert_eq!(grid(5, 8), "x.xx.xx.");
        assert_eq!(grid(2, 5), "x.x..");
        assert_eq!(grid(4, 7), "x.x.x.x");
        assert_eq!(grid(0, 4), "....");
        assert_eq!(grid(4, 4), "xxxx");
        assert_eq!(grid(1, 1), "x");
        for n in 1..=64 {
            for k in 0..=n {
                let g = bjorklund(k, n);
                assert_eq!(g.len(), n as usize);
                assert_eq!(g.iter().filter(|b| **b).count(), k as usize);
            }
        }
    }
}
