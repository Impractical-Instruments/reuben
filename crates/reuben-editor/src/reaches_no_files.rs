//! The editor is a client of the engine's server: outside the files it owns, its non-test code
//! names no store, no file history, no watcher, and no call that reads or probes a file.

use std::path::{Path, PathBuf};

/// Files whose subject is a file the editor itself owns, not an instrument.
const OWNERS: [&str; 3] = [
    // The editor's own config directory.
    "config.rs",
    // The `--io-map` file handed to the embedded engine.
    "audio.rs",
    // The standalone "Open file…" dialog.
    "pick.rs",
];

/// Modules compiled only for tests.
const TEST_ONLY: [&str; 2] = ["ui_tests.rs", "reaches_no_files.rs"];

const TOKENS: [&str; 14] = [
    "FsResolver",
    "DocumentHistory",
    "notify::",
    "canonicalize(",
    "read_dir(",
    "read_to_string(",
    "std::fs::",
    "File::open",
    "fs::metadata",
    "symlink_metadata",
    "std::fs::write",
    ".exists()",
    ".is_file()",
    ".is_dir()",
];

/// The lines of `text` that are compiled outside tests, numbered from 1.
///
/// Each `#[cfg(test)]` drops the item it gates: one line when that line ends in `;`, `,` or `}`,
/// everything through the `}` at the attribute's own indentation when it ends in `{`, which
/// rustfmt puts on the closing line of every block. Attributes and comments between the gate and
/// its item go with it. Counting braces instead would be thrown by the unbalanced JSON the tests
/// hold in string literals.
///
/// # Panics
///
/// On a gated item whose first line ends in anything else, such as a signature broken over
/// several lines: where it ends cannot be told from that line, and guessing could drop code that
/// is compiled.
fn compiled(text: &str) -> Vec<(usize, &str)> {
    let mut kept = Vec::new();
    let mut lines = text.lines().enumerate();
    while let Some((at, line)) = lines.next() {
        if line.trim() != "#[cfg(test)]" {
            kept.push((at + 1, line));
            continue;
        }
        let indent = &line[..line.len() - line.trim_start().len()];
        let mut item = lines.by_ref().map(|(_, line)| line);
        let Some(head) = item.find(|line| {
            let line = line.trim();
            !(line.starts_with("#[") || line.starts_with("//"))
        }) else {
            break;
        };
        let head = head.trim_end();
        if head.ends_with('{') {
            let close = format!("{indent}}}");
            item.find(|line| line.starts_with(&close));
        } else if !head.ends_with([';', ',', '}']) {
            panic!(
                "a #[cfg(test)] item at line {} starts `{}`, which does not end in `{{`, `;`, `,` \
                 or `}}`: put its head on one line",
                at + 1,
                head.trim()
            );
        }
    }
    kept
}

fn sources(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("list the editor's sources") {
        let path = entry.expect("a source entry").path();
        if path.is_dir() {
            sources(&path, found);
        } else if path.extension().is_some_and(|e| e == "rs") {
            found.push(path);
        }
    }
}

#[test]
fn the_editor_reaches_no_instrument_file() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(&src, &mut files);
    files.sort();
    let mut found = Vec::new();
    for file in &files {
        let relative = file
            .strip_prefix(&src)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        if OWNERS.contains(&relative.as_str()) || TEST_ONLY.contains(&relative.as_str()) {
            continue;
        }
        let text = std::fs::read_to_string(file).expect("read a source file");
        for (line, code) in compiled(&text) {
            for token in TOKENS.iter().filter(|token| code.contains(*token)) {
                found.push(format!(
                    "src/{relative}:{line}: `{token}` in {}",
                    code.trim()
                ));
            }
        }
    }
    assert!(files.len() > 30, "scanned only {files:?}");
    assert!(
        found.is_empty(),
        "the editor reaches files:\n{}",
        found.join("\n")
    );
}

#[test]
fn a_test_gate_drops_only_the_item_it_gates() {
    let text = "\
#[cfg(test)]
mod ui_tests;
fn kept_a() {}
struct S {
    #[cfg(test)]
    writes: usize,
    kept_b: usize,
}
fn f() {
    #[cfg(test)]
    if x {
        gone_a();
    }
    #[cfg(test)]
    {
        gone_b();
    }
    kept_c();
}
#[cfg(test)]
#[allow(dead_code)]
pub fn gone_c() -> bool {
    \"{ not json\";
}
fn kept_d() {}
#[cfg(test)]
mod tests {
    fn gone_d() {}
}
";
    let kept: Vec<&str> = compiled(text).into_iter().map(|(_, line)| line).collect();
    let all = kept.join("\n");
    for name in ["kept_a", "kept_b", "kept_c", "kept_d"] {
        assert!(all.contains(name), "{name} dropped:\n{all}");
    }
    for name in ["ui_tests", "writes", "gone_a", "gone_b", "gone_c", "gone_d"] {
        assert!(!all.contains(name), "{name} kept:\n{all}");
    }

    let broken = "\
#[cfg(test)]
pub(crate) fn gone(
    a: usize,
) -> usize {
    a
}
fn kept() {}
";
    let unread = std::panic::catch_unwind(|| compiled(broken).len());
    let said = unread.expect_err("a head that does not end its line is refused");
    let said = (said.downcast_ref::<String>())
        .map(String::as_str)
        .unwrap_or_default();
    assert!(said.contains("line 1") && said.contains("gone("), "{said}");
}
