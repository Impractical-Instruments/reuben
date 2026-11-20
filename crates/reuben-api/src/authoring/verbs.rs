//! The verbs themselves: the pure reads and the document edits, each taking the window's own
//! argument struct and answering with the window's own result shape.
//!
//! Every mutating verb funnels through [`run_edit`], which owns the two things a door would
//! otherwise re-implement: the `expect` write guard, and the split between a verb that *ran and
//! reported a problem* (an ordinary [`Answer`] carrying `ok: false`) and one that could not do the
//! job at all (a [`Refusal`]).

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use std::fmt;

use reuben_core::Registry;
use reuben_document::edit::{self as core_edit, EditError};
use reuben_document::format::{self as core_format, Layout, WirePorts};
use reuben_document::introspect::{self as core_introspect, PatchBoundary};
use reuben_document::projection::{Projector, Selection};
use reuben_document::resources::ResourceResolver;
use reuben_document::structure as core_structure;
use reuben_document::vocabulary::Section;
use reuben_document::{content_hash, LoadError, NormalizedDoc};

use super::args::*;
use super::result::{
    Boundary, Diag, DocumentView, EditResult, InstrumentStructure, OperatorInfo, Operators, Report,
    WireVerdict, WireVerdicts,
};
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

    let selection = selection_of(args)?;

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

/// The selection `args` asks for, refused when the view would drop it.
///
/// The grammar is the engine's, so no caller invents its own answer for select-and-type-at-once.
/// A view that does not narrow must refuse terms rather than ignore them: a caller that named terms
/// gets a plausible whole answer to a question it did not ask.
fn selection_of(args: &DescribeInstrument) -> Result<Selection, Refusal> {
    let selection =
        Selection::from_terms(&args.select, args.type_name.as_deref()).map_err(Refusal::new)?;
    if selection == Selection::All {
        return Ok(selection);
    }
    let whole = match args.view {
        InstrumentView::Nodes | InstrumentView::Pipes => return Ok(selection),
        InstrumentView::Index => "is every node, one line each",
        InstrumentView::Resources => "is the whole resources table",
        InstrumentView::Boundary => "is the whole face a host wires against",
    };
    Err(Refusal::new(format!(
        "the `{view}` view {whole} — it takes no `select` or `type`. Drop them, or read \
         `view: \"nodes\"` or `view: \"pipes\"`, which are selectable.",
        view = args.view.as_str()
    )))
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
             describe. Run `{validate}` for the full report of errors and \
             warnings, or read `view: \"index\"` — the structural views project even when \
             the document does not load.",
            validate = crate::tools::names::VALIDATE_INSTRUMENT
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

/// Write the document at `from` to the new source `to`, for a host that needs a second document
/// without ever holding the first one's text. The answer's `hash` is the copy's, and equals
/// `from`'s. Not a roster verb: no agent-facing door serves it.
///
/// `resources` reads `from` as spelled and reads and writes the copy by `to`'s
/// [`canonical`](Resources::canonical) id, so the place checked for an existing document is the
/// place written. The same call decides whether `to` is beside `from`: the two must resolve `.` to
/// the same identity. A store whose sources are exact keys therefore accepts any `to`, since none
/// of its references are relative to the document making them. A store that searches a second
/// location for a missing source (a library root) answers with a document found there, and the
/// copy is refused as already existing.
pub fn copy_instrument(
    args: &CopyInstrument,
    resources: &dyn Resources,
) -> Result<Answer<EditResult>, Refusal> {
    // No `expect`, as for `new_instrument`: a create has no prior document to have raced with.
    run_edit(&args.to, &None, resources, |to, reg, res| {
        core_edit::copy_instrument(&args.from, to, reg, res)
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

/// Remove an instantiate-time constant from a node, reverting it to the operator's default.
pub fn remove_instrument_constant(
    args: &RemoveInstrumentConstant,
    resources: &dyn Resources,
) -> Result<Answer<EditResult>, Refusal> {
    run_edit(&args.source, &args.expect, resources, |src, reg, res| {
        core_edit::remove_instrument_constant(src, &args.address, &args.name, reg, res)
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

/// Remove metadata fields from an interface pipe of either direction.
pub fn clear_instrument_interface_meta(
    args: &ClearInstrumentInterfaceMeta,
    resources: &dyn Resources,
) -> Result<Answer<EditResult>, Refusal> {
    let direction = match args.direction {
        PipeDirection::Input => core_edit::PipeDirection::Input,
        PipeDirection::Output => core_edit::PipeDirection::Output,
    };
    let fields: Vec<core_edit::MetaField> = (args.fields.iter())
        .map(|field| match field {
            MetaField::Channel => core_edit::MetaField::Channel,
            MetaField::Min => core_edit::MetaField::Min,
            MetaField::Max => core_edit::MetaField::Max,
            MetaField::Curve => core_edit::MetaField::Curve,
            MetaField::Unit => core_edit::MetaField::Unit,
        })
        .collect();
    run_edit(&args.source, &args.expect, resources, |src, reg, res| {
        core_edit::clear_instrument_interface_meta(src, &args.name, direction, &fields, reg, res)
    })
}

/// Feed an existing interface output pipe from another internal port, keeping its metadata and
/// layout. Not a roster verb: an agent removes and re-adds the pipe, and the editor needs the
/// change as one write, so that dropping a wire on the pipe is one undo step.
pub fn wire_instrument_interface_output(
    args: &WireInstrumentInterfaceOutput,
    resources: &dyn Resources,
) -> Result<Answer<EditResult>, Refusal> {
    run_edit(&args.source, &args.expect, resources, |src, reg, res| {
        core_edit::wire_instrument_interface_output(src, &args.name, &args.from, reg, res)
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

/// Point an existing resource at a new source: the one write that changes what every node
/// referencing it loads. Not a roster verb: no agent-facing door serves it.
pub fn set_instrument_resource(
    args: &SetInstrumentResource,
    resources: &dyn Resources,
) -> Result<Answer<EditResult>, Refusal> {
    run_edit(&args.source, &args.expect, resources, |src, reg, res| {
        core_edit::set_instrument_resource(src, &args.id, &args.resource_source, reg, res)
    })
}

/// Set the resource id a node reads through its `sample`, `voice` or `patch` slot. Not a roster
/// verb: no agent-facing door serves it yet.
pub fn set_instrument_node_resource(
    args: &SetInstrumentNodeResource,
    resources: &dyn Resources,
) -> Result<Answer<EditResult>, Refusal> {
    run_edit(&args.source, &args.expect, resources, |src, reg, res| {
        core_edit::set_instrument_node_resource(src, &args.address, &args.slot, &args.id, reg, res)
    })
}

/// Set canvas positions on many nodes and interface pipes in one write. Not a roster verb: the
/// position is the editor's, and no agent-facing door serves it.
pub fn set_instrument_layout(
    args: &SetInstrumentLayout,
    resources: &dyn Resources,
) -> Result<Answer<EditResult>, Refusal> {
    let positions: Vec<(core_edit::LayoutTarget, Layout)> = args
        .positions
        .iter()
        .map(|p| {
            let target = match &p.target {
                LayoutTarget::Node(address) => core_edit::LayoutTarget::Node(address.clone()),
                LayoutTarget::Output(OutputTarget { output }) => {
                    core_edit::LayoutTarget::Output(output.clone())
                }
            };
            (target, Layout { x: p.x, y: p.y })
        })
        .collect();
    run_edit(&args.source, &args.expect, resources, |src, reg, res| {
        core_edit::set_instrument_layout(src, &positions, reg, res)
    })
}

// --- editor queries -------------------------------------------------------------------------------

/// Judge a wire from one source to each target on port types alone: whether the loader's wire type
/// rule accepts it, and the load error it would raise if not. The document is read and its
/// endpoints resolved once per call, whatever the number of targets.
///
/// Not a roster verb: it is what an editor asks while a wire is dragged. A wire that would close a
/// cycle still answers `ok` — that is the edit's to report. A source the document cannot resolve
/// is a refusal, since no target could take it; a target it cannot resolve is that target's
/// `ok: false`.
pub fn wire_compatibility(
    args: &WireCompatibility,
    resources: &dyn Resources,
) -> Result<Answer<WireVerdicts>, Refusal> {
    let resolver = Adapter(resources);
    let json = read_document(&args.source, resources)?;
    let registry = Registry::builtin();
    let doc = NormalizedDoc::from_json(&json, &registry, Some(&resolver))
        .map_err(|e| Refusal::new(e.to_string()))?;
    let ports = WirePorts::resolve(&doc, &registry, &resolver).map_err(|e| {
        Refusal::new(format!(
            "{e}\n\nThe document does not load far enough to resolve its wire endpoints."
        ))
    })?;
    let from = match &args.from {
        WireSource::NodeOutput { node, port } => core_format::WireSource::Output {
            node: node.clone(),
            port: port.clone(),
        },
        WireSource::InterfaceInput { name } => {
            core_format::WireSource::InterfaceInput(name.clone())
        }
    };
    ports
        .check_source(&from)
        .map_err(|e| Refusal::new(e.to_string()))?;
    let targets = match &args.to {
        Some(to) => to.clone(),
        None => ports
            .targets()
            .into_iter()
            .map(|t| match t {
                core_format::WireTarget::Input { node, port } => {
                    WireTarget::NodeInput { node, port }
                }
                core_format::WireTarget::InterfaceOutput(name) => {
                    WireTarget::InterfaceOutput { name }
                }
            })
            .collect(),
    };
    let verdicts: Vec<WireVerdict> = targets
        .into_iter()
        .map(|target| {
            let to = match &target {
                WireTarget::NodeInput { node, port } => core_format::WireTarget::Input {
                    node: node.clone(),
                    port: port.clone(),
                },
                WireTarget::InterfaceOutput { name } => {
                    core_format::WireTarget::InterfaceOutput(name.clone())
                }
            };
            let reason = ports.check(&from, &to).err().map(|e| e.to_string());
            WireVerdict {
                target,
                ok: reason.is_none(),
                reason,
            }
        })
        .collect();
    let accepted = verdicts.iter().filter(|v| v.ok).count();
    let summary = format!(
        "{accepted} of {} target(s) accept a wire from {}",
        verdicts.len(),
        from.wire_ref()
    );
    Ok(Answer {
        output: WireVerdicts { verdicts },
        summary,
    })
}

/// Read a document as data: its nodes with their values, wires and resolved ports, its interface
/// pipes, its resources, the canvas positions, and its content hash. The document is read and
/// parsed once.
///
/// Not a roster verb: it is what an editor draws from, and the only read that reports `layout`. A
/// document that does not **load** is still read in full, with the load report saying where it is
/// broken; only one that cannot be read or parsed is a refusal.
///
/// `resources` must be scoped to `source`, as for every document verb: the document's references
/// resolve from the store's top level. To follow a resource into the document it names, read that
/// document through a store scoped to *its* canonical source. One store reused down the tree
/// resolves a nested document's references from the wrong place and reports them missing.
pub fn read_instrument(
    args: &ReadInstrument,
    resources: &dyn Resources,
) -> Result<Answer<InstrumentStructure>, Refusal> {
    let resolver = Adapter(resources);
    let json = read_document(&args.source, resources)?;
    let registry = Registry::builtin();
    let doc = NormalizedDoc::from_json(&json, &registry, Some(&resolver))
        .map_err(|e| Refusal::new(e.to_string()))?;
    let output = InstrumentStructure::from_core(core_structure::read(&doc, &registry, &resolver));
    let summary = format!(
        "{}: {} node(s), {}",
        output.instrument,
        output.nodes.len(),
        validate_summary(&output.report)
    );
    Ok(Answer { output, summary })
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
pub(super) fn expect_conflict(
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
            pre_existing: Vec::new(),
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
pub(crate) fn edit_summary(result: &EditResult) -> String {
    if result.written {
        let base = format!("written (content_hash {})", result.hash);
        match result.notes.len() {
            0 => base,
            n => format!("{base}; {n} note(s)"),
        }
    } else {
        let errors = match (result.pre_existing.len(), result.report.errors.len()) {
            (0, n) => format!("{n} error(s)"),
            (p, 0) => format!(
                "the document already had {p} error(s) before this edit (pre_existing); no edit is \
                 written until it validates, so fix those first"
            ),
            (p, n) => format!(
                "{n} error(s), and the document already had {p} more before this edit \
                 (pre_existing); no edit is written until it validates, so fix those first"
            ),
        };
        let notes = match result.notes.len() {
            0 => String::new(),
            n => format!("; {n} note(s)"),
        };
        format!(
            "not written — {errors}{notes}; the document is unchanged (content_hash {})",
            result.hash
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authoring::result::{
        DeclaredMeta, InputValue, NodeConstant, NodeInput, NodeResource, PortInfo, Position,
        ResourceEntry, StructureNode,
    };
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

    /// [`SEED`] with a second node wired from one that is not there: invalid on arrival.
    const DANGLING: &str = r#"{
        "format_version": 3,
        "instrument": "guard-test",
        "nodes": [
            { "type": "oscillator", "address": "/osc", "inputs": { "freq": 220.0 } },
            { "type": "mul_f32_signal", "address": "/amp", "inputs": { "a": { "from": "/gone" } } }
        ]
    }"#;

    /// A rejection carries what the document already had apart from what the edit added, through
    /// the window's own shape and into the one-line gloss.
    #[test]
    fn a_rejection_names_the_errors_the_document_already_had() {
        let store = MemoryStore::with("inst.json", DANGLING);
        let describe = SetInstrumentNodeDescription {
            source: "inst.json".to_string(),
            address: "/osc".to_string(),
            description: Some("the tone".to_string()),
            expect: None,
        };

        let answer = set_instrument_node_description(&describe, &store).expect("the verb ran");

        assert!(!answer.output.written && !answer.output.report.ok);
        assert!(
            answer.output.report.errors.is_empty(),
            "{:?}",
            answer.output
        );
        assert_eq!(answer.output.pre_existing.len(), 1, "{:?}", answer.output);
        assert_eq!(answer.output.pre_existing[0].node.as_deref(), Some("/amp"));
        assert!(
            answer
                .summary
                .contains("already had 1 error(s) before this edit")
                && answer.summary.contains("fix those first"),
            "{}",
            answer.summary
        );
        let wire = serde_json::to_value(&answer.output).expect("serializes");
        assert_eq!(wire["pre_existing"][0]["port"], "a", "{wire}");

        // Ahead of `/gone` in the load, so this one is the edit's own.
        let wire_in = WireInstrumentInput {
            source: "inst.json".to_string(),
            address: "/osc".to_string(),
            input: "freq".to_string(),
            from: "/nope".to_string(),
            expect: None,
        };
        let added = wire_instrument_input(&wire_in, &store).expect("the verb ran");
        assert!(!added.output.written);
        assert_eq!(added.output.report.errors.len(), 1, "{:?}", added.output);
        assert!(
            added
                .summary
                .starts_with("not written — 1 error(s); 1 note(s);"),
            "{}",
            added.summary
        );
    }

    /// A rejected edit to a valid document carries nothing as pre-existing, and its wire shape
    /// omits the field.
    #[test]
    fn a_rejection_of_a_valid_document_has_nothing_pre_existing() {
        let store = MemoryStore::with("inst.json", SEED);
        let wire_in = WireInstrumentInput {
            source: "inst.json".to_string(),
            address: "/osc".to_string(),
            input: "freq".to_string(),
            from: "/nope".to_string(),
            expect: None,
        };

        let answer = wire_instrument_input(&wire_in, &store).expect("the verb ran");

        assert!(!answer.output.written);
        assert_eq!(answer.output.report.errors.len(), 1);
        assert!(answer.output.pre_existing.is_empty());
        let wire = serde_json::to_value(&answer.output).expect("serializes");
        assert!(wire.get("pre_existing").is_none(), "{wire}");
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

    const PIPED: &str = r#"{
        "format_version": 3,
        "instrument": "pipe-test",
        "nodes": [ { "type": "oscillator", "address": "/osc",
                     "inputs": { "freq": { "from": "/cutoff" } } },
                   { "type": "voicer", "address": "/voices", "config": { "voices": 4 } } ],
        "interface": {
            "inputs": { "cutoff": { "type": "f32", "min": 20, "max": 20000, "curve": "exp",
                                    "unit": "Hz", "default": 440 } },
            "outputs": { "main": { "from": "/osc", "unit": "V" } }
        }
    }"#;

    /// The wire spelling of both new arguments, taken as a model would send it.
    fn clear_meta(args: serde_json::Value) -> Result<Answer<EditResult>, Refusal> {
        let store = MemoryStore::with("inst.json", PIPED);
        let args: ClearInstrumentInterfaceMeta =
            serde_json::from_value(args).expect("the arguments parse");
        clear_instrument_interface_meta(&args, &store)
    }

    #[test]
    fn clearing_meta_writes_through_the_store_and_notes_an_absent_field() {
        let store = MemoryStore::with("inst.json", PIPED);
        let args: ClearInstrumentInterfaceMeta = serde_json::from_value(serde_json::json!({
            "source": "inst.json", "name": "main", "direction": "output",
            "fields": ["unit", "channel"]
        }))
        .expect("the arguments parse");
        let answer = clear_instrument_interface_meta(&args, &store).expect("the pipe exists");
        assert!(answer.output.written, "{:?}", answer.output);
        assert_eq!(
            answer.output.notes,
            ["`channel` was not set on output pipe `main`; nothing to clear"]
        );
        assert!(
            !store.read("inst.json").contains("\"V\""),
            "{}",
            store.read("inst.json")
        );
    }

    #[test]
    fn clearing_meta_refuses_a_field_its_direction_cannot_carry_and_a_field_it_does_not_know() {
        let refusal = clear_meta(serde_json::json!({
            "source": "inst.json", "name": "main", "direction": "output", "fields": ["curve"]
        }))
        .expect_err("an output pipe has no curve");
        assert!(refusal.message.contains("`curve`"), "{refusal}");

        let missing = clear_meta(serde_json::json!({
            "source": "inst.json", "name": "ghost", "direction": "input", "fields": ["unit"]
        }))
        .expect_err("no such pipe");
        assert!(missing.message.contains("ghost"), "{missing}");

        for bad in [
            serde_json::json!({ "source": "i", "name": "main", "direction": "output",
                                "fields": ["label"] }),
            serde_json::json!({ "source": "i", "name": "main", "direction": "both",
                                "fields": ["unit"] }),
            serde_json::json!({ "source": "i", "name": "main", "fields": ["unit"] }),
        ] {
            assert!(
                serde_json::from_value::<ClearInstrumentInterfaceMeta>(bad.clone()).is_err(),
                "{bad} is not a clear"
            );
        }
    }

    #[test]
    fn removing_a_constant_writes_and_a_missing_node_is_a_refusal() {
        let store = MemoryStore::with("inst.json", PIPED);
        let remove = |address: &str| {
            remove_instrument_constant(
                &RemoveInstrumentConstant {
                    source: "inst.json".to_string(),
                    address: address.to_string(),
                    name: "voices".to_string(),
                    expect: None,
                },
                &store,
            )
        };
        let answer = remove("/voices").expect("the node exists");
        assert!(
            answer.output.written && answer.output.notes.is_empty(),
            "{:?}",
            answer.output
        );
        assert!(
            !store.read("inst.json").contains("config"),
            "{}",
            store.read("inst.json")
        );

        let again = remove("/voices").expect("an unset constant is noted");
        assert_eq!(again.output.hash, answer.output.hash);
        assert_eq!(again.output.notes.len(), 1, "{:?}", again.output);

        let refusal = remove("/ghost").expect_err("no such node");
        assert!(refusal.message.contains("/ghost"), "{refusal}");
    }

    /// The window's half of a copy: the answer is an ordinary written edit at `to`, carrying the
    /// one hash both documents now share, and an occupied `to` is a refusal.
    #[test]
    fn a_copy_lands_at_the_new_source_under_the_originals_hash() {
        let store = MemoryStore::with("inst.json", SEED);
        let args = CopyInstrument {
            from: "inst.json".to_string(),
            to: "copy.json".to_string(),
        };

        let answer = copy_instrument(&args, &store).expect("a copy to a free source");
        assert!(answer.output.written, "{:?}", answer.output);
        assert_eq!(
            answer.output.hash,
            hash_instrument(&store.read("inst.json"), &store).unwrap()
        );
        assert_eq!(
            answer.output.hash,
            hash_instrument(&store.read("copy.json"), &store).unwrap()
        );

        let copied = store.read("copy.json");
        let refusal = copy_instrument(&args, &store).expect_err("`to` is taken now");
        assert!(refusal.message.contains("copy.json"), "{refusal}");
        assert_eq!(store.read("copy.json"), copied);
    }

    /// Reachable by code, advertised to no agent: the roster names the verb only as a Host contract.
    #[test]
    fn no_roster_names_copy_instrument() {
        assert!(!crate::tools::CONTRACTS
            .iter()
            .any(|c| c.name == "copy_instrument" && c.kind != crate::tools::ContractKind::Host));
    }

    #[test]
    fn no_roster_names_set_instrument_resource() {
        assert!(!crate::tools::CONTRACTS
            .iter()
            .any(|c| c.name == "set_instrument_resource"
                && c.kind != crate::tools::ContractKind::Host));
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
            refusal
                .message
                .contains(crate::tools::names::VALIDATE_INSTRUMENT),
            "the refusal says what to reach for instead: {refusal}"
        );
    }

    /// A view either honours a selection or refuses it — never drops it. The views that narrow
    /// answer; the rest refuse and name the ones that would have narrowed.
    #[test]
    fn a_view_honours_a_selection_or_refuses_it() {
        let store = MemoryStore::with("nestable.json", NESTABLE);
        let terms: [(&str, Vec<String>, Option<String>); 2] = [
            ("select", vec!["/osc".to_string()], None),
            ("type", Vec::new(), Some("oscillator".to_string())),
        ];
        let views = [
            (InstrumentView::Index, false),
            (InstrumentView::Nodes, true),
            (InstrumentView::Pipes, true),
            (InstrumentView::Resources, false),
            (InstrumentView::Boundary, false),
        ];
        for (view, selectable) in views {
            for (spelling, select, type_name) in &terms {
                let args = DescribeInstrument {
                    source: "nestable.json".to_string(),
                    view,
                    select: select.clone(),
                    type_name: type_name.clone(),
                };
                let answer = describe_instrument(&args, &store);
                let view = view.as_str();
                if selectable {
                    answer.unwrap_or_else(|r| panic!("`{view}` narrows by `{spelling}`: {r}"));
                    continue;
                }
                let message = answer
                    .expect_err("a dropped selection is a wrong answer")
                    .message;
                for named in [&format!("`{view}`"), "\"nodes\"", "\"pipes\""] {
                    assert!(
                        message.contains(named),
                        "`{view}` + `{spelling}` refusal names {named}: {message}"
                    );
                }
            }
        }
    }

    /// No terms is no selection: every view answers it.
    #[test]
    fn every_view_answers_an_empty_selection() {
        let store = MemoryStore::with("nestable.json", NESTABLE);
        for view in [
            InstrumentView::Index,
            InstrumentView::Nodes,
            InstrumentView::Pipes,
            InstrumentView::Resources,
            InstrumentView::Boundary,
        ] {
            describe_instrument(
                &DescribeInstrument {
                    source: "nestable.json".to_string(),
                    view,
                    ..Default::default()
                },
                &store,
            )
            .unwrap_or_else(|r| panic!("`{}` with no terms: {r}", view.as_str()));
        }
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

    /// A node, an input pipe and an output pipe, where the input pipe and the output pipe are both
    /// named `main`.
    const TWINS: &str = r#"{
        "format_version": 3,
        "instrument": "twins",
        "interface": {
            "inputs": { "main": { "type": "f32", "default": 220.0 } },
            "outputs": { "main": { "from": "/osc.audio" } }
        },
        "nodes": [ { "type": "oscillator", "address": "/osc", "inputs": { "freq": { "from": "/main" } } } ]
    }"#;

    fn layout_args(positions: serde_json::Value, expect: Option<&str>) -> SetInstrumentLayout {
        serde_json::from_value(serde_json::json!({
            "source": "twins.json",
            "positions": positions,
            "expect": expect,
        }))
        .expect("well-formed layout arguments")
    }

    fn layout_at(doc: &serde_json::Value, pointer: &str) -> serde_json::Value {
        doc.pointer(pointer).cloned().unwrap_or_default()
    }

    #[test]
    fn set_layout_places_every_kind_of_target_in_one_write() {
        let store = MemoryStore::with("twins.json", TWINS);
        let answer = set_instrument_layout(
            &layout_args(
                serde_json::json!([
                    { "target": "/osc", "x": 10.0, "y": 20.0 },
                    { "target": "/main", "x": -50.0, "y": 20.0 },
                    { "target": { "output": "main" }, "x": 300.0, "y": 20.0 }
                ]),
                None,
            ),
            &store,
        )
        .expect("every target exists");
        assert!(answer.output.written, "{:?}", answer.output);

        let doc: serde_json::Value = serde_json::from_str(&store.read("twins.json")).unwrap();
        let xy = |x: f64, y: f64| serde_json::json!({ "x": x, "y": y });
        assert_eq!(layout_at(&doc, "/nodes/0/layout"), xy(10.0, 20.0));
        assert_eq!(
            layout_at(&doc, "/interface/inputs/main/layout"),
            xy(-50.0, 20.0)
        );
        assert_eq!(
            layout_at(&doc, "/interface/outputs/main/layout"),
            xy(300.0, 20.0)
        );
    }

    #[test]
    fn set_layout_refuses_an_unknown_target_of_either_kind_and_writes_nothing() {
        for ghost in [
            serde_json::json!("/ghost"),
            serde_json::json!({ "output": "ghost" }),
        ] {
            let store = MemoryStore::with("twins.json", TWINS);
            let refusal = set_instrument_layout(
                &layout_args(
                    serde_json::json!([
                        { "target": "/osc", "x": 1.0, "y": 1.0 },
                        { "target": ghost, "x": 2.0, "y": 2.0 }
                    ]),
                    None,
                ),
                &store,
            )
            .expect_err("an unknown target is a refusal");
            assert!(refusal.message.contains("ghost"), "{refusal}");
            assert_eq!(store.read("twins.json"), TWINS, "nothing was written");
        }
    }

    #[test]
    fn set_layout_honours_the_expect_guard() {
        let store = MemoryStore::with("twins.json", TWINS);
        let one = serde_json::json!([{ "target": "/osc", "x": 1.0, "y": 1.0 }]);

        let stale =
            set_instrument_layout(&layout_args(one.clone(), Some("deadbeefdeadbeef")), &store)
                .expect("a guard miss is an answer, not a refusal");
        assert!(!stale.output.written);
        assert_eq!(store.read("twins.json"), TWINS, "nothing was written");

        let current = set_instrument_layout(&layout_args(one, Some(&stale.output.hash)), &store)
            .expect("a matching guard is not a refusal");
        assert!(current.output.written, "{:?}", current.output);
        assert_ne!(
            current.output.hash, stale.output.hash,
            "a layout write moves the hash"
        );
    }

    // --- wire_compatibility ------------------------------------------------------------------

    /// The nested child: one signal face input, one signal face output.
    const WIRE_CHILD: &str = r#"{
        "format_version": 3,
        "instrument": "fx",
        "interface": {
            "inputs": { "in": { "type": "f32_buffer" } },
            "outputs": { "out": { "from": "/sat.audio" } }
        },
        "nodes": [ { "type": "saturator", "address": "/sat", "inputs": { "audio": { "from": "/in" } } } ]
    }"#;

    /// Every wiring class, no wires between nodes (so no single added wire between two distinct
    /// nodes can close a cycle), a subpatch face, and two interface outputs — one channel-bound.
    const WIRE_BASE: &str = r#"{
        "format_version": 3,
        "instrument": "wires",
        "resources": { "fx": "fx.json" },
        "interface": {
            "inputs": {
                "mode": { "type": "FilterMode" },
                "level": { "type": "f32", "default": 0.5, "min": 0, "max": 1 }
            },
            "outputs": {
                "main": { "from": "/flt.audio", "channel": 0 },
                "busy": { "from": "/env.active" }
            }
        },
        "nodes": [
            { "type": "lfo", "address": "/lfo" },
            { "type": "oscillator", "address": "/osc" },
            { "type": "filter", "address": "/flt" },
            { "type": "envelope", "address": "/env" },
            { "type": "round_f32_i32_value", "address": "/q" },
            { "type": "sub_i32_value", "address": "/si" },
            { "type": "transpose", "address": "/tr" },
            { "type": "harmony", "address": "/h" },
            { "type": "osc_out", "address": "/fb" },
            { "type": "subpatch", "address": "/sub", "patch": "fx" }
        ]
    }"#;

    fn out(node: &str, port: &str) -> WireSource {
        WireSource::NodeOutput {
            node: node.to_string(),
            port: port.to_string(),
        }
    }

    fn pipe(name: &str) -> WireSource {
        WireSource::InterfaceInput {
            name: name.to_string(),
        }
    }

    fn inp(node: &str, port: &str) -> WireTarget {
        WireTarget::NodeInput {
            node: node.to_string(),
            port: port.to_string(),
        }
    }

    fn tap(name: &str) -> WireTarget {
        WireTarget::InterfaceOutput {
            name: name.to_string(),
        }
    }

    /// The document with the wire written in, spelled as the document spells it — independently
    /// of the query's own spelling, so a disagreement between the two fails the test.
    fn with_wire(from: &WireSource, to: &WireTarget) -> String {
        let reference = match from {
            WireSource::NodeOutput { node, port } => format!("{node}.{port}"),
            WireSource::InterfaceInput { name } => format!("/{name}"),
        };
        let mut doc: serde_json::Value = serde_json::from_str(WIRE_BASE).unwrap();
        let slot = match to {
            WireTarget::NodeInput { node, port } => {
                let n = doc["nodes"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|n| n["address"] == node.as_str())
                    .unwrap();
                if n.get("inputs").is_none() {
                    n["inputs"] = serde_json::json!({});
                }
                &mut n["inputs"][port.as_str()]
            }
            WireTarget::InterfaceOutput { name } => {
                &mut doc["interface"]["outputs"][name.as_str()]["from"]
            }
        };
        *slot = match to {
            WireTarget::NodeInput { .. } => serde_json::json!({ "from": reference }),
            WireTarget::InterfaceOutput { .. } => serde_json::json!(reference),
        };
        doc.to_string()
    }

    fn wire_store(doc: &str) -> MemoryStore {
        let store = MemoryStore::with("inst.json", doc);
        store
            .texts
            .borrow_mut()
            .insert("fx.json".to_string(), WIRE_CHILD.to_string());
        store
    }

    fn validates(doc: &str) -> Report {
        let store = wire_store(doc);
        let args = ValidateInstrument {
            source: "inst.json".to_string(),
        };
        validate_instrument(&args, &store).unwrap().output
    }

    fn verdicts(from: &WireSource, to: Option<Vec<WireTarget>>) -> Vec<WireVerdict> {
        let store = wire_store(WIRE_BASE);
        let args = WireCompatibility {
            source: "inst.json".to_string(),
            from: from.clone(),
            to,
        };
        wire_compatibility(&args, &store)
            .expect("a resolvable source is not a refusal")
            .output
            .verdicts
    }

    /// The acceptance table: each pair is written into the document and validated, and the
    /// query — asked of the document *before* the wire — must give the same answer as `validate`,
    /// and the answer the row expects.
    #[test]
    fn the_query_and_validate_agree_on_every_pair_in_the_table() {
        assert!(validates(WIRE_BASE).ok, "the base document validates");
        let table: &[(WireSource, WireTarget, bool)] = &[
            (out("/lfo", "out"), inp("/osc", "freq"), true),
            (out("/env", "active"), inp("/osc", "freq"), true),
            (out("/osc", "audio"), inp("/env", "gate"), false),
            (out("/q", "out"), inp("/env", "gate"), true),
            (out("/env", "active"), inp("/si", "a"), false),
            (out("/tr", "notes"), inp("/osc", "freq"), false),
            (out("/tr", "notes"), inp("/fb", "in"), true),
            (out("/osc", "audio"), inp("/fb", "in"), false),
            (out("/h", "harmony"), inp("/fb", "in"), false),
            (pipe("mode"), inp("/flt", "mode"), true),
            (pipe("mode"), inp("/osc", "waveform"), false),
            (pipe("level"), inp("/flt", "cutoff"), true),
            // The subpatch face, both directions.
            (out("/osc", "audio"), inp("/sub", "in"), true),
            (out("/tr", "notes"), inp("/sub", "in"), false),
            (out("/sub", "out"), inp("/flt", "audio"), true),
            (out("/sub", "out"), inp("/env", "gate"), false),
            // Interface outputs: typeless, but a channel binding takes only a signal.
            (out("/osc", "audio"), tap("main"), true),
            (out("/env", "active"), tap("main"), false),
            (out("/tr", "notes"), tap("busy"), true),
            (out("/sub", "out"), tap("busy"), true),
        ];
        let mut failures = Vec::new();
        for (from, to, expected) in table {
            let report = validates(&with_wire(from, to));
            let verdict = &verdicts(from, Some(vec![to.clone()]))[0];
            if verdict.ok != report.ok || verdict.ok != *expected {
                failures.push(format!(
                    "{from:?} -> {to:?}: query {} ({:?}), validate {} ({:?}), expected {expected}",
                    verdict.ok, verdict.reason, report.ok, report.errors
                ));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// The batch form over the whole document: every source against every target the query
    /// enumerates, each checked against `validate`. Pairs on one node are left out — the only
    /// cycles a single wire can close here — since cycles are not the query's to answer.
    #[test]
    fn every_enumerated_target_agrees_with_validate_for_every_source() {
        let sources = [
            out("/lfo", "out"),
            out("/osc", "audio"),
            out("/flt", "audio"),
            out("/env", "cv"),
            out("/env", "active"),
            out("/q", "out"),
            out("/si", "out"),
            out("/tr", "notes"),
            out("/h", "harmony"),
            out("/sub", "out"),
            pipe("mode"),
            pipe("level"),
        ];
        let mut failures = Vec::new();
        let mut seen_face = false;
        let mut seen_tap = false;
        for from in &sources {
            for verdict in verdicts(from, None) {
                let same_node = matches!(
                    (from, &verdict.target),
                    (WireSource::NodeOutput { node: a, .. }, WireTarget::NodeInput { node: b, .. })
                        if a == b
                );
                if same_node {
                    continue;
                }
                seen_face |= verdict.target == inp("/sub", "in");
                seen_tap |= matches!(verdict.target, WireTarget::InterfaceOutput { .. });
                let report = validates(&with_wire(from, &verdict.target));
                if verdict.ok != report.ok {
                    failures.push(format!(
                        "{from:?} -> {:?}: query {} ({:?}), validate {} ({:?})",
                        verdict.target, verdict.ok, verdict.reason, report.ok, report.errors
                    ));
                }
            }
        }
        assert!(
            seen_face && seen_tap,
            "the enumeration reaches faces and interface outputs"
        );
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// A constant is not a wire target: the enumeration leaves it out, and asking for one names it.
    #[test]
    fn a_constant_is_never_a_compatible_target() {
        let store = wire_store(
            r#"{
            "format_version": 3, "instrument": "c",
            "nodes": [ { "type": "lfo", "address": "/lfo" }, { "type": "voicer", "address": "/v" } ]
        }"#,
        );
        let args = WireCompatibility {
            source: "inst.json".to_string(),
            from: out("/lfo", "out"),
            to: None,
        };
        let all = wire_compatibility(&args, &store).unwrap().output.verdicts;
        assert!(
            !all.iter().any(|v| v.target == inp("/v", "voices")),
            "{all:?}"
        );

        let args = WireCompatibility {
            to: Some(vec![inp("/v", "voices")]),
            ..args
        };
        let one = &wire_compatibility(&args, &store).unwrap().output.verdicts[0];
        assert!(
            !one.ok && one.reason.as_deref().unwrap().contains("voices"),
            "{one:?}"
        );
    }

    /// No target could take a source that is not there, so the question is refused, not answered
    /// once per target.
    #[test]
    fn an_unknown_source_is_a_refusal() {
        let store = wire_store(WIRE_BASE);
        let args = WireCompatibility {
            source: "inst.json".to_string(),
            from: out("/nobody", "out"),
            to: None,
        };
        assert!(wire_compatibility(&args, &store).is_err());
    }

    // --- read_instrument --------------------------------------------------------------------------

    fn read(source: &str, store: &dyn Resources) -> InstrumentStructure {
        let args = ReadInstrument {
            source: source.to_string(),
        };
        read_instrument(&args, store)
            .expect("a document that parses is read")
            .output
    }

    fn node<'a>(doc: &'a InstrumentStructure, address: &str) -> &'a StructureNode {
        doc.nodes
            .iter()
            .find(|n| n.address == address)
            .unwrap_or_else(|| panic!("no node {address}"))
    }

    fn names(ports: &[PortInfo]) -> Vec<&str> {
        ports.iter().map(|p| p.name.as_str()).collect()
    }

    #[test]
    fn a_read_reports_values_wires_and_ports_as_data() {
        let doc = read(
            "inst.json",
            &wire_store(&with_wire(&pipe("level"), &inp("/flt", "resonance"))),
        );
        assert_eq!(doc.instrument, "wires");
        assert!(doc.report.ok, "{:?}", doc.report);
        assert_eq!(doc.nodes.len(), 10);

        let flt = node(&doc, "/flt");
        assert_eq!(flt.type_name, "filter");
        assert_eq!(
            flt.inputs,
            vec![NodeInput {
                name: "resonance".to_string(),
                value: InputValue::Wire {
                    from: pipe("level")
                },
            }]
        );
        let registry = describe_operators(&DescribeOperators {
            name: Some("filter".to_string()),
            compact: false,
        })
        .unwrap()
        .output
        .operators
        .unwrap();
        assert_eq!(flt.ports.inputs, registry[0].inputs);
        assert_eq!(flt.ports.outputs, registry[0].outputs);
        assert_eq!(flt.child, None);

        assert_eq!(names_of_inputs(&doc), vec!["level", "mode"]);
        let main = doc.outputs.iter().find(|o| o.name == "main").unwrap();
        assert_eq!(main.from, out("/flt", "audio"));
        assert_eq!(main.channel, Some(0));
        assert_eq!(
            doc.resources,
            vec![ResourceEntry {
                id: "fx".to_string(),
                source: "fx.json".to_string(),
                canonical: "fx.json".to_string(),
            }]
        );
    }

    /// The window hands a door the report the edit built, so a rename the edit's migration made
    /// on disk reaches the caller as a warning on the node's new address.
    #[test]
    fn an_edit_that_migrates_a_refused_address_reports_the_rename() {
        let store = MemoryStore::with(
            "inst.json",
            r#"{ "format_version": 3, "instrument": "s",
                 "nodes": [ { "type": "oscillator", "address": "/my osc" } ] }"#,
        );
        let args = SetInstrumentName {
            source: "inst.json".to_string(),
            name: "t".to_string(),
            expect: None,
        };
        let answer = set_instrument_name(&args, &store).expect("edit");
        assert!(answer.output.written, "{:?}", answer.output.report);
        let warning = answer
            .output
            .report
            .warnings
            .iter()
            .find(|d| d.node.as_deref() == Some("/my_osc"))
            .unwrap_or_else(|| panic!("{:?}", answer.output.report.warnings));
        assert!(warning.message.contains("/my osc"), "{}", warning.message);
        assert!(store.read("inst.json").contains("/my_osc"));
    }

    #[test]
    fn adding_an_input_pipe_whose_name_mints_a_refused_address_is_refused() {
        let doc = r#"{ "format_version": 4, "instrument": "s", "nodes": [] }"#;
        let store = MemoryStore::with("inst.json", doc);
        let args = AddInstrumentInterfaceInput {
            source: "inst.json".to_string(),
            name: "my.tone".to_string(),
            type_name: "f32".to_string(),
            ..Default::default()
        };
        let answer = add_instrument_interface_input(&args, &store).expect("the verb runs");
        assert!(!answer.output.written, "{:?}", answer.output.report);
        let error = answer
            .output
            .report
            .errors
            .iter()
            .find(|d| d.port.as_deref() == Some("my.tone"))
            .unwrap_or_else(|| panic!("{:?}", answer.output.report.errors));
        assert!(error.message.contains("`.`"), "{}", error.message);
        assert_eq!(store.read("inst.json"), doc);
    }

    /// A pipe rename the edit's migration made reaches the caller as a warning on the new name.
    #[test]
    fn an_edit_that_migrates_a_refused_pipe_name_reports_the_rename() {
        let store = MemoryStore::with(
            "inst.json",
            r#"{ "format_version": 3, "instrument": "s",
                 "interface": { "inputs": { "my tone": { "type": "f32", "default": 0.5 } } },
                 "nodes": [] }"#,
        );
        let args = SetInstrumentName {
            source: "inst.json".to_string(),
            name: "t".to_string(),
            expect: None,
        };
        let answer = set_instrument_name(&args, &store).expect("edit");
        assert!(answer.output.written, "{:?}", answer.output.report);
        let warning = answer
            .output
            .report
            .warnings
            .iter()
            .find(|d| d.port.as_deref() == Some("my_tone"))
            .unwrap_or_else(|| panic!("{:?}", answer.output.report.warnings));
        assert!(warning.message.contains("my tone"), "{}", warning.message);
        assert!(store.read("inst.json").contains("my_tone"));
    }

    /// The loader resolves each sample once and names no node; the read names every node reading
    /// it, so an editor can show the missing file where it is used.
    #[test]
    fn a_sample_that_does_not_resolve_is_a_warning_on_each_node_reading_it() {
        let store = MemoryStore::with(
            "inst.json",
            r#"{ "format_version": 3, "instrument": "s",
                 "resources": { "take": "take.wav" },
                 "nodes": [
                   { "type": "granulator", "address": "/a", "sample": "take" },
                   { "type": "granulator", "address": "/b", "sample": "take" }
                 ] }"#,
        );
        let doc = read("inst.json", &store);
        assert!(doc.report.ok, "{:?}", doc.report);
        let mut at: Vec<_> = (doc.report.warnings.iter())
            .map(|d| (d.node.as_deref(), d.port.as_deref()))
            .collect();
        at.sort();
        assert_eq!(at, [(Some("/a"), None), (Some("/b"), None)]);
        assert!(doc.report.warnings[0].message.contains("take.wav"));
    }

    #[test]
    fn a_wire_whose_types_do_not_match_is_rejected_at_the_input_holding_it() {
        let store = MemoryStore::with(
            "inst.json",
            r#"{ "format_version": 3, "instrument": "w",
                 "interface": { "inputs": { "notes": { "type": "note" } } },
                 "nodes": [ { "type": "filter", "address": "/flt" } ] }"#,
        );
        let edit = wire_instrument_input(
            &WireInstrumentInput {
                source: "inst.json".to_string(),
                address: "/flt".to_string(),
                input: "audio".to_string(),
                from: "/notes".to_string(),
                expect: None,
            },
            &store,
        )
        .expect("the verb ran")
        .output;
        assert!(!edit.written);
        let error = &edit.report.errors[0];
        assert_eq!(
            (error.node.as_deref(), error.port.as_deref()),
            (Some("/flt"), Some("audio")),
            "{error:?}"
        );
    }

    fn names_of_inputs(doc: &InstrumentStructure) -> Vec<&str> {
        doc.inputs.iter().map(|i| i.port.name.as_str()).collect()
    }

    /// A literal, a constant and the sole-output sugar: the sugar reads back with the port it
    /// resolves to, so no caller re-derives the loader's rule.
    #[test]
    fn a_read_resolves_literals_constants_and_the_sole_output_sugar() {
        let store = MemoryStore::with(
            "inst.json",
            r#"{ "format_version": 3, "instrument": "v",
                 "nodes": [
                   { "type": "oscillator", "address": "/osc", "doc": "the source",
                     "inputs": { "freq": 220.0 } },
                   { "type": "filter", "address": "/flt",
                     "inputs": { "audio": { "from": "/osc" }, "mode": "Hp" } },
                   { "type": "voicer", "address": "/v", "config": { "voices": 4 } }
                 ] }"#,
        );
        let doc = read("inst.json", &store);
        assert_eq!(
            node(&doc, "/osc").description.as_deref(),
            Some("the source")
        );
        assert_eq!(
            node(&doc, "/osc").inputs[0].value,
            InputValue::Literal {
                value: serde_json::json!(220.0)
            }
        );
        let flt = node(&doc, "/flt");
        assert_eq!(
            flt.inputs[0].value,
            InputValue::Wire {
                from: out("/osc", "audio")
            }
        );
        assert_eq!(
            flt.inputs[1].value,
            InputValue::Literal {
                value: serde_json::json!("Hp")
            }
        );
        assert_eq!(
            node(&doc, "/v").constants,
            vec![NodeConstant {
                name: "voices".to_string(),
                value: serde_json::json!(4.0),
            }]
        );
    }

    #[test]
    fn a_read_carries_the_documents_description_and_an_output_pipes_declared_unit() {
        let store = MemoryStore::with(
            "inst.json",
            r#"{ "format_version": 3, "instrument": "v", "doc": "what it is for",
                 "interface": { "outputs": {
                   "level": { "from": "/osc.audio", "unit": "dB" } } },
                 "nodes": [ { "type": "oscillator", "address": "/osc" } ] }"#,
        );
        let doc = read("inst.json", &store);
        assert!(doc.report.ok, "{:?}", doc.report);
        assert_eq!(doc.description.as_deref(), Some("what it is for"));
        let level = &doc.outputs[0];
        assert_eq!(level.name, "level");
        assert_eq!(level.unit.as_deref(), Some("dB"));
    }

    /// An interface input is described from its declaration, and that is the port the boundary
    /// read reports once the document loads.
    #[test]
    fn an_interface_input_reads_as_the_port_the_boundary_reports() {
        let store = wire_store(WIRE_BASE);
        let doc = read("inst.json", &store);
        let boundary = describe_boundary(
            &DescribeBoundary {
                source: "inst.json".to_string(),
            },
            &store,
        )
        .unwrap()
        .output;
        let ports: Vec<PortInfo> = doc.inputs.iter().map(|i| i.port.clone()).collect();
        assert_eq!(ports, boundary.inputs);
    }

    /// The port carries what the pipe resolves to, which an undeclared range or curve does not
    /// show; `declared` is what the pipe itself writes, so a cleared field reads as absent.
    #[test]
    fn an_interface_input_reads_its_declared_range_and_curve_apart_from_the_effective_ones() {
        let store = MemoryStore::with(
            "inst.json",
            r#"{
                "format_version": 3,
                "instrument": "declared",
                "nodes": [ { "type": "oscillator", "address": "/osc",
                             "inputs": { "freq": { "from": "/bare" } } } ],
                "interface": { "inputs": {
                    "bare": { "type": "f32", "default": 440 },
                    "swept": { "type": "f32", "min": 20, "max": 20000, "curve": "exp" }
                } }
            }"#,
        );
        let doc = read("inst.json", &store);
        let pipe = |name: &str| {
            (doc.inputs.iter())
                .find(|i| i.port.name == name)
                .unwrap_or_else(|| panic!("{name}"))
                .clone()
        };
        let bare = pipe("bare");
        assert_eq!(bare.declared, DeclaredMeta::default());
        assert!(
            bare.port.min.is_some() && bare.port.curve.is_some(),
            "the port still reports what is effective: {:?}",
            bare.port
        );
        assert_eq!(
            pipe("swept").declared,
            DeclaredMeta {
                min: Some(20.0),
                max: Some(20000.0),
                curve: Some("exponential".to_string()),
            }
        );
    }

    #[test]
    fn a_subpatch_node_reports_its_childs_interface_as_ports() {
        let store = wire_store(WIRE_BASE);
        let doc = read("inst.json", &store);
        let child = describe_boundary(
            &DescribeBoundary {
                source: "fx.json".to_string(),
            },
            &store,
        )
        .unwrap()
        .output;
        let sub = node(&doc, "/sub");
        assert_eq!(names(&sub.ports.inputs), vec!["in"]);
        assert_eq!(sub.ports.inputs, child.inputs);
        assert_eq!(sub.ports.outputs, child.outputs);
        assert_eq!(sub.child.as_ref(), Some(&sub.ports));
        assert_eq!(
            sub.resources,
            vec![NodeResource {
                slot: "patch".to_string(),
                id: "fx".to_string(),
            }]
        );
    }

    /// A voicer's wires land on its own ports; the voice patch's interface is reported beside
    /// them, not in their place.
    #[test]
    fn a_voicer_node_reports_its_own_ports_and_its_voice_beside_them() {
        let store = MemoryStore::with(
            "inst.json",
            r#"{ "format_version": 3, "instrument": "poly",
                 "resources": { "v": "fx.json" },
                 "interface": { "outputs": { "main": { "from": "/v.audio" } } },
                 "nodes": [ { "type": "voicer", "address": "/v", "voice": "v" } ] }"#,
        );
        store
            .texts
            .borrow_mut()
            .insert("fx.json".to_string(), WIRE_CHILD.to_string());
        let doc = read("inst.json", &store);
        assert!(doc.report.ok, "{:?}", doc.report);
        let child = describe_boundary(
            &DescribeBoundary {
                source: "fx.json".to_string(),
            },
            &store,
        )
        .unwrap()
        .output;
        let voicer = node(&doc, "/v");
        assert_eq!(
            names(&voicer.ports.inputs),
            vec!["notes", "harmony", "voices"]
        );
        assert_eq!(names(&voicer.ports.outputs), vec!["audio"]);
        let hosted = voicer.child.as_ref().expect("the voice patch loaded");
        assert_eq!(hosted.inputs, child.inputs);
        assert_eq!(hosted.outputs, child.outputs);
    }

    #[test]
    fn a_position_written_with_set_layout_reads_back() {
        let store = MemoryStore::with("twins.json", TWINS);
        let written = set_instrument_layout(
            &layout_args(
                serde_json::json!([
                    { "target": "/osc", "x": 10.0, "y": 20.5 },
                    { "target": "/main", "x": -50.0, "y": 20.0 },
                    { "target": { "output": "main" }, "x": 300.0, "y": 20.0 }
                ]),
                None,
            ),
            &store,
        )
        .unwrap()
        .output;
        assert!(written.written, "{written:?}");

        let doc = read("twins.json", &store);
        assert_eq!(
            node(&doc, "/osc").layout,
            Some(Position { x: 10.0, y: 20.5 })
        );
        assert_eq!(doc.inputs[0].port.name, "main");
        assert_eq!(doc.inputs[0].layout, Some(Position { x: -50.0, y: 20.0 }));
        assert_eq!(doc.outputs[0].name, "main");
        assert_eq!(doc.outputs[0].layout, Some(Position { x: 300.0, y: 20.0 }));
    }

    #[test]
    fn a_position_never_set_is_absent() {
        let doc = read("twins.json", &MemoryStore::with("twins.json", TWINS));
        assert_eq!(node(&doc, "/osc").layout, None);
        assert_eq!(doc.inputs[0].layout, None);
        assert_eq!(doc.outputs[0].layout, None);
    }

    #[test]
    fn the_read_hash_is_an_edits_hash_and_passes_as_expect() {
        let store = MemoryStore::with("inst.json", SEED);
        let edited = set_instrument_input(&set_freq("inst.json", 440.0, None), &store)
            .unwrap()
            .output;
        let doc = read("inst.json", &store);
        assert_eq!(doc.hash, edited.hash);

        let guarded = set_instrument_input(&set_freq("inst.json", 880.0, Some(&doc.hash)), &store)
            .unwrap()
            .output;
        assert!(guarded.written, "{guarded:?}");
        assert_eq!(read("inst.json", &store).hash, guarded.hash);
    }

    /// The missing *source* of a dangling wire is no node of this document; validation and the
    /// read both put the error on the node and input that hold the wire.
    #[test]
    fn a_dangling_wire_still_reads_with_the_error_on_its_node() {
        let dangling = with_wire(&out("/ghost", "audio"), &inp("/flt", "audio"));
        let doc = read("inst.json", &wire_store(&dangling));
        assert!(!doc.report.ok);
        let validated = &validates(&dangling).errors[0];
        assert_eq!(
            (validated.node.as_deref(), validated.port.as_deref()),
            (Some("/flt"), Some("audio"))
        );
        assert_eq!(doc.report.errors.len(), 1, "{:?}", doc.report);
        let error = &doc.report.errors[0];
        assert_eq!(error.node.as_deref(), Some("/flt"));
        assert_eq!(error.port.as_deref(), Some("audio"));
        assert_eq!(error.message, validates(&dangling).errors[0].message);

        assert_eq!(doc.nodes.len(), 10);
        let flt = node(&doc, "/flt");
        assert_eq!(
            flt.inputs[0].value,
            InputValue::Wire {
                from: out("/ghost", "audio")
            }
        );
        assert!(!flt.ports.inputs.is_empty());
        assert_eq!(names(&node(&doc, "/sub").ports.outputs), vec!["out"]);
    }

    /// The loader stops at the first refused wire; the read reports each one, and an interface
    /// output's on the pipe.
    #[test]
    fn every_refused_wire_is_reported_where_it_is_held() {
        let mut broken: serde_json::Value =
            serde_json::from_str(&with_wire(&out("/osc", "audio"), &inp("/h", "root"))).unwrap();
        broken["interface"]["outputs"]["busy"]["from"] = serde_json::json!("/nobody.out");
        let doc = read("inst.json", &wire_store(&broken.to_string()));
        let held: Vec<(Option<&str>, Option<&str>)> = doc
            .report
            .errors
            .iter()
            .map(|d| (d.node.as_deref(), d.port.as_deref()))
            .collect();
        assert_eq!(held, vec![(Some("/h"), Some("root")), (None, Some("busy"))]);
    }

    #[test]
    fn a_child_that_does_not_load_is_its_nodes_diagnostic_not_a_refusal() {
        let store = MemoryStore::with("inst.json", WIRE_BASE);
        store.texts.borrow_mut().insert(
            "fx.json".to_string(),
            r#"{ "format_version": 3, "instrument": "fx",
                 "nodes": [ { "type": "no_such_operator", "address": "/x" } ] }"#
                .to_string(),
        );
        let doc = read("inst.json", &store);
        assert!(!doc.report.ok);
        assert_eq!(doc.nodes.len(), 10);
        let sub = node(&doc, "/sub");
        assert!(sub.ports.inputs.is_empty() && sub.ports.outputs.is_empty());
        assert_eq!(sub.child, None);
        assert!(
            doc.report.errors.iter().any(
                |d| d.node.as_deref() == Some("/sub") && d.message.contains("no_such_operator")
            ),
            "{:?}",
            doc.report
        );
        assert!(!node(&doc, "/flt").ports.inputs.is_empty());
        // The child's own error names a node of the child. Left in the report it would point at
        // nothing here, or at whichever of this document's nodes shares that address.
        assert_eq!(doc.report.errors.len(), 1, "{:?}", doc.report);
    }

    /// An unreadable child degrades a load rather than failing it, and the loader's own warning
    /// already names the node — the read adds no second one.
    #[test]
    fn a_child_that_is_not_there_is_a_warning_on_its_node() {
        let doc = read("inst.json", &MemoryStore::with("inst.json", WIRE_BASE));
        assert!(doc.report.ok, "{:?}", doc.report);
        let on_sub: Vec<&Diag> = doc
            .report
            .warnings
            .iter()
            .filter(|d| d.node.as_deref() == Some("/sub"))
            .collect();
        assert_eq!(on_sub.len(), 1, "{:?}", doc.report);
        assert!(node(&doc, "/sub").ports.inputs.is_empty());
    }

    #[test]
    fn only_an_unreadable_or_unparseable_document_is_a_refusal() {
        let args = ReadInstrument {
            source: "inst.json".to_string(),
        };
        assert!(read_instrument(&args, &MemoryStore::default()).is_err());
        assert!(read_instrument(&args, &MemoryStore::with("inst.json", "{ not json")).is_err());

        let unknown_type = r#"{ "format_version": 3, "instrument": "x",
            "nodes": [ { "type": "no_such_operator", "address": "/x" } ] }"#;
        let doc = read("inst.json", &MemoryStore::with("inst.json", unknown_type));
        assert!(!doc.report.ok);
        assert_eq!(doc.report.errors[0].node.as_deref(), Some("/x"));
        assert!(node(&doc, "/x").ports.inputs.is_empty());
    }

    #[test]
    fn no_roster_names_the_read() {
        assert!(!crate::tools::CONTRACTS
            .iter()
            .any(|c| c.name == "read_instrument" && c.kind != crate::tools::ContractKind::Host));
    }

    #[cfg(feature = "fs-resolver")]
    mod library {
        use super::*;
        use crate::FsResolver;
        use std::path::Path;

        fn library(file: &str) -> String {
            format!("{}/../../instruments/{file}", env!("CARGO_MANIFEST_DIR"))
        }

        /// Every wire the read reports, as the pair `wire_compatibility` is asked about.
        fn wires(doc: &InstrumentStructure) -> Vec<(WireSource, WireTarget)> {
            let mut out = Vec::new();
            for n in &doc.nodes {
                for input in &n.inputs {
                    if let InputValue::Wire { from } = &input.value {
                        out.push((from.clone(), inp(&n.address, &input.name)));
                    }
                }
            }
            for o in &doc.outputs {
                out.push((o.from.clone(), tap(&o.name)));
            }
            out
        }

        #[test]
        fn acid_techno_reads_whole_and_every_wire_is_one_validate_accepts() {
            let source = library("acid-techno.json");
            let store = FsResolver::for_document(&source).stat_only();
            let doc = read(&source, &store);
            assert_eq!(doc.nodes.len(), 53);

            let validated = validate_instrument(
                &ValidateInstrument {
                    source: source.clone(),
                },
                &store,
            )
            .unwrap()
            .output;
            assert!(validated.ok, "{validated:?}");
            assert_eq!(doc.report, validated);

            let wires = wires(&doc);
            assert!(wires.len() > doc.nodes.len(), "{} wires", wires.len());
            for (from, to) in wires {
                let args = WireCompatibility {
                    source: source.clone(),
                    from: from.clone(),
                    to: Some(vec![to.clone()]),
                };
                let verdict = &wire_compatibility(&args, &store)
                    .unwrap_or_else(|e| panic!("{from:?} is no source of the document: {e}"))
                    .output
                    .verdicts[0];
                assert!(verdict.ok, "{from:?} -> {to:?}: {:?}", verdict.reason);
            }
        }

        #[test]
        fn a_library_voicer_and_subpatch_report_their_childs_interface() {
            let boundary = |file: &str| {
                let source = library(file);
                describe_boundary(
                    &DescribeBoundary {
                        source: source.clone(),
                    },
                    &FsResolver::for_document(&source).stat_only(),
                )
                .unwrap()
                .output
            };

            let source = library("acid-techno.json");
            let doc = read(&source, &FsResolver::for_document(&source).stat_only());
            let kick = boundary("voices/kick-voice.json");
            let hosted = node(&doc, "/kick_v").child.as_ref().expect("the voice");
            assert_eq!(hosted.inputs, kick.inputs);
            assert_eq!(hosted.outputs, kick.outputs);

            let source = library("voices/kick-voice.json");
            let doc = read(&source, &FsResolver::for_document(&source).stat_only());
            let vca = boundary("voices/shaped-vca.json");
            let sub = node(&doc, "/vca");
            assert_eq!(sub.ports.inputs, vca.inputs);
            assert_eq!(sub.ports.outputs, vca.outputs);
        }

        /// The walk a host makes to find every file an instrument is built from: each document
        /// read through a store scoped to it, each document resource followed by its canonical
        /// source.
        #[test]
        fn every_file_under_a_library_instrument_is_found_by_following_canonical_sources() {
            fn walk(source: &str, found: &mut Vec<String>) {
                let doc = read(source, &FsResolver::for_document(source));
                assert!(doc.report.ok, "{source}: {:?}", doc.report);
                assert!(
                    doc.report
                        .warnings
                        .iter()
                        .all(|w| !w.message.contains("not found")),
                    "{source}: {:?}",
                    doc.report
                );
                for resource in &doc.resources {
                    assert!(
                        Path::new(&resource.canonical).is_file(),
                        "{source}: {} is not a file",
                        resource.canonical
                    );
                    if resource.canonical.ends_with(".json") && !found.contains(&resource.canonical)
                    {
                        found.push(resource.canonical.clone());
                        walk(&resource.canonical, found);
                    }
                }
            }
            let mut found = Vec::new();
            walk(&library("acid-techno.json"), &mut found);

            let names: Vec<&str> = found
                .iter()
                .filter_map(|p| Path::new(p).file_name()?.to_str())
                .collect();
            for nested in ["kick-voice.json", "shaped-vca.json", "hat-voice.json"] {
                assert!(names.contains(&nested), "{nested} not reached: {names:?}");
            }
        }

        /// From the top-level document and from a nested voice file alike, a resource resolves
        /// next to the document that references it.
        #[test]
        fn a_resource_resolves_relative_to_the_document_that_references_it() {
            for (file, id, lands) in [
                ("acid-techno.json", "kick-voice", "voices/kick-voice.json"),
                (
                    "voices/kick-voice.json",
                    "shaped-vca",
                    "voices/shaped-vca.json",
                ),
            ] {
                let source = library(file);
                let doc = read(&source, &FsResolver::for_document(&source));
                let entry = doc.resources.iter().find(|r| r.id == id).unwrap();
                // Both sides through the filesystem: on Windows it spells a path its own way.
                let expected = Path::new(&library(lands)).canonicalize().unwrap();
                let found = Path::new(&entry.canonical).canonicalize().unwrap();
                assert_eq!(found, expected, "{file}");
                assert!(Path::new(&entry.canonical).is_file());
            }
        }
    }
}
