//! No-dangling-references seed (ADR-0059 §8, seeded by the schema deletion — reuben#458):
//! every artifact or tool the repo's live text names must actually ship. The instrument JSON
//! Schema was deleted outright — its one real job, the registry guard, moved to same-commit
//! native≡wasm describe parity in the lane that builds the wasm (ADR-0059 §4) — so nothing
//! greppable (docs, skills, code, tool/resource descriptions) may still point a reader at the
//! retired machinery. Grow the test class by adding a retired artifact's tokens here when the
//! artifact goes; the tripwire keeps it from leaking back into live prose.
//!
//! `docs/adr/` is exempt: ADRs are decision history, and the history names the schema on
//! purpose (ADR-0056: history does not relocate).

use std::fs;
use std::path::{Path, PathBuf};

/// The retired schema machinery's greppable tokens — the committed schema file, the
/// generator example, the MCP resource URI, and the loose prose phrase (review round 1
/// found it surviving in skill frontmatter, untouched by the three exact tokens). Built by
/// concatenation so this file never trips over its own needles. Lowercase: matching
/// lowercases each line, so capitalized prose forms are caught too.
fn retired_tokens() -> [String; 4] {
    [
        ["instrument", ".schema.json"].concat(),
        ["gen", "_schema"].concat(),
        ["reuben://", "schema/instrument"].concat(),
        ["instrument", " schema"].concat(),
    ]
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

/// Collect every file under `dir`, skipping what can't carry a live reference: `.git` (a dir
/// in a checkout, a pointer file in a worktree), build output (`target`), and dependency
/// trees (`node_modules`) — names tooling reserves at any depth — plus nested checkouts,
/// where the `worktrees` name is reserved only directly under `.claude`. Directories are
/// never entered through a symlink (`DirEntry::file_type` reports the link itself), so a
/// symlinked directory cycle can't recurse; a symlink to a file is pushed and read through
/// downstream, and a symlink to a directory fails `read_to_string` and is skipped there.
fn walk(dir: &Path, files: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()));
    for entry in entries {
        let entry = entry.expect("dir entry");
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == ".git" || name == "target" || name == "node_modules" {
            continue;
        }
        if name == "worktrees" && dir.file_name().is_some_and(|parent| parent == ".claude") {
            continue;
        }
        let path = entry.path();
        let file_type = entry.file_type().expect("file type");
        if file_type.is_dir() {
            walk(&path, files);
        } else {
            files.push(path);
        }
    }
}

#[test]
fn no_live_text_references_the_retired_schema_machinery() {
    let root = repo_root();
    let adr_dir = root.join("docs").join("adr");
    let tokens = retired_tokens();

    let mut files = Vec::new();
    walk(&root, &mut files);

    let mut offenders = Vec::new();
    for path in files {
        // Decision history names the schema on purpose — the exemption is the directory,
        // never a per-file allowlist.
        if path.starts_with(&adr_dir) {
            continue;
        }
        // Binary files (samples, layouts, …) can't carry a greppable reference; skip on
        // non-UTF-8 rather than maintaining an extension list.
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let rel = path.strip_prefix(&root).unwrap_or(&path).to_path_buf();
        for (i, line) in text.lines().enumerate() {
            // Prose capitalizes freely; the needles are lowercase, so lowercase the hay.
            let line = line.to_lowercase();
            for token in &tokens {
                if line.contains(token.as_str()) {
                    offenders.push(format!("{}:{}: `{token}`", rel.display(), i + 1));
                }
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "the instrument JSON Schema is deleted (ADR-0059 §4) — live text may not reference \
         its retired machinery (fix the reference; don't resurrect the artifact):\n{}",
        offenders.join("\n")
    );
}
