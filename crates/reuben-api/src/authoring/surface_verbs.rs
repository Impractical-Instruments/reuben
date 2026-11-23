//! The surface-doc verbs: a read and three one-control edits over an instrument's MIDI map or
//! on-screen surface doc, each control checked against the instrument's interface inputs with the
//! same checks `reuben play`'s MIDI router and the editor's panel use.
//!
//! An edit answers the [`EditResult`] the instrument verbs answer. It is refused, with nothing
//! written, when the control it sets would be skipped; a control that was already skipped never
//! blocks an edit to another one.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use reuben_document::text_hash;

use super::args::{
    DescribeBoundary, DescribeInstrumentSurface, MoveInstrumentSurfaceControl,
    RemoveInstrumentSurfaceControl, SetInstrumentSurfaceControl,
};
use super::result::{Boundary, ControlView, Diag, EditResult, Report, SurfaceView};
use super::verbs::{describe_boundary, edit_summary, expect_miss, Answer, Refusal};
use crate::resources::{ResolveError, Resources, Undecoded};
use crate::surface::{
    check_midi_control, check_screen_control, render_surface, surface_hash, SurfaceControl,
    SurfaceDoc, SurfaceKind,
};

/// Read an instrument's surface doc of one kind, each control checked against the instrument.
///
/// A doc that does not parse is an answer with `error` set, not a refusal: the caller still needs
/// its `source` to say which file is broken.
pub fn describe_instrument_surface(
    args: &DescribeInstrumentSurface,
    resources: &dyn Resources,
) -> Result<Answer<SurfaceView>, Refusal> {
    let at = locate(&args.source, args.surface, resources)?;
    let mut view = SurfaceView {
        source: at.clone(),
        exists: false,
        hash: None,
        instrument: None,
        cols: None,
        controls: Vec::new(),
        warnings: Vec::new(),
        error: None,
    };
    let name = doc_name(args.surface);
    let summary = match read(&at, resources)? {
        Stored::Missing => format!("`{}` has no {name}", args.source),
        Stored::Unparsed { error, .. } => {
            let summary = format!("the {name} {error}");
            view.exists = true;
            view.error = Some(error);
            summary
        }
        Stored::Parsed(doc) => {
            let hash = surface_hash(&doc);
            let checked = boundary(&args.source, resources);
            match &checked {
                Ok(boundary) => view
                    .warnings
                    .extend(doc_warning(&doc, boundary, args.surface)),
                Err(why) => view
                    .warnings
                    .push(unloadable(&args.source, args.surface, why)),
            }
            view.controls = doc
                .controls
                .into_iter()
                .map(|control| {
                    let found = checked
                        .as_ref()
                        .map(|boundary| check(&control, boundary, args.surface));
                    let (skipped, warnings) = match found {
                        Ok(Ok(warnings)) => (None, warnings),
                        Ok(Err(why)) => (Some(why), Vec::new()),
                        // Nothing to check against: the doc-level warning says so.
                        Err(_) => (None, Vec::new()),
                    };
                    ControlView {
                        control,
                        skipped,
                        warnings,
                    }
                })
                .collect();
            let skipped = view.controls.iter().filter(|c| c.skipped.is_some()).count();
            let summary = format!(
                "{} control(s), {skipped} skipped (content_hash {hash})",
                view.controls.len()
            );
            view.exists = true;
            view.hash = Some(hash);
            view.instrument = Some(doc.instrument);
            view.cols = doc.cols;
            summary
        }
    };
    Ok(Answer {
        output: view,
        summary,
    })
}

/// Replace the control at `index`, or append it. The first set on a missing doc creates it for
/// the instrument as it is named now.
pub fn set_instrument_surface_control(
    args: &SetInstrumentSurfaceControl,
    resources: &dyn Resources,
) -> Result<Answer<EditResult>, Refusal> {
    edit_surface(
        &args.source,
        args.surface,
        &args.expect,
        resources,
        |doc, boundary| {
            let mut doc = doc.unwrap_or_else(|| SurfaceDoc {
                instrument: boundary.instrument.clone(),
                cols: None,
                controls: Vec::new(),
            });
            if let Some(index) = args.index {
                in_range(index, doc.controls.len(), args.surface)?;
            }
            if let Err(why) = check(&args.control, boundary, args.surface) {
                return Ok(Err(diag(Some(&args.control.bind), why)));
            }
            match args.index {
                Some(index) => doc.controls[index] = args.control.clone(),
                None => doc.controls.push(args.control.clone()),
            }
            Ok(Ok(doc))
        },
    )
}

/// Remove the control at `index`. The last one removed leaves an empty doc, not a missing one.
pub fn remove_instrument_surface_control(
    args: &RemoveInstrumentSurfaceControl,
    resources: &dyn Resources,
) -> Result<Answer<EditResult>, Refusal> {
    edit_surface(
        &args.source,
        args.surface,
        &args.expect,
        resources,
        |doc, _| {
            let mut doc = existing(doc, &args.source, args.surface)?;
            in_range(args.index, doc.controls.len(), args.surface)?;
            doc.controls.remove(args.index);
            Ok(Ok(doc))
        },
    )
}

/// Move the control at `from` so it ends up at `to`.
pub fn move_instrument_surface_control(
    args: &MoveInstrumentSurfaceControl,
    resources: &dyn Resources,
) -> Result<Answer<EditResult>, Refusal> {
    edit_surface(
        &args.source,
        args.surface,
        &args.expect,
        resources,
        |doc, _| {
            let mut doc = existing(doc, &args.source, args.surface)?;
            for index in [args.from, args.to] {
                in_range(index, doc.controls.len(), args.surface)?;
            }
            let control = doc.controls.remove(args.from);
            doc.controls.insert(args.to, control);
            Ok(Ok(doc))
        },
    )
}

// --- the shared edit pipeline ---------------------------------------------------------------------

/// What a surface doc's source holds.
enum Stored {
    Missing,
    /// Text that is not a doc this reader knows, with the hash of its bytes as its identity.
    Unparsed {
        hash: String,
        error: String,
    },
    Parsed(SurfaceDoc),
}

/// Run one surface edit: find and read the doc, apply the `expect` guard, read the instrument's
/// interface inputs, let `change` make the new doc, then write it and report on every control.
///
/// `change` answers `Err` for a request that is not coherent (an index out of range, a doc that is
/// not there), `Ok(Err)` for a control it will not write, and `Ok(Ok)` for the doc to write.
fn edit_surface(
    source: &str,
    surface: SurfaceKind,
    expect: &Option<String>,
    resources: &dyn Resources,
    change: impl FnOnce(Option<SurfaceDoc>, &Boundary) -> Result<Result<SurfaceDoc, Diag>, Refusal>,
) -> Result<Answer<EditResult>, Refusal> {
    let at = locate(source, surface, resources)?;
    let current = match read(&at, resources)? {
        Stored::Missing => None,
        Stored::Unparsed { hash, error } => {
            let message = format!("the {} {error}; it is left as it is", doc_name(surface));
            return Ok(not_written(hash, diag(None, message), String::new()));
        }
        Stored::Parsed(doc) => Some(doc),
    };
    // A missing doc's hash is empty, which no `expect` a caller was handed can match.
    let (hash, zoom) = current.as_ref().map_or_else(Default::default, |doc| {
        (surface_hash(doc), render_surface(doc, surface))
    });
    if let Some(expected) = expect.as_ref().filter(|expected| **expected != hash) {
        return Ok(expect_miss(hash, expected, zoom));
    }
    let boundary = match boundary(source, resources) {
        Ok(boundary) => boundary,
        Err(why) => {
            let message = unloadable(source, surface, &why);
            return Ok(not_written(hash, diag(None, message), zoom));
        }
    };
    let doc = match change(current, &boundary)? {
        Ok(doc) => doc,
        Err(error) => return Ok(not_written(hash, error, zoom)),
    };
    resources
        .write_text(&at, &format!("{}\n", doc.to_json_pretty()))
        .map_err(|e| Refusal::new(format!("could not write `{at}`: {e}")))?;
    let mut warnings: Vec<Diag> = doc_warning(&doc, &boundary, surface)
        .map(|message| diag(None, message))
        .into_iter()
        .collect();
    for control in &doc.controls {
        let found = match check(control, &boundary, surface) {
            Ok(adjusted) => adjusted,
            Err(why) => vec![why],
        };
        warnings.extend(
            found
                .into_iter()
                .map(|message| diag(Some(&control.bind), message)),
        );
    }
    let output = EditResult {
        report: Report {
            ok: true,
            errors: Vec::new(),
            warnings,
        },
        pre_existing: Vec::new(),
        written: true,
        hash: surface_hash(&doc),
        notes: Vec::new(),
        zoom: render_surface(&doc, surface),
    };
    let summary = edit_summary(&output);
    Ok(Answer { output, summary })
}

/// Where `source`'s surface doc of `surface` kind is, as the store spells it.
fn locate(
    source: &str,
    surface: SurfaceKind,
    resources: &dyn Resources,
) -> Result<String, Refusal> {
    resources
        .surface_source(&resources.canonical(source, None), surface)
        .ok_or_else(|| Refusal::new("this store keeps no surface docs"))
}

fn read(at: &str, resources: &dyn Resources) -> Result<Stored, Refusal> {
    match resources.read_text(at) {
        Err(ResolveError::NotFound(_)) => Ok(Stored::Missing),
        Err(e) => Err(Refusal::new(format!("could not read `{at}`: {e}"))),
        Ok(text) => Ok(match SurfaceDoc::parse(&text) {
            Ok(doc) => Stored::Parsed(doc),
            Err(e) => Stored::Unparsed {
                hash: text_hash(text.as_bytes()),
                error: format!("at `{at}` {e}"),
            },
        }),
    }
}

/// The instrument's interface inputs, or why it has none to check against: the boundary read's
/// own reason, without the advice on what to read instead that follows it.
fn boundary(source: &str, resources: &dyn Resources) -> Result<Boundary, String> {
    let args = DescribeBoundary {
        source: source.to_string(),
    };
    describe_boundary(&args, &Undecoded(resources))
        .map(|answer| answer.output)
        .map_err(|refusal| {
            refusal
                .message
                .split("\n\n")
                .next()
                .unwrap_or_default()
                .to_string()
        })
}

/// The adjustments that make `control` usable, or why it is skipped.
fn check(
    control: &SurfaceControl,
    boundary: &Boundary,
    surface: SurfaceKind,
) -> Result<Vec<String>, String> {
    match surface {
        SurfaceKind::Midi => check_midi_control(control, boundary).map(|(_, warnings)| warnings),
        SurfaceKind::Screen => {
            check_screen_control(control, boundary).map(|(_, warnings)| warnings)
        }
    }
}

fn doc_warning(doc: &SurfaceDoc, boundary: &Boundary, surface: SurfaceKind) -> Option<String> {
    (doc.instrument != boundary.instrument).then(|| {
        format!(
            "the {} was written for instrument `{}`, not `{}`",
            doc_name(surface),
            doc.instrument,
            boundary.instrument
        )
    })
}

fn unloadable(source: &str, surface: SurfaceKind, why: &str) -> String {
    format!(
        "`{source}` does not load, so its {} cannot be checked: {why}",
        doc_name(surface)
    )
}

fn existing(
    doc: Option<SurfaceDoc>,
    source: &str,
    surface: SurfaceKind,
) -> Result<SurfaceDoc, Refusal> {
    doc.ok_or_else(|| Refusal::new(format!("`{source}` has no {}", doc_name(surface))))
}

fn in_range(index: usize, len: usize, surface: SurfaceKind) -> Result<(), Refusal> {
    if index < len {
        return Ok(());
    }
    Err(Refusal::new(format!(
        "index {index} is out of range: the {} has {len} control(s)",
        doc_name(surface)
    )))
}

fn doc_name(surface: SurfaceKind) -> &'static str {
    match surface {
        SurfaceKind::Midi => "MIDI map",
        SurfaceKind::Screen => "surface doc",
    }
}

fn diag(port: Option<&str>, message: String) -> Diag {
    Diag {
        node: None,
        port: port.map(str::to_string),
        message,
    }
}

fn not_written(hash: String, error: Diag, zoom: String) -> Answer<EditResult> {
    let output = EditResult {
        report: Report {
            ok: false,
            errors: vec![error],
            warnings: Vec::new(),
        },
        pre_existing: Vec::new(),
        written: false,
        hash,
        notes: Vec::new(),
        zoom,
    };
    let summary = edit_summary(&output);
    Answer { output, summary }
}

#[cfg(test)]
mod tests;
