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

use crate::descriptor::{Port, PortType};
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

/// The small step on a linear port, as a fraction of its declared range.
const SLIGHT_FRACTION: f64 = 0.10;
/// The default step on a linear port, as a fraction of its declared range.
const DEFAULT_FRACTION: f64 = 0.25;
/// The small step on an exponential port, as a ratio. The range never enters.
const SLIGHT_RATIO: f64 = 1.2;
/// The default step on an exponential port, as a ratio.
const DEFAULT_RATIO: f64 = 1.5;
/// The step on an integer port, in whole counts — one is the smallest musical unit on a count, so
/// `slightly` and the default step are the same move there.
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

/// Land a computed number: tidy it, then hold it inside the declared range — refusing anything that
/// is not a finite value inside a finite range.
///
/// The finiteness check is not defensive tidiness. JSON has no spelling for an infinity, so serde
/// writes one as `null`: an unguarded `inf` would *erase* the slot on the way to disk, and a null
/// seed is legal, so write-iff-valid would report the erasure as a successful edit.
fn land(moved: f64, min: f64, max: f64) -> Result<Scalar, String> {
    if !(moved.is_finite() && min.is_finite() && max.is_finite() && min <= max) {
        return Err(format!(
            "would move to {moved} within [{min}..{max}], which is not a finite value in a finite \
             range — check the declared min/max"
        ));
    }
    Ok(Scalar::Number(tidy(moved).clamp(min, max)))
}

/// Assign the exact value a `set` names, refusing when the range will not hold it.
///
/// Clamping is right for a relative move — *warmer* just means as far down as this port goes — and
/// wrong for an assignment, which names a specific thing: a minor 3rd, a rotation of 0. A clamped
/// assignment is a *different* value, and the row's own description then labels it, so
/// `s2 → 5` would be reported as "a minor 3rd" while being a perfect 4th. Out of range is not a
/// smaller move here; it is another one. see rules: agent-mcp
fn assign(asked: f64, min: f64, max: f64) -> Result<Scalar, String> {
    match land(asked, min, max)? {
        Scalar::Number(got) if got == asked => Ok(Scalar::Number(got)),
        _ => Err(format!(
            "cannot be set to {asked} — its range here is [{min}..{max}], and a clamped assignment \
             would be a different value than the one this move names"
        )),
    }
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
            Scalar::Number(n) if n.is_finite() => Ok(*n),
            Scalar::Number(n) => Err(format!(
                "holds {n}, which is not a finite value to move from"
            )),
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
            assign(*v, widen(meta.min), widen(meta.max))
        }
        (PortClass::Int(meta), Direction::Set, Some(Magnitude::Absolute(v))) => {
            assign(v.round(), f64::from(meta.min), f64::from(meta.max))
        }

        (PortClass::Int(meta), _, magnitude) => {
            let steps = match magnitude {
                None | Some(Magnitude::Slightly) => INT_STEP,
                Some(other) => match other {
                    Magnitude::Steps(n) => *n,
                    _ => return Err(format!("is a count and cannot move by ({other})")),
                },
            };
            let signed = if up { steps } else { -steps };
            let moved = number(from)?.round() as i64 + signed;
            land(moved as f64, f64::from(meta.min), f64::from(meta.max))
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
            land(moved, widen(meta.min), widen(meta.max))
        }

        (PortClass::Enum(_), _, _) => Err("is a closed set with no direction to push".to_string()),
        (PortClass::Unmovable, _, _) => Err("carries no value an intent move can move".to_string()),
    }
}

// --- the report ------------------------------------------------------------------------------

/// One move that landed: where, what was there, and what is there now — plus the row's own hedge.
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

/// Record a skip, unless the same op, input and reason are already on the list.
///
/// The reason carries the address it is about, so this is a slot-level dedupe: three consumers of
/// one at-its-ceiling pipe are one problem, and three copies of it read as three. The successful
/// path already collapses those same three to one applied line.
fn note_skip(skipped: &mut Vec<SkippedMove>, op: &str, input: &str, reason: String) {
    if skipped
        .iter()
        .any(|s| s.op == op && s.input == input && s.reason == reason)
    {
        return;
    }
    skipped.push(SkippedMove {
        op: op.to_string(),
        input: input.to_string(),
        reason,
    });
}

/// The whole batch's effect: what moved, what did not, and the two things the caller might have
/// meant differently — a reading passed over, and a `target` term that named nothing.
/// see rules: agent-mcp
pub(super) struct IntentReport {
    word: String,
    section: Section,
    /// The other sections this word also has a row in — the readings passed over.
    alternatives: Vec<Section>,
    applied: Vec<AppliedMove>,
    skipped: Vec<SkippedMove>,
    /// `target` terms that named nothing this row could move.
    unmatched: Vec<String>,
    /// Caveats about a move that *did* land, for the edit's notes.
    pub(super) notes: Vec<String>,
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

/// Where a move's value lives, once the wire is followed.
enum Slot {
    /// A literal on the node itself: an input, or a plan-time constant in its `config` block.
    Node { idx: usize, constant: bool },
    /// The interface input pipe feeding this input. Its seed is the scalar a human authored, so
    /// moving that turns the knob instead of unplugging the cable.
    Pipe { name: String },
}

/// One resolved target: everything the narrowing, the arithmetic and the report need, worked out
/// before the document is touched.
struct Target {
    slot: Slot,
    /// The address the report names — the node's, or the pipe's when the wire was followed.
    address: String,
    input: String,
    /// The quantity contract to move against, already merged for a pipe.
    port: Port,
    /// What the document currently holds in the slot, if anything.
    held: Option<Scalar>,
    /// The logical input channel a pipe binds, when it binds one.
    channel: Option<usize>,
}

/// The quantity contract an intent move sizes a pipe's seed against: what the pipe **declares**,
/// falling back field by field to the port it feeds.
///
/// An omitted `min`/`max` is not a range — the loader fills it with the type-wide ±1e6 sentinel, and
/// a quarter of that is ±500,000 written into the document. An omitted `curve` is not an assertion
/// of linearity either. What the pipe says wins, because that is the range a human authored; what it
/// does not say, the port it feeds already knows. see rules: agent-mcp
fn pipe_contract(pipe: &InputPipeDoc, mut declared: Port, fed: &Port) -> Port {
    let bare_range = pipe.min.is_none() || pipe.max.is_none();
    // Neither side declares a span to take a fraction of, so there is no contract to move against.
    // Clearing the meta is what turns that into a reported skip instead of a number.
    if bare_range && declared.meta.is_some() && fed.meta.is_none() {
        declared.meta = None;
        return declared;
    }
    if let (Some(meta), Some(port)) = (declared.meta.as_mut(), fed.meta.as_ref()) {
        if pipe.min.is_none() {
            meta.min = port.min;
        }
        if pipe.max.is_none() {
            meta.max = port.max;
        }
        if pipe.curve.is_none() {
            meta.curve = port.curve;
        }
    }
    if let (PortType::I32 { meta: Some(meta) }, PortType::I32 { meta: Some(port) }) =
        (&mut declared.ty, &fed.ty)
    {
        if pipe.min.is_none() {
            meta.min = port.min;
        }
        if pipe.max.is_none() {
            meta.max = port.max;
        }
    }
    declared
}

/// Does one of the addresses this target is reachable through pass the caller's narrowing?
///
/// A pipe address counts, because it is the address the report hands back: narrowing by an address
/// this verb just echoed has to reach the same slot. see rules: agent-mcp
fn selected(sel: &Selection, addresses: &[&str]) -> bool {
    match sel {
        Selection::All => true,
        Selection::Names(names) => names.iter().any(|n| addresses.contains(&n.as_str())),
        // The row's `op` is the type predicate, so the verb exposes no second one and
        // `Selection::from_terms` cannot build this arm from the arguments it is handed.
        Selection::Type(_) => false,
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
    let mut notes: Vec<String> = Vec::new();
    // The destination, not the address the caller reached it through: `/clock.tempo` and
    // `/pad_clock.tempo` are one pipe seed, so they are one edit reported once. Only a slot that was
    // actually *written* lands here, so a slot a later move could still move is never swallowed.
    let mut written: BTreeSet<(String, String)> = BTreeSet::new();
    let mut matched_terms: BTreeSet<String> = BTreeSet::new();

    for m in &row.moves {
        let Some(reg) = registry.get(&m.op) else {
            note_skip(
                &mut skipped,
                &m.op,
                &m.input,
                format!("`{}` is not a registered operator", m.op),
            );
            continue;
        };
        let descriptor = &reg.descriptor;
        let Some(port) = descriptor
            .inputs
            .iter()
            .chain(descriptor.constants.iter())
            .find(|p| p.name == m.input)
        else {
            note_skip(
                &mut skipped,
                &m.op,
                &m.input,
                format!("`{}` has no input named `{}`", m.op, m.input),
            );
            continue;
        };
        let constant = descriptor.constants.iter().any(|p| p.name == m.input);

        let candidates: Vec<usize> = doc
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.type_name == m.op)
            .map(|(i, _)| i)
            .collect();

        // The row's hedge belongs to the *move*, not to each target it reaches, so it is said once.
        // Eight copies of one curated sentence is what a per-target echo of it costs on the widest
        // fan-out in the library.
        let mut said = false;
        let mut narrowed = 0usize;
        for idx in candidates.iter().copied() {
            let node_address = doc.nodes[idx].address.clone();
            // Resolved *before* narrowing, because the address the caller narrows by may be the
            // pipe's rather than the node's — and the pipe's is the one the report handed back.
            let resolved = resolve(doc, idx, port, constant, m);
            let mut reach = vec![node_address.as_str()];
            if let Ok(t) = &resolved {
                reach.push(t.address.as_str());
            }
            for address in &reach {
                matched_terms.insert((*address).to_string());
            }
            if !selected(&selection, &reach) {
                continue;
            }
            narrowed += 1;
            let outcome = match resolved {
                Err(reason) => Err(reason),
                Ok(target) => write_target(doc, target, m, &mut written, &mut notes),
            };
            match outcome {
                Ok(Some(mut landed)) => {
                    if std::mem::replace(&mut said, true) {
                        landed.description = None;
                    }
                    applied.push(landed);
                }
                // Already written through another address — the dedupe, not a skip.
                Ok(None) => {}
                Err(reason) => note_skip(&mut skipped, &m.op, &m.input, reason),
            }
        }
        if narrowed == 0 {
            note_skip(
                &mut skipped,
                &m.op,
                &m.input,
                if candidates.is_empty() {
                    format!("no `{}` node in the document", m.op)
                } else {
                    format!("no `{}` node inside the given target", m.op)
                },
            );
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
        notes,
    })
}

/// Follow the wire and work out what this node's move actually addresses, writing nothing.
fn resolve(
    doc: &InstrumentDoc,
    idx: usize,
    port: &Port,
    constant: bool,
    m: &Move,
) -> Result<Target, String> {
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

    match slot {
        Slot::Node { idx, constant } => {
            let node = &doc.nodes[idx];
            let held = if constant {
                node.config.get(&m.input).map(super::config_scalar)
            } else {
                node.inputs.get(&m.input).and_then(super::input_scalar)
            };
            Ok(Target {
                slot: Slot::Node { idx, constant },
                address: node_address,
                input: m.input.clone(),
                port: port.clone(),
                held,
                channel: None,
            })
        }
        Slot::Pipe { name } => {
            let pipe = input_pipe(doc, &name);
            let minted = pipe_descriptor(&name, pipe).map_err(|e| e.to_string())?;
            let descriptor = minted.descriptor;
            let declared = descriptor
                .inputs
                .into_iter()
                .find(|p| p.name == PIPE_INPUT_PORT)
                .expect("a pipe descriptor declares its one input port");
            Ok(Target {
                address: format!("/{name}"),
                input: PIPE_INPUT_PORT.to_string(),
                port: pipe_contract(pipe, declared, port),
                held: pipe.default.as_ref().map(super::seed_scalar),
                channel: pipe.channel,
                slot: Slot::Pipe { name },
            })
        }
    }
}

/// Size the step and write it. `Ok(None)` is the dedupe — this destination was already moved.
fn write_target(
    doc: &mut InstrumentDoc,
    target: Target,
    m: &Move,
    written: &mut BTreeSet<(String, String)>,
    notes: &mut Vec<String>,
) -> Result<Option<AppliedMove>, String> {
    let Target {
        slot,
        address,
        input,
        port,
        held,
        channel,
    } = target;
    if written.contains(&(address.clone(), input.clone())) {
        return Ok(None);
    }
    let class = PortClass::of(&port);
    let Some(from) = held.or_else(|| port_default(class)) else {
        return Err(format!(
            "`{address}.{input}` carries no value an intent move can move"
        ));
    };
    let to = step(class, &from, m).map_err(|why| format!("`{address}.{input}` {why}"))?;
    // A move that arrives where it started is not a move. This is the one thing the report cannot
    // afford to get wrong: it is the only account of the edit there is, so "3 applied" over a
    // byte-identical document has the agent telling someone it changed the sound.
    // see rules: agent-mcp
    if to == from {
        return Err(format!(
            "`{address}.{input}` is already {}, so this move changes nothing",
            to.render()
        ));
    }

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
    written.insert((address.clone(), input.clone()));
    // A seed on a channel-bound pipe materializes only when nothing feeds that channel, so on a
    // live rig this edit can be silent. The caller never named this pipe — the wire led here — so
    // saying it is the difference between an honest report and a false claim of audibility.
    if let Some(ch) = channel {
        notes.push(format!(
            "`{address}` is bound to input channel {ch}: its value applies only when nothing feeds \
             that channel"
        ));
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
