//! The verbs themselves: the pure reads and the document edits, each taking the window's own
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
use reuben_core::vocabulary::Section;
use reuben_core::{content_hash, LoadError, NormalizedDoc, Registry};

use super::args::*;
use super::result::{Boundary, Diag, DocumentView, EditResult, OperatorInfo, Operators, Report};
use crate::resources::{Adapter, Resources};

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
    pub(crate) fn new(message: impl Into<String>) -> Self {
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
            operators: Some(operators.iter().map(OperatorInfo::from_core).collect()),
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
        let boundary = boundary_of(&json, &registry, &resolver)?;
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

/// Read a document's nesting face **structurally** — the same question
/// [`describe_instrument`] answers as a rendered line under `view: "boundary"`, in the shape a
/// program consumes instead of the shape a model reads.
///
/// Both are one code path down to the engine call, so the surface a control-surface generator
/// builds against and the surface a model is told about cannot describe different pipes.
pub fn describe_boundary(
    args: &DescribeBoundary,
    resources: &dyn Resources,
) -> Result<Answer<Boundary>, Refusal> {
    let resolver = Adapter(resources);
    let json = read_document(&args.source, resources)?;
    let boundary = boundary_of(&json, &Registry::builtin(), &resolver)?;
    let summary = describe_boundary_summary(&boundary);
    Ok(Answer {
        output: Boundary::from_core(&boundary),
        summary,
    })
}

/// The engine's boundary description, with the one refusal both boundary reads share: a document
/// that will not **load** has no boundary at all, because an output pipe's type is inherited from
/// the internal port feeding it and nothing was built to inherit from. The message says what to
/// reach for instead, since going blind is the worst answer to "this did not load".
fn boundary_of(
    json: &str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<PatchBoundary, Refusal> {
    core_introspect::describe_patch(json, registry, resolver).map_err(|message| {
        Refusal::new(format!(
            "{message}\n\nThe document could not be loaded, so there is no boundary to \
             describe. Run `validate_instrument` for the full report of errors and \
             warnings, or read `view: \"index\"` — the structural views project even when \
             the document does not load."
        ))
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
    let report = Report::from_core(core_introspect::validate(&json, &registry, &resolver));
    let summary = validate_summary(&report);
    Ok(Answer {
        output: report,
        summary,
    })
}

/// Project one document to its **library-index signature line** — name, role, `(inputs) → outputs`
/// — through the same load path [`describe_instrument`] uses, so a line never advertises a face the
/// document does not have.
///
/// Not a roster verb: no door serves it to a caller. It is the per-document half of a generated
/// artifact (`instruments/index.md`), and the sweep that walks a library and orders the lines is a
/// host's, because only a host knows what its library *is*.
pub fn library_index_line(source: &str, resources: &dyn Resources) -> Result<String, Refusal> {
    let resolver = Adapter(resources);
    let json = read_document(source, resources)?;
    core_introspect::library_index_line(&json, &Registry::builtin(), &resolver)
        .map_err(Refusal::new)
}

/// The content identity of a document a host is **holding** — the same opaque token a document
/// verb hands back after writing, minted standing alone rather than as the by-product of an edit.
///
/// By value, because the bytes are the question. A door needs this for a document sitting at no
/// source it can name — the one it has installed, say, while the store's copy at that key has moved
/// on under an unshipped edit. Hashing what a source reads back *now* composes from `read_text` and
/// this.
///
/// Not a roster verb: no door serves it to a caller. `resources` is here because normalizing a
/// document resolves the references it makes.
pub fn hash_instrument(json: &str, resources: &dyn Resources) -> Result<String, Refusal> {
    let resolver = Adapter(resources);
    hash_document(json, &Registry::builtin(), &resolver).map_err(|e| Refusal::new(e.to_string()))
}

// --- document verbs -------------------------------------------------------------------------------

/// Create a new, valid, minimal document at `source` and write it — the from-scratch start move.
pub fn new_instrument(
    args: &NewInstrument,
    resources: &dyn Resources,
) -> Result<Answer<EditResult>, Refusal> {
    // The one verb with no `expect` field to pass: there is no prior document to have raced with,
    // and the refusal-to-overwrite below is the guard that fits a create.
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

/// Set a node input — or an interface input pipe's seed, addressed as the node it mints — to a
/// literal value. The point-edit.
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

/// Apply one intent word as a batch of value edits. The one place a `section` word becomes a
/// section, and an unknown one is a refusal rather than a broadened search.
/// see rules: agent-mcp
pub fn set_instrument_inputs_by_intent(
    args: &SetInstrumentInputsByIntent,
    resources: &dyn Resources,
) -> Result<Answer<EditResult>, Refusal> {
    let section = match args.section.as_deref() {
        None => None,
        Some(word) => Some(Section::parse(word).ok_or_else(|| {
            Refusal::new(format!(
                "`{word}` is not a section of the intent vocabulary — the sections are timbral, \
                 rhythmic and tonal, and most words need none"
            ))
        })?),
    };
    run_edit(&args.source, &args.expect, resources, |src, reg, res| {
        core_edit::set_instrument_inputs_by_intent(src, &args.word, section, &args.target, reg, res)
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
            args.value.clone(),
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

/// Update an interface input pipe's quantity contract.
pub fn set_instrument_interface_input_meta(
    args: &SetInstrumentInterfaceInputMeta,
    resources: &dyn Resources,
) -> Result<Answer<EditResult>, Refusal> {
    run_edit(&args.source, &args.expect, resources, |src, reg, res| {
        core_edit::set_instrument_interface_input_meta(
            src,
            &args.name,
            args.channel,
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
    let result = EditResult::from_core(
        verb(source, &registry, &resolver).map_err(|why| Refusal::new(why.to_string()))?,
    );
    let summary = edit_summary(&result);
    Ok(Answer {
        output: result,
        summary,
    })
}

/// The document verbs' optimistic-concurrency `expect` guard: compare the caller's expected content
/// hash against the **source's** current one, before writing to it.
///
/// Not the swap guard, which compares against the hash the engine has **installed** and is a
/// different question with a different answer shape (`expect-guard-is-a-door-concern` governs that
/// one, and it stays where it is — nothing here touches it). The two are easy to conflate because
/// they share a field name and a hash format.
///
/// `Some` is the ready-to-return ordinary answer for a miss — nothing written, the real hash and
/// the current node index handed back, so reconciling costs no extra round trip. `None` means
/// proceed, and is also what a read or mint failure returns: that is the verb's to surface, not the
/// guard's.
fn expect_conflict(
    expect: &Option<String>,
    source: &str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Option<Answer<EditResult>> {
    let expected = expect.as_ref()?;
    let json = resolver.resolve_text(source).ok()?;
    let actual = hash_document(&json, registry, resolver).ok()?;
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

/// Normalize a document and hash its canonical bytes — the one path both the standing
/// [`hash_instrument`] verb and the `expect` guard mint through, so the token a door quotes back
/// and the token the guard compares it against cannot be arrived at differently.
fn hash_document(
    json: &str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<String, LoadError> {
    NormalizedDoc::from_json(json, registry, Some(resolver)).map(|doc| content_hash(&doc))
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
            n => format!("{base}; {n} note(s)"),
        }
    } else {
        format!(
            "not written — {} error(s); the document is unchanged (content_hash {})",
            result.report.errors.len(),
            result.hash
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::{ResolveError, SampleBuffer};
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    /// A store with no filesystem under it: sources are exact keys.
    ///
    /// The point is not convenience. `Resources` is the seam a host that is *not* a filesystem
    /// fills — the browser is the one this repo is headed for — and until something implements it
    /// without a path in sight, "host-implemented" is a claim with no evidence. This is the
    /// evidence, and it is also what lets the guard below be tested without temp files.
    #[derive(Default)]
    struct MemoryStore {
        texts: RefCell<BTreeMap<String, String>>,
    }

    impl MemoryStore {
        fn with(source: &str, text: &str) -> Self {
            let store = Self::default();
            store
                .texts
                .borrow_mut()
                .insert(source.to_string(), text.to_string());
            store
        }

        fn read(&self, source: &str) -> String {
            self.texts.borrow()[source].clone()
        }
    }

    impl Resources for MemoryStore {
        fn read_samples(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
            Err(ResolveError::NotFound(source.to_string()))
        }

        fn read_text(&self, source: &str) -> Result<String, ResolveError> {
            self.texts
                .borrow()
                .get(source)
                .cloned()
                .ok_or_else(|| ResolveError::NotFound(source.to_string()))
        }

        fn write_text(&self, source: &str, text: &str) -> Result<(), ResolveError> {
            self.texts
                .borrow_mut()
                .insert(source.to_string(), text.to_string());
            Ok(())
        }
    }

    const SEED: &str = r#"{
        "format_version": 3,
        "instrument": "guard-test",
        "nodes": [ { "type": "oscillator", "address": "/osc", "inputs": { "freq": 220.0 } } ]
    }"#;

    fn set_freq(source: &str, value: f64, expect: Option<&str>) -> SetInstrumentInput {
        SetInstrumentInput {
            source: source.to_string(),
            address: "/osc".to_string(),
            input: "freq".to_string(),
            value: serde_json::json!(value),
            expect: expect.map(str::to_string),
        }
    }

    /// An unguarded edit writes, and hands back the hash a guarded follow-up must quote.
    #[test]
    fn an_edit_writes_through_the_store_and_returns_the_new_hash() {
        let store = MemoryStore::with("inst.json", SEED);
        let answer = set_instrument_input(&set_freq("inst.json", 440.0, None), &store)
            .expect("an edit to a real node is not a refusal");

        assert!(answer.output.written, "{:?}", answer.output);
        assert!(answer.output.report.ok);
        assert!(store.read("inst.json").contains("440"));
        assert!(
            answer.summary.contains(&answer.output.hash),
            "the gloss quotes the hash a guarded write will need: {}",
            answer.summary
        );
    }

    /// A guard quoting the current hash proceeds — the case a guard that always rejected would
    /// still pass the miss test below.
    #[test]
    fn the_expect_guard_lets_a_current_hash_through() {
        let store = MemoryStore::with("inst.json", SEED);
        let hash = set_instrument_input(&set_freq("inst.json", 440.0, None), &store)
            .expect("first write")
            .output
            .hash;

        let answer = set_instrument_input(&set_freq("inst.json", 880.0, Some(&hash)), &store)
            .expect("a matching guard is not a refusal");
        assert!(answer.output.written, "{:?}", answer.output);
        assert!(store.read("inst.json").contains("880"));
    }

    /// A guard quoting a stale hash rejects — as an ordinary answer, not a refusal: the verb
    /// worked, and its report is what the caller reconciles against. Nothing is written.
    #[test]
    fn the_expect_guard_rejects_a_stale_hash_without_writing() {
        let store = MemoryStore::with("inst.json", SEED);
        let before = store.read("inst.json");

        let answer = set_instrument_input(
            &set_freq("inst.json", 440.0, Some("deadbeefdeadbeef")),
            &store,
        )
        .expect("a guard miss is an answer, not a refusal");

        assert!(!answer.output.written);
        assert!(!answer.output.report.ok);
        assert_eq!(store.read("inst.json"), before, "nothing was written");
        // The real hash, so reconciling costs no extra round trip — and it is the *actual* one,
        // never the expected one the caller already holds.
        assert_ne!(answer.output.hash, "deadbeefdeadbeef");
        assert!(
            answer.output.zoom.contains("/osc"),
            "the guard echoes the current index: {:?}",
            answer.output.zoom
        );
    }

    /// A precondition that cannot hold is a refusal, not a rejected edit — the split every door
    /// renders as its own can't-do-the-job signal.
    #[test]
    fn a_missing_target_is_a_refusal_not_a_rejected_edit() {
        let store = MemoryStore::with("inst.json", SEED);
        let mut args = set_freq("inst.json", 440.0, None);
        args.address = "/ghost".to_string();

        let refusal = set_instrument_input(&args, &store).expect_err("no such node");
        assert!(refusal.message.contains("/ghost"), "{refusal}");
    }

    /// A document with a face to read: one input pipe carrying declared metadata, one output tap.
    const NESTABLE: &str = r#"{
        "format_version": 3,
        "instrument": "nestable",
        "nodes": [ { "type": "oscillator", "address": "/osc" } ],
        "interface": {
            "inputs": { "freq": { "type": "f32_buffer", "default": 220.0, "min": 20.0,
                                  "max": 20000.0, "curve": "exp", "unit": "Hz" } },
            "outputs": { "out": { "from": "/osc.audio" } }
        }
    }"#;

    /// The two boundary reads are one answer in two shapes: a program takes the structured pipes,
    /// a model takes the rendered line. They run different code above the engine call, which is
    /// exactly how a door ends up believing in pipes the other door does not serve — so this pins
    /// them to the same names.
    #[test]
    fn both_boundary_reads_describe_the_same_pipes() {
        let store = MemoryStore::with("nestable.json", NESTABLE);

        let structured = describe_boundary(
            &DescribeBoundary {
                source: "nestable.json".to_string(),
            },
            &store,
        )
        .expect("a loadable document has a boundary")
        .output;
        let rendered = describe_instrument(
            &DescribeInstrument {
                source: "nestable.json".to_string(),
                view: InstrumentView::Boundary,
                ..Default::default()
            },
            &store,
        )
        .expect("the same document, the same verb")
        .output;

        assert_eq!(structured.instrument, "nestable");
        let names: Vec<&str> = structured
            .inputs
            .iter()
            .chain(&structured.outputs)
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(names, ["freq", "out"], "{structured:?}");
        for name in names {
            assert!(
                rendered.text.contains(name),
                "the rendered face is missing `{name}`: {}",
                rendered.text
            );
        }
        // The declared metadata survives the trip through the window's own port shape — the thing
        // a control surface builds its widget range out of.
        let freq = &structured.inputs[0];
        assert_eq!((freq.min, freq.max), (Some(20.0), Some(20000.0)));
        assert_eq!(freq.unit, "Hz");
        assert_eq!(freq.curve.as_deref(), Some("exponential"));
    }

    /// A document that will not load has no boundary either way: an output pipe's type is
    /// inherited from the port feeding it, and nothing was built to inherit from.
    #[test]
    fn a_boundary_read_of_an_unloadable_document_is_a_refusal() {
        let store = MemoryStore::with("broken.json", r#"{"format_version": 3}"#);
        let refusal = describe_boundary(
            &DescribeBoundary {
                source: "broken.json".to_string(),
            },
            &store,
        )
        .expect_err("nothing to describe");
        assert!(
            refusal.message.contains("validate_instrument"),
            "the refusal says what to reach for instead: {refusal}"
        );
    }

    /// A source the store does not hold is a refusal naming the source as the caller spelled it.
    #[test]
    fn an_unreadable_source_is_a_refusal() {
        let store = MemoryStore::default();
        let refusal = validate_instrument(
            &ValidateInstrument {
                source: "nope.json".to_string(),
            },
            &store,
        )
        .expect_err("nothing to validate");
        assert!(refusal.message.contains("nope.json"), "{refusal}");
    }

    /// The standing verb and the write's by-product are the same token for the same content — the
    /// property that lets a door quote one into an `expect` guard that compares the other.
    #[test]
    fn the_standing_hash_equals_the_hash_an_edit_hands_back() {
        let store = MemoryStore::with("inst.json", SEED);
        let written = set_instrument_input(&set_freq("inst.json", 440.0, None), &store)
            .expect("an edit to a real node is not a refusal")
            .output
            .hash;

        let standing =
            hash_instrument(&store.read("inst.json"), &store).expect("a written document hashes");
        assert_eq!(standing, written);
    }

    /// Content identity, not text identity: reformatting the source moves neither.
    #[test]
    fn the_standing_hash_is_stable_across_formatting_and_moves_on_a_changed_value() {
        let store = MemoryStore::default();
        let pretty = hash_instrument(SEED, &store).expect("the seed hashes");
        let compact = hash_instrument(
            &SEED.split_whitespace().collect::<Vec<_>>().join(" "),
            &store,
        )
        .expect("the reflowed seed hashes");
        let changed = hash_instrument(&SEED.replace("220.0", "440.0"), &store)
            .expect("the retuned seed hashes");

        assert_eq!(pretty, compact);
        assert_ne!(pretty, changed);
    }

    /// A document that will not normalize has no identity to report — a refusal that says why, not
    /// an empty token. Both stages, because they fail in different places: bytes that are not JSON
    /// never reach the loader, and a format version from the future parses fine and is refused
    /// inside it.
    #[test]
    fn a_document_that_will_not_normalize_is_a_refusal() {
        let store = MemoryStore::default();
        for (why, json) in [
            ("unparseable", r#"{"format_version": 3"#.to_string()),
            (
                "from the future",
                SEED.replace("\"format_version\": 3", "\"format_version\": 99"),
            ),
        ] {
            let refusal = hash_instrument(&json, &store).expect_err(why);
            assert!(!refusal.message.is_empty(), "{why} says why it has no hash");
        }
    }

    /// Identity is not validity: a document naming an operator the registry does not hold still
    /// hashes. Normalization is structural, and a door that refused here would leave a caller
    /// unable to name the very document it is trying to repair.
    #[test]
    fn a_document_that_will_not_load_still_has_an_identity() {
        let store = MemoryStore::default();
        hash_instrument(&SEED.replace("oscillator", "nosuchoperator"), &store)
            .expect("an unloadable document is still some particular document");
    }
}
