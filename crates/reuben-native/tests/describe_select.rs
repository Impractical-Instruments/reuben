//! Integration: `reuben describe <path> --view … --select/--select-type` through the built binary.
//! A view either honours a selection or refuses it — never prints its whole answer as though the
//! terms had been read. `boundary` matters most here: the binary sends it down its own path, so
//! the window's check has to be reached on that path too.

use std::path::PathBuf;
use std::process::{Command, Output};

/// A library instrument with nodes, pipes and a loadable boundary.
fn instrument() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../instruments/acid-techno.json")
        .display()
        .to_string()
}

fn describe(view: &str, terms: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_reuben"))
        .args(["describe", &instrument(), "--view", view])
        .args(terms)
        .output()
        .expect("the reuben binary runs")
}

#[test]
fn a_view_honours_a_selection_or_refuses_it() {
    let spellings: [&[&str]; 2] = [&["--select", "/nope"], &["--select-type", "oscillator"]];
    for (view, selectable) in [
        ("index", false),
        ("nodes", true),
        ("pipes", true),
        ("resources", false),
        ("boundary", false),
    ] {
        for terms in spellings {
            let out = describe(view, terms);
            let stderr = String::from_utf8_lossy(&out.stderr);
            if selectable {
                assert!(out.status.success(), "`{view}` {terms:?} narrows: {stderr}");
                continue;
            }
            assert!(
                !out.status.success(),
                "`{view}` {terms:?} must refuse, not print the whole view: {}",
                String::from_utf8_lossy(&out.stdout)
            );
            assert!(
                out.stdout.is_empty(),
                "a refused view prints nothing on stdout"
            );
            for named in [format!("`{view}`"), "\"nodes\"".into(), "\"pipes\"".into()] {
                assert!(
                    stderr.contains(&named),
                    "`{view}` {terms:?} refusal names {named}: {stderr}"
                );
            }
        }
    }
}

#[test]
fn every_view_answers_without_terms() {
    for view in ["index", "nodes", "pipes", "resources", "boundary"] {
        let out = describe(view, &[]);
        assert!(
            out.status.success(),
            "`{view}` with no terms: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
