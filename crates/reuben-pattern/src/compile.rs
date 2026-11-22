//! [`Ast`] → [`Pattern`]: a flat arena of nodes the query walks.
//!
//! Every construct lowers to one of six node kinds. `< >` is a sequence slowed by its total
//! weight; `{ }%n` is a stack of sequences each sped up to `n` steps per cycle; euclid is a
//! sequence holding only its pulse steps. Shared children (the copies from `!`, the pulses of a
//! euclid) are compiled once and referenced by index.

use alloc::vec::Vec;
use core::ops::Range;

use crate::ast::{Ast, Expr, ExprKind, Step};
use crate::error::Error;
use crate::time::Rational;
use crate::{Pattern, Value};

/// Deepest node nesting a compiled pattern may have; bounds the query's recursion.
pub const MAX_DEPTH: u32 = 64;

/// Largest numerator or denominator the cumulative time scale of any node may reach. A node's
/// scale is how many outer cycles one of its own cycles spans (`[a b]` gives `a` the scale 1/2;
/// `a/3` gives 3). Bounding it bounds the denominators the query produces, and with them the
/// caller spans that can overflow (see [`QueryResult::overflowed`](crate::QueryResult::overflowed)).
pub const MAX_SCALE: i64 = 1 << 20;

#[derive(Clone, Debug)]
pub(crate) enum Node {
    Atom(Value),
    Silence,
    /// Steps `steps.0..steps.1` of [`Pattern::steps`].
    Seq {
        steps: (u32, u32),
    },
    /// Children `children.0..children.1` of [`Pattern::children`].
    Stack {
        children: (u32, u32),
    },
    /// One child per cycle, picked by hashing `(seed, id, cycle)`.
    Choice {
        children: (u32, u32),
        id: u32,
    },
    /// The child's time runs `factor` times as fast.
    Fast {
        child: u32,
        factor: Rational,
    },
    /// Removes each event whose `hash(seed, id, whole.begin)` is below `threshold`, so an event is
    /// removed with probability `threshold / 2^64`.
    Degrade {
        child: u32,
        threshold: u128,
        id: u32,
    },
}

/// One slot of a sequence: the child plays in `[c + begin, c + begin + len)` of every cycle `c`.
#[derive(Clone, Debug)]
pub(crate) struct SeqStep {
    pub(crate) node: u32,
    pub(crate) begin: Rational,
    pub(crate) len: Rational,
}

/// Compile a parsed pattern.
pub fn compile(ast: &Ast) -> Result<Pattern, Error> {
    let mut c = Compiler {
        nodes: Vec::new(),
        steps: Vec::new(),
        children: Vec::new(),
        next_id: 0,
    };
    let root = c.expr(&ast.root, Rational::ONE, 1)?;
    Ok(Pattern {
        nodes: c.nodes,
        steps: c.steps,
        children: c.children,
        root,
    })
}

struct Compiler {
    nodes: Vec<Node>,
    steps: Vec<SeqStep>,
    children: Vec<u32>,
    next_id: u32,
}

fn scaled(scale: Rational, by: Option<Rational>, span: &Range<usize>) -> Result<Rational, Error> {
    let s = by
        .and_then(|b| scale.checked_mul(b))
        .filter(|s| s.numer().abs() <= MAX_SCALE && s.denom() <= MAX_SCALE);
    s.ok_or_else(|| Error::new(span.clone(), "pattern subdivides or stretches too far"))
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

impl Compiler {
    fn push(&mut self, node: Node) -> u32 {
        self.nodes.push(node);
        (self.nodes.len() - 1) as u32
    }

    fn id(&mut self) -> u32 {
        self.next_id += 1;
        self.next_id - 1
    }

    fn list(&mut self, ids: Vec<u32>) -> (u32, u32) {
        let start = self.children.len() as u32;
        self.children.extend(ids);
        (start, self.children.len() as u32)
    }

    fn expr(&mut self, e: &Expr, scale: Rational, depth: u32) -> Result<u32, Error> {
        if depth > MAX_DEPTH {
            return Err(Error::new(e.span.clone(), "pattern nested too deeply"));
        }
        let d = depth + 1;
        Ok(match &e.kind {
            ExprKind::Number(n) => self.push(Node::Atom(Value::Number(*n))),
            ExprKind::Note(m) => self.push(Node::Atom(Value::Note(*m))),
            ExprKind::Rest => self.push(Node::Silence),
            ExprKind::Seq(steps) => self.seq(steps, scale, d, &e.span)?,
            ExprKind::Stack(layers) | ExprKind::Choice(layers) => {
                let ids = layers
                    .iter()
                    .map(|l| self.expr(l, scale, d))
                    .collect::<Result<Vec<_>, _>>()?;
                let children = self.list(ids);
                if matches!(e.kind, ExprKind::Stack(_)) {
                    self.push(Node::Stack { children })
                } else {
                    let id = self.id();
                    self.push(Node::Choice { children, id })
                }
            }
            ExprKind::Alt(steps) => {
                let total = total_weight(steps, &e.span)?;
                let inner = scaled(scale, Some(total), &e.span)?;
                let child = self.seq(steps, inner, d + 1, &e.span)?;
                let factor = Rational::ONE.checked_div(total).expect("total weight > 0");
                self.push(Node::Fast { child, factor })
            }
            ExprKind::Poly { layers, steps } => {
                let base = match steps {
                    Some(n) => *n,
                    None => total_weight(&layers[0], &e.span)?,
                };
                let mut ids = Vec::with_capacity(layers.len());
                for layer in layers {
                    let total = total_weight(layer, &e.span)?;
                    let factor = base
                        .checked_div(total)
                        .ok_or_else(|| Error::new(e.span.clone(), "polymeter ratio too large"))?;
                    let inner = scaled(scale, Rational::ONE.checked_div(factor), &e.span)?;
                    let child = self.seq(layer, inner, d + 2, &e.span)?;
                    ids.push(self.push(Node::Fast { child, factor }));
                }
                if ids.len() == 1 {
                    ids[0]
                } else {
                    let children = self.list(ids);
                    self.push(Node::Stack { children })
                }
            }
            ExprKind::Fast(inner, f) | ExprKind::Slow(inner, f) => {
                let factor = if matches!(e.kind, ExprKind::Fast(..)) {
                    *f
                } else {
                    Rational::ONE.checked_div(*f).expect("speed > 0")
                };
                let s = scaled(scale, Rational::ONE.checked_div(factor), &e.span)?;
                let child = self.expr(inner, s, d)?;
                self.push(Node::Fast { child, factor })
            }
            ExprKind::Degrade(inner, p) => {
                let child = self.expr(inner, scale, d)?;
                let threshold = ((p.numer() as u128) << 64) / p.denom() as u128;
                let id = self.id();
                self.push(Node::Degrade {
                    child,
                    threshold,
                    id,
                })
            }
            ExprKind::Euclid {
                expr,
                pulses,
                steps,
                rotation,
            } => {
                let n = *steps;
                let len = Rational::new(1, i64::from(n)).expect("n >= 1");
                let child = self.expr(expr, scaled(scale, Some(len), &e.span)?, d + 1)?;
                let pattern = bjorklund(*pulses, n);
                let r = rotation.rem_euclid(i64::from(n)) as usize;
                let start = self.steps.len() as u32;
                for i in 0..n as usize {
                    if pattern[(i + r) % n as usize] {
                        self.steps.push(SeqStep {
                            node: child,
                            begin: Rational::new(i as i64, i64::from(n)).expect("n >= 1"),
                            len,
                        });
                    }
                }
                let end = self.steps.len() as u32;
                self.push(Node::Seq {
                    steps: (start, end),
                })
            }
        })
    }

    fn seq(
        &mut self,
        steps: &[Step],
        scale: Rational,
        depth: u32,
        span: &Range<usize>,
    ) -> Result<u32, Error> {
        if depth > MAX_DEPTH {
            return Err(Error::new(span.clone(), "pattern nested too deeply"));
        }
        if let [only] = steps {
            if only.copies == 1 {
                return self.expr(&only.expr, scale, depth);
            }
        }
        let total = total_weight(steps, span)?;
        let mut slots = Vec::new();
        let mut offset = Rational::ZERO;
        for step in steps {
            let len = step.weight.checked_div(total).expect("total weight > 0");
            let child = self.expr(
                &step.expr,
                scaled(scale, Some(len), &step.expr.span)?,
                depth + 1,
            )?;
            let silent = matches!(self.nodes[child as usize], Node::Silence);
            for _ in 0..step.copies {
                if !silent {
                    slots.push(SeqStep {
                        node: child,
                        begin: offset.checked_div(total).expect("total weight > 0"),
                        len,
                    });
                }
                offset = offset.checked_add(step.weight).expect("bounded by total");
            }
        }
        let start = self.steps.len() as u32;
        self.steps.extend(slots);
        let end = self.steps.len() as u32;
        Ok(self.push(Node::Seq {
            steps: (start, end),
        }))
    }
}

fn total_weight(steps: &[Step], span: &Range<usize>) -> Result<Rational, Error> {
    steps
        .iter()
        .try_fold(Rational::ZERO, |acc, s| {
            s.weight
                .checked_mul(Rational::int(i64::from(s.copies)))
                .and_then(|w| acc.checked_add(w))
        })
        .filter(|t| t.numer() <= MAX_SCALE && t.denom() <= MAX_SCALE)
        .ok_or_else(|| Error::new(span.clone(), "step weights too large"))
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
