//! The compile-time bound on a query's work.
//!
//! [`bound`] returns, for a node and a span length, the most units of work (`spend()` calls in the
//! walk) a query of that length can cost, wherever the span lies. Each node kind mirrors its walk:
//! one unit per node visit, per cycle iterated, and per sequence step examined.
//!
//! Two facts make the bound tight. A span of length `len` touches at most `ceil(len) + 1` cycles,
//! or `ceil(len)` when it starts or ends on a cycle boundary ("anchored"). And in a sequence only
//! the first and last slots a span meets can be partial: every other slot lies wholly inside the
//! span and costs one whole child cycle. A partial slot is entered at a child cycle edge, so the
//! child sees an anchored span, except when the whole span lies inside one slot. Anchoring is what
//! keeps nesting additive: an anchored span has at most one partial slot per level, and an anchored
//! span of whole cycles (what a whole slot asks of its child) has none.
//!
//! The recursion is memoised by `(node, len, anchored)`. A node only ever sees lengths derived
//! from the root's by the slot ratios on its path, capped at one cycle, so each node is evaluated
//! at most about twice per level of depth.

use alloc::collections::BTreeMap;

use crate::compile::Node;
use crate::time::Rational;
use crate::Pattern;

type Memo = BTreeMap<(u32, Rational, bool), Rational>;

/// The most units of work a query of `len` cycles can spend on `pat`; `None` on arithmetic
/// overflow, which callers treat as unbounded.
pub(crate) fn work(pat: &Pattern, len: Rational) -> Option<Rational> {
    // Any span lies inside a longer one, and the walk does no less over the longer, so rounding
    // the length up keeps the bound sound while keeping its arithmetic on a small grid.
    let grid = 1024;
    let up = ceil(len.checked_mul(int(grid))?)?;
    bound(
        pat,
        pat.root,
        Rational::new(up, grid)?,
        false,
        &mut Memo::new(),
    )
}

fn ceil(x: Rational) -> Option<i64> {
    let (n, d) = (i128::from(x.numer()), i128::from(x.denom()));
    i64::try_from((n + d - 1).div_euclid(d)).ok()
}

fn int(n: i64) -> Rational {
    Rational::int(n)
}

fn add(a: Rational, b: Rational) -> Option<Rational> {
    a.checked_add(b)
}

fn bound(
    pat: &Pattern,
    id: u32,
    len: Rational,
    anchored: bool,
    memo: &mut Memo,
) -> Option<Rational> {
    if !len.is_positive() {
        return Some(Rational::ZERO);
    }
    if let Some(w) = memo.get(&(id, len, anchored)) {
        return Some(*w);
    }
    let touched = int(ceil(len)?.checked_add(i64::from(!anchored))?);
    // Anchored with a whole number of cycles: both ends lie on cycle boundaries, so nothing is
    // partial. This is what a whole sequence slot asks of its child.
    let exact = anchored && len.denom() == 1;
    let w = match &pat.nodes[id as usize] {
        Node::Silence => Rational::ONE,
        Node::Atom(_) => add(Rational::ONE, touched)?,
        Node::Fast { child, factor } => {
            let edge = anchored && factor.denom() == 1;
            add(
                Rational::ONE,
                bound(pat, *child, len.checked_mul(*factor)?, edge, memo)?,
            )?
        }
        Node::Degrade { child, .. } => {
            add(Rational::ONE, bound(pat, *child, len, anchored, memo)?)?
        }
        Node::Stack { children } => {
            let mut w = Rational::ONE;
            for &c in &pat.children[children.0 as usize..children.1 as usize] {
                w = add(w, bound(pat, c, len, anchored, memo)?)?;
            }
            w
        }
        Node::Choice { children, .. } => {
            let one_piece = len.min(Rational::ONE);
            let (mut whole, mut end, mut single) = (Rational::ZERO, Rational::ZERO, Rational::ZERO);
            for &c in &pat.children[children.0 as usize..children.1 as usize] {
                whole = whole.max(bound(pat, c, Rational::ONE, true, memo)?);
                end = end.max(bound(pat, c, one_piece, true, memo)?);
                single = single.max(bound(pat, c, one_piece, anchored, memo)?);
            }
            // One unit per touched cycle and one child visit per piece: whole cycles between two
            // end pieces entered at an edge, or a single piece entered anywhere.
            let interior = touched.checked_sub(int(2))?.max(Rational::ZERO);
            let pieces = if exact {
                touched.checked_mul(whole)?
            } else {
                interior
                    .checked_mul(whole)?
                    .checked_add(end.checked_mul(int(2))?)?
                    .max(single)
            };
            add(add(Rational::ONE, touched)?, pieces)?
        }
        Node::Seq { steps } => {
            let slots = &pat.steps[steps.0 as usize..steps.1 as usize];
            let (mut rate, mut per_cycle) = (Rational::ZERO, Rational::ZERO);
            let (mut partial, mut single) = (Rational::ZERO, Rational::ZERO);
            for s in slots {
                let whole = add(
                    Rational::ONE,
                    bound(pat, s.node, Rational::ONE, true, memo)?,
                )?;
                rate = rate.max(whole.checked_div(s.len)?);
                per_cycle = add(per_cycle, whole)?;
                let x = len.checked_div(s.len)?;
                let part = bound(pat, s.node, x.min(Rational::ONE), true, memo)?;
                partial = partial.max(add(Rational::ONE, part)?);
                if x <= Rational::ONE {
                    let inside = bound(pat, s.node, x, anchored, memo)?;
                    single = single.max(add(Rational::ONE, inside)?);
                }
            }
            // The partial slots: one, entered at its edge, for an anchored span; otherwise one
            // slot entered anywhere or two entered at their edges.
            let partials = if exact {
                Rational::ZERO
            } else if anchored {
                partial
            } else {
                single.max(partial.checked_mul(int(2))?)
            };
            // The node visit, and per touched cycle one unit for the cycle and one for the step
            // that ends the scan.
            let base = add(Rational::ONE, touched.checked_mul(int(2))?)?;
            // Whole slots charged against the span they cover, or every slot once per cycle.
            let by_length = len.checked_mul(rate)?;
            let by_cycle = touched.checked_mul(per_cycle)?;
            add(add(base, by_length.min(by_cycle))?, partials)?
        }
    };
    memo.insert((id, len, anchored), w);
    Some(w)
}
