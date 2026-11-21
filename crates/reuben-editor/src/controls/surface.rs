//! Reading a surface doc: the presentation layer over an instrument's interface inputs, in the
//! format `surfaces/surface.schema.json` defines.
//!
//! A surface doc names inputs and says how to present them; it carries no ranges or defaults of
//! its own beyond an optional narrowing. Resolving it against the instrument is
//! [`Panel::from_surface`](super::model::Panel::from_surface).

use std::fmt;

use serde::Deserialize;

/// The one `surface_version` this reader knows.
const SURFACE_VERSION: i64 = 1;

/// A parsed surface doc. Unknown keys are ignored, as the other readers of this format ignore them.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SurfaceDoc {
    /// The instrument the doc was written for.
    pub instrument: String,
    #[serde(default)]
    pub cols: Option<usize>,
    #[serde(default)]
    pub controls: Vec<SurfaceControl>,
}

/// One entry of a surface doc's `controls`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SurfaceControl {
    /// The interface input's name.
    pub bind: String,
    #[serde(default)]
    pub label: Option<String>,
    /// Kept as written: a kind this reader does not know is a skipped control, not a doc that
    /// fails to parse.
    #[serde(default)]
    pub widget: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub note: Option<i64>,
    #[serde(default)]
    pub velocity: Option<f64>,
    #[serde(default)]
    pub degree: Option<i64>,
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
                "declares surface_version {found}; this editor reads version {SURFACE_VERSION}"
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
            }
        );
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
}
