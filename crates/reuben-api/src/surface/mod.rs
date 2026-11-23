//! A surface doc: the presentation layer over an instrument's interface inputs, in the format
//! `surfaces/surface.schema.json` defines. One doc shape serves both kinds of surface: a screen
//! doc (`<stem>.json`) and a MIDI map (`<stem>.midi.json`).
//!
//! A surface doc names inputs and says how to present or drive them; it carries no ranges or
//! defaults of its own beyond an optional narrowing. Checking a screen control against the
//! instrument is [`screen::check_screen_control`].

pub mod midi;
pub mod pattern;
pub mod screen;

use std::fmt;
use std::format;
use std::string::{String, ToString};
use std::vec::Vec;

use reuben_document::text_hash;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use midi::{check_midi_control, MidiKind, MidiRoute, Scale};
pub use pattern::{OscPattern, PatternError};
pub use screen::{check_screen_control, clamp, default_label, takes_a_value, ScreenControl};

/// The one `surface_version` this reader knows.
const SURFACE_VERSION: i64 = 1;

/// Which of an instrument's two surface docs an edit or read addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceKind {
    Screen,
    Midi,
}

impl SurfaceKind {
    /// What this kind appends to the instrument's stem, before `.json`.
    pub fn suffix(self) -> &'static str {
        match self {
            Self::Screen => "",
            Self::Midi => ".midi",
        }
    }
}

/// A parsed surface doc. Unknown keys are ignored, as the other readers of this format ignore them.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct SurfaceDoc {
    /// The instrument the doc was written for.
    pub instrument: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cols: Option<usize>,
    #[serde(default)]
    pub controls: Vec<SurfaceControl>,
}

/// One entry of a surface doc's `controls`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct SurfaceControl {
    /// The interface input's name.
    pub bind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Kept as written: a kind this reader does not know is a skipped control, not a doc that
    /// fails to parse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub widget: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub velocity: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degree: Option<i64>,
    /// The OSC address pattern over translated MIDI addresses that drives this input; read only
    /// from a MIDI map (`<stem>.midi.json`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub midi: Option<String>,
}

/// Why a surface doc could not be used.
#[derive(Debug)]
pub enum SurfaceError {
    Parse(serde_json::Error),
    /// The doc declares a `surface_version` other than the one this reader knows.
    Version(serde_json::Value),
}

impl fmt::Display for SurfaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "is not a surface doc: {e}"),
            Self::Version(found) => write!(
                f,
                "declares surface_version {found}; only version {SURFACE_VERSION} is read"
            ),
        }
    }
}

impl SurfaceDoc {
    pub fn parse(text: &str) -> Result<Self, SurfaceError> {
        let value: serde_json::Value = serde_json::from_str(text).map_err(SurfaceError::Parse)?;
        // Checked before the shape: a newer version may have a shape this reader would misreport
        // as malformed.
        let version = value.get("surface_version").cloned().unwrap_or_default();
        if version.as_i64() != Some(SURFACE_VERSION) {
            return Err(SurfaceError::Version(version));
        }
        serde_json::from_value(value).map_err(SurfaceError::Parse)
    }

    /// The canonical bytes a write puts on disk, less the trailing newline. `surface_version`
    /// leads because a reader checks it before the shape.
    pub fn to_json_pretty(&self) -> String {
        #[derive(Serialize)]
        struct Wire<'a> {
            surface_version: i64,
            #[serde(flatten)]
            doc: &'a SurfaceDoc,
        }
        serde_json::to_string_pretty(&Wire {
            surface_version: SURFACE_VERSION,
            doc: self,
        })
        .expect("a surface doc is plain data")
    }
}

/// The identity of a surface doc: a hash over its [`SurfaceDoc::to_json_pretty`] bytes, so two
/// spellings of one doc agree. An opaque token, compared for equality only.
pub fn surface_hash(doc: &SurfaceDoc) -> String {
    text_hash(doc.to_json_pretty().as_bytes())
}

/// The controls of `doc`, one per line, as every surface edit and restore answers with. A MIDI
/// map shows what drives each input; a screen doc shows how each is drawn. A narrowed range is
/// appended as `min..max`, with the unset end left blank.
pub fn render_surface(doc: &SurfaceDoc, surface: SurfaceKind) -> String {
    let mut lines = Vec::with_capacity(doc.controls.len());
    for (index, control) in doc.controls.iter().enumerate() {
        let mut line = match surface {
            SurfaceKind::Midi => format!(
                "{index} {} \u{2190} {}",
                control.bind,
                control.midi.as_deref().unwrap_or("?")
            ),
            SurfaceKind::Screen => format!(
                "{index} {} {} {}",
                control.bind,
                control.widget.as_deref().unwrap_or("-"),
                control
                    .label
                    .clone()
                    .unwrap_or_else(|| default_label(&control.bind))
            ),
        };
        if control.min.is_some() || control.max.is_some() {
            let end = |v: Option<f64>| v.map(|v| v.to_string()).unwrap_or_default();
            line.push_str(&format!(" {}..{}", end(control.min), end(control.max)));
        }
        lines.push(line);
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::string::ToString;
    #[test]
    fn a_doc_parses_with_only_its_required_keys() {
        let doc = SurfaceDoc::parse(
            r#"{"surface_version":1,"instrument":"toy","controls":[{"bind":"level"}]}"#,
        )
        .expect("parses");
        assert_eq!(doc.instrument, "toy");
        assert_eq!(doc.cols, None);
        assert_eq!(doc.controls.len(), 1);
        assert_eq!(doc.controls[0].bind, "level");
        assert_eq!(doc.controls[0].widget, None);
    }

    #[test]
    fn a_doc_keeps_every_presentation_key() {
        let doc = SurfaceDoc::parse(
            r#"{"surface_version":1,"instrument":"toy","cols":7,"controls":[
                {"bind":"n","label":"C","widget":"note-toggle","group":"keys","note":60,
                 "velocity":0.5,"min":0,"max":1,"degree":2}]}"#,
        )
        .expect("parses");
        assert_eq!(doc.cols, Some(7));
        assert_eq!(
            doc.controls[0],
            SurfaceControl {
                bind: "n".to_string(),
                label: Some("C".to_string()),
                widget: Some("note-toggle".to_string()),
                group: Some("keys".to_string()),
                min: Some(0.0),
                max: Some(1.0),
                note: Some(60),
                velocity: Some(0.5),
                degree: Some(2),
                midi: None,
            }
        );
    }

    #[test]
    fn parses_a_midi_control() {
        let doc = SurfaceDoc::parse(
            r#"{"surface_version":1,"instrument":"x","controls":[
                {"bind":"cutoff","midi":"/midi/1/cc/74","min":200,"max":8000}]}"#,
        )
        .expect("parses");
        assert_eq!(doc.controls[0].midi.as_deref(), Some("/midi/1/cc/74"));
        assert_eq!(doc.controls[0].min, Some(200.0));
    }

    #[test]
    fn another_version_is_refused_by_number() {
        for (text, found) in [
            (
                r#"{"surface_version":2,"instrument":"t","controls":[]}"#,
                "2",
            ),
            (r#"{"instrument":"t","controls":[]}"#, "null"),
        ] {
            let error = SurfaceDoc::parse(text).expect_err("refused");
            assert!(matches!(error, SurfaceError::Version(_)), "{error}");
            assert!(
                error.to_string().contains(&format!("version {found}")),
                "{error}"
            );
        }
    }

    #[test]
    fn malformed_docs_are_parse_errors() {
        for text in [
            "{",
            r#"{"surface_version":1,"controls":[]}"#,
            r#"{"surface_version":1,"instrument":"t","controls":[{"label":"no bind"}]}"#,
            r#"{"surface_version":1,"instrument":"t","controls":[{"bind":"x","min":"low"}]}"#,
        ] {
            let error = SurfaceDoc::parse(text).expect_err("refused");
            assert!(matches!(error, SurfaceError::Parse(_)), "{text}: {error}");
        }
    }

    #[test]
    fn to_json_pretty_round_trips() {
        let doc = SurfaceDoc::parse(
            r#"{"surface_version":1,"instrument":"toy","cols":3,"controls":[
                {"bind":"cutoff","midi":"/midi/1/cc/74","min":200,"max":8000},
                {"bind":"n","label":"C","widget":"note-toggle","note":60}]}"#,
        )
        .expect("parses");
        let text = doc.to_json_pretty();
        assert!(text.starts_with("{\n  \"surface_version\": 1"), "{text}");
        assert_eq!(SurfaceDoc::parse(&text).expect("reparses"), doc);
    }

    #[test]
    fn hash_ignores_formatting() {
        let compact = SurfaceDoc::parse(
            r#"{"surface_version":1,"instrument":"t","controls":[{"bind":"a"}]}"#,
        )
        .unwrap();
        let spread = SurfaceDoc::parse(
            "{\n \"controls\": [ {\"bind\": \"a\"} ],\n \"instrument\": \"t\",\n \"surface_version\": 1 }",
        )
        .unwrap();
        assert_eq!(surface_hash(&compact), surface_hash(&spread));
    }

    #[test]
    fn render_surface_lists_each_control_by_index() {
        let midi = SurfaceDoc::parse(
            r#"{"surface_version":1,"instrument":"t","controls":[
                {"bind":"cutoff","midi":"/midi/1/cc/74","min":200,"max":8000},
                {"bind":"level","midi":"/midi/1/cc/7"}]}"#,
        )
        .unwrap();
        assert_eq!(
            render_surface(&midi, SurfaceKind::Midi),
            "0 cutoff \u{2190} /midi/1/cc/74 200..8000\n1 level \u{2190} /midi/1/cc/7"
        );
        let screen = SurfaceDoc::parse(
            r#"{"surface_version":1,"instrument":"t","controls":[
                {"bind":"cutoff","label":"Cut","widget":"fader","min":0.2}]}"#,
        )
        .unwrap();
        assert_eq!(
            render_surface(&screen, SurfaceKind::Screen),
            "0 cutoff fader Cut 0.2.."
        );
        assert_eq!(SurfaceKind::Midi.suffix(), ".midi");
        assert_eq!(SurfaceKind::Screen.suffix(), "");
    }
}
