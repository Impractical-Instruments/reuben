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
        .arg("--temporary-server")
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

fn reuben(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_reuben"))
        .args(args)
        .arg("--temporary-server")
        .output()
        .expect("the reuben binary runs")
}

/// The instrument flags mean nothing to the operator listing: each one is refused there, with or
/// without an operator name and in compact mode, rather than printing operators as if it was read.
#[test]
fn the_operator_route_refuses_the_instrument_flags() {
    let flags: [(&str, &[&str]); 3] = [
        ("--view", &["--view", "boundary"]),
        ("--select", &["--select", "/nope"]),
        ("--select-type", &["--select-type", "oscillator"]),
    ];
    let routes: [&[&str]; 3] = [
        &["describe"],
        &["describe", "oscillator"],
        &["describe", "--compact"],
    ];
    for (flag, terms) in flags {
        for route in routes {
            let args: Vec<&str> = route.iter().chain(terms).copied().collect();
            let out = reuben(&args);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(!out.status.success(), "{args:?} must refuse");
            assert!(
                out.stdout.is_empty(),
                "{args:?} printed: {}",
                String::from_utf8_lossy(&out.stdout)
            );
            assert!(
                stderr.contains(flag) && stderr.contains("instrument"),
                "{args:?} refusal names {flag} and what it applies to: {stderr}"
            );
        }
    }
}

/// The CLI refuses in the window's order: a source that is not there is reported as missing on
/// every view, before any selection is looked at.
#[test]
fn a_missing_source_is_reported_before_the_selection_on_every_view() {
    for view in ["index", "resources", "boundary"] {
        let out = reuben(&[
            "describe",
            "no/such/instrument.json",
            "--view",
            view,
            "--select",
            "x",
        ]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "`{view}`: {stderr}");
        assert!(
            !stderr.contains("takes no `select`"),
            "`{view}` reports the missing source, not the selection: {stderr}"
        );
    }
}
