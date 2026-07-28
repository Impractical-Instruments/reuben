//! The closed document-manipulation vocabulary: the finite set of verbs an agent uses to author an
//! instrument document without ever touching its bytes. Each verb is a pure
//! `(source, …) -> EditResult` function that funnels through [`edit_existing`]/[`finish`]: read,
//! apply one surgical edit, re-validate the whole document, write iff valid, and echo back the
//! post-write hash plus the [`projection`](crate::projection) of what the verb touched. All
//! engine-free — always-available pure tools, never reaching a live engine.
//!
//! There are no transactions: a lone unwired node loads clean and renders silence, so
//! `new → add → add → wire → wire` is valid at every step. [`remove_instrument_node`] and
//! [`remove_instrument_interface_input`] delete an address the rest of the document may still name,
//! so they cascade: they auto-unwire every consumer and report exactly what they broke in
//! [`EditResult::notes`]; [`rename_instrument_node`] rewrites those refs instead of dropping them.
//!
//! **Nothing mechanical proves this vocabulary carves the format at its joints.** There is no
//! completeness guard on the write side at all, and the read side's
//! [`FIELD_COVERAGE`](crate::projection::FIELD_COVERAGE) — which does prove every leaf field is
//! reachable — could not have caught the failure that matters most here either: it disposes of one
//! field at a time and never asks whether two of them are one concept. A concept split across two
//! verbs is green under any such table, which is exactly how a node input literal and an interface
//! pipe's seed came to be written by two different verbs. The witness is a reader, or the consumer
//! that had to guess which verb it wanted.
//!
//! see rules: agent-mcp

use std::collections::BTreeMap;
use std::fmt;

use serde::Serialize;
use serde_json::Value;

use crate::contract::{content_hash, Report};
use crate::format::{
    ConfigValue, CurveDoc, InputPipeDoc, InputValue, InstrumentDoc, InterfaceDoc, InterfaceEntry,
    NodeDoc, NormalizedDoc, OutputPipeDoc, PipeDefault, FORMAT_VERSION, PIPE_INPUT_PORT,
};
use crate::introspect::validate;
use crate::projection::{Projector, Scalar, Selection};
use crate::resources::{ResolveError, ResourceResolver};
use crate::Registry;

/// The shape every document-manipulation verb returns — internal, and not the shape a door
/// advertises: the window declares its own, and only that one is serialized onto a wire.
#[derive(Debug, Clone, Serialize)]
pub struct EditResult {
    /// The whole-document validation report. `ok` is the write decision: `true` means the edit
    /// passed and was persisted; `false` means nothing was written and `errors` says why.
    pub report: Report,
    /// Whether the edit was actually written to the source. Equals `report.ok` — surfaced
    /// explicitly so a small model never has to infer "did my change land?" from the report shape.
    pub written: bool,
    /// The content hash of what is **now persisted** at the source: the new document on a
    /// successful write, the unchanged prior document on a rejected one. The token a later
    /// `expect`-guarded write compares — opaque, compare-only.
    pub hash: String,
    /// What the edit broke or degraded on the way — the cascade a `remove_instrument_node`
    /// unwired, the refs a `rename_instrument_node` rewrote. Empty for a clean surgical edit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// The rendered echo of what the verb did — the node zoom of an added node, the pipe view of a
    /// changed pipe, the index after a removal, or, for a value edit, the one `address.input from →
    /// to` line. The agent's read of the result, in the same compact grammar it reads the rest of
    /// the document through.
    pub zoom: String,
}

/// A verb could not do its job — distinct from a *rejected* edit (an invalid result, which is an
/// ordinary [`EditResult`] with `report.ok == false`). These are the can't-even-try cases: the
/// source is unreadable or unwritable, its bytes are not a document, or the edit's precondition
/// (the addressed node/pipe/resource must exist, or must not already) does not hold.
#[derive(Debug)]
pub enum EditError {
    /// The source could not be read through the resolver.
    Read(ResolveError),
    /// The valid new document could not be written back through the resolver.
    Write(ResolveError),
    /// The source's current bytes are not a loadable document (a parse or version failure).
    Parse(String),
    /// The edit's structural precondition does not hold: the addressed node/pipe/resource is
    /// absent, already present, or the given value is the wrong shape for its slot.
    Target(String),
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EditError::Read(e) => write!(f, "could not read the document: {e}"),
            EditError::Write(e) => write!(f, "could not write the document: {e}"),
            EditError::Parse(e) => write!(f, "the source is not a loadable document: {e}"),
            EditError::Target(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for EditError {}

/// What a verb's mutation touched, so the pipeline can render the right echo back.
enum Echo {
    /// Zoom these node addresses (`/` is the document header).
    Nodes(Selection),
    /// The interface pipe view, optionally narrowed to these pipe names.
    Pipes(Selection),
    /// The resources view.
    Resources,
    /// The node index — the right echo after a removal, which has no node to zoom.
    Index,
    /// The one value the caller named, before and after.
    Change(ValueChange),
}

/// A value edit's whole effect: the slot the caller addressed, what was in it, and what is in it
/// now. Not a projection — the prior document is gone by the time one could be cut, so `from` is
/// carried out of the mutation itself. see rules: agent-mcp
struct ValueChange {
    address: String,
    input: String,
    /// `None` when the slot held nothing — an input at its descriptor default, or a pipe with no
    /// declared seed.
    from: Option<Scalar>,
    to: Scalar,
}

impl ValueChange {
    fn render(&self) -> String {
        let from = match &self.from {
            Some(v) => v.render(),
            None => "(unset)".to_string(),
        };
        format!(
            "{}.{} {from} → {}",
            self.address,
            self.input,
            self.to.render()
        )
    }
}

/// A mutation's outcome: what to echo, and any cascade notes it produced.
struct Applied {
    echo: Echo,
    notes: Vec<String>,
}

impl Applied {
    /// A clean edit with no cascade to report.
    fn clean(echo: Echo) -> Self {
        Applied {
            echo,
            notes: Vec::new(),
        }
    }
}

// --- the shared pipeline ------------------------------------------------------------------------

/// Read the current document, apply `mutate`, then finish (re-normalize · validate · write-iff-valid
/// · hash · project). The one place every existing-document verb funnels through, so write-iff-valid,
/// the hash contract, and the projection echo cannot drift verb to verb.
fn edit_existing(
    source: &str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
    mutate: impl FnOnce(&mut InstrumentDoc) -> Result<Applied, EditError>,
) -> Result<EditResult, EditError> {
    let json = resolver.resolve_text(source).map_err(EditError::Read)?;
    let current = NormalizedDoc::from_json(&json, registry, Some(resolver))
        .map_err(|e| EditError::Parse(e.to_string()))?;
    // The hash of what is persisted right now — returned unchanged when the edit is rejected.
    let current_hash = content_hash(&current);
    let mut doc = current.into_inner();
    let applied = mutate(&mut doc)?;
    finish(source, registry, resolver, doc, applied, current_hash)
}

/// Re-normalize the edited document, validate it whole, write iff valid, and build the result. The
/// hash is the post-write content hash: the new document's when written, the prior document's when
/// rejected. The zoom always projects the *attempted* document, so a rejected edit still shows the
/// author what they tried alongside the errors that stopped it.
fn finish(
    source: &str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
    doc: InstrumentDoc,
    applied: Applied,
    current_hash: String,
) -> Result<EditResult, EditError> {
    // Re-enter the mint gate the same way a host holding a raw doc would; idempotent on an
    // already-normalized document, but it is what keeps the written bytes canonical.
    let normalized = NormalizedDoc::from_doc(doc, registry, Some(resolver))
        .map_err(|e| EditError::Parse(e.to_string()))?;
    let new_json = normalized.to_json_pretty();
    let report = validate(&new_json, registry, resolver);
    let (written, hash) = if report.ok {
        resolver
            .write_text(source, &new_json)
            .map_err(EditError::Write)?;
        (true, content_hash(&normalized))
    } else {
        (false, current_hash)
    };
    let zoom = render_echo(&new_json, registry, resolver, &applied.echo);
    Ok(EditResult {
        report,
        written,
        hash,
        notes: applied.notes,
        zoom,
    })
}

/// Render the echo view of the (attempted) document. A projection failure is reported inline rather
/// than propagated — the edit's own report is the deliverable, and a missing echo must never mask it.
fn render_echo(
    json: &str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
    echo: &Echo,
) -> String {
    // Before the projector is built, because a change echo needs no projection at all.
    if let Echo::Change(change) = echo {
        return change.render();
    }
    match Projector::new(json, registry, resolver) {
        Ok(p) => match echo {
            Echo::Nodes(sel) => p.zoom(sel).render(),
            Echo::Pipes(sel) => p.pipes(sel).render(),
            Echo::Resources => p.resources().render(),
            Echo::Index => p.index().render(),
            Echo::Change(_) => unreachable!("handled above"),
        },
        Err(e) => format!("(projection unavailable: {e})"),
    }
}

// --- value coercion ------------------------------------------------------------------------------

/// Coerce a JSON value into a **literal** input (a number or an enum symbol) — a wire-ref is
/// rejected here on purpose: wiring is [`wire_instrument_input`]'s job, so the two verbs stay
/// distinct on the persistent-vs-connected axis.
fn literal_input(value: Value) -> Result<InputValue, EditError> {
    match value {
        Value::Number(n) => n
            .as_f64()
            .map(InputValue::Number)
            .ok_or_else(|| EditError::Target("input value is not a finite number".into())),
        Value::String(s) => Ok(InputValue::Symbol(s)),
        Value::Object(_) => Err(EditError::Target(
            "a wire-ref belongs on `wire_instrument_input`, not `set_instrument_input`".into(),
        )),
        _ => Err(EditError::Target(
            "input value must be a number or an enum symbol string".into(),
        )),
    }
}

/// Coerce a JSON value into a config constant (a number, or a forward-compatible enum symbol).
fn config_value(value: Value) -> Result<ConfigValue, EditError> {
    match value {
        Value::Number(n) => n
            .as_f64()
            .map(ConfigValue::Number)
            .ok_or_else(|| EditError::Target("constant value is not a finite number".into())),
        Value::String(s) => Ok(ConfigValue::Symbol(s)),
        _ => Err(EditError::Target(
            "constant value must be a number or a symbol string".into(),
        )),
    }
}

/// Coerce a JSON value into an [`InputValue`] accepting *any* form (literal or wire-ref) — the
/// one-shot [`add_instrument_node`] path, where an input may be either.
fn any_input(name: &str, value: Value) -> Result<InputValue, EditError> {
    serde_json::from_value(value).map_err(|e| {
        EditError::Target(format!(
            "input `{name}` is not a valid value or wire-ref: {e}"
        ))
    })
}

/// Coerce a JSON value into a pipe default (number or enum symbol).
fn pipe_default(value: Value) -> Result<PipeDefault, EditError> {
    serde_json::from_value(value)
        .map_err(|e| EditError::Target(format!("pipe default must be a number or a symbol: {e}")))
}

/// The literal forms a value verb reports, collapsed onto the one shape the projection reads them
/// through. A wire has no literal to report, so it is `None` — the caller refuses on one before
/// asking.
fn input_scalar(v: &InputValue) -> Option<Scalar> {
    match v {
        InputValue::Number(n) => Some(Scalar::Number(*n)),
        InputValue::Symbol(s) => Some(Scalar::Symbol(s.clone())),
        InputValue::Wire { .. } => None,
    }
}

fn config_scalar(v: &ConfigValue) -> Scalar {
    match v {
        ConfigValue::Number(n) => Scalar::Number(*n),
        ConfigValue::Symbol(s) => Scalar::Symbol(s.clone()),
    }
}

fn seed_scalar(v: &PipeDefault) -> Scalar {
    match v {
        PipeDefault::Number(n) => Scalar::Number(*n),
        PipeDefault::Symbol(s) => Scalar::Symbol(s.clone()),
    }
}

/// Parse a curve token (`"lin"`/`"exp"`).
fn curve(token: &str) -> Result<CurveDoc, EditError> {
    match token {
        "lin" => Ok(CurveDoc::Lin),
        "exp" => Ok(CurveDoc::Exp),
        other => Err(EditError::Target(format!(
            "curve must be `lin` or `exp`, not `{other}`"
        ))),
    }
}

// --- wire cascade helpers ------------------------------------------------------------------------

/// Does a wire-ref `from` reference `address`? Matches the sole-output form (`/clock`) and any
/// port form (`/clock.trig`), never a prefix collision (`/clockwork`).
fn wire_targets(from: &str, address: &str) -> bool {
    from == address || from.starts_with(&format!("{address}."))
}

/// Rewrite a wire-ref that references `old` to reference `new`, preserving the `.port` suffix;
/// `None` if it does not reference `old`.
fn rewrite_wire(from: &str, old: &str, new: &str) -> Option<String> {
    if from == old {
        Some(new.to_string())
    } else {
        from.strip_prefix(&format!("{old}."))
            .map(|rest| format!("{new}.{rest}"))
    }
}

/// Unwire every reference to `address` — node inputs wired from it, interface outputs fed from it —
/// returning a note per severed connection. The cascade the two address-removing verbs share:
/// [`remove_instrument_node`] deletes a node address, [`remove_instrument_interface_input`] deletes
/// the `/<name>` address its pipe minted, and either way a surviving wire-ref is a **fatal**
/// `LoadError::UnknownNode`, not a warning — so the removal severs and reports rather than leaving
/// the author a rejected write and a discovery exercise.
fn cascade_unwire(doc: &mut InstrumentDoc, address: &str) -> Vec<String> {
    let mut notes = Vec::new();
    for node in &mut doc.nodes {
        let hits: Vec<String> = node
            .inputs
            .iter()
            .filter(|(_, v)| matches!(v, InputValue::Wire { from } if wire_targets(from, address)))
            .map(|(k, _)| k.clone())
            .collect();
        for input in hits {
            if let Some(InputValue::Wire { from }) = node.inputs.remove(&input) {
                notes.push(format!("unwired {}.{input} (was {from})", node.address));
            }
        }
    }
    if let Some(iface) = &mut doc.interface {
        let hits: Vec<String> = iface
            .outputs
            .iter()
            .filter(|(_, e)| matches!(e, InterfaceEntry::Feed(f) if wire_targets(&f.from, address)))
            .map(|(k, _)| k.clone())
            .collect();
        for name in hits {
            if let Some(InterfaceEntry::Feed(f)) = iface.outputs.remove(&name) {
                notes.push(format!(
                    "removed interface output `{name}` (fed from {})",
                    f.from
                ));
            }
        }
    }
    notes
}

/// Resolve an address to a document node, refusing an interface input pipe **in terms of what it
/// is**. `cannot` completes the sentence — why this verb's operation is meaningless on a boundary
/// input, and what reaches it instead.
///
/// A pipe address is not absent, so it must never be reported as absent; that is the whole point of
/// the shared namespace. see rules: agent-mcp
fn node_index_for(
    doc: &InstrumentDoc,
    address: &str,
    cannot: impl FnOnce(&str) -> String,
) -> Result<usize, EditError> {
    match address_target(doc, address)? {
        AddressTarget::Node(idx) => Ok(idx),
        AddressTarget::Pipe(name) => Err(EditError::Target(format!(
            "`{address}` is this document's own interface input pipe `{name}` — its boundary, fed \
             from outside the graph. {}",
            cannot(&name)
        ))),
    }
}

/// Find a node index by address, for the verbs that act on a node as a whole. A pipe is declared
/// rather than added, so the way through is the interface half of the vocabulary.
fn node_index(doc: &InstrumentDoc, address: &str) -> Result<usize, EditError> {
    node_index_for(doc, address, |name| {
        format!(
            "It is declared in `interface.inputs`, not added as a node, so the interface verbs are \
             what reach it: `remove_instrument_interface_input` to drop it (cascading over the \
             address it mints), or `set_instrument_interface_input_meta` with the name `{name}`."
        )
    })
}

/// What an address in the flat namespace resolves to.
enum AddressTarget {
    /// A document node, by index.
    Node(usize),
    /// An interface input pipe, by entry name — the node its `/<name>` mints.
    Pipe(String),
}

/// Resolve an address the way the loader's namespace does: the document's own nodes, then the
/// addresses the interface's input pipes mint. Nodes first only because the loader rejects a
/// collision between the two outright, so at most one can answer.
fn address_target(doc: &InstrumentDoc, address: &str) -> Result<AddressTarget, EditError> {
    if let Some(idx) = doc.nodes.iter().position(|n| n.address == address) {
        return Ok(AddressTarget::Node(idx));
    }
    let name = address.strip_prefix('/').filter(|n| {
        doc.interface
            .as_ref()
            .is_some_and(|i| matches!(i.inputs.get(*n), Some(InterfaceEntry::Pipe(_))))
    });
    match name {
        Some(n) => Ok(AddressTarget::Pipe(n.to_string())),
        None => Err(EditError::Target(format!(
            "no node or interface input pipe at address `{address}`"
        ))),
    }
}

/// The input pipe [`address_target`] just resolved. Separate from the resolution because the lookup
/// borrows the document immutably and the write needs it mutably.
fn input_pipe_mut<'a>(doc: &'a mut InstrumentDoc, name: &str) -> &'a mut InputPipeDoc {
    match doc.interface.as_mut().and_then(|i| i.inputs.get_mut(name)) {
        Some(InterfaceEntry::Pipe(pipe)) => pipe,
        _ => unreachable!("`address_target` resolved this name to an input pipe"),
    }
}

/// Borrow the document's interface, minting an empty one if absent — every interface verb needs a
/// place to write.
fn interface_mut(doc: &mut InstrumentDoc) -> &mut InterfaceDoc {
    doc.interface.get_or_insert_with(InterfaceDoc::default)
}

// --- document verbs ------------------------------------------------------------------------------

/// Create a new, guaranteed-valid minimal document at `source` — the from-scratch start move,
/// one-shot: it lands the whole document (name + empty nodes) written, not returned by value. Refuses
/// to overwrite an existing document, so a mistyped `source` never silently clobbers another instrument.
pub fn new_instrument(
    source: &str,
    name: &str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<EditResult, EditError> {
    if resolver.resolve_text(source).is_ok() {
        return Err(EditError::Target(format!(
            "a document already exists at `{source}`; edit it, or choose a new source"
        )));
    }
    let doc = InstrumentDoc {
        format_version: FORMAT_VERSION,
        instrument: name.to_string(),
        doc: None,
        resources: BTreeMap::new(),
        interface: None,
        nodes: Vec::new(),
        outputs: Vec::new(),
        migration: Default::default(),
    };
    finish(
        source,
        registry,
        resolver,
        doc,
        Applied::clean(Echo::Nodes(Selection::names(["/"]))),
        String::new(),
    )
}

/// Rename the instrument (the top-level `instrument` name).
pub fn set_instrument_name(
    source: &str,
    name: &str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<EditResult, EditError> {
    edit_existing(source, registry, resolver, |doc| {
        doc.instrument = name.to_string();
        Ok(Applied::clean(Echo::Nodes(Selection::names(["/"]))))
    })
}

/// Set (or, with `None`, clear) the instrument's human/agent note (`doc` on disk, `description` in
/// the projection).
pub fn set_instrument_description(
    source: &str,
    description: Option<&str>,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<EditResult, EditError> {
    edit_existing(source, registry, resolver, |doc| {
        doc.doc = description.map(str::to_string);
        Ok(Applied::clean(Echo::Nodes(Selection::names(["/"]))))
    })
}

// --- node verbs ----------------------------------------------------------------------------------

/// Add a node **fully formed in one call**, mirroring the shape a node zoom reads back: required
/// `address` + `type`, plus inputs literal-or-wired, config constants,
/// description, and any resource-slot reference). Atomic under write-iff-valid: a wire to a missing
/// source, or a duplicate address, rejects the whole call.
#[allow(clippy::too_many_arguments)]
pub fn add_instrument_node(
    source: &str,
    address: &str,
    type_name: &str,
    inputs: BTreeMap<String, Value>,
    config: BTreeMap<String, Value>,
    description: Option<&str>,
    sample: Option<&str>,
    voice: Option<&str>,
    patch: Option<&str>,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<EditResult, EditError> {
    edit_existing(source, registry, resolver, |doc| {
        if doc.nodes.iter().any(|n| n.address == address) {
            return Err(EditError::Target(format!(
                "a node already exists at address `{address}`"
            )));
        }
        let mut input_map = BTreeMap::new();
        for (name, value) in inputs {
            let v = any_input(&name, value)?;
            input_map.insert(name, v);
        }
        let mut config_map = BTreeMap::new();
        for (name, value) in config {
            config_map.insert(name, config_value(value)?);
        }
        doc.nodes.push(NodeDoc {
            type_name: type_name.to_string(),
            address: address.to_string(),
            doc: description.map(str::to_string),
            config: config_map,
            inputs: input_map,
            sample: sample.map(str::to_string),
            voice: voice.map(str::to_string),
            patch: patch.map(str::to_string),
            control: None,
        });
        Ok(Applied::clean(Echo::Nodes(Selection::names([address]))))
    })
}

/// Remove a node, **cascading** the breakage: every consumer wired from it is auto-unwired and every
/// interface output fed from it is dropped, each reported in `notes`. The commonest structural edit
/// stays one call, not a six-call unwire-them-first discovery exercise.
pub fn remove_instrument_node(
    source: &str,
    address: &str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<EditResult, EditError> {
    edit_existing(source, registry, resolver, |doc| {
        let idx = node_index(doc, address)?;
        doc.nodes.remove(idx);
        let notes = cascade_unwire(doc, address);
        Ok(Applied {
            echo: Echo::Index,
            notes,
        })
    })
}

/// Rename a node, **rewiring** every consumer to the new address (the cascade posture of
/// [`remove_instrument_node`], but preserving rather than dropping the connections). Refuses if the
/// target address is already taken — by a node, or by the `/<name>` an interface input pipe mints.
/// Renaming a node to the address it already has is a no-op, reported in `notes`.
pub fn rename_instrument_node(
    source: &str,
    from: &str,
    to: &str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<EditResult, EditError> {
    edit_existing(source, registry, resolver, |doc| {
        // Source first: a rename of a node that isn't there must say *that*, not report the
        // destination's precondition for an edit that could never have run.
        let idx = node_index(doc, from)?;
        if from == to {
            return Ok(Applied {
                echo: Echo::Nodes(Selection::names([to])),
                notes: vec![format!(
                    "`{from}` is already its address; nothing to rename"
                )],
            });
        }
        if doc.nodes.iter().any(|n| n.address == to) {
            return Err(EditError::Target(format!(
                "a node already exists at address `{to}`"
            )));
        }
        // The interface's input pipes mint `/<name>` into the same address namespace, so a
        // collision there is the same precondition — caught here, where the message can name the
        // pipe, rather than downstream as a bare `duplicate node address`.
        if let Some(pipe) = to.strip_prefix('/') {
            if doc
                .interface
                .as_ref()
                .is_some_and(|i| i.inputs.contains_key(pipe))
            {
                return Err(EditError::Target(format!(
                    "the interface input `{pipe}` already mints the address `{to}`"
                )));
            }
        }
        doc.nodes[idx].address = to.to_string();
        let mut notes = Vec::new();
        for node in &mut doc.nodes {
            for (input, v) in node.inputs.iter_mut() {
                if let InputValue::Wire { from: wf } = v {
                    if let Some(new_ref) = rewrite_wire(wf, from, to) {
                        notes.push(format!(
                            "rewired {}.{input}: {wf} → {new_ref}",
                            node.address
                        ));
                        *wf = new_ref;
                    }
                }
            }
        }
        if let Some(iface) = &mut doc.interface {
            for (name, e) in iface.outputs.iter_mut() {
                if let InterfaceEntry::Feed(f) = e {
                    if let Some(new_ref) = rewrite_wire(&f.from, from, to) {
                        notes.push(format!(
                            "rewired interface output `{name}`: {} → {new_ref}",
                            f.from
                        ));
                        f.from = new_ref;
                    }
                }
            }
        }
        Ok(Applied {
            echo: Echo::Nodes(Selection::names([to])),
            notes,
        })
    })
}

/// Set (or clear) a node's description.
pub fn set_instrument_node_description(
    source: &str,
    address: &str,
    description: Option<&str>,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<EditResult, EditError> {
    edit_existing(source, registry, resolver, |doc| {
        let idx = node_index(doc, address)?;
        doc.nodes[idx].doc = description.map(str::to_string);
        Ok(Applied::clean(Echo::Nodes(Selection::names([address]))))
    })
}

// --- input verbs ---------------------------------------------------------------------------------

/// Set a node input, or an interface input pipe's seed, to a **literal** value (a number or an enum
/// symbol) — the point-edit that replaces re-emitting the whole document for a one-value tweak.
///
/// One address space, because an `interface.inputs` entry **is a node**: it is addressed as the
/// `/<name>` it mints, and its one input is [`PIPE_INPUT_PORT`]. On disk the pipe's slot is still
/// spelled `default`.
///
/// Refuses an input that currently holds a wire rather than severing it, naming
/// [`unwire_instrument_input`] as the way through. see rules: agent-mcp
pub fn set_instrument_input(
    source: &str,
    address: &str,
    input: &str,
    value: Value,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<EditResult, EditError> {
    edit_existing(source, registry, resolver, |doc| {
        let v = literal_input(value)?;
        let change = match address_target(doc, address)? {
            AddressTarget::Node(idx) => {
                let node = &mut doc.nodes[idx];
                if let Some(InputValue::Wire { from }) = node.inputs.get(input) {
                    return Err(EditError::Target(format!(
                        "`{address}.{input}` is wired from `{from}`; setting a value here would \
                         sever that wire, so it is refused. Call `unwire_instrument_input` first \
                         if severing it is what you want."
                    )));
                }
                let from = node.inputs.get(input).and_then(input_scalar);
                let to = input_scalar(&v).expect("a literal input has a literal to report");
                node.inputs.insert(input.to_string(), v);
                ValueChange {
                    address: address.to_string(),
                    input: input.to_string(),
                    from,
                    to,
                }
            }
            AddressTarget::Pipe(name) => {
                if input != PIPE_INPUT_PORT {
                    return Err(EditError::Target(format!(
                        "`{address}` is an interface input pipe: a pass-through whose only input \
                         is `{PIPE_INPUT_PORT}`, not `{input}`"
                    )));
                }
                let seed = match v {
                    InputValue::Number(n) => PipeDefault::Number(n),
                    InputValue::Symbol(s) => PipeDefault::Symbol(s),
                    InputValue::Wire { .. } => unreachable!("`literal_input` rejects a wire-ref"),
                };
                let pipe = input_pipe_mut(doc, &name);
                let from = pipe.default.as_ref().map(seed_scalar);
                let to = seed_scalar(&seed);
                pipe.default = Some(seed);
                ValueChange {
                    address: address.to_string(),
                    input: input.to_string(),
                    from,
                    to,
                }
            }
        };
        Ok(Applied::clean(Echo::Change(change)))
    })
}

/// Wire a node input from a source port (`/node.port`, or `/node` sole-output sugar).
///
/// This document's own interface input pipes are addressable here and **refuse**: a boundary input
/// is fed from outside the graph, so a wire from inside it would stop it being a boundary. Wiring
/// *from* one is ordinary and unaffected. see rules: agent-mcp
pub fn wire_instrument_input(
    source: &str,
    address: &str,
    input: &str,
    from: &str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<EditResult, EditError> {
    edit_existing(source, registry, resolver, |doc| {
        let idx = node_index_for(doc, address, |name| {
            format!(
                "A live send, a channel binding, or the host's wire onto this face when the \
                 document is nested is what feeds it, so a wire from inside would stop it being a \
                 boundary. Wire a consumer *from* `/{name}` instead (pass it as `from`), or set \
                 its value with `set_instrument_input`."
            )
        })?;
        doc.nodes[idx].inputs.insert(
            input.to_string(),
            InputValue::Wire {
                from: from.to_string(),
            },
        );
        Ok(Applied::clean(Echo::Nodes(Selection::names([address]))))
    })
}

/// Unwire a node input, reverting it to the operator's descriptor default. A no-op input (nothing
/// set) is reported, not an error.
///
/// This document's own interface input pipes are addressable here and **refuse**: nothing inside
/// the graph feeds a boundary input, so there is no wire on one to clear. see rules: agent-mcp
pub fn unwire_instrument_input(
    source: &str,
    address: &str,
    input: &str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<EditResult, EditError> {
    edit_existing(source, registry, resolver, |doc| {
        let idx = node_index_for(doc, address, |_| {
            "Nothing inside the graph feeds it, so it carries no wire to clear. Its value is \
             `set_instrument_input`'s, and dropping the pipe altogether is \
             `remove_instrument_interface_input`'s."
                .to_string()
        })?;
        let notes = if doc.nodes[idx].inputs.remove(input).is_some() {
            Vec::new()
        } else {
            vec![format!(
                "`{input}` was not set on `{address}`; nothing to unwire"
            )]
        };
        Ok(Applied {
            echo: Echo::Nodes(Selection::names([address])),
            notes,
        })
    })
}

// --- config verb ---------------------------------------------------------------------------------

/// Set an instantiate-time constant on a node (e.g. a Voicer's `voices`).
///
/// This document's own interface input pipes are addressable here and **refuse**: a pipe is a
/// loader-built pass-through with no `config` block, so it has no plan-time constant to set.
/// see rules: agent-mcp
pub fn set_instrument_constant(
    source: &str,
    address: &str,
    name: &str,
    value: Value,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<EditResult, EditError> {
    edit_existing(source, registry, resolver, |doc| {
        let idx = node_index_for(doc, address, |pipe| {
            format!(
                "It is a loader-built pass-through with no `config` block, so it has no plan-time \
                 constant to set. Its quantity contract is \
                 `set_instrument_interface_input_meta` with the name `{pipe}`, and its value is \
                 `set_instrument_input`'s."
            )
        })?;
        let v = config_value(value)?;
        let node = &mut doc.nodes[idx];
        let from = node.config.get(name).map(config_scalar);
        let to = config_scalar(&v);
        node.config.insert(name.to_string(), v);
        Ok(Applied::clean(Echo::Change(ValueChange {
            address: address.to_string(),
            input: name.to_string(),
            from,
            to,
        })))
    })
}

// --- interface pipe verbs ------------------------------------------------------------------------

/// Add an interface **input** pipe: a declared-type boundary input that mints an address internal
/// nodes consume from, with optional channel binding and numeric metadata.
///
/// `value` is the pipe's seed — `default` on disk, and the same slot
/// [`set_instrument_input`] writes later through the address this entry mints.
#[allow(clippy::too_many_arguments)]
pub fn add_instrument_interface_input(
    source: &str,
    name: &str,
    ty: &str,
    channel: Option<usize>,
    value: Option<Value>,
    min: Option<f64>,
    max: Option<f64>,
    curve_token: Option<&str>,
    unit: Option<&str>,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<EditResult, EditError> {
    let default = value.map(pipe_default).transpose()?;
    let curve = curve_token.map(curve).transpose()?;
    edit_existing(source, registry, resolver, |doc| {
        let iface = interface_mut(doc);
        if iface.inputs.contains_key(name) {
            return Err(EditError::Target(format!(
                "an interface input `{name}` already exists"
            )));
        }
        iface.inputs.insert(
            name.to_string(),
            InterfaceEntry::Pipe(InputPipeDoc {
                ty: ty.to_string(),
                channel,
                default,
                min,
                max,
                curve,
                unit: unit.map(str::to_string),
                label: None,
                widget: None,
            }),
        );
        Ok(Applied::clean(Echo::Pipes(Selection::names([name]))))
    })
}

/// Add an interface **output** pipe: a master tap fed from an internal port.
#[allow(clippy::too_many_arguments)]
pub fn add_instrument_interface_output(
    source: &str,
    name: &str,
    from: &str,
    channel: Option<usize>,
    min: Option<f64>,
    max: Option<f64>,
    unit: Option<&str>,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<EditResult, EditError> {
    edit_existing(source, registry, resolver, |doc| {
        let iface = interface_mut(doc);
        if iface.outputs.contains_key(name) {
            return Err(EditError::Target(format!(
                "an interface output `{name}` already exists"
            )));
        }
        iface.outputs.insert(
            name.to_string(),
            InterfaceEntry::Feed(OutputPipeDoc {
                from: from.to_string(),
                channel,
                label: None,
                unit: unit.map(str::to_string),
                widget: None,
                min,
                max,
            }),
        );
        Ok(Applied::clean(Echo::Pipes(Selection::names([name]))))
    })
}

/// Remove an interface input pipe, **cascading** the breakage exactly as
/// [`remove_instrument_node`] does: the pipe minted `/<name>` as an address internal nodes consume
/// from, so every consumer is auto-unwired and reported in `notes` rather than left dangling for the
/// loader to reject.
pub fn remove_instrument_interface_input(
    source: &str,
    name: &str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<EditResult, EditError> {
    edit_existing(source, registry, resolver, |doc| {
        let iface = interface_mut(doc);
        if iface.inputs.remove(name).is_none() {
            return Err(EditError::Target(format!(
                "no interface input `{name}` to remove"
            )));
        }
        let notes = cascade_unwire(doc, &format!("/{name}"));
        Ok(Applied {
            echo: Echo::Pipes(Selection::All),
            notes,
        })
    })
}

/// Remove an interface output pipe.
pub fn remove_instrument_interface_output(
    source: &str,
    name: &str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<EditResult, EditError> {
    edit_existing(source, registry, resolver, |doc| {
        let iface = interface_mut(doc);
        if iface.outputs.remove(name).is_none() {
            return Err(EditError::Target(format!(
                "no interface output `{name}` to remove"
            )));
        }
        Ok(Applied::clean(Echo::Pipes(Selection::All)))
    })
}

/// Update an existing interface **input** pipe's *quantity contract*: its channel binding, range,
/// curve and display unit. Each `Some` field is written; a `None` leaves that field unchanged. Only
/// valid on an input pipe (the `Pipe` variant).
///
/// The pipe's seed is **not** here — a pipe's value is set through
/// [`set_instrument_input`], in the one address space that also reaches every node input.
/// see rules: agent-mcp
#[allow(clippy::too_many_arguments)]
pub fn set_instrument_interface_input_meta(
    source: &str,
    name: &str,
    channel: Option<usize>,
    min: Option<f64>,
    max: Option<f64>,
    curve_token: Option<&str>,
    unit: Option<&str>,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<EditResult, EditError> {
    let curve = curve_token.map(curve).transpose()?;
    edit_existing(source, registry, resolver, |doc| {
        let iface = interface_mut(doc);
        match iface.inputs.get_mut(name) {
            Some(InterfaceEntry::Pipe(pipe)) => {
                if channel.is_some() {
                    pipe.channel = channel;
                }
                if min.is_some() {
                    pipe.min = min;
                }
                if max.is_some() {
                    pipe.max = max;
                }
                if curve.is_some() {
                    pipe.curve = curve;
                }
                if unit.is_some() {
                    pipe.unit = unit.map(str::to_string);
                }
                Ok(Applied::clean(Echo::Pipes(Selection::names([name]))))
            }
            Some(_) => Err(EditError::Target(format!("`{name}` is not an input pipe"))),
            None => Err(EditError::Target(format!("no interface input `{name}`"))),
        }
    })
}

/// Update the metadata of an existing interface **output** pipe. Each `Some` field is written; a
/// `None` leaves that field unchanged. Only valid on an output pipe (the `Feed` variant).
#[allow(clippy::too_many_arguments)]
pub fn set_instrument_interface_output_meta(
    source: &str,
    name: &str,
    channel: Option<usize>,
    min: Option<f64>,
    max: Option<f64>,
    unit: Option<&str>,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<EditResult, EditError> {
    edit_existing(source, registry, resolver, |doc| {
        let iface = interface_mut(doc);
        match iface.outputs.get_mut(name) {
            Some(InterfaceEntry::Feed(pipe)) => {
                if channel.is_some() {
                    pipe.channel = channel;
                }
                if min.is_some() {
                    pipe.min = min;
                }
                if max.is_some() {
                    pipe.max = max;
                }
                if unit.is_some() {
                    pipe.unit = unit.map(str::to_string);
                }
                Ok(Applied::clean(Echo::Pipes(Selection::names([name]))))
            }
            Some(_) => Err(EditError::Target(format!("`{name}` is not an output pipe"))),
            None => Err(EditError::Target(format!("no interface output `{name}`"))),
        }
    })
}

// --- resource verbs ------------------------------------------------------------------------------

/// Add a resource to the document's id→source table.
pub fn add_instrument_resource(
    source: &str,
    id: &str,
    resource_source: &str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<EditResult, EditError> {
    edit_existing(source, registry, resolver, |doc| {
        if doc.resources.contains_key(id) {
            return Err(EditError::Target(format!(
                "a resource `{id}` already exists"
            )));
        }
        doc.resources
            .insert(id.to_string(), resource_source.to_string());
        Ok(Applied::clean(Echo::Resources))
    })
}

/// Remove a resource from the document's id→source table.
pub fn remove_instrument_resource(
    source: &str,
    id: &str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<EditResult, EditError> {
    edit_existing(source, registry, resolver, |doc| {
        if doc.resources.remove(id).is_none() {
            return Err(EditError::Target(format!("no resource `{id}` to remove")));
        }
        Ok(Applied::clean(Echo::Resources))
    })
}

#[cfg(test)]
mod tests;
