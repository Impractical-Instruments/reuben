//! The authoring guide's `lanes:` section tags and the mechanical lane cut.
//!
//! `docs/agents/authoring.md` is authored once and delivered per lane: the repo
//! skills point at it, the MCP sidecar serves it in-band, and the web lane bundles a
//! build-time slice — the full guide **minus checkout-only sections** (the filesystem
//! sample workflow, the ADR index). The slice is never hand-paraphrased: every Markdown
//! heading carries a trailing `<!-- lanes: ... -->` tag naming the lanes its section ships
//! to, and [`lane_cut`] keeps exactly the sections tagged for the requested lane — a
//! deterministic function of the tags + the guide, emitted by
//! `cargo run -p reuben-core --example guide_cut -- <lane>` and held honest by the
//! `guide_lanes` test (an untagged or unknown-lane heading fails, so a new section cannot
//! silently skip the cut).

use std::fmt;

/// A delivery lane for the authoring guide: repo **skills** (checkout,
/// pointers work), **MCP** clients (in-band resources), **web** chat (no checkout — the
/// bundled slice is its only grounding).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    Skills,
    Mcp,
    Web,
}

impl Lane {
    /// Every lane a `lanes:` tag may name, in tag order.
    pub const ALL: [Lane; 3] = [Lane::Skills, Lane::Mcp, Lane::Web];

    /// The name a `lanes:` tag (or a CLI arg) uses for this lane.
    pub fn name(self) -> &'static str {
        match self {
            Lane::Skills => "skills",
            Lane::Mcp => "mcp",
            Lane::Web => "web",
        }
    }

    /// Parse a tag entry / CLI arg; `None` for anything but the three known lane names.
    pub fn parse(s: &str) -> Option<Lane> {
        Lane::ALL.into_iter().find(|lane| lane.name() == s)
    }
}

/// Why a guide failed the lane cut: every heading must carry a well-formed `lanes:` tag,
/// so the failure names the offending heading line for the fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaneCutError {
    /// A heading with no trailing `<!-- lanes: ... -->` tag (or a comment that isn't one).
    UntaggedHeading { line: usize, heading: String },
    /// A `lanes:` tag naming something that isn't a lane — a typo would otherwise
    /// silently drop the section from that lane's cut.
    UnknownLane { line: usize, lane: String },
    /// A `lanes:` tag with no lanes at all — a section that ships nowhere is dead prose.
    EmptyLanes { line: usize },
}

impl fmt::Display for LaneCutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LaneCutError::UntaggedHeading { line, heading } => write!(
                f,
                "authoring guide line {line}: heading has no `<!-- lanes: ... -->` tag: {heading}"
            ),
            LaneCutError::UnknownLane { line, lane } => write!(
                f,
                "authoring guide line {line}: unknown lane {lane:?} (known: skills, mcp, web)"
            ),
            LaneCutError::EmptyLanes { line } => {
                write!(
                    f,
                    "authoring guide line {line}: `lanes:` tag names no lanes"
                )
            }
        }
    }
}

impl std::error::Error for LaneCutError {}

/// Cut the guide for one delivery lane: keep exactly the sections whose heading tag names
/// `lane`, verbatim and in source order, with the tag comments stripped from the emitted
/// headings. A section is its ATX heading plus everything up to the next heading, at any
/// level; fenced code blocks are opaque (a `#` line inside one is content, not a heading).
///
/// Errors rather than guesses on an untagged heading, an unknown lane name, or an empty
/// tag — the guide must stay fully tagged for the cut to be trusted.
pub fn lane_cut(source: &str, lane: Lane) -> Result<String, LaneCutError> {
    let mut out = String::with_capacity(source.len());
    let mut in_fence = false;
    // The prose before any heading belongs to every lane; the guide opens with its `#`
    // title, so today this span is empty.
    let mut keeping = true;

    for (idx, raw) in source.lines().enumerate() {
        if raw.trim_start().starts_with("```") {
            in_fence = !in_fence;
        } else if !in_fence && is_heading(raw) {
            let line = idx + 1;
            let (heading, lanes) = parse_tagged_heading(raw, line)?;
            keeping = lanes.contains(&lane);
            if keeping {
                out.push_str(heading.trim_end());
                out.push('\n');
            }
            continue;
        }
        if keeping {
            out.push_str(raw);
            out.push('\n');
        }
    }
    Ok(out)
}

/// An ATX heading outside a fence: 1–6 `#`s then a space.
fn is_heading(line: &str) -> bool {
    let hashes = line.len() - line.trim_start_matches('#').len();
    (1..=6).contains(&hashes) && line[hashes..].starts_with(' ')
}

/// Split a heading line into its text and the lanes its trailing
/// `<!-- lanes: a,b,c -->` tag names.
fn parse_tagged_heading(raw: &str, line: usize) -> Result<(&str, Vec<Lane>), LaneCutError> {
    let untagged = || LaneCutError::UntaggedHeading {
        line,
        heading: raw.trim().to_string(),
    };
    let trimmed = raw.trim_end();
    let comment_at = trimmed.rfind("<!--").ok_or_else(untagged)?;
    let comment = trimmed[comment_at..]
        .strip_prefix("<!--")
        .and_then(|c| c.strip_suffix("-->"))
        .ok_or_else(untagged)?;
    let list = comment.trim().strip_prefix("lanes:").ok_or_else(untagged)?;

    let mut lanes = Vec::new();
    for entry in list.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        lanes.push(Lane::parse(entry).ok_or(LaneCutError::UnknownLane {
            line,
            lane: entry.to_string(),
        })?);
    }
    if lanes.is_empty() {
        return Err(LaneCutError::EmptyLanes { line });
    }
    Ok((&trimmed[..comment_at], lanes))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TAGGED: &str = "\
# Title <!-- lanes: skills,mcp -->

Intro prose.

## Everywhere <!-- lanes: skills,mcp,web -->

Body kept in every lane.

```json
# not a heading — fence content
```

## Checkout only <!-- lanes: skills,mcp -->

Filesystem gesture.

## Tail <!-- lanes: skills,mcp,web -->

Last section.
";

    #[test]
    fn cut_keeps_tagged_sections_and_strips_tags() {
        let web = lane_cut(TAGGED, Lane::Web).expect("tagged guide cuts clean");
        assert_eq!(
            web,
            "## Everywhere\n\nBody kept in every lane.\n\n```json\n# not a heading — fence \
             content\n```\n\n## Tail\n\nLast section.\n"
        );
        let skills = lane_cut(TAGGED, Lane::Skills).expect("tagged guide cuts clean");
        assert!(skills.contains("# Title\n"));
        assert!(skills.contains("Filesystem gesture."));
        assert!(
            !skills.contains("lanes:"),
            "tags are cut metadata, not content"
        );
    }

    #[test]
    fn untagged_heading_is_an_error_not_a_guess() {
        let err = lane_cut("## No tag here\n", Lane::Web).unwrap_err();
        assert_eq!(
            err,
            LaneCutError::UntaggedHeading {
                line: 1,
                heading: "## No tag here".to_string()
            }
        );
    }

    #[test]
    fn unknown_lane_and_empty_tag_are_errors() {
        let err = lane_cut("## H <!-- lanes: web,cli -->\n", Lane::Web).unwrap_err();
        assert_eq!(
            err,
            LaneCutError::UnknownLane {
                line: 1,
                lane: "cli".to_string()
            }
        );
        let err = lane_cut("## H <!-- lanes: -->\n", Lane::Web).unwrap_err();
        assert_eq!(err, LaneCutError::EmptyLanes { line: 1 });
    }

    #[test]
    fn fenced_hash_lines_are_content_not_headings() {
        let src = "## H <!-- lanes: web -->\n```\n# looks like a heading\n```\n";
        let cut = lane_cut(src, Lane::Web).expect("fence content needs no tag");
        assert!(cut.contains("# looks like a heading"));
    }
}
