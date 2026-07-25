//! The verbs themselves: three pure reads and nineteen document edits, each taking the window's own
//! argument struct and answering with the window's own result shape.
//!
//! Every mutating verb funnels through [`run_edit`], which owns the two things a door would
//! otherwise re-implement: the `expect` write guard, and the split between a verb that *ran and
//! reported a problem* (an ordinary [`Answer`] carrying `ok: false`) and one that could not do the
//! job at all (a [`Refusal`]). see rules: agent-mcp

use std::fmt;

use reuben_core::edit::{self as core_edit, EditError};
use reuben_core::introspect::{self as core_introspect, PatchBoundary};
use reuben_core::projection::{Projector, Selection};
use reuben_core::resources::ResourceResolver;
use reuben_core::{content_hash, NormalizedDoc, Registry};

use super::args::*;
use super::resources::{Adapter, Resources};
use super::result::{Diag, DocumentView, EditResult, OperatorInfo, Operators, Report};

/// A verb's answer: the payload a door advertises to its caller, and the one-line gloss it shows a
/// human reading the transcript.
///
/// The gloss is here rather than in each door because it is model- and human-facing prose, and a
/// per-door copy is exactly the duplication this window exists to delete.
#[derive(Debug, Clone)]
pub struct Answer<T> {
    pub output: T,
    pub summary: String,
}

/// The verb could not do its job — an unreadable source, bytes that are not a document, arguments
/// that do not describe a coherent request, or a precondition that does not hold.
///
/// Distinct from a verb that *ran* and reported a problem: a rejected edit and a failing validation
/// are ordinary [`Answer`]s carrying `ok: false`, because the tool worked. A door renders a
/// `Refusal` as its own can't-do-the-job signal — `isError` over MCP, a non-zero exit on a CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub message: String,
}

impl Refusal {
    fn new(message: impl Into<String>) -> Self {
        Refusal {
            message: message.into(),
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Refusal {}

// --- pure reads -----------------------------------------------------------------------------------

/// List the operator set — full port objects, or the compact signature-line mode. Engine-free and
/// resource-free: the registry is the whole input. An unknown `name` is a refusal, because there is
/// no such operator to describe.
pub fn describe_operators(args: &DescribeOperators) -> Result<Answer<Operators>, Refusal> {
    let registry = Registry::builtin();
    if args.compact {
        let signatures = core_introspect::describe_compact(&registry, args.name.as_deref())
            .map_err(Refusal::new)?;
        let summary = format!("{} operator signature(s) (compact)", signatures.len());
        return Ok(Answer {
            output: Operators {
                operators: None,
                signatures: Some(signatures),
            },
            summary,
        });
    }
    let operators =
        core_introspect::describe(&registry, args.name.as_deref()).map_err(Refusal::new)?;
    let summary = describe_operators_summary(&operators);
    Ok(Answer {
        output: Operators {
            operators: Some(operators.iter().map(OperatorInfo::from).collect()),
            signatures: None,
        },
        summary,
    })
}

/// Read a structural view of a document. A document that fails to **load** still projects
/// (`loadable: false` in the header); only one that cannot be *minted* — unreadable, unparseable,
/// wrong format version — is a refusal.
pub fn describe_instrument(
    args: &DescribeInstrument,
    resources: &dyn Resources,
) -> Result<Answer<DocumentView>, Refusal> {
    let resolver = Adapter(resources);
    let json = read_document(&args.source, resources)?;
    let registry = Registry::builtin();

    // The grammar is the engine's, so no door invents its own answer for select-and-type-at-once.
    // Checked **before** the view splits, because the boundary path below has no selection to build
    // and would otherwise accept terms it then ignores — the same silent-precedence trap this call
    // exists to close, one branch further in. A caller that named terms a view cannot honour has
    // not asked a coherent question.
    let selection =
        Selection::from_terms(&args.select, args.type_name.as_deref()).map_err(Refusal::new)?;
    if args.view == InstrumentView::Boundary && selection != Selection::All {
        return Err(Refusal::new(
            "the `boundary` view is the whole face a host wires against — it takes no `select` \
             or `type`. Drop them, or read `view: \"pipes\"`, which is selectable.",
        ));
    }

    // The boundary is not a projection: it needs children *loaded* to inherit an output pipe's
    // type, so it keeps its own path — and its own "no boundary to describe" failure.
    if args.view == InstrumentView::Boundary {
        let boundary =
            core_introspect::describe_patch(&json, &registry, &resolver).map_err(|message| {
                Refusal::new(format!(
                    "{message}\n\nThe document could not be loaded, so there is no boundary to \
                     describe. Run `validate_instrument` for the full report of errors and \
                     warnings, or read `view: \"index\"` — the structural views project even when \
                     the document does not load."
                ))
            })?;
        let summary = describe_boundary_summary(&boundary);
        return Ok(Answer {
            output: DocumentView {
                view: InstrumentView::Boundary.as_str().to_string(),
                text: render_boundary(&boundary),
            },
            summary,
        });
    }

    let projector = Projector::new(&json, &registry, &resolver).map_err(Refusal::new)?;
    let text = match args.view {
        InstrumentView::Index => projector.index().render(),
        InstrumentView::Nodes => projector.zoom(&selection).render(),
        InstrumentView::Pipes => projector.pipes(&selection).render(),
        InstrumentView::Resources => projector.resources().render(),
        InstrumentView::Boundary => unreachable!("handled above"),
    };
    let view = args.view.as_str();
    let summary = format!("{view} view of {} ({} chars)", args.source, text.len());
    Ok(Answer {
        output: DocumentView {
            view: view.to_string(),
            text,
        },
        summary,
    })
}

/// Validate a document through the engine's own load + instantiate path. A *failing* validation is
/// an ordinary answer carrying `{ ok: false, errors, warnings }` — the verb worked; only an
/// unreadable source is a refusal.
pub fn validate_instrument(
    args: &ValidateInstrument,
    resources: &dyn Resources,
) -> Result<Answer<Report>, Refusal> {
    let resolver = Adapter(resources);
    let json = read_document(&args.source, resources)?;
    let registry = Registry::builtin();
    let report = Report::from(core_introspect::validate(&json, &registry, &resolver));
    let summary = validate_summary(&report);
    Ok(Answer {
        output: report,
        summary,
    })
}

// --- document verbs -------------------------------------------------------------------------------

/// Create a new, valid, minimal document at `source` and write it — the from-scratch start move.
pub fn new_instrument(
    args: &NewInstrument,
    resources: &dyn Resources,
) -> Result<Answer<EditResult>, Refusal> {
    run_edit(&args.source, &None, resources, |src, reg, res| {
        core_edit::new_instrument(src, &args.name, reg, res)
    })
}

/// Rename the instrument (its top-level `instrument` name).
pub fn set_instrument_name(
    args: &SetInstrumentName,
    resources: &dyn Resources,
) -> Result<Answer<EditResult>, Refusal> {
    run_edit(&args.source, &args.expect, resources, |src, reg, res| {
        core_edit::set_instrument_name(src, &args.name, reg, res)
    })
}

/// Set (or clear) the instrument's note.
pub fn set_instrument_description(
    args: &SetInstrumentDescription,
    resources: &dyn Resources,
) -> Result<Answer<EditResult>, Refusal> {
    run_edit(&args.source, &args.expect, resources, |src, reg, res| {
        core_edit::set_instrument_description(src, args.description.as_deref(), reg, res)
    })
}

/// Add a node fully formed in one call.
pub fn add_instrument_node(
    args: &AddInstrumentNode,
    resources: &dyn Resources,
) -> Result<Answer<EditResult>, Refusal> {
    run_edit(&args.source, &args.expect, resources, |src, reg, res| {
        core_edit::add_instrument_node(
            src,
            &args.address,
            &args.type_name,
            args.inputs.clone(),
            args.config.clone(),
            args.description.as_deref(),
            args.sample.as_deref(),
            args.voice.as_deref(),
            args.patch.as_deref(),
            reg,
            res,
        )
    })
}

/// Remove a node, cascading: auto-unwire every consumer and report what broke.
pub fn remove_instrument_node(
    args: &RemoveInstrumentNode,
    resources: &dyn Resources,
) -> Result<Answer<EditResult>, Refusal> {
    run_edit(&args.source, &args.expect, resources, |src, reg, res| {
        core_edit::remove_instrument_node(src, &args.address, reg, res)
    })
}

/// Rename a node, rewiring every consumer to the new address.
pub fn rename_instrument_node(
    args: &RenameInstrumentNode,
    resources: &dyn Resources,
) -> Result<Answer<EditResult>, Refusal> {
    run_edit(&args.source, &args.expect, resources, |src, reg, res| {
        core_edit::rename_instrument_node(src, &args.from, &args.to, reg, res)
    })
}

/// Set (or clear) a node's description.
pub fn set_instrument_node_description(
    args: &SetInstrumentNodeDescription,
    resources: &dyn Resources,
) -> Result<Answer<EditResult>, Refusal> {
    run_edit(&args.source, &args.expect, resources, |src, reg, res| {
        core_edit::set_instrument_node_description(
            src,
            &args.address,
            args.description.as_deref(),
            reg,
            res,
        )
    })
}

/// Set a node input to a literal value — the point-edit.
pub fn set_instrument_input(
    args: &SetInstrumentInput,
    resources: &dyn Resources,
) -> Result<Answer<EditResult>, Refusal> {
    run_edit(&args.source, &args.expect, resources, |src, reg, res| {
        core_edit::set_instrument_input(
            src,
            &args.address,
            &args.input,
            args.value.clone(),
            reg,
            res,
        )
    })
}

/// Wire a node input from a source port.
pub fn wire_instrument_input(
    args: &WireInstrumentInput,
    resources: &dyn Resources,
) -> Result<Answer<EditResult>, Refusal> {
    run_edit(&args.source, &args.expect, resources, |src, reg, res| {
        core_edit::wire_instrument_input(src, &args.address, &args.input, &args.from, reg, res)
    })
}

/// Unwire a node input, reverting it to the operator's default.
pub fn unwire_instrument_input(
    args: &UnwireInstrumentInput,
    resources: &dyn Resources,
) -> Result<Answer<EditResult>, Refusal> {
    run_edit(&args.source, &args.expect, resources, |src, reg, res| {
        core_edit::unwire_instrument_input(src, &args.address, &args.input, reg, res)
    })
}

/// Set an instantiate-time constant on a node.
pub fn set_instrument_constant(
    args: &SetInstrumentConstant,
    resources: &dyn Resources,
) -> Result<Answer<EditResult>, Refusal> {
    run_edit(&args.source, &args.expect, resources, |src, reg, res| {
        core_edit::set_instrument_constant(
            src,
            &args.address,
            &args.name,
            args.value.clone(),
            reg,
            res,
        )
    })
}

/// Add an interface input pipe.
pub fn add_instrument_interface_input(
    args: &AddInstrumentInterfaceInput,
    resources: &dyn Resources,
) -> Result<Answer<EditResult>, Refusal> {
    run_edit(&args.source, &args.expect, resources, |src, reg, res| {
        core_edit::add_instrument_interface_input(
            src,
            &args.name,
            &args.type_name,
            args.channel,
            args.default.clone(),
            args.min,
            args.max,
            args.curve.as_deref(),
            args.unit.as_deref(),
            reg,
            res,
        )
    })
}

/// Add an interface output pipe (a master tap).
pub fn add_instrument_interface_output(
    args: &AddInstrumentInterfaceOutput,
    resources: &dyn Resources,
) -> Result<Answer<EditResult>, Refusal> {
    run_edit(&args.source, &args.expect, resources, |src, reg, res| {
        core_edit::add_instrument_interface_output(
            src,
            &args.name,
            &args.from,
            args.channel,
            args.min,
            args.max,
            args.unit.as_deref(),
            reg,
            res,
        )
    })
}

/// Remove an interface input pipe.
pub fn remove_instrument_interface_input(
    args: &RemoveInstrumentInterfacePipe,
    resources: &dyn Resources,
) -> Result<Answer<EditResult>, Refusal> {
    run_edit(&args.source, &args.expect, resources, |src, reg, res| {
        core_edit::remove_instrument_interface_input(src, &args.name, reg, res)
    })
}

/// Remove an interface output pipe.
pub fn remove_instrument_interface_output(
    args: &RemoveInstrumentInterfacePipe,
    resources: &dyn Resources,
) -> Result<Answer<EditResult>, Refusal> {
    run_edit(&args.source, &args.expect, resources, |src, reg, res| {
        core_edit::remove_instrument_interface_output(src, &args.name, reg, res)
    })
}

/// Update an interface input pipe's metadata.
pub fn set_instrument_interface_input_meta(
    args: &SetInstrumentInterfaceInputMeta,
    resources: &dyn Resources,
) -> Result<Answer<EditResult>, Refusal> {
    run_edit(&args.source, &args.expect, resources, |src, reg, res| {
        core_edit::set_instrument_interface_input_meta(
            src,
            &args.name,
            args.channel,
            args.default.clone(),
            args.min,
            args.max,
            args.curve.as_deref(),
            args.unit.as_deref(),
            reg,
            res,
        )
    })
}

/// Update an interface output pipe's metadata.
pub fn set_instrument_interface_output_meta(
    args: &SetInstrumentInterfaceOutputMeta,
    resources: &dyn Resources,
) -> Result<Answer<EditResult>, Refusal> {
    run_edit(&args.source, &args.expect, resources, |src, reg, res| {
        core_edit::set_instrument_interface_output_meta(
            src,
            &args.name,
            args.channel,
            args.min,
            args.max,
            args.unit.as_deref(),
            reg,
            res,
        )
    })
}

/// Add a resource to the document's id→source table.
pub fn add_instrument_resource(
    args: &AddInstrumentResource,
    resources: &dyn Resources,
) -> Result<Answer<EditResult>, Refusal> {
    run_edit(&args.source, &args.expect, resources, |src, reg, res| {
        core_edit::add_instrument_resource(src, &args.id, &args.resource_source, reg, res)
    })
}

/// Remove a resource from the document's id→source table.
pub fn remove_instrument_resource(
    args: &RemoveInstrumentResource,
    resources: &dyn Resources,
) -> Result<Answer<EditResult>, Refusal> {
    run_edit(&args.source, &args.expect, resources, |src, reg, res| {
        core_edit::remove_instrument_resource(src, &args.id, reg, res)
    })
}

// --- the shared edit pipeline ---------------------------------------------------------------------

/// Run one document verb: apply the `expect` guard, invoke the engine's verb, and map its outcome.
/// Every mutating verb above is one call to this, so the guard and the
/// rejected-edit-versus-refusal split cannot drift verb to verb.
fn run_edit(
    source: &str,
    expect: &Option<String>,
    resources: &dyn Resources,
    verb: impl FnOnce(
        &str,
        &Registry,
        &dyn ResourceResolver,
    ) -> Result<core_edit::EditResult, EditError>,
) -> Result<Answer<EditResult>, Refusal> {
    let resolver = Adapter(resources);
    let registry = Registry::builtin();
    if let Some(conflict) = expect_conflict(expect, source, &registry, &resolver) {
        return Ok(conflict);
    }
    let result = EditResult::from(
        verb(source, &registry, &resolver).map_err(|why| Refusal::new(why.to_string()))?,
    );
    let summary = edit_summary(&result);
    Ok(Answer {
        output: result,
        summary,
    })
}

/// The optimistic-concurrency `expect` guard: compare the caller's expected content hash against
/// the source's current one.
///
/// `Some` is the ready-to-return ordinary answer for a miss — nothing written, the real hash and
/// the current node index handed back, so reconciling costs no extra round trip. `None` means
/// proceed, and is also what a read or mint failure returns: that is the verb's to surface, not the
/// guard's.
///
/// It lives here, once, rather than in each door — see rules: agent-mcp.
fn expect_conflict(
    expect: &Option<String>,
    source: &str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Option<Answer<EditResult>> {
    let expected = expect.as_ref()?;
    let json = resolver.resolve_text(source).ok()?;
    let actual = NormalizedDoc::from_json(&json, registry, Some(resolver))
        .ok()
        .map(|doc| content_hash(&doc))?;
    if actual == *expected {
        return None;
    }
    // The index, not a zoom: no verb ran, so there is no narrower thing to show.
    let zoom = match Projector::new(&json, registry, resolver) {
        Ok(p) => p.index().render(),
        Err(e) => format!("(projection unavailable: {e})"),
    };
    Some(Answer {
        output: EditResult {
            report: Report {
                ok: false,
                errors: vec![Diag {
                    node: None,
                    port: None,
                    message: format!(
                        "expect guard: the document is now {actual}, not the expected {expected} \
                         — re-read it and reconcile before writing"
                    ),
                }],
                warnings: Vec::new(),
            },
            written: false,
            hash: actual,
            notes: Vec::new(),
            zoom,
        },
        summary: "write rejected by the expect guard — nothing changed".to_string(),
    })
}

/// Read a document's text through the host's store, naming the source the caller used when it is
/// not there to read.
fn read_document(source: &str, resources: &dyn Resources) -> Result<String, Refusal> {
    resources
        .read_text(source)
        .map_err(|e| Refusal::new(format!("could not read instrument source `{source}`: {e}")))
}

// --- the one-line glosses -------------------------------------------------------------------------

/// Render a [`PatchBoundary`] in the projection's line grammar, so `view: "boundary"` reads like
/// every other view even though it is cut by a different code path.
fn render_boundary(boundary: &PatchBoundary) -> String {
    let mut lines = vec![format!("{} (boundary)", boundary.instrument)];
    if boundary.is_empty() {
        lines.push("no interface — nests, but exposes nothing to wire".to_string());
    }
    for (label, ports) in [("in", &boundary.inputs), ("out", &boundary.outputs)] {
        for p in ports.iter() {
            lines.push(format!("{label} {}", p.signature_fragment()));
        }
    }
    for (label, dark) in [
        ("in", &boundary.dark_inputs),
        ("out", &boundary.dark_outputs),
    ] {
        for d in dark.iter() {
            lines.push(format!("{label} {d} (dark — unresolved this load)"));
        }
    }
    for w in &boundary.warnings {
        lines.push(format!("warning: {}", w.message));
    }
    lines.join("\n")
}

/// One-line gloss of an operator listing: a single operator's port counts, or the roster.
fn describe_operators_summary(operators: &[core_introspect::OperatorInfo]) -> String {
    match operators {
        [one] => format!(
            "{}: {} input(s), {} output(s)",
            one.type_name,
            one.inputs.len(),
            one.outputs.len()
        ),
        many => {
            let names: Vec<&str> = many.iter().map(|o| o.type_name.as_str()).collect();
            format!("{} operators: {}", many.len(), names.join(", "))
        }
    }
}

/// One-line gloss of a described instrument boundary.
fn describe_boundary_summary(boundary: &PatchBoundary) -> String {
    format!(
        "{}: {} boundary input(s), {} output(s)",
        boundary.instrument,
        boundary.inputs.len(),
        boundary.outputs.len()
    )
}

/// One-line gloss of a validation report.
fn validate_summary(report: &Report) -> String {
    if report.ok {
        match report.warnings.len() {
            0 => "valid".to_string(),
            n => format!("valid ({n} warning(s))"),
        }
    } else {
        format!(
            "invalid: {} error(s), {} warning(s)",
            report.errors.len(),
            report.warnings.len()
        )
    }
}

/// One-line gloss of a document-verb result.
fn edit_summary(result: &EditResult) -> String {
    if result.written {
        let base = format!("written (content_hash {})", result.hash);
        match result.notes.len() {
            0 => base,
            n => format!("{base}; {n} cascade note(s)"),
        }
    } else {
        format!(
            "not written — {} error(s); the document is unchanged (content_hash {})",
            result.report.errors.len(),
            result.hash
        )
    }
}
