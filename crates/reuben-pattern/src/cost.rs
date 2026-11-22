//! The compile-time bound on a cycle query's work.
//!
//! [`cycle_work`] is the most units of work (`spend()` calls in the walk) one cycle query can
//! cost, for any cycle. It recurses through [`bound`]: for a node and a span length, the most work
//! a visit with that span can cost, wherever the span lies. Each node kind mirrors its walk: one
//! unit per node visit, per cycle iterated, per sequence step examined, and per `?` filter each
//! candidate onset is tested against.
//!
//! Two facts make the bound tight. A span of length `len` touches at most `ceil(len) + 1` cycles,
//! or `ceil(len)` when it starts or ends on a cycle boundary ("anchored"). And in a sequence only
//! the first and last slots a span meets can be partial: every other slot lies wholly inside the
//! span and costs one whole child cycle. A partial slot is entered at a child cycle edge, so the
//! child sees an anchored span, except when the whole span lies inside one slot. Anchoring is what
//! keeps nesting additive: an anchored span has at most one partial slot per level, and an anchored
//! span of whole cycles (what a whole slot asks of its child) has none.
//!
//! A third fact handles `< >`, polymeter and other speed changes: a cycle query's ends are
//! integers, and a speed factor `p/q` puts them on multiples of `1/q`. A slot that starts on that
//! grid and is no longer than its spacing can never be partial, so `<a b c>` is charged one whole
//! step per cycle.
//!
//! The recursion is memoised by `(node, len, anchored, grid, filters)`. A node only ever sees
//! lengths derived from the root's by the ratios on its path, capped at one cycle, so each node is
//! evaluated a few times per level of depth, not once per path.

use alloc::collections::BTreeMap;

use crate::compile::Node;
use crate::time::Rational;
use crate::Pattern;

type Memo = BTreeMap<(u32, Rational, bool, Option<i64>, u32), Rational>;

/// The most units of work one cycle query can spend on `pat`; `None` on arithmetic overflow, which
/// callers treat as unbounded. A cycle is a span of one whole cycle on its boundaries.
pub(crate) fn cycle_work(pat: &Pattern) -> Option<Rational> {
    let span = Visit {
        len: Rational::ONE,
        anchored: true,
        grid: Some(1),
        filters: 0,
    };
    bound(pat, pat.root, span, &mut Memo::new())
}

/// What the bound knows about a span: its length; whether one end lies on a cycle boundary;
/// `Some(g)` when both ends are known to be multiples of `1/g`; and how many `?` nodes lie above,
/// each tested once per candidate onset.
#[derive(Clone, Copy)]
struct Visit {
    len: Rational,
    anchored: bool,
    grid: Option<i64>,
    filters: u32,
}

/// A grid finer than this is treated as unknown; nothing is lost but tightness.
const MAX_LATTICE: i64 = 1 << 40;

fn finer(g: Option<i64>, by: i64) -> Option<i64> {
    g?.checked_mul(by).filter(|g| *g <= MAX_LATTICE)
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

fn bound(pat: &Pattern, id: u32, v: Visit, memo: &mut Memo) -> Option<Rational> {
    let Visit {
        len,
        anchored,
        grid,
        filters,
    } = v;
    if !len.is_positive() {
        return Some(Rational::ZERO);
    }
    let key = (id, len, anchored, grid, filters);
    if let Some(w) = memo.get(&key) {
        return Some(*w);
    }
    // Both ends on cycle boundaries: nothing is partial. This is what a whole sequence slot, or a
    // whole cycle query, asks of a node.
    let exact = len.denom() == 1 && (anchored || grid == Some(1));
    let touched = if exact {
        len
    } else {
        int(ceil(len)?.checked_add(i64::from(!anchored))?)
    };
    let whole_cycle = Visit {
        len: Rational::ONE,
        anchored: true,
        grid: Some(1),
        filters,
    };
    let w = match &pat.nodes[id as usize] {
        Node::Silence => Rational::ONE,
        Node::Atom(_) => {
            let per_onset = int(i64::from(filters).checked_add(1)?);
            add(Rational::ONE, touched.checked_mul(per_onset)?)?
        }
        Node::Fast { child, factor } => {
            // Ends on multiples of 1/g land on multiples of p/(q g), so of 1/(q g).
            let sub = Visit {
                len: len.checked_mul(*factor)?,
                anchored: anchored && factor.denom() == 1,
                grid: finer(grid, factor.denom()),
                filters,
            };
            add(Rational::ONE, bound(pat, *child, sub, memo)?)?
        }
        Node::Degrade { child, .. } => {
            let sub = Visit {
                filters: filters.checked_add(1)?,
                ..v
            };
            add(Rational::ONE, bound(pat, *child, sub, memo)?)?
        }
        Node::Stack { children } => {
            let mut w = Rational::ONE;
            for &c in &pat.children[children.0 as usize..children.1 as usize] {
                w = add(w, bound(pat, c, v, memo)?)?;
            }
            w
        }
        Node::Choice { children, .. } => {
            let one_piece = len.min(Rational::ONE);
            let (mut whole, mut end, mut single) = (Rational::ZERO, Rational::ZERO, Rational::ZERO);
            let end_piece = Visit {
                len: one_piece,
                anchored: true,
                ..v
            };
            let single_piece = Visit {
                len: one_piece,
                ..v
            };
            for &c in &pat.children[children.0 as usize..children.1 as usize] {
                whole = whole.max(bound(pat, c, whole_cycle, memo)?);
                end = end.max(bound(pat, c, end_piece, memo)?);
                single = single.max(bound(pat, c, single_piece, memo)?);
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
                let whole = add(Rational::ONE, bound(pat, s.node, whole_cycle, memo)?)?;
                rate = rate.max(whole.checked_div(s.len)?);
                per_cycle = add(per_cycle, whole)?;
                // A span whose ends lie on multiples of 1/g cannot end inside a slot that starts
                // on one and is no longer than 1/g: that slot is only ever whole.
                let on_grid = grid.and_then(|g| {
                    let begin = s.begin.checked_mul(int(g))?;
                    Some((g, begin.denom() == 1))
                });
                if let Some((g, true)) = on_grid {
                    if s.len.checked_mul(int(g))? <= Rational::ONE {
                        continue;
                    }
                }
                // Inside the slot, ends on multiples of 1/g map to multiples of 1/(g n), with n
                // the slot length's numerator, when the slot starts on the grid.
                let sub_grid = match on_grid {
                    Some((g, true)) => finer(Some(g), s.len.numer()),
                    _ => None,
                };
                let x = len.checked_div(s.len)?;
                let edge = Visit {
                    len: x.min(Rational::ONE),
                    anchored: true,
                    grid: sub_grid,
                    filters,
                };
                partial = partial.max(add(Rational::ONE, bound(pat, s.node, edge, memo)?)?);
                if x <= Rational::ONE {
                    let inside = Visit {
                        len: x,
                        anchored,
                        grid: sub_grid,
                        filters,
                    };
                    single = single.max(add(Rational::ONE, bound(pat, s.node, inside, memo)?)?);
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
    memo.insert(key, w);
    Some(w)
}
