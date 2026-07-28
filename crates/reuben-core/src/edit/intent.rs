//! The intent verb's machinery: one curated word resolved into a batch of value edits, the
//! arithmetic that sizes each one, and the report that says what landed.
//!
//! Everything here runs inside the one mutation closure
//! [`set_instrument_inputs_by_intent`](super::set_instrument_inputs_by_intent) hands to
//! `edit_existing`, so the batch is atomic: one validation, one write, one hash, and a
//! half-applied document that cannot be represented.
//!
//! see rules: agent-mcp

use std::collections::BTreeSet;

use crate::descriptor::Port;
use crate::format::{
    parse_wire, pipe_descriptor, ConfigValue, InputPipeDoc, InputValue, InstrumentDoc,
    InterfaceEntry, PipeDefault, PIPE_INPUT_PORT,
};
use crate::introspect::widen;
use crate::projection::{Scalar, Selection};
use crate::vocabulary::{Direction, Magnitude, Move, PortClass, Section, Vocabulary};
use crate::Registry;

use super::{address_target, input_pipe_mut, AddressTarget, EditError};

// --- the step rule ---------------------------------------------------------------------------

// Per **curve class**, not per range. A fraction of the declared range is right only where the
// declared range *is* the musical range; on a wide port it is a safety envelope, and a fraction of
// it produces nonsense (a *slightly faster* clock reading 120 -> 220 BPM). A ratio is the right
// geometry for a port declared exponential, and one is the smallest musical unit on a count — so
// `slightly` and the default step collapse there. Frozen first guesses, not tunable.
// see rules: agent-mcp

/// The small step on a linear port, as a fraction of its declared range.
const SLIGHT_FRACTION: f64 = 0.10;
/// The default step on a linear port, as a fraction of its declared range.
const DEFAULT_FRACTION: f64 = 0.25;
/// The small step on an exponential port, as a ratio. The range never enters.
const SLIGHT_RATIO: f64 = 1.2;
/// The default step on an exponential port, as a ratio.
const DEFAULT_RATIO: f64 = 1.5;
/// The step on an integer port, in whole counts.
const INT_STEP: i64 = 1;

/// Round away binary-fraction noise without inventing precision. Six significant digits is far more
/// than any control's audible resolution and far less than an `f64`'s, so the step is stable across
/// repeats — and the document does not grow `2500.0000000000005` where it means `2500`.
fn tidy(v: f64) -> f64 {
    if v == 0.0 || !v.is_finite() {
        return v;
    }
    let digits = 5 - v.abs().log10().floor() as i32;
    let scale = 10f64.powi(digits.clamp(-30, 30));
    (v * scale).round() / scale
}

/// The port's own unwired value — what a slot holds when the document has written nothing into it.
fn port_default(class: PortClass<'_>) -> Option<Scalar> {
    match class {
        PortClass::Scalar(m) => Some(Scalar::Number(widen(m.default))),
        PortClass::Int(m) => Some(Scalar::Number(f64::from(m.default))),
        PortClass::Enum(e) => Some(Scalar::Symbol(e.default_symbol().to_string())),
        PortClass::Unmovable => None,
    }
}

/// Apply one move's direction + magnitude to `from`, against the port's own quantity contract.
/// `Err` is a reason to skip this target, never a reason to fail the batch.
fn step(class: PortClass<'_>, from: &Scalar, m: &Move) -> Result<Scalar, String> {
    let number = |s: &Scalar| -> Result<f64, String> {
        match s {
            Scalar::Number(n) => Ok(*n),
            Scalar::Symbol(sym) => Err(format!(
                "holds the symbol {sym:?}, which is not a number this move can arrive at"
            )),
        }
    };
    let up = m.direction == Direction::Up;
    // Checked before the arms below rather than relied on from the sweep: a `set` that fell through
    // to them would read as a relative move and quietly step the port the wrong way.
    if m.direction == Direction::Set
        && !matches!(
            m.magnitude,
            Some(Magnitude::Absolute(_) | Magnitude::Symbol(_))
        )
    {
        return Err("is a `set` with nothing to set".to_string());
    }
    match (class, m.direction, m.magnitude.as_ref()) {
        // An assignment: the row already carries the destination.
        (PortClass::Enum(e), Direction::Set, Some(Magnitude::Symbol(s))) => {
            if !e.variants.contains(&s.as_str()) {
                return Err(format!("has no variant {s:?}"));
            }
            Ok(Scalar::Symbol(s.clone()))
        }
        (PortClass::Scalar(meta), Direction::Set, Some(Magnitude::Absolute(v))) => {
            Ok(Scalar::Number(v.clamp(widen(meta.min), widen(meta.max))))
        }
        (PortClass::Int(meta), Direction::Set, Some(Magnitude::Absolute(v))) => Ok(Scalar::Number(
            v.round().clamp(f64::from(meta.min), f64::from(meta.max)),
        )),

        // A count: one is the smallest musical unit, so `slightly` and the default are one move.
        (PortClass::Int(meta), _, magnitude) => {
            let steps = match magnitude {
                None | Some(Magnitude::Slightly) => INT_STEP,
                Some(Magnitude::Steps(n)) => *n,
                Some(other) => return Err(format!("is a count and cannot move by ({other})")),
            };
            let signed = if up { steps } else { -steps };
            let moved = number(from)?.round() as i64 + signed;
            Ok(Scalar::Number(
                (moved as f64).clamp(f64::from(meta.min), f64::from(meta.max)),
            ))
        }

        // A swept scalar: a ratio where the port declares an exponential response, a fraction of
        // the range where it declares a linear one.
        (PortClass::Scalar(meta), _, magnitude) => {
            let v = number(from)?;
            let moved = match magnitude {
                Some(Magnitude::By(by)) => {
                    if up {
                        v + by
                    } else {
                        v - by
                    }
                }
                Some(Magnitude::Ratio(r)) => {
                    if up {
                        v * r
                    } else {
                        v / r
                    }
                }
                None | Some(Magnitude::Slightly) => {
                    let slight = magnitude.is_some();
                    match meta.curve {
                        crate::descriptor::Curve::Exponential => {
                            let ratio = if slight { SLIGHT_RATIO } else { DEFAULT_RATIO };
                            if up {
                                v * ratio
                            } else {
                                v / ratio
                            }
                        }
                        crate::descriptor::Curve::Linear => {
                            let fraction = if slight {
                                SLIGHT_FRACTION
                            } else {
                                DEFAULT_FRACTION
                            };
                            let delta = fraction * (widen(meta.max) - widen(meta.min));
                            if up {
                                v + delta
                            } else {
                                v - delta
                            }
                        }
                    }
                }
                Some(other) => return Err(format!("cannot move by ({other})")),
            };
            Ok(Scalar::Number(
                tidy(moved).clamp(widen(meta.min), widen(meta.max)),
            ))
        }

        (PortClass::Enum(_), _, _) => Err("is a closed set with no direction to push".to_string()),
        (PortClass::Unmovable, _, _) => Err("carries no value an intent move can move".to_string()),
    }
}

// --- the report ------------------------------------------------------------------------------

/// One move that landed: where, what was there, and what is there now — plus the row's own hedge,
/// so the curated caveat arrives *after* the act rather than as a branch before it.
pub(super) struct AppliedMove {
    address: String,
    input: String,
    from: Option<Scalar>,
    to: Scalar,
    description: Option<String>,
}

/// One move with nowhere to land, named the way the row names it: by operator type and input, not
/// by an address that does not exist.
pub(super) struct SkippedMove {
    op: String,
    input: String,
    reason: String,
}

/// The whole batch's effect. Not a projection: a value edit's echo is the change, and nine node
/// zooms would be larger than the document read this verb exists to avoid.
pub(super) struct IntentReport {
    word: String,
    section: Section,
    /// The other sections this word also has a row in — the readings passed over.
    alternatives: Vec<Section>,
    applied: Vec<AppliedMove>,
    skipped: Vec<SkippedMove>,
    /// `target` terms that named nothing this row could move.
    unmatched: Vec<String>,
}

impl IntentReport {
    pub(super) fn render(&self) -> String {
        let mut out = format!(
            "{} ({}): {} applied, {} skipped",
            self.word,
            self.section.name(),
            self.applied.len(),
            self.skipped.len()
        );
        for other in &self.alternatives {
            out.push_str(&format!(
                "\nalso a {} word — pass section {:?} to take that reading instead",
                other.name(),
                other.name()
            ));
        }
        for a in &self.applied {
            let from = match &a.from {
                Some(v) => v.render(),
                None => "(unset)".to_string(),
            };
            out.push_str(&format!(
                "\n{}.{} {from} → {}",
                a.address,
                a.input,
                a.to.render()
            ));
            if let Some(d) = &a.description {
                out.push_str(&format!(" [{d}]"));
            }
        }
        for s in &self.skipped {
            out.push_str(&format!("\nskipped {}.{} — {}", s.op, s.input, s.reason));
        }
        if !self.unmatched.is_empty() {
            out.push_str(&format!("\nunmatched: {}", self.unmatched.join(", ")));
        }
        out
    }
}

// --- resolution ------------------------------------------------------------------------------

/// Where a move's value actually lives, once the wire is followed.
enum Slot {
    /// A literal on the node itself: an input, or a plan-time constant in its `config` block.
    Node { idx: usize, constant: bool },
    /// The interface input pipe feeding this input. Its seed is the scalar a human authored, so
    /// moving that turns the knob instead of unplugging the cable.
    Pipe { name: String },
}

/// Does this node pass the caller's narrowing? The row's `op` is already the type predicate, so an
/// empty `target` is the whole match set.
fn selected(sel: &Selection, address: &str, type_name: &str) -> bool {
    match sel {
        Selection::All => true,
        Selection::Names(names) => names.iter().any(|n| n == address),
        Selection::Type(ty) => ty == type_name,
    }
}

/// Apply one intent word to the document. The whole batch, in curated move order and document node
/// order within a move, deduplicated by the slot each target resolves to.
pub(super) fn apply(
    doc: &mut InstrumentDoc,
    registry: &Registry,
    word: &str,
    section: Option<Section>,
    target: &[String],
) -> Result<IntentReport, EditError> {
    let vocabulary = Vocabulary::builtin();
    let (row, alternatives) = vocabulary.row(word, section).map_err(EditError::Target)?;
    let selection = Selection::from_terms(target, None).map_err(EditError::Target)?;

    let mut applied: Vec<AppliedMove> = Vec::new();
    let mut skipped: Vec<SkippedMove> = Vec::new();
    // The destination, not the address the caller reached it through: `/clock.tempo` and
    // `/pad_clock.tempo` are one pipe seed, so they are one edit reported once.
    let mut written: BTreeSet<(String, String)> = BTreeSet::new();
    let mut matched_terms: BTreeSet<String> = BTreeSet::new();

    for m in &row.moves {
        let Some(reg) = registry.get(&m.op) else {
            skipped.push(SkippedMove {
                op: m.op.clone(),
                input: m.input.clone(),
                reason: format!("`{}` is not a registered operator", m.op),
            });
            continue;
        };
        let descriptor = &reg.descriptor;
        let Some(port) = descriptor
            .inputs
            .iter()
            .chain(descriptor.constants.iter())
            .find(|p| p.name == m.input)
        else {
            skipped.push(SkippedMove {
                op: m.op.clone(),
                input: m.input.clone(),
                reason: format!("`{}` has no input named `{}`", m.op, m.input),
            });
            continue;
        };
        let constant = descriptor.constants.iter().any(|p| p.name == m.input);

        let candidates: Vec<usize> = doc
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.type_name == m.op && selected(&selection, &n.address, &n.type_name))
            .map(|(i, _)| i)
            .collect();
        for &idx in &candidates {
            matched_terms.insert(doc.nodes[idx].address.clone());
        }
        if candidates.is_empty() {
            skipped.push(SkippedMove {
                op: m.op.clone(),
                input: m.input.clone(),
                reason: match &selection {
                    Selection::All => format!("no `{}` node in the document", m.op),
                    _ => format!("no `{}` node inside the given target", m.op),
                },
            });
            continue;
        }

        // The row's hedge belongs to the *move*, not to each target it reaches, so it is said once.
        // Eight copies of one curated sentence is what a per-target echo of it costs on the widest
        // fan-out in the library.
        let mut said = false;
        for idx in candidates {
            match one(doc, idx, port, constant, m, &mut written) {
                Ok(Some(mut landed)) => {
                    if std::mem::replace(&mut said, true) {
                        landed.description = None;
                    }
                    applied.push(landed);
                }
                // Already written through another address — the dedupe, not a skip.
                Ok(None) => {}
                Err(reason) => skipped.push(SkippedMove {
                    op: m.op.clone(),
                    input: m.input.clone(),
                    reason,
                }),
            }
        }
    }

    let unmatched: Vec<String> = match &selection {
        Selection::Names(names) => names
            .iter()
            .filter(|n| !matched_terms.contains(n.as_str()))
            .cloned()
            .collect(),
        _ => Vec::new(),
    };

    Ok(IntentReport {
        word: row.word.clone(),
        section: row.section,
        alternatives,
        applied,
        skipped,
        unmatched,
    })
}

/// Move one node's slot: follow the wire, size the step against whatever contract the slot's own
/// port declares, and write it. `Ok(None)` is the dedupe — this destination was already moved.
fn one(
    doc: &mut InstrumentDoc,
    idx: usize,
    port: &Port,
    constant: bool,
    m: &Move,
    written: &mut BTreeSet<(String, String)>,
) -> Result<Option<AppliedMove>, String> {
    let node_address = doc.nodes[idx].address.clone();
    let slot = match (constant, doc.nodes[idx].inputs.get(&m.input)) {
        (false, Some(InputValue::Wire { from })) => {
            let (source, _) = parse_wire(from);
            match address_target(doc, source) {
                Ok(AddressTarget::Pipe(name)) => Slot::Pipe { name },
                _ => {
                    return Err(format!(
                        "`{node_address}.{}` is wired from `{from}`, a modulation source with no \
                         value to move",
                        m.input
                    ))
                }
            }
        }
        _ => Slot::Node { idx, constant },
    };

    // The slot's own quantity contract: an operator port's declared meta, or — for a pipe — the
    // range and curve a human authored on the boundary, which is the range that means something
    // musically. Both arms hand back the same shape, so the arithmetic below sees one.
    let (address, input, slot_port, held) = match &slot {
        Slot::Node { idx, constant } => {
            let node = &doc.nodes[*idx];
            let held = if *constant {
                node.config.get(&m.input).map(super::config_scalar)
            } else {
                node.inputs.get(&m.input).and_then(super::input_scalar)
            };
            (node_address, m.input.clone(), port.clone(), held)
        }
        Slot::Pipe { name } => {
            let pipe = input_pipe(doc, name);
            let (descriptor, _) = pipe_descriptor(name, pipe).map_err(|e| e.to_string())?;
            let in_port = descriptor
                .inputs
                .into_iter()
                .find(|p| p.name == PIPE_INPUT_PORT)
                .expect("a pipe descriptor declares its one input port");
            (
                format!("/{name}"),
                PIPE_INPUT_PORT.to_string(),
                in_port,
                pipe.default.as_ref().map(super::seed_scalar),
            )
        }
    };

    if !written.insert((address.clone(), input.clone())) {
        return Ok(None);
    }
    let class = PortClass::of(&slot_port);
    let Some(from) = held.or_else(|| port_default(class)) else {
        return Err(format!(
            "`{address}.{input}` carries no value an intent move can move"
        ));
    };
    let to = step(class, &from, m).map_err(|why| format!("`{address}.{input}` {why}"))?;

    match slot {
        Slot::Node {
            idx,
            constant: true,
        } => {
            doc.nodes[idx].config.insert(input.clone(), config_of(&to));
        }
        Slot::Node { idx, .. } => {
            doc.nodes[idx].inputs.insert(input.clone(), literal_of(&to));
        }
        Slot::Pipe { name } => input_pipe_mut(doc, &name).default = Some(seed_of(&to)),
    }
    Ok(Some(AppliedMove {
        address,
        input,
        from: Some(from),
        to,
        description: m.description.clone(),
    }))
}

/// The read half of [`input_pipe_mut`]: the pipe an address has already resolved to.
fn input_pipe<'a>(doc: &'a InstrumentDoc, name: &str) -> &'a InputPipeDoc {
    match doc.interface.as_ref().and_then(|i| i.inputs.get(name)) {
        Some(InterfaceEntry::Pipe(pipe)) => pipe,
        _ => unreachable!("`address_target` resolved this name to an input pipe"),
    }
}

fn literal_of(s: &Scalar) -> InputValue {
    match s {
        Scalar::Number(n) => InputValue::Number(*n),
        Scalar::Symbol(sym) => InputValue::Symbol(sym.clone()),
    }
}

fn config_of(s: &Scalar) -> ConfigValue {
    match s {
        Scalar::Number(n) => ConfigValue::Number(*n),
        Scalar::Symbol(sym) => ConfigValue::Symbol(sym.clone()),
    }
}

fn seed_of(s: &Scalar) -> PipeDefault {
    match s {
        Scalar::Number(n) => PipeDefault::Number(*n),
        Scalar::Symbol(sym) => PipeDefault::Symbol(sym.clone()),
    }
}
