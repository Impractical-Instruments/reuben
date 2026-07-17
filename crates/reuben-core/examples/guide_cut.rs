//! Emit one delivery lane's cut of the authoring guide to stdout (ADR-0059 §3).
//!
//! `cargo run -p reuben-core --example guide_cut -- web`
//!
//! The web build bundles the `web` slice into its stable prefix — the full guide minus
//! checkout-only sections, mechanically sliced by the headings' `lanes:` tags, never
//! hand-paraphrased (ADR-0051). The guide is read from the checkout at run time (the
//! ADR-0051 §4 posture: a stale build must not emit a stale guide); the `guide_lanes`
//! test keeps every heading tagged so the cut can be trusted.

use std::path::Path;

use reuben_core::guide::{lane_cut, Lane};

fn main() {
    let arg = std::env::args()
        .nth(1)
        .expect("usage: guide_cut <skills|mcp|web>");
    let lane = Lane::parse(&arg)
        .unwrap_or_else(|| panic!("unknown lane {arg:?} — usage: guide_cut <skills|mcp|web>"));

    let guide = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/agents/authoring.md");
    let source =
        std::fs::read_to_string(&guide).unwrap_or_else(|e| panic!("read {}: {e}", guide.display()));
    let cut = lane_cut(&source, lane).unwrap_or_else(|e| panic!("{e}"));
    print!("{cut}");
}
