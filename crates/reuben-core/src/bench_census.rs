//! The micro-bench census: which kinds the per-operator micro layer benches, and the bench id each
//! one is gated under — generated from [`Registry::builtin`] into two checked-in files by
//! `cargo run -p reuben-core --example gen_bench_census`.
//!
//! - [`KINDS_FILE`], one kind per line. The CI perf gate reads it with a shell script, at the PR
//!   head and at the baseline commit, without building either side — which is why the operator set
//!   has to exist as text at all: an `m::*` census entry generates kinds (`add_f32_value`, …) that
//!   appear nowhere in the source as literals.
//! - The `micro_bench_ops!` block in [`IAI_BENCH`]. gungraun's `#[bench::<id>]` attributes are
//!   compile-time, so the bench cannot walk the registry.
//!
//! Compiled into every host build, unlike `bench_support`, so the staleness tests in
//! `reuben-document/tests/bench_census.rs` run under a plain `cargo test` and the generator needs no
//! feature flag. They live beside the files they check: this crate's own tests also compile in a
//! copy of it that has no `reuben-document` beside it. Left out of bare-metal builds, where its
//! string sorting would only add machine code to an image nothing benches.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use crate::registry::Registry;

/// The command that rewrites both generated files. Every staleness failure names it.
pub const REGENERATE: &str = "cargo run -p reuben-core --example gen_bench_census";

/// Workspace-relative path of the kinds file. `.github/scripts/perf-gate.sh` names the same path.
pub const KINDS_FILE: &str = "crates/reuben-document/benches/micro_kinds.txt";

/// Workspace-relative path of the iai bench whose `micro_bench_ops!` block is generated.
pub const IAI_BENCH: &str = "crates/reuben-document/benches/micro_iai.rs";

/// The bench-only zero-DSP probe: benched, never registered (see `bench_support::overhead`).
pub const OVERHEAD: &str = "overhead";

/// A second workload for the sample player: loop mode, which the one-shot recipe never reaches.
/// Bench-only like [`OVERHEAD`], but it benches a registered operator rather than a probe, so the
/// harness resolves it through [`VARIANTS`] to the operator it drives.
pub const SAMPLE_LOOP: &str = "sample_loop";

/// Workload kinds that bench a mode of a registered operator, as `(workload kind, operator kind)`.
/// Each gets its own gated case, so a mode the default recipe never reaches has a baseline of its
/// own.
pub const VARIANTS: &[(&str, &str)] = &[(SAMPLE_LOOP, "sample")];

/// Bench ids that predate the generator and differ from their kind, as `(kind, id)`.
///
/// The perf gate matches a callgrind case to its baseline by id, so these are carried verbatim:
/// renaming one would drop that operator's history from the gate. Every kind not listed here is
/// benched under its own name, which is why this table never grows.
const LEGACY_IDS: &[(&str, &str)] = &[
    ("abs_f32_signal", "abs"),
    ("abs_f32_value", "abs_value"),
    ("add_f32_signal", "add"),
    ("add_f32_value", "add_value"),
    ("ceil_f32_i32_value", "ceil_to_i32"),
    ("ceil_f32_signal", "ceil"),
    ("ceil_f32_value", "ceil_value"),
    ("clamp_f32_signal", "clamp"),
    ("clamp_f32_value", "clamp_value"),
    ("differentiate_f32_signal", "differentiate"),
    ("div_f32_signal", "div"),
    ("div_f32_value", "div_value"),
    ("floor_f32_i32_value", "floor_to_i32"),
    ("floor_f32_signal", "floor"),
    ("floor_f32_value", "floor_value"),
    ("integrate_f32_signal", "integrate"),
    ("map_f32_signal", "map"),
    ("map_f32_value", "map_value"),
    ("max_f32_signal", "max"),
    ("max_f32_value", "max_value"),
    ("min_f32_signal", "min"),
    ("min_f32_value", "min_value"),
    ("modulo_f32_signal", "modulo"),
    ("modulo_f32_value", "modulo_value"),
    ("mul_f32_signal", "mul"),
    ("mul_f32_value", "mul_value"),
    ("negate_f32_signal", "negate"),
    ("negate_f32_value", "negate_value"),
    ("power_f32_signal", "power"),
    ("power_f32_value", "power_value"),
    ("reciprocal_f32_signal", "reciprocal"),
    ("reciprocal_f32_value", "reciprocal_value"),
    ("round_f32_i32_value", "round_to_i32"),
    ("round_f32_signal", "round"),
    ("round_f32_value", "round_value"),
    ("sub_f32_signal", "sub"),
    ("sub_f32_value", "sub_value"),
    ("trunc_f32_i32_value", "trunc_to_i32"),
    ("trunc_f32_signal", "trunc"),
    ("trunc_f32_value", "trunc_value"),
];

/// Every micro-bench kind: the registry's operators plus the bench-only [`OVERHEAD`] and
/// [`VARIANTS`] cases, byte-sorted.
pub fn kinds(registry: &Registry) -> Vec<&'static str> {
    let mut kinds: Vec<&'static str> = registry
        .type_names()
        .chain([OVERHEAD])
        .chain(VARIANTS.iter().map(|(variant, _)| *variant))
        .collect();
    kinds.sort_unstable();
    kinds.dedup();
    kinds
}

/// Rust keywords in any edition, strict and reserved. A bench id is an identifier in
/// `#[bench::<id>]`; `r#<kw>` compiles there, but gungraun then names the callgrind case `r#<kw>`.
const KEYWORDS: &[&str] = &[
    "abstract", "as", "async", "await", "become", "box", "break", "const", "continue", "crate",
    "do", "dyn", "else", "enum", "extern", "false", "final", "fn", "for", "gen", "if", "impl",
    "in", "let", "loop", "macro", "match", "mod", "move", "mut", "override", "priv", "pub", "ref",
    "return", "self", "static", "struct", "super", "trait", "true", "try", "type", "typeof",
    "unsafe", "unsized", "use", "virtual", "where", "while", "yield",
];

/// The bench id `kind` is gated under: its legacy id, else the kind itself, else — for a kind that
/// is a Rust keyword — the kind with an `_op` suffix.
pub fn bench_id(kind: &str) -> String {
    if let Some((_, id)) = LEGACY_IDS.iter().find(|(k, _)| *k == kind) {
        return String::from(*id);
    }
    let mut id = String::from(kind);
    if KEYWORDS.contains(&kind) {
        id.push_str("_op");
    }
    id
}

/// The kinds file's text. Lines starting with `#` are comments; every other line is one kind.
pub fn render_kinds_file(kinds: &[&str]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# Generated by `{REGENERATE}`. Do not hand-edit.");
    let _ = writeln!(
        out,
        "# Every kind the micro bench layer benches, one per line; the perf gate reads it."
    );
    for kind in kinds {
        let _ = writeln!(out, "{kind}");
    }
    out
}

/// The kinds a kinds file lists, in file order.
pub fn parse_kinds_file(text: &str) -> Vec<&str> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect()
}

/// The `micro_bench_ops! { … }` invocation for `kinds`, one `<id> => "<kind>",` line each.
pub fn render_iai_census(kinds: &[&str]) -> String {
    let mut out = String::from("micro_bench_ops! {\n");
    for kind in kinds {
        let _ = writeln!(out, "    {} => \"{kind}\",", bench_id(kind));
    }
    out.push_str("}\n");
    out
}

/// `src` with its `micro_bench_ops! {` block — from that line through the next line that starts
/// with `}` — replaced by `census`. `None` if `src` has no complete block.
pub fn splice_iai_census(src: &str, census: &str) -> Option<String> {
    let start = line_start(src, "micro_bench_ops! {")?;
    let close = start + line_start(&src[start..], "}")?;
    let end = src[close..].find('\n').map_or(src.len(), |i| close + i + 1);
    let mut out = String::with_capacity(src.len());
    out.push_str(&src[..start]);
    out.push_str(census);
    out.push_str(&src[end..]);
    Some(out)
}

/// Byte offset of the first line of `src` that starts with `prefix`.
fn line_start(src: &str, prefix: &str) -> Option<usize> {
    let mut offset = 0;
    for line in src.split_inclusive('\n') {
        if line.starts_with(prefix) {
            return Some(offset);
        }
        offset += line.len();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bench_only_kinds_are_never_registered() {
        let registry = Registry::builtin();
        assert!(
            registry.get(OVERHEAD).is_none(),
            "`{OVERHEAD}` is bench-only and must never be a registered operator — registering it \
             would leak it into the schema and patchable graphs"
        );
        for (variant, op) in VARIANTS {
            assert!(
                registry.get(op).is_some() && registry.get(variant).is_none(),
                "variant {variant:?} must drive a registered operator ({op:?}) under a name the \
                 registry does not claim"
            );
        }
    }

    /// A legacy id for a kind that no longer exists is harmless, but two kinds sharing an id
    /// would collide as callgrind cases.
    #[test]
    fn bench_ids_are_unique() {
        let kinds = kinds(&Registry::builtin());
        let mut ids: Vec<String> = kinds.iter().map(|k| bench_id(k)).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len(), "two micro-bench kinds share a bench id");
    }

    #[test]
    fn a_keyword_kind_gets_an_identifier_id() {
        assert_eq!(bench_id("loop"), "loop_op");
        assert_eq!(bench_id("self"), "self_op");
        assert_eq!(bench_id("looper"), "looper");
        assert_eq!(bench_id("abs_f32_signal"), "abs");
        assert_eq!(
            render_iai_census(&["match"]),
            "micro_bench_ops! {\n    match_op => \"match\",\n}\n"
        );
    }

    #[test]
    fn splice_replaces_exactly_the_block() {
        let src =
            "// micro_bench_ops! { in prose\nmicro_bench_ops! {\n    a => \"a\",\n}\n\nafter!();\n";
        let out = splice_iai_census(src, "micro_bench_ops! {\n    b => \"b\",\n}\n").unwrap();
        assert_eq!(
            out,
            "// micro_bench_ops! { in prose\nmicro_bench_ops! {\n    b => \"b\",\n}\n\nafter!();\n"
        );
        assert_eq!(splice_iai_census("no block here\n", ""), None);
    }

    #[test]
    fn kinds_file_round_trips() {
        let kinds = ["a_kind", "b_kind"];
        assert_eq!(parse_kinds_file(&render_kinds_file(&kinds)), kinds);
    }
}
