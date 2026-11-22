//! The Render-side query: a recursive walk of the compiled tree into a caller's buffer.
//!
//! Each node is visited with three things: the queried span in its own time, an affine map from
//! its own time to the root's (`root = a * t + b`), and a clip window in root time (the query span
//! cut by every enclosing slot). A leaf maps its whole event up through the map and takes the part
//! as the whole cut by the clip window, so nothing is transformed on the way back up and no
//! intermediate event list exists.
//!
//! Times are [`Pos`] (integer cycle plus fraction), so a fraction's terms stay bounded by the span's
//! denominator and the compile-time limits however many cycles have elapsed. The walk allocates
//! nothing: its state is the recursion (bounded by [`MAX_DEPTH`](crate::MAX_DEPTH)) and a
//! stack-linked chain of active `?` filters. Its work is bounded by [`QUERY_BUDGET`].

use crate::compile::{Node, SeqStep};
use crate::time::{Pos, Rational, Span};
use crate::{Pattern, Value};

/// Most units of work one query may do before it stops and sets [`QueryResult::exhausted`]. A unit
/// is one node visit, one cycle a node iterates, or one sequence step it examines. This is the
/// backstop behind [`MAX_DENSITY`](crate::MAX_DENSITY): it bounds every query, whatever the pattern
/// or span. `examples/query_cost.rs` measures a query that spends the whole budget at 0.23-0.53 ms
/// (i7-13620H, release), about 5% of a 512-frame block at 48 kHz. A block of up to 4096 frames at
/// 999 BPM against a pattern at the density cap stays within it.
pub const QUERY_BUDGET: u32 = 1 << 12;

/// One event fragment: a value, the full extent of its event, and the part of it inside the query.
///
/// # How a consumer plays events
///
/// Trigger on [`has_onset`](Event::has_onset) and nothing else: that fragment starts the event, and
/// its `whole.end` is when the event releases (legato). Everything a note needs is in the onset
/// fragment, so the release never depends on a later query.
///
/// A fragment without an onset is a continuation, and a consumer may ignore it. Some continuations
/// belong to no onset any query reports: a slowed pattern inside a sequence slot (`[0 1]/1.5 2`)
/// is cut to its slot each cycle, so in a later cycle its fragment can carry a `whole` that starts
/// before the slot. That matches the Tidal reference's meaning of whole and part (a fragment's
/// `whole` is the event it was cut from), and it is why a fragment's `part.end == whole.end` is
/// not a note-off signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Event {
    /// The event this fragment was cut from, onset to release. May extend past the query span.
    pub whole: Span,
    /// The part of `whole` that lies inside the queried span; never empty.
    pub part: Span,
    /// The event's value.
    pub value: Value,
}

impl Event {
    /// A placeholder for initialising a buffer; never returned by a query.
    pub const EMPTY: Event = Event {
        whole: Span::new(Rational::ZERO, Rational::ZERO),
        part: Span::new(Rational::ZERO, Rational::ZERO),
        value: Value::Number(Rational::ZERO),
    };

    /// `true` if this fragment holds the event's onset: the note-on belongs in this query, and the
    /// release is at `whole.end`. Across any partition of time into adjacent queries, each onset is
    /// in exactly one, with the same `whole`.
    pub fn has_onset(&self) -> bool {
        self.part.begin == self.whole.begin
    }
}

/// What a query wrote, and what it could not.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QueryResult {
    /// Events written to the front of the buffer.
    pub written: usize,
    /// Events the query produced after the buffer was full. They are lost; the first `written`
    /// events are a prefix of the full answer in walk order.
    pub dropped: usize,
    /// Sub-queries abandoned because a time computation overflowed `i64` (see [`Rational`]). Each
    /// hides an unknown number of events. Cannot happen for a span whose denominator is at most
    /// `2^43` (see [`MAX_GROWTH`](crate::MAX_GROWTH)).
    pub overflowed: usize,
    /// `true` if the walk stopped after [`QUERY_BUDGET`] units of work. The events written are
    /// still correct, but later ones in walk order are missing and not counted in `dropped`.
    pub exhausted: bool,
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
    fn to_span(self) -> Option<Span> {
        Some(Span::new(
            self.begin.to_rational()?,
            self.end.to_rational()?,
        ))
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
    out: &'o mut [Event],
    res: QueryResult,
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

/// Everything a node visit needs besides the node: its span, its map to root time, the root-time
/// window its fragments are cut to, the `?` filters above it, and its structural path.
#[derive(Clone, Copy)]
struct Visit<'f> {
    span: PSpan,
    map: Map,
    clip: PSpan,
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

    fn emit(&mut self, ev: Event) {
        if let Some(slot) = self.out.get_mut(self.res.written) {
            *slot = ev;
            self.res.written += 1;
        } else {
            self.res.dropped += 1;
        }
    }

    fn removed(&self, onset: Pos, path: u64, mut filter: Option<&Filter<'_>>) -> bool {
        while let Some(f) = filter {
            let h = hash(
                self.seed,
                f.id,
                path,
                onset.cyc,
                onset.frac.numer(),
                onset.frac.denom(),
            );
            if u128::from(h) < f.threshold {
                return true;
            }
            filter = f.parent;
        }
        false
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
                for c in cycles(v.span) {
                    if !self.spend() {
                        return Some(());
                    }
                    let whole = v.map.span(PSpan::cycle(c)?)?;
                    let part = whole.intersect(&v.clip);
                    if part.is_empty() || self.removed(whole.begin, v.path, v.filter) {
                        continue;
                    }
                    self.emit(Event {
                        whole: whole.to_span()?,
                        part: part.to_span()?,
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
                for &child in &pat.children[children.0 as usize..children.1 as usize] {
                    self.node(child, v);
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
            filter: v.filter,
            // Copies and pulses share one child; the path tells their random draws apart.
            path: mix(v.path ^ index),
        };
        self.node(step.node, sub);
        Some(())
    }
}

impl Pattern {
    /// Write the events overlapping `span` into `out`, and report how many were written, dropped
    /// for lack of room, lost to overflow, or cut off by the work budget.
    ///
    /// Never allocates, locks, or panics; safe on the Render thread. `seed` selects the random
    /// outcome of every `?` and `|`: the same pattern, span and seed always give the same events.
    /// Work is capped at [`QUERY_BUDGET`] units, so the worst case is bounded whatever the pattern.
    ///
    /// Events come in walk order, not time order; sort the written prefix if order matters. An
    /// event that crosses an edge of `span` is returned as the fragment inside it (see [`Event`]),
    /// so splitting a span into adjacent queries yields every onset exactly once.
    ///
    /// # Stack
    ///
    /// The walk recurses once per tree level, at most [`MAX_DEPTH`](crate::MAX_DEPTH) deep. Measured
    /// on x86_64 for a pattern at that depth, a query needs up to 28 KiB of stack in a release
    /// build and up to 112 KiB unoptimised; give the calling thread at least 32 KiB (release) or
    /// 128 KiB (debug) to spare.
    pub fn query(&self, span: Span, seed: u64, out: &mut [Event]) -> QueryResult {
        let mut walk = Walk {
            pat: self,
            seed,
            out,
            res: QueryResult::default(),
            work: 0,
        };
        let root = PSpan {
            begin: Pos::from_rational(span.begin),
            end: Pos::from_rational(span.end),
        };
        let visit = Visit {
            span: root,
            map: Map {
                a: Rational::ONE,
                b: Pos::int(0),
            },
            clip: root,
            filter: None,
            path: 0,
        };
        walk.node(self.root, visit);
        walk.res
    }
}
