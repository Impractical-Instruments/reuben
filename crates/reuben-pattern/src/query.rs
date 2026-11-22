//! The Render-side query: every note whose onset lies in one cycle, by a recursive walk of the
//! compiled tree into a caller's buffer.
//!
//! Each node is visited with the part of its own timeline that the queried cycle covers, an affine
//! map from its own time to the root's (`root = a * t + b`), a clip window in root time (the cycle
//! cut by every enclosing slot), and the end of the innermost enclosing slot. A leaf maps each
//! onset in its part up through the map, so nothing is transformed on the way back up and no
//! intermediate list exists.
//!
//! Times are [`Pos`] (integer cycle plus fraction). The cycle number only ever meets integers and
//! the pattern's own ratios, so fractions stay over the pattern's denominators (see
//! [`MAX_GROWTH`](crate::MAX_GROWTH) and [`MAX_GRID`](crate::MAX_GRID)) whatever the cycle. The walk
//! allocates nothing: its state is the recursion (bounded by [`MAX_DEPTH`](crate::MAX_DEPTH)) and a
//! stack-linked chain of active `?` filters. Its work is bounded at compile time (see
//! [`Pattern::max_cycle_work`]), and by [`QUERY_BUDGET`] as a backstop.

use crate::compile::{Node, SeqStep};
use crate::time::{Pos, Rational};
use crate::{Pattern, Value};

/// Most units of work one [`Pattern::cycle`] query may do before it stops and sets
/// [`CycleResult::exhausted`]. A unit is one node visit, one cycle a node iterates, one sequence
/// step it examines, or one `?` filter an onset is tested against.
///
/// A backstop, unreachable for a compiled pattern: compilation proves every pattern's cycle query
/// needs at most [`MAX_WORK`](crate::MAX_WORK) units, and this leaves a quarter more for headroom.
pub const QUERY_BUDGET: u32 = crate::MAX_WORK + crate::MAX_WORK / 4;

/// The most cycles from zero that [`Pattern::cycle`] is guaranteed to handle without overflow:
/// any `cycle` with `|cycle| <= MAX_CYCLE` has every note it should. Beyond it, a time may not fit
/// and the query counts it in [`CycleResult::overflowed`] instead.
pub const MAX_CYCLE: i64 = 1 << 31;

/// One note: where in its cycle it starts, when it releases, and its value.
///
/// # Contract
///
/// A note belongs to the cycle its onset lies in, and only that cycle reports it. A note from a
/// slowed pattern that starts in one cycle and sounds into the next is reported once, by the first.
///
/// `release` is the end of the note's event, cut to the end of the sequence step it plays in (and
/// of every step enclosing that one), so a step never sounds into the next one. In `[0 1]/1.5 2`,
/// the `1` starting at 3/8 would hold until 3/4 but releases at 1/2, where `2` starts. This
/// diverges from the Tidal reference's model, in which an event holds for its whole extent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Note {
    /// The onset, as a fraction of the cycle from its start: in `[0, 1)`.
    pub onset: Rational,
    /// The release, relative to the same cycle start: greater than `onset`, and above 1 when the
    /// note sounds into a later cycle.
    pub release: Rational,
    /// The note's value.
    pub value: Value,
}

impl Note {
    /// A placeholder for initialising a buffer; never returned by a query.
    pub const EMPTY: Note = Note {
        onset: Rational::ZERO,
        release: Rational::ZERO,
        value: Value::Number(Rational::ZERO),
    };
}

/// What a cycle query wrote, and what it could not.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CycleResult {
    /// Notes written to the front of the buffer.
    pub written: usize,
    /// Notes found after the buffer was full. They are lost; the first `written` notes are a
    /// prefix of the full answer in walk order.
    pub dropped: usize,
    /// Parts of the walk abandoned because a time did not fit an `i64`. Zero for any
    /// `|cycle| <=` [`MAX_CYCLE`].
    pub overflowed: usize,
    /// `true` if the walk stopped after [`QUERY_BUDGET`] units of work. Unreachable for a compiled
    /// pattern. If it fires, the written notes are correct and a prefix of the full answer in walk
    /// order (not time order), and the rest are missing and not counted in `dropped`.
    pub exhausted: bool,
    /// Units of work the query spent; at most [`Pattern::max_cycle_work`].
    pub work: u32,
}

#[derive(Clone, Copy)]
struct PSpan {
    begin: Pos,
    end: Pos,
}

impl PSpan {
    fn is_empty(&self) -> bool {
        self.begin >= self.end
    }
    fn intersect(&self, o: &PSpan) -> PSpan {
        PSpan {
            begin: self.begin.max(o.begin),
            end: self.end.min(o.end),
        }
    }
    fn cycle(c: i64) -> Option<PSpan> {
        Some(PSpan {
            begin: Pos::int(c),
            end: Pos::int(c.checked_add(1)?),
        })
    }
}

/// A node's time to root time: `a * t + b`.
#[derive(Clone, Copy)]
struct Map {
    a: Rational,
    b: Pos,
}

impl Map {
    fn apply(&self, t: Pos) -> Option<Pos> {
        t.mul(self.a)?.add(self.b)
    }
    fn span(&self, s: PSpan) -> Option<PSpan> {
        Some(PSpan {
            begin: self.apply(s.begin)?,
            end: self.apply(s.end)?,
        })
    }
}

/// An active `?` on the path from the root to the current node.
struct Filter<'f> {
    id: u32,
    threshold: u128,
    parent: Option<&'f Filter<'f>>,
}

fn mix(mut z: u64) -> u64 {
    // splitmix64's finaliser: a bijective avalanche over u64.
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The only source of randomness: a pure function of its inputs, each folded in through a full
/// avalanche so no two inputs can trade bits.
fn hash(seed: u64, id: u32, path: u64, a: i64, b: i64, c: i64) -> u64 {
    let mut h = mix(seed);
    for x in [u64::from(id), path, a as u64, b as u64, c as u64] {
        h = mix(h ^ x);
    }
    h
}

struct Walk<'p, 'o> {
    pat: &'p Pattern,
    seed: u64,
    /// The queried cycle's start in root time; notes are reported relative to it.
    origin: Pos,
    out: &'o mut [Note],
    res: CycleResult,
    work: u32,
}

/// The integer cycles a non-empty span touches.
fn cycles(span: PSpan) -> core::ops::RangeInclusive<i64> {
    // `end` is exclusive, so a span ending exactly on a cycle boundary does not touch that cycle.
    let last = if span.end.frac == Rational::ZERO {
        span.end.cyc.saturating_sub(1)
    } else {
        span.end.cyc
    };
    span.begin.cyc..=last
}

/// The integer times (onsets of an atom's events) in a non-empty span.
fn onsets(span: PSpan) -> core::ops::Range<i64> {
    let first = if span.begin.frac == Rational::ZERO {
        span.begin.cyc
    } else {
        span.begin.cyc.saturating_add(1)
    };
    let end = if span.end.frac == Rational::ZERO {
        span.end.cyc
    } else {
        span.end.cyc.saturating_add(1)
    };
    first..end
}

/// Everything a node visit needs besides the node: its span, its map to root time, the root-time
/// window its onsets must lie in, the end of its enclosing slots, the `?` filters above it, and its
/// structural path.
#[derive(Clone, Copy)]
struct Visit<'f> {
    span: PSpan,
    map: Map,
    clip: PSpan,
    /// The earliest end, in root time, of the sequence slots enclosing this node; `None` above the
    /// first one.
    slot_end: Option<Pos>,
    filter: Option<&'f Filter<'f>>,
    path: u64,
}

impl Walk<'_, '_> {
    /// Spend one unit of work; `false` once the budget is gone.
    fn spend(&mut self) -> bool {
        if self.work >= QUERY_BUDGET {
            self.res.exhausted = true;
            return false;
        }
        self.work += 1;
        true
    }

    fn emit(&mut self, note: Note) {
        if let Some(slot) = self.out.get_mut(self.res.written) {
            *slot = note;
            self.res.written += 1;
        } else {
            self.res.dropped += 1;
        }
    }

    /// `Some(true)` if a `?` above removes the note at `onset`; `None` if the budget ran out.
    fn removed(&mut self, onset: Pos, path: u64, mut filter: Option<&Filter<'_>>) -> Option<bool> {
        while let Some(f) = filter {
            if !self.spend() {
                return None;
            }
            let h = hash(
                self.seed,
                f.id,
                path,
                onset.cyc,
                onset.frac.numer(),
                onset.frac.denom(),
            );
            if u128::from(h) < f.threshold {
                return Some(true);
            }
            filter = f.parent;
        }
        Some(false)
    }

    fn node(&mut self, id: u32, v: Visit<'_>) {
        if v.span.is_empty() || v.clip.is_empty() || !self.spend() {
            return;
        }
        if self.node_inner(id, v).is_none() {
            self.res.overflowed += 1;
        }
    }

    fn node_inner(&mut self, id: u32, v: Visit<'_>) -> Option<()> {
        let pat = self.pat;
        match &pat.nodes[id as usize] {
            Node::Silence => {}
            Node::Atom(value) => {
                for c in onsets(v.span) {
                    if !self.spend() {
                        return Some(());
                    }
                    let onset = v.map.apply(Pos::int(c))?;
                    if onset < v.clip.begin || onset >= v.clip.end {
                        continue;
                    }
                    match self.removed(onset, v.path, v.filter) {
                        None => return Some(()),
                        Some(true) => continue,
                        Some(false) => {}
                    }
                    let end = v.map.apply(Pos::int(c.checked_add(1)?))?;
                    let release = v.slot_end.map_or(end, |e| e.min(end));
                    self.emit(Note {
                        onset: onset.sub(self.origin)?.to_rational()?,
                        release: release.sub(self.origin)?.to_rational()?,
                        value: *value,
                    });
                }
            }
            Node::Seq { steps } => {
                let (lo, hi) = (steps.0 as usize, steps.1 as usize);
                let steps = &pat.steps[lo..hi];
                let first = v.span.begin.cyc;
                for c in cycles(v.span) {
                    if !self.spend() {
                        return Some(());
                    }
                    // In the first cycle, skip the steps that end before the span starts.
                    let skip = if c == first {
                        steps.partition_point(|s| s.end <= v.span.begin.frac)
                    } else {
                        0
                    };
                    for (i, step) in steps.iter().enumerate().skip(skip) {
                        if !self.spend() {
                            return Some(());
                        }
                        let slot_begin = Pos {
                            cyc: c,
                            frac: step.begin,
                        };
                        if slot_begin >= v.span.end {
                            break;
                        }
                        self.step(step, (lo + i) as u64, c, slot_begin, v)?;
                    }
                }
            }
            Node::Stack { children } => {
                let (lo, hi) = (children.0 as usize, children.1 as usize);
                for (i, &child) in pat.children[lo..hi].iter().enumerate() {
                    // Layers draw independently too. The top bit keeps these paths apart from
                    // sequence-step ones, which index a different arena.
                    let path = mix(v.path ^ (1 << 63 | (lo + i) as u64));
                    self.node(child, Visit { path, ..v });
                }
            }
            Node::Choice { children, id } => {
                let children = &pat.children[children.0 as usize..children.1 as usize];
                for c in cycles(v.span) {
                    if !self.spend() {
                        return Some(());
                    }
                    let h = hash(self.seed, *id, v.path, c, 0, 0);
                    let pick = ((u128::from(h) * children.len() as u128) >> 64) as usize;
                    let cycle = PSpan::cycle(c)?;
                    let sub = Visit {
                        span: v.span.intersect(&cycle),
                        clip: v.clip.intersect(&v.map.span(cycle)?),
                        ..v
                    };
                    self.node(children[pick], sub);
                }
            }
            Node::Fast { child, factor } => {
                let sub = Visit {
                    span: PSpan {
                        begin: v.span.begin.mul(*factor)?,
                        end: v.span.end.mul(*factor)?,
                    },
                    map: Map {
                        a: v.map.a.checked_div(*factor)?,
                        b: v.map.b,
                    },
                    ..v
                };
                self.node(*child, sub);
            }
            Node::Degrade {
                child,
                threshold,
                id,
            } => {
                let f = Filter {
                    id: *id,
                    threshold: *threshold,
                    parent: v.filter,
                };
                self.node(
                    *child,
                    Visit {
                        filter: Some(&f),
                        ..v
                    },
                );
            }
        }
        Some(())
    }

    /// Visit one sequence slot in cycle `c`: the child's cycle `c` fills
    /// `[slot_begin, slot_begin + len)`, so inner `t` is outer `slot_begin + (t - c) * len`.
    fn step(
        &mut self,
        step: &SeqStep,
        index: u64,
        c: i64,
        slot_begin: Pos,
        v: Visit<'_>,
    ) -> Option<()> {
        let slot = PSpan {
            begin: slot_begin,
            end: slot_begin.add_frac(step.len)?,
        };
        let clipped = slot.intersect(&v.span);
        if clipped.is_empty() {
            return Some(());
        }
        let cycle = Pos::int(c);
        let to_child = |t: Pos| t.sub(slot_begin)?.div(step.len)?.add(cycle);
        let a = v.map.a.checked_mul(step.len)?;
        // b = map(slot_begin) - c * a, so that map'(c) = map(slot_begin).
        let at_slot = v.map.apply(slot_begin)?;
        let sub = Visit {
            span: PSpan {
                begin: to_child(clipped.begin)?,
                end: to_child(clipped.end)?,
            },
            map: Map {
                a,
                b: at_slot.sub(cycle.mul(a)?)?,
            },
            clip: v.clip.intersect(&v.map.span(slot)?),
            slot_end: {
                let end = v.map.apply(slot.end)?;
                Some(v.slot_end.map_or(end, |e| e.min(end)))
            },
            filter: v.filter,
            // Copies and pulses share one child; the path tells their random draws apart.
            path: mix(v.path ^ index),
        };
        self.node(step.node, sub);
        Some(())
    }
}

impl Pattern {
    /// Write every note whose onset lies in cycle `cycle`, that is in `[cycle, cycle + 1)`, into
    /// `out`, and report how many were written, dropped for lack of room, or lost.
    ///
    /// Never allocates, locks, or panics; safe on the Render thread. `seed` selects the random
    /// outcome of every `?` and `|`: the same pattern, cycle and seed always give the same notes.
    /// Work is at most [`Pattern::max_cycle_work`] units, so the call never exhausts the budget.
    ///
    /// Notes come in walk order, not time order; sort the written prefix if order matters.
    ///
    /// # Stack
    ///
    /// The walk recurses once per tree level, at most [`MAX_DEPTH`](crate::MAX_DEPTH) deep. Measured
    /// on x86_64 for patterns at that depth, a query needs up to 40 KiB of stack in a release build
    /// and up to 160 KiB unoptimised; give the calling thread at least 48 KiB (release) or
    /// 192 KiB (debug).
    pub fn cycle(&self, cycle: i64, seed: u64, out: &mut [Note]) -> CycleResult {
        let mut walk = Walk {
            pat: self,
            seed,
            origin: Pos::int(cycle),
            out,
            res: CycleResult::default(),
            work: 0,
        };
        if let Some(root) = PSpan::cycle(cycle) {
            let visit = Visit {
                span: root,
                map: Map {
                    a: Rational::ONE,
                    b: Pos::int(0),
                },
                clip: root,
                slot_end: None,
                filter: None,
                path: 0,
            };
            walk.node(self.root, visit);
        } else {
            walk.res.overflowed += 1;
        }
        walk.res.work = walk.work;
        walk.res
    }
}
