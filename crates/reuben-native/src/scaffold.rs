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
use reuben_contract::{validate, Curve, F32Meta, OperatorSpec, PortSpec, PortTy};
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
        // Only a `f32_buffer` output carries a per-sample buffer the silence stub zeroes (ADR-0030);
        // `note`/`harmony`/`enum`/`f32` outputs do not.
        .filter(|p| matches!(p.ty, PortTy::F32Buffer(_)))
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

    // The single-source contract (ADR-0025): the macro plants the IN_/OUT_/C_ index consts AND the
    // matching `Descriptor` from these same tokens, so name↔slot drift is impossible.
    out.push_str(
        "// Single-source contract (ADR-0025): one declaration -> IN_/OUT_/C_ consts + Descriptor, no drift.\n",
    );
    out.push_str(&render_contract_call(spec));
    out.push('\n');

    // State struct — empty by default; Stage B adds per-voice state fields (reset in `spawn`).
    out.push_str("#[derive(Default)]\n");
    out.push_str(&format!(
        "pub struct {st} {{\n    // TODO Stage B: add per-voice state fields here (reset on `spawn`).\n}}\n\n"
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
    if !spec.constants.is_empty() {
        out.push_str(&format!(
            "    constants: {{ {} }},\n",
            render_macro_ports(&spec.constants)
        ));
    }
    if !spec.resources.is_empty() {
        let rs: Vec<&str> = spec.resources.iter().map(String::as_str).collect();
        out.push_str(&format!("    resources: {{ {} }},\n", rs.join(", ")));
    }
    out.push_str("});\n");
    out
}

/// The macro's port-list body. Each port renders by its [`Arg`] type (ADR-0030): `f32_buffer`,
/// `f32 { .. }`, `enum(VocabType)`, `note`, `harmony`, or `arg`. Mirrors the
/// `operator_contract!` grammar exactly.
fn render_macro_ports(ports: &[PortSpec]) -> String {
    ports
        .iter()
        .map(render_macro_port)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The `{ LO..=HI, default D, "unit", curve }` block of a meta-carrying port, in macro grammar —
/// shared by the `f32` and `f32_buffer { .. }` arms of [`render_macro_port`].
fn render_f32_meta(m: &F32Meta) -> String {
    // Exhaustive, so a future `Curve` variant is a compile error here rather than silently
    // rendering as `lin`.
    let curve = match m.curve {
        Curve::Exponential => "exp",
        Curve::Linear => "lin",
    };
    format!(
        "{{ {:?}..={:?}, default {:?}, {:?}, {} }}",
        m.min, m.max, m.default, m.unit, curve
    )
}

/// One port in the macro grammar — see [`render_macro_ports`]. Exhaustive over the shared
/// [`PortTy`] (issue #217). (The stringly-era renderer fell a meta-carrying `f32_buffer`
/// through the bare-type arm, silently dropping its declared default/range from the generated
/// contract; the enum's payload makes that unrepresentable.)
fn render_macro_port(p: &PortSpec) -> String {
    match &p.ty {
        // A materialized scalar control carries its `{ .. }` meta.
        PortTy::F32(m) => format!("{}: f32 {}", p.name, render_f32_meta(m)),
        // A signal port with a scalar default + knob range (ADR-0031 decision (a)).
        PortTy::F32Buffer(Some(m)) => format!("{}: f32_buffer {}", p.name, render_f32_meta(m)),
        PortTy::F32Buffer(None) => format!("{}: f32_buffer", p.name),
        // A bounded integer control / constant carries its integer `{ .. }` meta (ADR-0035).
        PortTy::I32(m) => format!(
            "{}: i32 {{ {}..={}, default {} }}",
            p.name, m.min, m.max, m.default
        ),
        // A held vocab enum names its shared vocab type.
        PortTy::Enum(vocab) => format!("{}: enum({})", p.name, vocab),
        // `note` / `harmony` / `pitch` / `arg` need no extra syntax.
        PortTy::Note => format!("{}: note", p.name),
        PortTy::Harmony => format!("{}: harmony", p.name),
        PortTy::Pitch => format!("{}: pitch", p.name),
        PortTy::Arg => format!("{}: arg", p.name),
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
        out.push_str("            io.write(port)[..n].fill(0.0);\n        }\n");
    }
    out.push_str("    }\n");
    out
}

/// The `#[cfg(test)]` module — an `OpDriver` harness plus one intentionally-failing placeholder so
/// the author starts Stage B red (ADR-0021). `OpDriver` drives the operator through the *real*
/// engine seeding + per-node step, so the test surface can't drift from production.
fn render_test_module(spec: &OperatorSpec) -> String {
    let name = &spec.type_name;
    let st = struct_name(name);
    format!(
        "#[cfg(test)]\nmod tests {{\n    // These imports are unused until Stage B fills in the real test below.\n    #[allow(unused_imports)]\n    use super::*;\n    #[allow(unused_imports)]\n    use crate::op_driver::OpDriver;\n\n    const SR: f32 = 48_000.0;\n\n    #[test]\n    #[allow(non_snake_case)]\n    fn TODO_{name}_meets_its_contract() {{\n        // Stage B (ADR-0021): replace this with the real behavioral oracle from the\n        // contract. Drive `{st}` through the real engine with `OpDriver` — by-const port\n        // addressing, looped production-size blocks (see `lfo.rs` for the canonical pattern):\n        //\n        //     let out = OpDriver::for_type({st}::new(), SR)\n        //         .set(IN_PORT, value) // held scalar or constant audio-in (sticky/ZOH)\n        //         .render(n)           // ceil(n/block) real per-node steps\n        //         .output(OUT_PORT)    // a signal output (use `.emits()` for events)\n        //         .to_vec();\n        //\n        // then assert on observable output. The scaffold ships this red on purpose.\n        let _sr = SR;\n        panic!(\"create-operator: implement the `{name}` behavior test-first (ADR-0021)\");\n    }}\n}}\n"
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
    fn ports_are_declared_in_the_contract_call_by_type() {
        // Ordinals are the macro's job; the scaffold just declares each port by its Arg type
        // (ADR-0030): `note` / `harmony` / `f32_buffer`.
        let src = render(
            r#"{ "type_name": "v",
                 "inputs": [ {"name":"notes","ty":"note"}, {"name":"ctx","ty":"harmony"} ],
                 "outputs": [ {"name":"freq","ty":"f32_buffer"}, {"name":"gate","ty":"f32_buffer"} ] }"#,
        );
        assert!(
            src.contains("inputs: { notes: note, ctx: harmony }"),
            "{src}"
        );
        assert!(
            src.contains("outputs: { freq: f32_buffer, gate: f32_buffer }"),
            "{src}"
        );
    }

    #[test]
    fn f32_inputs_render_in_the_macro_grammar_with_curve() {
        let src = render(
            r#"{ "type_name": "lfoish",
                 "inputs": [ {"name":"rate","ty":"f32","f32":{"min":0.01,"max":20.0,"default":5.0,"unit":"Hz","curve":"exponential"}} ] }"#,
        );
        assert!(
            src.contains(r#"rate: f32 { 0.01..=20.0, default 5.0, "Hz", exp }"#),
            "{src}"
        );
    }

    #[test]
    fn process_stub_writes_silence_to_signal_outputs_only() {
        let src =
            render(r#"{ "type_name": "o", "outputs": [ {"name":"audio","ty":"f32_buffer"} ] }"#);
        assert!(src.contains("io.write(port)[..n].fill(0.0)"), "{src}");
        assert!(src.contains("for port in [OUT_AUDIO]"), "{src}");
    }

    #[test]
    fn renders_typed_ports() {
        // The contract surface declares each port by its Arg type (ADR-0030): a `f32_buffer` wire, a
        // materialized `f32 { .. }` with a default, and an `enum(VocabType)` naming a shared vocab
        // — each must render in macro grammar.
        let src = render(
            r#"{ "type_name": "f",
                 "inputs": [ {"name":"audio","ty":"f32_buffer"},
                             {"name":"cutoff","ty":"f32",
                              "f32":{"min":20.0,"max":20000.0,"default":1000.0,"unit":"Hz","curve":"exponential"}},
                             {"name":"mode","ty":"enum","vocab":"FilterMode"} ],
                 "outputs": [ {"name":"audio","ty":"f32_buffer"} ] }"#,
        );
        assert!(
            src.contains(
                r#"inputs: { audio: f32_buffer, cutoff: f32 { 20.0..=20000.0, default 1000.0, "Hz", exp }, mode: enum(FilterMode) }"#
            ),
            "{src}"
        );
        assert!(src.contains("outputs: { audio: f32_buffer }"), "{src}");
        // A `f32_buffer` output gets a silence-stub write.
        assert!(src.contains("for port in [OUT_AUDIO]"), "{src}");
    }

    #[test]
    fn constant_renders_in_the_contract() {
        let src = render(
            r#"{ "type_name": "vox",
                 "constants": [ {"name":"voices","ty":"i32","i32":{"min":1,"max":16,"default":4}} ] }"#,
        );
        assert!(
            src.contains("constants: { voices: i32 { 1..=16, default 4 } },"),
            "{src}"
        );
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
        // The test surface is the real engine via `OpDriver`, never a hand-rolled `Io::new`
        // (the third-impl drift this whole effort retired).
        assert!(
            src.contains("use crate::op_driver::OpDriver;")
                && src.contains("OpDriver::for_type(MyOp::new(), SR)"),
            "placeholder must drive through OpDriver, not Io::new:\n{src}"
        );
        assert!(
            !src.contains("Io::new"),
            "scaffold must not emit a hand-rolled Io::new harness:\n{src}"
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
    fn wires_mod_rs_sorted_after_the_last_member() {
        // "zzz_op" sorts after every existing module, so `insert_at` stays `None` and the
        // end-of-run fallback (`last_member + 1`) decides the position — a classic
        // off-by-one site: dropping the `+ 1` would insert *before* the current last
        // member, and `run_scaffold` would then write the unsorted run into the real
        // `operators/mod.rs`. The mid-run insertion in `wires_mod_rs_sorted` never
        // reaches this branch.
        let out = scaffold_real(r#"{ "type_name": "zzz_op" }"#);
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
        assert_eq!(
            mods.last(),
            Some(&"zzz_op"),
            "the new module must land at the end of the run: {mods:?}"
        );
        // Same fallback, independently, for the `pub use` run.
        let uses: Vec<&str> = out
            .mod_rs
            .lines()
            .filter_map(|l| {
                l.trim()
                    .strip_prefix("pub use ")
                    .and_then(|r| r.split("::").next())
            })
            .collect();
        let mut sorted_uses = uses.clone();
        sorted_uses.sort();
        assert_eq!(uses, sorted_uses, "pub use run must stay sorted: {uses:?}");
        assert_eq!(
            uses.last(),
            Some(&"zzz_op"),
            "the new re-export must land at the end of the run: {uses:?}"
        );
    }

    #[test]
    fn errors_when_mod_rs_has_no_module_run() {
        // A mod.rs with no `pub mod` lines gives `insert_line_sorted` nothing to sort
        // against: it must refuse loudly rather than guess a position — `run_scaffold`
        // writes the result straight into real source, so a silent bad insert corrupts
        // `operators/mod.rs`.
        let err = scaffold(
            &spec(r#"{ "type_name": "x_op" }"#),
            &ScaffoldInputs {
                mod_rs: "// empty\n",
            },
        )
        .unwrap_err();
        assert!(
            err.contains("no existing module lines"),
            "expected the empty-run error, got: {err}"
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
    fn rejects_bad_port_type_and_curve() {
        // Both now fail at the JSON parse (issue #217): the port type and the curve are enums,
        // so `run_scaffold`'s deserialize rejects them before `validate()` ever runs.
        let bad_type = r#"{ "type_name": "x", "inputs": [ {"name":"a","ty":"audio"} ] }"#;
        let e = serde_json::from_str::<OperatorSpec>(bad_type)
            .expect_err("unknown port type must fail at deserialize");
        assert!(e.to_string().contains("type"), "{e}");
        let bad_curve = r#"{ "type_name": "x", "inputs": [ {"name":"a","ty":"f32","f32":{"min":0,"max":1,"default":0,"curve":"log"}} ] }"#;
        let e = serde_json::from_str::<OperatorSpec>(bad_curve)
            .expect_err("unknown curve must fail at deserialize");
        let msg = e.to_string();
        assert!(
            msg.contains("linear") && msg.contains("exponential"),
            "error must name the legal curves: {msg}"
        );
    }

    #[test]
    fn rejects_inverted_range_and_out_of_range_default() {
        let inverted = r#"{ "type_name": "x", "constants": [ {"name":"a","ty":"i32","i32":{"min":1,"max":0,"default":0}} ] }"#;
        assert!(scaffold_err(inverted).contains("min"));
        let oob = r#"{ "type_name": "x", "constants": [ {"name":"a","ty":"i32","i32":{"min":0,"max":1,"default":5}} ] }"#;
        assert!(scaffold_err(oob).contains("outside"));
    }

    #[test]
    fn rejects_duplicate_input_and_constant_names() {
        let dup = r#"{ "type_name": "x", "inputs": [ {"name":"a","ty":"f32_buffer"}, {"name":"a","ty":"f32_buffer"} ] }"#;
        assert!(scaffold_err(dup).contains("duplicate"));
        let dup_const = r#"{ "type_name": "x", "constants": [ {"name":"v","ty":"i32","i32":{"min":1,"max":4,"default":2}}, {"name":"v","ty":"i32","i32":{"min":1,"max":8,"default":4}} ] }"#;
        assert!(scaffold_err(dup_const).contains("duplicate"));
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
