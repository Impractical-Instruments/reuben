//! `scaffold-operator` (ADR-0021): generate a new Operator's Rust skeleton and wire its
//! registration sites from a contract spec.
//!
//! The deterministic, error-prone half of authoring an Operator is mechanical: a new file in
//! `operators/`, plus sorted inserts into `operators/mod.rs`. Registration itself is compile-time
//! and self-contained (ADR-0024): the generated file carries its own `register_operator!` line,
//! so the scaffold no longer edits `registry.rs`. Like the `describe`/`validate` introspection
//! (ADR-0020), this lives as pure functions over source **text** — the binary does the filesystem
//! I/O around them — so the tricky sorted-insertion logic is tested directly.
//!
//! The contract itself (ports/params) is emitted as a single `operator_contract!` call (ADR-0025):
//! the scaffold no longer writes a hand const block *and* a `Descriptor` literal that could drift.
//! The spec types and the validator are shared with that macro via the `reuben-contract` crate —
//! one validator, not a scaffold copy and a macro copy that could themselves diverge.

use std::path::Path;
use std::process::Command;

use reuben_contract::naming::{screaming, struct_name};
use reuben_contract::{validate, LaneSpec, OperatorSpec, ParamSpec, PortSpec};
use serde::Serialize;

/// The current contents of the registration file the scaffold must edit (`operators/mod.rs`).
pub struct ScaffoldInputs<'a> {
    pub mod_rs: &'a str,
}

/// What the scaffold produced: the new operator file plus the edited `mod.rs`. Registration is
/// in the operator file itself (`register_operator!`, ADR-0024), so `registry.rs` is untouched.
#[derive(Debug)]
pub struct ScaffoldOutputs {
    /// File stem (also the module name), e.g. `"my_op"` — the binary writes `operators/<stem>.rs`.
    pub file_stem: String,
    pub operator_rs: String,
    pub mod_rs: String,
}

/// Generate the skeleton + `mod.rs` edits for `spec`, given the current `mod.rs`.
pub fn scaffold(spec: &OperatorSpec, inputs: &ScaffoldInputs) -> Result<ScaffoldOutputs, String> {
    validate(spec).map_err(|e| e.to_string())?;
    let stem = spec.type_name.clone();
    let st = struct_name(&stem);

    let operator_rs = render_operator(spec);
    let mod_rs = edit_mod(inputs.mod_rs, &stem, &st)?;

    Ok(ScaffoldOutputs {
        file_stem: stem,
        operator_rs,
        mod_rs,
    })
}

/// What `run_scaffold` did: the file it created and the files it edited, for the agent loop.
#[derive(Debug, Serialize)]
pub struct ScaffoldReport {
    pub type_name: String,
    pub struct_name: String,
    pub created: String,
    pub edited: Vec<String>,
    /// Whether `cargo fmt` ran successfully over the touched crate.
    pub formatted: bool,
}

/// Read a contract spec from `spec_path`, generate the operator under `core_root`
/// (`crates/reuben-core/src`), and write the new operator file plus the edited `operators/mod.rs`
/// — refusing to clobber an existing operator file. The operator self-registers at compile time
/// (ADR-0024), so `registry.rs` is not touched. Best-effort `cargo fmt` finalises the re-emitted
/// `mod.rs` lists.
pub fn run_scaffold(spec_path: &Path, core_root: &Path) -> Result<ScaffoldReport, String> {
    let spec_text = std::fs::read_to_string(spec_path)
        .map_err(|e| format!("read spec {}: {e}", spec_path.display()))?;
    let spec: OperatorSpec =
        serde_json::from_str(&spec_text).map_err(|e| format!("parse spec: {e}"))?;

    let mod_path = core_root.join("operators/mod.rs");
    let mod_rs = std::fs::read_to_string(&mod_path)
        .map_err(|e| format!("read {}: {e}", mod_path.display()))?;

    let out = scaffold(&spec, &ScaffoldInputs { mod_rs: &mod_rs })?;

    let op_path = core_root.join(format!("operators/{}.rs", out.file_stem));
    if op_path.exists() {
        return Err(format!(
            "{} already exists — refusing to overwrite",
            op_path.display()
        ));
    }

    std::fs::write(&op_path, &out.operator_rs)
        .map_err(|e| format!("write {}: {e}", op_path.display()))?;
    std::fs::write(&mod_path, &out.mod_rs)
        .map_err(|e| format!("write {}: {e}", mod_path.display()))?;

    let formatted = Command::new("cargo")
        .args(["fmt", "-p", "reuben-core"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    Ok(ScaffoldReport {
        type_name: spec.type_name.clone(),
        struct_name: struct_name(&spec.type_name),
        created: op_path.display().to_string(),
        edited: vec![mod_path.display().to_string()],
        formatted,
    })
}

/// Insert `pub mod <stem>;` and `pub use <stem>::<St>;` into `operators/mod.rs`, each in sorted
/// position within its run of like lines. Errors if the module is already declared.
fn edit_mod(src: &str, stem: &str, st: &str) -> Result<String, String> {
    let src = insert_line_sorted(
        src,
        |l| l.strip_prefix("pub mod ").and_then(|r| r.strip_suffix(';')),
        stem,
        &format!("pub mod {stem};"),
        "module",
    )?;
    insert_line_sorted(
        &src,
        |l| {
            l.strip_prefix("pub use ")
                .and_then(|r| r.split("::").next())
        },
        stem,
        &format!("pub use {stem}::{st};"),
        "re-export",
    )
}

/// Insert a full `line` into the contiguous run of lines for which `key_of` returns a sort key,
/// keeping that run sorted. Errors if `new_key` is already a member (a duplicate registration).
fn insert_line_sorted(
    src: &str,
    key_of: impl Fn(&str) -> Option<&str>,
    new_key: &str,
    line: &str,
    what: &str,
) -> Result<String, String> {
    let mut lines: Vec<&str> = src.lines().collect();
    let mut last_member = None;
    let mut insert_at = None;
    for (i, l) in lines.iter().enumerate() {
        if let Some(key) = key_of(l.trim()) {
            if key == new_key {
                return Err(format!("{what} for {new_key:?} already exists"));
            }
            last_member = Some(i);
            if insert_at.is_none() && key > new_key {
                insert_at = Some(i);
            }
        }
    }
    let at = insert_at
        .or_else(|| last_member.map(|i| i + 1))
        .ok_or_else(|| format!("no existing {what} lines to sort against"))?;
    lines.insert(at, line);
    let mut out = lines.join("\n");
    if src.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

/// Signal-output port consts in signal-index order — the ports the silence stub writes. These
/// const names (`OUT_<NAME>`) are the same the `operator_contract!` macro emits, so the stub
/// references real consts.
fn signal_output_consts(spec: &OperatorSpec) -> Vec<String> {
    spec.outputs
        .iter()
        .filter(|p| !matches!(p.kind.as_str(), "message" | "context"))
        .map(|p| format!("OUT_{}", screaming(&p.name)))
        .collect()
}

/// Render the operator's source file from its contract.
fn render_operator(spec: &OperatorSpec) -> String {
    let name = &spec.type_name;
    let st = struct_name(name);
    let has_resources = !spec.resources.is_empty();

    let mut out = String::new();
    out.push_str(&format!(
        "//! {name} — TODO one-line description (ADR-0021 scaffold; fill in Stage B).\n\n"
    ));

    // Imports — pull in the bind_resources types only when there are resources. The contract
    // macro emits fully-qualified `Descriptor`/`Port`/etc paths, so the file needs only
    // `Descriptor` (for the `descriptor()` delegate's return type).
    if has_resources {
        out.push_str("use std::sync::Arc;\n\n");
    }
    out.push_str("use crate::descriptor::Descriptor;\n");
    out.push_str("use crate::operator::{Io, Operator};\n");
    if has_resources {
        out.push_str("use crate::resources::{ResolvedRefs, ResourceStore};\n");
    }
    out.push('\n');

    // The single-source contract (ADR-0025): the macro plants the IN_/OUT_/P_ index consts AND the
    // matching `Descriptor` from these same tokens, so name↔slot drift is impossible.
    out.push_str(
        "// Ports/params declared once (ADR-0025): the macro plants the IN_/OUT_/P_ index consts and the\n",
    );
    out.push_str("// matching `Descriptor` from one source, so they cannot drift.\n");
    out.push_str(&render_contract_call(spec));
    out.push('\n');

    // State struct — empty by default; Stage B adds per-Lane fields (reset in `spawn`).
    out.push_str("#[derive(Default)]\n");
    out.push_str(&format!(
        "pub struct {st} {{\n    // TODO Stage B: add per-Lane state fields here (reset on `spawn`).\n}}\n\n"
    ));
    out.push_str(&format!(
        "impl {st} {{\n    pub fn new() -> Self {{\n        Self::default()\n    }}\n}}\n\n"
    ));

    // impl Operator. `descriptor()` delegates to the macro-planted inherent `contract()` (ADR-0025).
    out.push_str(&format!("impl Operator for {st} {{\n"));
    out.push_str("    fn descriptor() -> Descriptor {\n        Self::contract()\n    }\n\n");
    out.push_str(&render_process(spec));
    out.push_str(
        "\n    fn spawn(&self) -> Box<dyn Operator> {\n        Box::new(Self::new())\n    }\n",
    );
    if has_resources {
        out.push_str(
            "\n    fn bind_resources(&mut self, _store: &Arc<ResourceStore>, _refs: &ResolvedRefs) {\n        // TODO Stage B: clone the Arc and resolve handles by slot name (ADR-0016).\n    }\n",
        );
    }
    out.push_str("}\n\n");

    // Compile-time self-registration (ADR-0024).
    out.push_str(&format!("crate::register_operator!({st});\n\n"));

    out.push_str(&render_test_module(spec));
    out
}

/// Render the `operator_contract!` invocation (ADR-0025) — the one declaration of the contract.
fn render_contract_call(spec: &OperatorSpec) -> String {
    let st = struct_name(&spec.type_name);
    let mut out = format!("crate::operator_contract!({st} {{\n");
    out.push_str(&format!("    type_name: {:?},\n", spec.type_name));
    if !spec.inputs.is_empty() {
        out.push_str(&format!(
            "    inputs: {{ {} }},\n",
            render_macro_ports(&spec.inputs)
        ));
    }
    if !spec.outputs.is_empty() {
        out.push_str(&format!(
            "    outputs: {{ {} }},\n",
            render_macro_ports(&spec.outputs)
        ));
    }
    if !spec.params.is_empty() {
        out.push_str("    params: {\n");
        for p in &spec.params {
            out.push_str(&format!("        {}\n", render_macro_param(p)));
        }
        out.push_str("    },\n");
    }
    if !spec.resources.is_empty() {
        let rs: Vec<&str> = spec.resources.iter().map(String::as_str).collect();
        out.push_str(&format!("    resources: {{ {} }},\n", rs.join(", ")));
    }
    out.push_str(&format!("    lanes: {},\n", render_macro_lanes(spec)));
    out.push_str("});\n");
    out
}

/// `name: kind, name: kind` — the macro's port-list body.
fn render_macro_ports(ports: &[PortSpec]) -> String {
    ports
        .iter()
        .map(|p| format!("{}: {}", p.name, p.kind))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `name: { MIN..=MAX, default D, "unit", curve },` — one param in the macro grammar.
fn render_macro_param(p: &ParamSpec) -> String {
    let curve = if p.curve == "exponential" {
        "exp"
    } else {
        "lin"
    };
    format!(
        "{}: {{ {:?}..={:?}, default {:?}, {:?}, {} }},",
        p.name, p.min, p.max, p.default, p.unit, curve
    )
}

/// `inherit` or `from_param(<param>)`.
fn render_macro_lanes(spec: &OperatorSpec) -> String {
    match &spec.lanes {
        LaneSpec::Inherit => "inherit".to_string(),
        LaneSpec::FromParam(name) => format!("from_param({name})"),
    }
}

/// The `process` stub — fills every signal output with silence (a valid, silent operator).
fn render_process(spec: &OperatorSpec) -> String {
    let mut out = String::from("    fn process(&mut self, io: &mut Io) {\n");
    out.push_str(
        "        // TODO Stage B: implement the DSP. This stub writes silence — the operator is\n        // structurally valid but makes no sound until you fill it in (ADR-0021).\n",
    );
    let sig_outs = signal_output_consts(spec);
    if sig_outs.is_empty() {
        out.push_str("        let _ = io;\n");
    } else {
        out.push_str("        let n = io.frames();\n");
        out.push_str(&format!(
            "        for port in [{}] {{\n",
            sig_outs.join(", ")
        ));
        out.push_str("            io.output(port)[..n].fill(0.0);\n        }\n");
    }
    out.push_str("    }\n");
    out
}

/// The `#[cfg(test)]` module — an `Io::new` harness plus one intentionally-failing placeholder so
/// the author starts Stage B red (ADR-0021).
fn render_test_module(spec: &OperatorSpec) -> String {
    let name = &spec.type_name;
    format!(
        "#[cfg(test)]\nmod tests {{\n    // These imports are unused until Stage B fills in the real test below.\n    #[allow(unused_imports)]\n    use super::*;\n    #[allow(unused_imports)]\n    use crate::operator::Io;\n\n    const SR: f32 = 48_000.0;\n\n    #[test]\n    #[allow(non_snake_case)]\n    fn TODO_{name}_meets_its_contract() {{\n        // Stage B (ADR-0021): replace this with the real behavioral oracle from the\n        // contract — drive `process` via `Io::new` (see `lfo.rs` for the pattern) and assert\n        // observable output. The scaffold ships this red on purpose.\n        let _sr = SR;\n        panic!(\"create-operator: implement the `{name}` behavior test-first (ADR-0021)\");\n    }}\n}}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(json: &str) -> OperatorSpec {
        serde_json::from_str(json).expect("valid spec")
    }

    // The real `mod.rs`, compiled in — the strongest fixture for the sorted inserts.
    const REAL_MOD: &str = include_str!("../../reuben-core/src/operators/mod.rs");

    fn scaffold_real(json: &str) -> ScaffoldOutputs {
        scaffold(&spec(json), &ScaffoldInputs { mod_rs: REAL_MOD })
            .expect("scaffold against real files")
    }

    fn render(json: &str) -> String {
        scaffold_real(json).operator_rs
    }

    #[test]
    fn renders_an_operator_file_that_delegates_to_the_contract() {
        let src = render(r#"{ "type_name": "my_op" }"#);
        // The contract is declared once via the macro, and descriptor() delegates to it (ADR-0025).
        assert!(
            src.contains("crate::operator_contract!(MyOp {"),
            "should emit the contract macro call:\n{src}"
        );
        assert!(
            src.contains("type_name: \"my_op\""),
            "contract should carry the type name:\n{src}"
        );
        assert!(
            src.contains("fn descriptor() -> Descriptor {\n        Self::contract()\n    }"),
            "descriptor() should delegate to the macro-planted contract():\n{src}"
        );
        assert!(
            src.contains("impl Operator for MyOp"),
            "struct name should be PascalCase of the type:\n{src}"
        );
    }

    #[test]
    fn ports_are_declared_in_the_contract_call_by_kind() {
        // Per-kind ordinals are now the macro's job; the scaffold just declares the ports by kind.
        let src = render(
            r#"{ "type_name": "v",
                 "inputs": [ {"name":"notes","kind":"message"}, {"name":"ctx","kind":"context"} ],
                 "outputs": [ {"name":"freq","kind":"signal"}, {"name":"gate","kind":"signal"} ] }"#,
        );
        assert!(
            src.contains("inputs: { notes: message, ctx: context }"),
            "{src}"
        );
        assert!(
            src.contains("outputs: { freq: signal, gate: signal }"),
            "{src}"
        );
    }

    #[test]
    fn params_render_in_the_macro_grammar_with_curve() {
        let src = render(
            r#"{ "type_name": "lfoish",
                 "params": [ {"name":"rate","min":0.01,"max":20.0,"default":5.0,"unit":"Hz","curve":"exponential"} ] }"#,
        );
        assert!(
            src.contains(r#"rate: { 0.01..=20.0, default 5.0, "Hz", exp },"#),
            "{src}"
        );
    }

    #[test]
    fn process_stub_writes_silence_to_signal_outputs_only() {
        let src =
            render(r#"{ "type_name": "o", "outputs": [ {"name":"audio","kind":"signal"} ] }"#);
        assert!(src.contains("io.output(port)[..n].fill(0.0)"), "{src}");
        assert!(src.contains("for port in [OUT_AUDIO]"), "{src}");
    }

    #[test]
    fn lane_expander_renders_from_param() {
        let src = render(
            r#"{ "type_name": "vox",
                 "params": [ {"name":"voices","min":1.0,"max":16.0,"default":4.0} ],
                 "lanes": { "from_param": "voices" } }"#,
        );
        assert!(src.contains("lanes: from_param(voices),"), "{src}");
    }

    #[test]
    fn resources_render_in_the_contract_and_pull_in_bind_resources() {
        let src = render(r#"{ "type_name": "smp", "resources": ["wave"] }"#);
        assert!(src.contains("resources: { wave },"), "{src}");
        assert!(src.contains("fn bind_resources("), "{src}");
        assert!(src.contains("use std::sync::Arc;"), "{src}");
    }

    #[test]
    fn ships_an_intentionally_red_placeholder_test() {
        let src = render(r#"{ "type_name": "my_op" }"#);
        assert!(src.contains("fn TODO_my_op_meets_its_contract()"), "{src}");
        assert!(
            src.contains("panic!(\"create-operator: implement the `my_op`"),
            "placeholder must start red:\n{src}"
        );
    }

    #[test]
    fn wires_mod_rs_sorted() {
        // "tremolo" sorts between "snap" and "voicer".
        let out = scaffold_real(r#"{ "type_name": "tremolo" }"#);
        assert!(out.mod_rs.contains("pub mod tremolo;"), "{}", out.mod_rs);
        assert!(
            out.mod_rs.contains("pub use tremolo::Tremolo;"),
            "{}",
            out.mod_rs
        );
        let mods: Vec<&str> = out
            .mod_rs
            .lines()
            .filter_map(|l| {
                l.trim()
                    .strip_prefix("pub mod ")
                    .and_then(|r| r.strip_suffix(';'))
            })
            .collect();
        let mut sorted = mods.clone();
        sorted.sort();
        assert_eq!(mods, sorted, "pub mod run must stay sorted: {mods:?}");
        assert!(
            mods.windows(2).all(|w| w[0] != w[1]),
            "no duplicate modules: {mods:?}"
        );
    }

    #[test]
    fn render_emits_self_registration() {
        // The operator wires itself in (ADR-0024): the generated file carries its own
        // `register_operator!` call — there is no registry.rs edit to make.
        let out = scaffold_real(r#"{ "type_name": "tremolo" }"#);
        assert!(
            out.operator_rs
                .contains("crate::register_operator!(Tremolo);"),
            "generated file must self-register:\n{}",
            out.operator_rs
        );
    }

    fn scaffold_err(json: &str) -> String {
        scaffold(&spec(json), &ScaffoldInputs { mod_rs: REAL_MOD })
            .expect_err("spec should be rejected")
    }

    #[test]
    fn rejects_non_snake_case_type_name() {
        assert!(scaffold_err(r#"{ "type_name": "MyOp" }"#).contains("snake_case"));
        assert!(scaffold_err(r#"{ "type_name": "2cool" }"#).contains("snake_case"));
        assert!(scaffold_err(r#"{ "type_name": "" }"#).contains("empty"));
    }

    #[test]
    fn rejects_bad_port_kind_and_curve() {
        let bad_kind = r#"{ "type_name": "x", "inputs": [ {"name":"a","kind":"audio"} ] }"#;
        assert!(scaffold_err(bad_kind).contains("kind"));
        let bad_curve = r#"{ "type_name": "x", "params": [ {"name":"a","min":0,"max":1,"default":0,"curve":"log"} ] }"#;
        assert!(scaffold_err(bad_curve).contains("curve"));
    }

    #[test]
    fn rejects_inverted_range_and_out_of_range_default() {
        let inverted =
            r#"{ "type_name": "x", "params": [ {"name":"a","min":1,"max":0,"default":0} ] }"#;
        assert!(scaffold_err(inverted).contains("min"));
        let oob = r#"{ "type_name": "x", "params": [ {"name":"a","min":0,"max":1,"default":5} ] }"#;
        assert!(scaffold_err(oob).contains("outside"));
    }

    #[test]
    fn rejects_duplicate_names_and_dangling_lane_param() {
        let dup = r#"{ "type_name": "x", "inputs": [ {"name":"a","kind":"signal"}, {"name":"a","kind":"signal"} ] }"#;
        assert!(scaffold_err(dup).contains("duplicate"));
        let dangling = r#"{ "type_name": "x", "lanes": { "from_param": "voices" } }"#;
        assert!(scaffold_err(dangling).contains("from_param"));
    }

    #[test]
    fn refuses_to_register_a_duplicate_type() {
        // "reverb" already has a module in mod.rs — the sorted insert must reject it rather than
        // emit a second `pub mod reverb;` (which would then fail to compile).
        let err = scaffold(
            &spec(r#"{ "type_name": "reverb" }"#),
            &ScaffoldInputs { mod_rs: REAL_MOD },
        )
        .unwrap_err();
        assert!(
            err.contains("reverb") || err.to_lowercase().contains("already"),
            "duplicate should be rejected: {err}"
        );
    }
}
