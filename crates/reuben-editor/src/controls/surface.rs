//! Reading a surface doc: the presentation layer over an instrument's interface inputs, in the
//! format `surfaces/surface.schema.json` defines.
//!
//! A surface doc names inputs and says how to present them; it carries no ranges or defaults of
//! its own beyond an optional narrowing. Resolving it against the instrument is
//! [`Panel::from_surface`](super::model::Panel::from_surface).

use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The one `surface_version` this reader knows.
const SURFACE_VERSION: i64 = 1;

/// How many directories, counting the instrument's own, are searched for a `surfaces/` directory.
/// Three reaches the repository layout from both `instruments/<name>.json` and
/// `instruments/patches/<name>.json`.
const SEARCH_DEPTH: usize = 3;

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
    Read(std::io::Error),
    Parse(serde_json::Error),
    /// The doc declares a `surface_version` other than the one this reader knows.
    Version(serde_json::Value),
}

impl fmt::Display for SurfaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(e) => write!(f, "could not be read: {e}"),
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

    pub fn read(path: &Path) -> Result<Self, SurfaceError> {
        Self::parse(&std::fs::read_to_string(path).map_err(SurfaceError::Read)?)
    }
}

/// The surface doc for the instrument file at `instrument`, if one exists: `surfaces/<stem>.json`
/// under the instrument's own directory or the nearest of its next two ancestors. So
/// `instruments/acid-techno.json` finds `surfaces/acid-techno.json` beside `instruments/`.
pub fn locate(instrument: &Path) -> Option<PathBuf> {
    let name = Path::new(instrument.file_stem()?).with_extension("json");
    // `Path::new("x.json").parent()` is the empty path, which names the working directory only by
    // accident of how `join` treats it; make the instrument absolute so ancestors are real.
    let instrument = std::path::absolute(instrument).ok()?;
    instrument
        .ancestors()
        .skip(1)
        .take(SEARCH_DEPTH)
        .map(|dir| dir.join("surfaces").join(&name))
        .find(|candidate| candidate.is_file())
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

    fn tree(test: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "reuben_editor_surface_{test}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("instruments/patches")).expect("instrument dirs");
        std::fs::create_dir_all(root.join("surfaces")).expect("a surfaces dir");
        root
    }

    #[test]
    fn the_doc_is_found_in_a_surfaces_directory_beside_the_instruments_directory() {
        let root = tree("beside");
        let surface = root.join("surfaces/toy.json");
        std::fs::write(&surface, "{}").expect("a surface doc");

        assert_eq!(
            locate(&root.join("instruments/toy.json")),
            Some(surface.clone())
        );
        assert_eq!(
            locate(&root.join("instruments/patches/toy.json")),
            Some(surface)
        );
        assert_eq!(locate(&root.join("instruments/other.json")), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_nearest_surfaces_directory_wins_and_the_search_stops_two_levels_up() {
        let root = tree("nearest");
        let far = root.join("surfaces/toy.json");
        std::fs::write(&far, "{}").expect("a surface doc");
        let near = root.join("instruments/surfaces/toy.json");
        std::fs::create_dir_all(near.parent().expect("a parent")).expect("a nearer surfaces dir");
        std::fs::write(&near, "{}").expect("a nearer surface doc");
        assert_eq!(locate(&root.join("instruments/toy.json")), Some(near));

        let deep = root.join("instruments/patches/deeper");
        std::fs::create_dir_all(&deep).expect("a deeper dir");
        std::fs::write(root.join("surfaces/deep.json"), "{}").expect("a surface doc");
        assert_eq!(locate(&deep.join("deep.json")), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
