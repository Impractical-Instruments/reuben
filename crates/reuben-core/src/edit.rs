//! The **closed document-manipulation vocabulary**: the finite set of verbs an agent uses to
//! author an instrument document without ever touching its bytes (#583, #603). Each verb is a pure
//! `(source, …) -> EditResult` function — read the document through the resolver seam
//! ([`ResourceResolver::resolve_text`]), apply one surgical edit, re-validate the **whole** document
//! through the loader, **write iff valid** ([`ResourceResolver::write_text`]), and echo back the
//! post-write content hash plus the [`projection`](crate::projection) of what the verb touched.
//! All engine-free — always-available pure tools, never reaching a live engine.
//!
//! # The vocabulary is derived, and its completeness is guarded
//!
//! #583 closed the escape hatch: the agent may not emit document bytes, so **anything no verb can
//! reach is unreachable**. That makes completeness a *correctness* requirement, not a nicety. The
//! guard is mechanical — [`VERB_COVERAGE`] dispositions every leaf field of the format into the verb
//! that writes it (or an explicit `omit:` reason), and a schemars field-walk fails the build the
//! moment the format grows a field no verb reaches. This is the write-side mirror of the projection's
//! read-side [`FIELD_COVERAGE`](crate::projection::FIELD_COVERAGE): together they prove the agent can
//! both *see* and *reach* every field the format can express.
//!
//! # Write-iff-valid, cascade, and the door's guard
//!
//! There are no transactions: a lone unwired node loads clean and renders silence, so
//! `new → add → add → wire → wire` is valid at every step. The one destructive verb —
//! [`remove_instrument_node`] — would leave dangling wire-refs, so it **cascades**: it auto-unwires
//! every consumer and **reports exactly what it broke** in [`EditResult::notes`]; [`rename_instrument_node`]
//! rewrites those refs instead of dropping them, same channel. The `expect`-hash write guard is a
//! **door** concern (`agent-mcp.md#expect-guard-is-a-door-concern`): core's write stays unguarded
//! last-write-wins, the post-write hash is always returned, and the door does the content-hash compare.
//!
//! see engine rules: agent-mcp

use std::collections::BTreeMap;
use std::fmt;

use serde::Serialize;
use serde_json::Value;

use crate::contract::{content_hash, Report};
use crate::format::{
    ConfigValue, CurveDoc, InputPipeDoc, InputValue, InstrumentDoc, InterfaceDoc, InterfaceEntry,
    NodeDoc, NormalizedDoc, OutputPipeDoc, PipeDefault, FORMAT_VERSION,
};
use crate::introspect::validate;
use crate::projection::{Projector, Selection};
use crate::resources::{ResolveError, ResourceResolver};
use crate::Registry;

/// The result of one document-manipulation verb — the shape every verb returns: the validation
/// report (`ok` iff the edit was written), the content hash of what is **now persisted**, any
/// cascade/degrade notes the edit produced, and the rendered projection of what it touched.
///
/// Derives `schemars::JsonSchema` behind the default-off `schemars` feature so a door can advertise
/// it as one `outputSchema`; the play/CLI build never compiles schemars.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
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
    /// What the edit broke or degraded on the way — the cascade a [`remove_instrument_node`]
    /// unwired, the refs a [`rename_instrument_node`] rewrote. Empty for a clean surgical edit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// The rendered [`projection`](crate::projection) of what the verb touched — the node zoom of
    /// an added node, the pipe view of a changed pipe, the index after a removal. The agent's read
    /// of the result, in the same compact grammar it reads the rest of the document through.
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

/// What a verb's mutation touched, so the pipeline can render the right projection view back.
enum Echo {
    /// Zoom these node addresses (`/` is the document header).
    Nodes(Selection),
    /// The interface pipe view, optionally narrowed to these pipe names.
    Pipes(Selection),
    /// The resources view.
    Resources,
    /// The node index — the right echo after a removal, which has no node to zoom.
    Index,
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
    match Projector::new(json, registry, resolver) {
        Ok(p) => match echo {
            Echo::Nodes(sel) => p.zoom(sel).render(),
            Echo::Pipes(sel) => p.pipes(sel).render(),
            Echo::Resources => p.resources().render(),
            Echo::Index => p.index().render(),
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

/// Find a node index by address, or the precondition error naming it absent.
fn node_index(doc: &InstrumentDoc, address: &str) -> Result<usize, EditError> {
    doc.nodes
        .iter()
        .position(|n| n.address == address)
        .ok_or_else(|| EditError::Target(format!("no node at address `{address}`")))
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

/// Add a node **fully formed in one call** — the zoom-mirroring add (#611): required `address` +
/// `type`, plus the same shape a node zoom reads back (inputs literal-or-wired, config constants,
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
        let mut notes = Vec::new();
        // Unwire every node input wired from the departing node.
        for node in &mut doc.nodes {
            let hits: Vec<String> = node
                .inputs
                .iter()
                .filter(
                    |(_, v)| matches!(v, InputValue::Wire { from } if wire_targets(from, address)),
                )
                .map(|(k, _)| k.clone())
                .collect();
            for input in hits {
                if let Some(InputValue::Wire { from }) = node.inputs.remove(&input) {
                    notes.push(format!("unwired {}.{input} (was {from})", node.address));
                }
            }
        }
        // Drop every interface output fed from the departing node.
        if let Some(iface) = &mut doc.interface {
            let hits: Vec<String> = iface
                .outputs
                .iter()
                .filter(
                    |(_, e)| matches!(e, InterfaceEntry::Feed(f) if wire_targets(&f.from, address)),
                )
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
        Ok(Applied {
            echo: Echo::Index,
            notes,
        })
    })
}

/// Rename a node, **rewiring** every consumer to the new address (the cascade posture of
/// [`remove_instrument_node`], but preserving rather than dropping the connections). Refuses if the
/// target address is already taken.
pub fn rename_instrument_node(
    source: &str,
    from: &str,
    to: &str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<EditResult, EditError> {
    edit_existing(source, registry, resolver, |doc| {
        if doc.nodes.iter().any(|n| n.address == to) {
            return Err(EditError::Target(format!(
                "a node already exists at address `{to}`"
            )));
        }
        let idx = node_index(doc, from)?;
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

/// Set a node input to a **literal** value (a number or an enum symbol) — the point-edit that
/// replaces re-emitting the whole document for a one-value tweak.
pub fn set_instrument_input(
    source: &str,
    address: &str,
    input: &str,
    value: Value,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<EditResult, EditError> {
    edit_existing(source, registry, resolver, |doc| {
        let idx = node_index(doc, address)?;
        let v = literal_input(value)?;
        doc.nodes[idx].inputs.insert(input.to_string(), v);
        Ok(Applied::clean(Echo::Nodes(Selection::names([address]))))
    })
}

/// Wire a node input from a source port (`/node.port`, or `/node` sole-output sugar).
pub fn wire_instrument_input(
    source: &str,
    address: &str,
    input: &str,
    from: &str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<EditResult, EditError> {
    edit_existing(source, registry, resolver, |doc| {
        let idx = node_index(doc, address)?;
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
pub fn unwire_instrument_input(
    source: &str,
    address: &str,
    input: &str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<EditResult, EditError> {
    edit_existing(source, registry, resolver, |doc| {
        let idx = node_index(doc, address)?;
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
pub fn set_instrument_constant(
    source: &str,
    address: &str,
    name: &str,
    value: Value,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<EditResult, EditError> {
    edit_existing(source, registry, resolver, |doc| {
        let idx = node_index(doc, address)?;
        let v = config_value(value)?;
        doc.nodes[idx].config.insert(name.to_string(), v);
        Ok(Applied::clean(Echo::Nodes(Selection::names([address]))))
    })
}

// --- interface pipe verbs ------------------------------------------------------------------------

/// Add an interface **input** pipe: a declared-type boundary input that mints an address internal
/// nodes consume from, with optional channel binding and numeric metadata.
#[allow(clippy::too_many_arguments)]
pub fn add_instrument_interface_input(
    source: &str,
    name: &str,
    ty: &str,
    channel: Option<usize>,
    default: Option<Value>,
    min: Option<f64>,
    max: Option<f64>,
    curve_token: Option<&str>,
    unit: Option<&str>,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<EditResult, EditError> {
    let default = default.map(pipe_default).transpose()?;
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

/// Remove an interface input pipe.
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
        Ok(Applied::clean(Echo::Pipes(Selection::All)))
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

/// Update the metadata of an existing interface **input** pipe. Each `Some` field is written; a
/// `None` leaves that field unchanged. Only valid on an input pipe (the `Pipe` variant).
#[allow(clippy::too_many_arguments)]
pub fn set_instrument_interface_input_meta(
    source: &str,
    name: &str,
    channel: Option<usize>,
    default: Option<Value>,
    min: Option<f64>,
    max: Option<f64>,
    curve_token: Option<&str>,
    unit: Option<&str>,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<EditResult, EditError> {
    let default = default.map(pipe_default).transpose()?;
    let curve = curve_token.map(curve).transpose()?;
    edit_existing(source, registry, resolver, |doc| {
        let iface = interface_mut(doc);
        match iface.inputs.get_mut(name) {
            Some(InterfaceEntry::Pipe(pipe)) => {
                if channel.is_some() {
                    pipe.channel = channel;
                }
                if default.is_some() {
                    pipe.default = default;
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

// --- the completeness guard ----------------------------------------------------------------------

/// The write-side coverage table: every leaf field of the instrument format, dispositioned into the
/// verb (or verbs) that can write it — or an explicit `omit:` reason for a field no verb reaches
/// (stamped by the writer, a v1-only migration form, or retired presentation). This is the write-side
/// mirror of the projection's [`FIELD_COVERAGE`](crate::projection::FIELD_COVERAGE): the guard below
/// walks the real [`InstrumentDoc`] schema and fails the build when the format grows a field no verb
/// can reach, making "the vocabulary is complete" a mechanical fact rather than a claim.
///
/// The interface maps enumerate **one untagged union** ([`InterfaceEntry`]) under both `inputs` and
/// `outputs`, so each map lists every variant's fields; a field belonging only to the *other*
/// variant appears here as an `omit:` — the input map can never carry a `Feed`'s `from`, nor the
/// output map a `Pipe`'s `type`/`default`/`curve`.
pub const VERB_COVERAGE: &[(&str, &str)] = &[
    // --- the document itself ---
    (
        "format_version",
        "omit:stamped as the current FORMAT_VERSION on every save; never agent-set",
    ),
    ("instrument", "new_instrument, set_instrument_name"),
    ("doc", "set_instrument_description"),
    (
        "resources{}",
        "add_instrument_resource, remove_instrument_resource",
    ),
    // --- nodes ---
    ("nodes[].type", "add_instrument_node"),
    (
        "nodes[].address",
        "add_instrument_node, rename_instrument_node",
    ),
    (
        "nodes[].doc",
        "add_instrument_node, set_instrument_node_description",
    ),
    (
        "nodes[].config{}",
        "add_instrument_node, set_instrument_constant",
    ),
    (
        "nodes[].inputs{}",
        "add_instrument_node, set_instrument_input, unwire_instrument_input",
    ),
    (
        "nodes[].inputs{}.from",
        "add_instrument_node, wire_instrument_input",
    ),
    ("nodes[].sample", "add_instrument_node"),
    ("nodes[].voice", "add_instrument_node"),
    ("nodes[].patch", "add_instrument_node"),
    (
        "nodes[].control",
        "omit:retired deserialize-only sink (drained to a deprecation warning at the mint)",
    ),
    // --- v1-only master-tap list: migrated into interface.outputs, never written back ---
    (
        "outputs[].node",
        "omit:v1-only, migrated into interface.outputs",
    ),
    (
        "outputs[].port",
        "omit:v1-only, migrated into interface.outputs",
    ),
    (
        "outputs[].channel",
        "omit:v1-only, migrated into interface.outputs",
    ),
    // --- interface inputs (the Pipe variant is the real one here) ---
    (
        "interface.inputs{}",
        "omit:v1-only bare Target string form (migrated at the mint)",
    ),
    ("interface.inputs{}.type", "add_instrument_interface_input"),
    (
        "interface.inputs{}.channel",
        "add_instrument_interface_input, set_instrument_interface_input_meta",
    ),
    (
        "interface.inputs{}.default",
        "add_instrument_interface_input, set_instrument_interface_input_meta",
    ),
    (
        "interface.inputs{}.min",
        "add_instrument_interface_input, set_instrument_interface_input_meta",
    ),
    (
        "interface.inputs{}.max",
        "add_instrument_interface_input, set_instrument_interface_input_meta",
    ),
    (
        "interface.inputs{}.curve",
        "add_instrument_interface_input, set_instrument_interface_input_meta",
    ),
    (
        "interface.inputs{}.unit",
        "add_instrument_interface_input, set_instrument_interface_input_meta",
    ),
    (
        "interface.inputs{}.from",
        "omit:Feed-variant field; an input pipe never carries `from` (untagged-union artifact)",
    ),
    ("interface.inputs{}.target", "omit:v1-only migration form"),
    (
        "interface.inputs{}.label",
        "omit:retired presentation, lives in a surface doc",
    ),
    (
        "interface.inputs{}.widget",
        "omit:retired presentation, lives in a surface doc",
    ),
    // --- interface outputs (the Feed variant is the real one here) ---
    (
        "interface.outputs{}",
        "omit:v1-only bare Target string form (migrated at the mint)",
    ),
    (
        "interface.outputs{}.type",
        "omit:Pipe-variant field; an output pipe never carries `type` (untagged-union artifact)",
    ),
    (
        "interface.outputs{}.channel",
        "add_instrument_interface_output, set_instrument_interface_output_meta",
    ),
    (
        "interface.outputs{}.default",
        "omit:Pipe-variant field; an output pipe never carries `default` (untagged-union artifact)",
    ),
    (
        "interface.outputs{}.min",
        "add_instrument_interface_output, set_instrument_interface_output_meta",
    ),
    (
        "interface.outputs{}.max",
        "add_instrument_interface_output, set_instrument_interface_output_meta",
    ),
    (
        "interface.outputs{}.curve",
        "omit:Pipe-variant field; an output pipe never carries `curve` (untagged-union artifact)",
    ),
    (
        "interface.outputs{}.unit",
        "add_instrument_interface_output, set_instrument_interface_output_meta",
    ),
    (
        "interface.outputs{}.from",
        "add_instrument_interface_output",
    ),
    ("interface.outputs{}.target", "omit:v1-only migration form"),
    (
        "interface.outputs{}.label",
        "omit:retired presentation, lives in a surface doc",
    ),
    (
        "interface.outputs{}.widget",
        "omit:retired presentation, lives in a surface doc",
    ),
];

#[cfg(test)]
mod tests;
