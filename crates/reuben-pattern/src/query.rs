//! The Render-side query: a recursive walk of the compiled tree into a caller's buffer.
//!
//! Each node is queried with a span in its own time and an affine map from its own time to the
//! caller's (`outer = a * inner + b`). Leaves emit events already in caller time, so nothing is
//! transformed on the way back up and no intermediate event list exists. The walk allocates
//! nothing: its only state is the recursion itself, bounded by [`MAX_DEPTH`](crate::MAX_DEPTH),
//! and a stack-linked chain of active `?` filters.

use crate::compile::Node;
use crate::time::{Rational, Span};
use crate::{Pattern, Value};

/// One event fragment: a value, the full extent of its event, and the part of it inside the query.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Event {
    /// The whole event, onset to offset. May extend past the query span on either side.
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

    /// `true` if this fragment holds the event's onset, i.e. a note-on belongs in this query.
    /// Across any partition of time into adjacent queries, each event's onset is in exactly one.
    pub fn has_onset(&self) -> bool {
        self.part.begin == self.whole.begin
    }

    /// `true` if this fragment holds the event's offset, i.e. its note-off belongs in this query.
    pub fn has_offset(&self) -> bool {
        self.part.end == self.whole.end
    }
}

/// What a query wrote, and what it could not.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QueryResult {
    /// Events written to the front of the buffer.
    pub written: usize,
    /// Events the query produced after the buffer was full. They are lost; the first
    /// `written` events are a prefix of the full answer in walk order.
    pub dropped: usize,
    /// Sub-queries abandoned because a time computation overflowed `i64` (see
    /// [`Rational`]). Each one hides an unknown number of events. Zero for spans of realistic
    /// size; nonzero means the caller's span endpoints have very large numerators or denominators.
    pub overflowed: usize,
}

#[derive(Clone, Copy)]
struct Map {
    a: Rational,
    b: Rational,
}

impl Map {
    fn apply(&self, t: Rational) -> Option<Rational> {
        self.a.checked_mul(t)?.checked_add(self.b)
    }
    fn span(&self, s: Span) -> Option<Span> {
        Some(Span::new(self.apply(s.begin)?, self.apply(s.end)?))
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

/// The only source of randomness: a pure function of the seed, a node's structural id, and a
/// position in time. Same inputs, same output, on every platform.
fn hash(seed: u64, id: u32, a: i64, b: i64) -> u64 {
    mix(mix(mix(seed ^ (u64::from(id) << 32)) ^ a as u64) ^ b as u64)
}

struct Walk<'p, 'o> {
    pat: &'p Pattern,
    seed: u64,
    out: &'o mut [Event],
    res: QueryResult,
}

/// Iterate the integer cycles that `span` touches.
fn cycles(span: Span) -> impl Iterator<Item = i64> {
    let first = span.begin.floor();
    // `end` is exclusive, so a span ending exactly on a cycle boundary does not touch that cycle.
    let last = span.end.floor() - i64::from(Rational::int(span.end.floor()) == span.end);
    first..=last
}

impl Walk<'_, '_> {
    fn emit(&mut self, ev: Event) {
        if let Some(slot) = self.out.get_mut(self.res.written) {
            *slot = ev;
            self.res.written += 1;
        } else {
            self.res.dropped += 1;
        }
    }

    fn removed(&self, onset: Rational, mut filter: Option<&Filter<'_>>) -> bool {
        while let Some(f) = filter {
            let h = hash(self.seed, f.id, onset.numer(), onset.denom());
            if u128::from(h) < f.threshold {
                return true;
            }
            filter = f.parent;
        }
        false
    }

    fn node(&mut self, id: u32, span: Span, map: Map, filter: Option<&Filter<'_>>) {
        if span.is_empty() {
            return;
        }
        if self.node_inner(id, span, map, filter).is_none() {
            self.res.overflowed += 1;
        }
    }

    fn node_inner(
        &mut self,
        id: u32,
        span: Span,
        map: Map,
        filter: Option<&Filter<'_>>,
    ) -> Option<()> {
        let pat = self.pat;
        match &pat.nodes[id as usize] {
            Node::Silence => {}
            Node::Atom(value) => {
                for c in cycles(span) {
                    let whole = Span::new(Rational::int(c), Rational::int(c.checked_add(1)?));
                    let part = whole.intersect(&span);
                    if part.is_empty() {
                        continue;
                    }
                    let whole = map.span(whole)?;
                    if self.removed(whole.begin, filter) {
                        continue;
                    }
                    let part = map.span(part)?;
                    self.emit(Event {
                        whole,
                        part,
                        value: *value,
                    });
                }
            }
            Node::Seq { steps } => {
                let steps = &pat.steps[steps.0 as usize..steps.1 as usize];
                for c in cycles(span) {
                    let cycle = Rational::int(c);
                    for step in steps {
                        let slot_begin = cycle.checked_add(step.begin)?;
                        if slot_begin >= span.end {
                            break;
                        }
                        let slot = Span::new(slot_begin, slot_begin.checked_add(step.len)?);
                        let clipped = slot.intersect(&span);
                        if clipped.is_empty() {
                            continue;
                        }
                        // The child's cycle `c` fills this slot: inner t ↦ slot_begin + (t - c) * len.
                        let to_child = |t: Rational| -> Option<Rational> {
                            t.checked_sub(slot_begin)?
                                .checked_div(step.len)?
                                .checked_add(cycle)
                        };
                        let child_span =
                            Span::new(to_child(clipped.begin)?, to_child(clipped.end)?);
                        let offset = slot_begin.checked_sub(cycle.checked_mul(step.len)?)?;
                        let child_map = Map {
                            a: map.a.checked_mul(step.len)?,
                            b: map.a.checked_mul(offset)?.checked_add(map.b)?,
                        };
                        self.node(step.node, child_span, child_map, filter);
                    }
                }
            }
            Node::Stack { children } => {
                for &child in &pat.children[children.0 as usize..children.1 as usize] {
                    self.node(child, span, map, filter);
                }
            }
            Node::Choice { children, id } => {
                let children = &pat.children[children.0 as usize..children.1 as usize];
                for c in cycles(span) {
                    let h = hash(self.seed, *id, c, 0);
                    let pick = ((u128::from(h) * children.len() as u128) >> 64) as usize;
                    let cycle = Span::new(Rational::int(c), Rational::int(c.checked_add(1)?));
                    self.node(children[pick], cycle.intersect(&span), map, filter);
                }
            }
            Node::Fast { child, factor } => {
                let child_span = Span::new(
                    span.begin.checked_mul(*factor)?,
                    span.end.checked_mul(*factor)?,
                );
                let child_map = Map {
                    a: map.a.checked_div(*factor)?,
                    b: map.b,
                };
                self.node(*child, child_span, child_map, filter);
            }
            Node::Degrade {
                child,
                threshold,
                id,
            } => {
                let f = Filter {
                    id: *id,
                    threshold: *threshold,
                    parent: filter,
                };
                self.node(*child, span, map, Some(&f));
            }
        }
        Some(())
    }
}

impl Pattern {
    /// Write the events overlapping `span` into `out`, and report how many were written, dropped
    /// for lack of room, and lost to overflow.
    ///
    /// Never allocates, locks, or panics; safe on the Render thread. `seed` selects the random
    /// outcome of every `?` and `|`: the same pattern, span, and seed always give the same events.
    ///
    /// Events come in walk order, not time order; sort the written prefix if order matters. An
    /// event that crosses an edge of `span` is returned as the fragment inside it (see
    /// [`Event::part`]), so splitting a span into adjacent queries yields every onset exactly once.
    pub fn query(&self, span: Span, seed: u64, out: &mut [Event]) -> QueryResult {
        let mut walk = Walk {
            pat: self,
            seed,
            out,
            res: QueryResult::default(),
        };
        let identity = Map {
            a: Rational::ONE,
            b: Rational::ZERO,
        };
        walk.node(self.root, span, identity, None);
        walk.res
    }
}
