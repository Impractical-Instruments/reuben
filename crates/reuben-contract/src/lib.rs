//! reuben-contract — the single source of an operator's port/constant contract (ADR-0025, ADR-0030).
//!
//! Every operator declares its ports and constants **once**. Two consumers turn that one
//! declaration into code: the [`operator_contract!`](../reuben_macros) proc-macro (which emits
//! the `IN_`/`OUT_`/`C_` index consts and the `Descriptor`) and the
//! [`scaffold`](../reuben_native) (which emits the macro call for a brand-new operator). Both
//! must agree on what a *valid* contract is and how names map to consts, so that shared logic —
//! the spec types, the naming rules, and [`validate`] — lives here, in a crate both depend on.
//! Putting it anywhere else would re-create the very drift this layer exists to remove.
//!
//! A port carries an **[`Arg`](reuben_core::message::Arg) type** (ADR-0030, ADR-0035), named by
//! [`PortSpec::ty`]: `f32_buffer` (a dense per-sample signal), `f32` (a materialized scalar control
//! with a `{ .. }` meta block), `i32` (a bounded integer control / constant), `enum` (a held vocab
//! enum, naming its shared `vocab` type), `note`, `harmony`, or `arg` (the type-agnostic
//! pass-through, issue #141). The retired `Shape`/legacy-`kind` two-surface world is gone.

use serde::Deserialize;

pub mod naming;

/// The type-wide default range for a `number` operand — the **one** definition of the `±1e6`
/// sentinel both macros reference (issue #127). It is *descriptor metadata* (a control-surface fader
/// span and a loader validation/clamp bound), **not** a numeric type bound: `f32::MAX` (`3.4e38`) is
/// deliberately not used, because you can't sweep a knob across it, exponential curve-mapping over it
/// is meaningless, and a `default` that must validate inside `[min, max]` near `f32::MAX` invites
/// `inf`/`NaN`. `±1e6` is the "effectively unbounded but still finite and knob-able" value.
///
/// Both `operator_contract!` and `number_operator_contract!` expose this as the `min`/`max` grammar
/// sentinel (in a range endpoint and in `default`), so no operator contract repeats the literal.
pub const NUMBER_MIN: f32 = -1_000_000.0;
/// The upper half of the type-wide default range. See [`NUMBER_MIN`].
pub const NUMBER_MAX: f32 = 1_000_000.0;

/// The `{ min, max, default, unit, curve }` block on a `f32` port (ADR-0030): its unwired
/// default, range, and display metadata. Required on a `f32` port (a bare per-sample wire is
/// `f32_buffer`, not `f32`).
#[derive(Debug, Clone, Deserialize)]
pub struct F32Meta {
    pub min: f32,
    pub max: f32,
    pub default: f32,
    #[serde(default)]
    pub unit: String,
    #[serde(default = "default_curve")]
    pub curve: String,
}

/// The `{ min, max, default }` block on an `i32` port (ADR-0035): a bounded integer control /
/// constant (a count like `voices`). No unit/curve — a count is not a swept knob.
#[derive(Debug, Clone, Deserialize)]
pub struct I32Meta {
    pub min: i32,
    pub max: i32,
    pub default: i32,
}

/// A port in the contract — carrying one [`Arg`](reuben_core::message::Arg) type (ADR-0030),
/// named by [`ty`](Self::ty). `f32` ports carry their `{ .. }` meta in [`f32`](Self::f32);
/// `enum` ports name their shared `vocab` type in [`vocab`](Self::vocab). All other types
/// (`f32_buffer`, `note`, `harmony`) need neither.
///
/// Kept as `String` fields (not enums) so the struct round-trips from the scaffold's JSON spec and
/// from the proc-macro's parsed tokens with no conversion.
#[derive(Debug, Clone, Deserialize)]
pub struct PortSpec {
    pub name: String,
    /// The port's [`Arg`](reuben_core::message::Arg) type: `f32_buffer` | `f32` | `enum` | `note` |
    /// `harmony` | `arg`.
    pub ty: String,
    /// `Some` for a `f32` port — its materialized default/range.
    #[serde(default)]
    pub f32: Option<F32Meta>,
    /// `Some` for an `i32` port (ADR-0035) — its bounded integer range/default.
    #[serde(default)]
    pub i32: Option<I32Meta>,
    /// `Some` for an `enum` port — the shared `vocab` enum type name (PascalCase, e.g.
    /// `"FilterMode"`); the descriptor reads its `VARIANTS`/default from `Type::enum_meta(name)`.
    #[serde(default)]
    pub vocab: Option<String>,
}

fn default_curve() -> String {
    "linear".to_string()
}

/// The contract for an Operator — one declaration of its ports, instantiate-time
/// [`Constant`s](reuben_core::descriptor::Descriptor::constants), and resources. The scaffold
/// hand-authors / deserializes it; the proc-macro parses it from `operator_contract!` syntax.
/// Mirrors a [`reuben_core::descriptor::Descriptor`].
#[derive(Debug, Clone, Deserialize)]
pub struct OperatorSpec {
    pub type_name: String,
    #[serde(default)]
    pub inputs: Vec<PortSpec>,
    #[serde(default)]
    pub outputs: Vec<PortSpec>,
    /// Instantiate-time **`Constant`** ports (ADR-0035) — plan-time config (e.g. a voicer's
    /// `voices`), each an immutable [`PortSpec`]. Empty for the common operator. Mirrors
    /// [`reuben_core::descriptor::Descriptor::constants`].
    #[serde(default)]
    pub constants: Vec<PortSpec>,
    #[serde(default)]
    pub resources: Vec<String>,
}

/// The legal port [`Arg`](reuben_core::message::Arg) types (ADR-0030, ADR-0035). Centralised so
/// both the scaffold and the macro reject the same set. `arg` is the type-agnostic pass-through
/// (issue #141): the port carries *any* Arg as a raw Event stream — the `osc_out` sink's input.
pub const PORT_TYPES: [&str; 7] = ["f32_buffer", "f32", "i32", "enum", "note", "harmony", "arg"];

/// Where in the spec a validation error sits, so the proc-macro can attach a source span to the
/// offending token. The scaffold ignores the locus and just formats the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locus {
    TypeName,
    Input(usize),
    Output(usize),
    Constant(usize),
}

/// A rejected contract: a human-readable reason plus where it lives.
#[derive(Debug, Clone)]
pub struct ContractError {
    pub locus: Locus,
    pub message: String,
}

impl ContractError {
    fn new(locus: Locus, message: impl Into<String>) -> Self {
        Self {
            locus,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ContractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// A non-empty snake_case identifier: a lowercase letter then `[a-z0-9_]`. This is the rule for
/// `type_name` and for every port/param name. Requiring it on names (not just `type_name`) keeps
/// `naming::screaming` injective — distinct names can never collapse to the same `IN_`/`OUT_`/`P_`
/// const — and guarantees the names the macro emits as tokens are valid Rust identifiers.
fn is_snake_case(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// A non-empty Rust type identifier: an ASCII letter then `[A-Za-z0-9_]`. The rule for the `vocab`
/// type an `enum` port names (`FilterMode`, `SnapDir`) — emitted as a path segment, so it must be a
/// valid identifier (PascalCase is conventional but not enforced here).
fn is_ident(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Validate one port's internal consistency (ADR-0030): `ty` legal; `f32` meta present and valid
/// **iff** `f32`; `vocab` type named and identifier-shaped **iff** `enum`.
fn validate_port(at: Locus, label: &str, p: &PortSpec) -> Result<(), ContractError> {
    if !PORT_TYPES.contains(&p.ty.as_str()) {
        return Err(ContractError::new(
            at,
            format!(
                "{label} {:?}: type {:?} must be one of {PORT_TYPES:?}",
                p.name, p.ty
            ),
        ));
    }
    // `arg` is **input-only** (issue #141): it is legal only where the operator treats the payload
    // as opaque — a pure carrier's inbound port. An `arg` output would put an untyped source on the
    // graph, and a typed input downstream of it would need plan-time type flow *through* the
    // carrier to recover the true source type — machinery no operator has earned. Fail closed here
    // (the one validator) until an in-graph carrier does.
    if p.ty == "arg" && label != "input" {
        return Err(ContractError::new(
            at,
            format!(
                "{label} {:?}: `arg` is input-only (the pass-through carries whatever its wired \
                 source declares; an `arg` {label} would have no type authority at all)",
                p.name
            ),
        ));
    }
    // A `{ .. }` meta block is **required** on `f32` (it's a scalar control) and **optional** on
    // `f32_buffer` (ADR-0031 decision (a): a signal port with a scalar default + knob, e.g.
    // `oscillator.freq`). No other port type may carry one.
    match (p.ty.as_str(), &p.f32) {
        ("f32", None) => {
            return Err(ContractError::new(
                at,
                format!(
                    "{label} {:?}: a `f32` port needs a {{ .. }} meta block",
                    p.name
                ),
            ));
        }
        ("f32" | "f32_buffer", Some(m)) => {
            if m.min > m.max {
                return Err(ContractError::new(
                    at,
                    format!("{label} {:?}: min {} > max {}", p.name, m.min, m.max),
                ));
            }
            if m.default < m.min || m.default > m.max {
                return Err(ContractError::new(
                    at,
                    format!(
                        "{label} {:?}: default {} outside [{}, {}]",
                        p.name, m.default, m.min, m.max
                    ),
                ));
            }
            if !matches!(m.curve.as_str(), "linear" | "exponential") {
                return Err(ContractError::new(
                    at,
                    format!(
                        "{label} {:?}: curve {:?} must be \"linear\" or \"exponential\"",
                        p.name, m.curve
                    ),
                ));
            }
        }
        (_, Some(_)) => {
            return Err(ContractError::new(
                at,
                format!(
                    "{label} {:?}: only a `f32` or `f32_buffer` port carries a {{ .. }} meta block",
                    p.name
                ),
            ));
        }
        (_, None) => {}
    }
    // An `i32` port (ADR-0035) needs an integer `{ .. }` meta block; no other type may carry one.
    match (p.ty.as_str(), &p.i32) {
        ("i32", None) => {
            return Err(ContractError::new(
                at,
                format!(
                    "{label} {:?}: an `i32` port needs a {{ .. }} meta block",
                    p.name
                ),
            ));
        }
        ("i32", Some(m)) => {
            if m.min > m.max {
                return Err(ContractError::new(
                    at,
                    format!("{label} {:?}: min {} > max {}", p.name, m.min, m.max),
                ));
            }
            if m.default < m.min || m.default > m.max {
                return Err(ContractError::new(
                    at,
                    format!(
                        "{label} {:?}: default {} outside [{}, {}]",
                        p.name, m.default, m.min, m.max
                    ),
                ));
            }
        }
        (_, Some(_)) => {
            return Err(ContractError::new(
                at,
                format!(
                    "{label} {:?}: only an `i32` port carries an integer {{ .. }} meta block",
                    p.name
                ),
            ));
        }
        (_, None) => {}
    }
    // `vocab` type only on `enum`, and required there.
    match (p.ty.as_str(), &p.vocab) {
        ("enum", None) => {
            return Err(ContractError::new(
                at,
                format!("{label} {:?}: an `enum` port must name its vocab type, e.g. `enum(FilterMode)`", p.name),
            ));
        }
        ("enum", Some(v)) => {
            if !is_ident(v) {
                return Err(ContractError::new(
                    at,
                    format!(
                        "{label} {:?}: vocab type {v:?} must be an identifier",
                        p.name
                    ),
                ));
            }
        }
        (_, Some(_)) => {
            return Err(ContractError::new(
                at,
                format!(
                    "{label} {:?}: only an `enum` port names a vocab type",
                    p.name
                ),
            ));
        }
        (_, None) => {}
    }
    Ok(())
}

/// Reject a malformed contract before any code is generated. A bad spec would otherwise emit
/// source that fails to compile (duplicate consts, dangling constant param) far from its cause.
/// This is the **one** validator: the macro runs it at expansion time (turning each error into a
/// spanned `compile_error!`), the scaffold runs it before writing a file.
pub fn validate(spec: &OperatorSpec) -> Result<(), ContractError> {
    let name = &spec.type_name;
    if name.is_empty() {
        return Err(ContractError::new(Locus::TypeName, "type_name is empty"));
    }
    if !is_snake_case(name) {
        return Err(ContractError::new(
            Locus::TypeName,
            format!("type_name {name:?} must be snake_case: a lowercase letter then [a-z0-9_]"),
        ));
    }
    // Reserved: interface pipes are **loader-built** (ADR-0038 §2) — declared through
    // `interface.inputs` entries, never a registered operator — and the save path identifies
    // pipe nodes by this type name. Refused here (the one validator: macro + scaffold) so a
    // scaffolded/hand-written `pipe` operator fails before any code is generated; the registry
    // carries the same reservation for embedders registering descriptors directly.
    if name == "pipe" {
        return Err(ContractError::new(
            Locus::TypeName,
            "type_name \"pipe\" is reserved: interface pipes are loader-built (ADR-0038), \
             declared as `interface.inputs` entries, never as an operator type",
        ));
    }

    for (i, p) in spec.constants.iter().enumerate() {
        let at = Locus::Constant(i);
        if !is_snake_case(&p.name) {
            return Err(ContractError::new(
                at,
                format!(
                    "constant name {:?} must be snake_case: a lowercase letter then [a-z0-9_]",
                    p.name
                ),
            ));
        }
        validate_port(at, "constant", p)?;
    }

    for (is_input, ports) in [(true, &spec.inputs), (false, &spec.outputs)] {
        let label = if is_input { "input" } else { "output" };
        let mut seen = std::collections::BTreeSet::new();
        for (i, p) in ports.iter().enumerate() {
            let at = if is_input {
                Locus::Input(i)
            } else {
                Locus::Output(i)
            };
            if !is_snake_case(&p.name) {
                return Err(ContractError::new(
                    at,
                    format!(
                        "{label} port name {:?} must be snake_case: a lowercase letter then [a-z0-9_]",
                        p.name
                    ),
                ));
            }
            if !seen.insert(p.name.as_str()) {
                return Err(ContractError::new(
                    at,
                    format!("duplicate {label} port name {:?}", p.name),
                ));
            }
            validate_port(at, label, p)?;
        }
    }

    let mut seen_const = std::collections::BTreeSet::new();
    for (i, p) in spec.constants.iter().enumerate() {
        if !seen_const.insert(p.name.as_str()) {
            return Err(ContractError::new(
                Locus::Constant(i),
                format!("duplicate constant name {:?}", p.name),
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(json: &str) -> OperatorSpec {
        serde_json::from_str(json).expect("valid json spec")
    }

    fn err(json: &str) -> ContractError {
        validate(&spec(json)).expect_err("spec should be rejected")
    }

    #[test]
    fn accepts_a_minimal_spec() {
        assert!(validate(&spec(r#"{ "type_name": "my_op" }"#)).is_ok());
    }

    #[test]
    fn rejects_non_snake_case_type_name_at_type_name_locus() {
        for bad in [r#"{ "type_name": "MyOp" }"#, r#"{ "type_name": "2cool" }"#] {
            let e = err(bad);
            assert_eq!(e.locus, Locus::TypeName);
            assert!(e.message.contains("snake_case"), "{}", e.message);
        }
        assert_eq!(err(r#"{ "type_name": "" }"#).locus, Locus::TypeName);
    }

    #[test]
    fn rejects_bad_port_type_at_that_port() {
        let e = err(r#"{ "type_name": "x", "inputs": [ {"name":"a","ty":"audio"} ] }"#);
        assert_eq!(e.locus, Locus::Input(0));
        assert!(e.message.contains("type"), "{}", e.message);
    }

    #[test]
    fn rejects_bad_ranges_on_an_i32_constant() {
        let inverted = err(
            r#"{ "type_name": "x", "constants": [ {"name":"voices","ty":"i32","i32":{"min":32,"max":1,"default":8}} ] }"#,
        );
        assert_eq!(inverted.locus, Locus::Constant(0));
        assert!(inverted.message.contains("min"), "{}", inverted.message);
        let oob = err(
            r#"{ "type_name": "x", "constants": [ {"name":"voices","ty":"i32","i32":{"min":1,"max":32,"default":99}} ] }"#,
        );
        assert!(oob.message.contains("outside"), "{}", oob.message);
        // An `i32` port without its meta block is rejected.
        let bare = err(r#"{ "type_name": "x", "constants": [ {"name":"voices","ty":"i32"} ] }"#);
        assert!(bare.message.contains("meta"), "{}", bare.message);
    }

    #[test]
    fn rejects_non_snake_case_port_name_at_that_port() {
        for bad in ["in gain", "2x", "Freq"] {
            let json = format!(
                r#"{{ "type_name": "x", "inputs": [ {{"name":{bad:?},"ty":"f32_buffer"}} ] }}"#
            );
            let e = err(&json);
            assert_eq!(e.locus, Locus::Input(0), "{}", e.message);
            assert!(e.message.contains("snake_case"), "{}", e.message);
        }
    }

    #[test]
    fn rejects_non_snake_case_constant_name_at_that_constant() {
        // `Voices` would otherwise screaming-collide with `voices` into one `C_VOICES` const.
        let e = err(
            r#"{ "type_name": "x", "constants": [ {"name":"Voices","ty":"i32","i32":{"min":1,"max":32,"default":8}} ] }"#,
        );
        assert_eq!(e.locus, Locus::Constant(0), "{}", e.message);
        assert!(e.message.contains("snake_case"), "{}", e.message);
    }

    #[test]
    fn accepts_the_full_port_vocabulary() {
        // A filter-shaped contract plus the discrete carriers: f32_buffer, f32-with-meta, enum
        // (naming its vocab type), note, harmony.
        assert!(validate(&spec(
            r#"{ "type_name": "filter",
                 "inputs": [
                   {"name":"audio","ty":"f32_buffer"},
                   {"name":"cutoff","ty":"f32","f32":{"min":20,"max":20000,"default":1000,"unit":"Hz","curve":"exponential"}},
                   {"name":"mode","ty":"enum","vocab":"FilterMode"},
                   {"name":"notes","ty":"note"},
                   {"name":"ctx","ty":"harmony"} ],
                 "outputs": [ {"name":"audio","ty":"f32_buffer"} ] }"#
        ))
        .is_ok());
    }

    #[test]
    fn rejects_malformed_ports() {
        // `f32` needs a meta block.
        let bare_float = err(r#"{ "type_name": "x", "inputs": [ {"name":"a","ty":"f32"} ] }"#);
        assert_eq!(bare_float.locus, Locus::Input(0));
        assert!(
            bare_float.message.contains("meta"),
            "{}",
            bare_float.message
        );

        // `enum` must name its vocab type.
        let no_vocab = err(r#"{ "type_name": "x", "inputs": [ {"name":"a","ty":"enum"} ] }"#);
        assert!(no_vocab.message.contains("vocab"), "{}", no_vocab.message);

        // A port that is neither `f32` nor `f32_buffer` can't carry f32 meta (ADR-0031 decision (a)
        // extended the optional meta block to `f32_buffer`).
        let stray_meta = err(
            r#"{ "type_name": "x", "inputs": [ {"name":"a","ty":"note","f32":{"min":0,"max":1,"default":0}} ] }"#,
        );
        assert!(stray_meta.message.contains("f32"), "{}", stray_meta.message);

        // ...but an `f32_buffer` *may* carry one (a signal control with a scalar default), and a
        // bad range in it is still validated.
        assert!(validate(&spec(
            r#"{ "type_name": "x", "inputs": [ {"name":"freq","ty":"f32_buffer","f32":{"min":20,"max":20000,"default":440,"unit":"Hz","curve":"exponential"}} ] }"#,
        ))
        .is_ok());
        let buf_oob = err(
            r#"{ "type_name": "x", "inputs": [ {"name":"freq","ty":"f32_buffer","f32":{"min":0,"max":1,"default":5}} ] }"#,
        );
        assert!(buf_oob.message.contains("outside"), "{}", buf_oob.message);

        // Out-of-range f32 default.
        let oob = err(
            r#"{ "type_name": "x", "inputs": [ {"name":"a","ty":"f32","f32":{"min":0,"max":1,"default":5}} ] }"#,
        );
        assert!(oob.message.contains("outside"), "{}", oob.message);
    }

    #[test]
    fn arg_is_input_only() {
        // The type-agnostic pass-through (issue #141) is legal as an input...
        assert!(validate(&spec(
            r#"{ "type_name": "osc_out", "inputs": [ {"name":"in","ty":"arg"} ] }"#
        ))
        .is_ok());
        // ...but an `arg` output or constant fails closed: an untyped source on the graph would
        // need plan-time type flow through the carrier.
        let out = err(r#"{ "type_name": "x", "outputs": [ {"name":"tap","ty":"arg"} ] }"#);
        assert_eq!(out.locus, Locus::Output(0));
        assert!(out.message.contains("input-only"), "{}", out.message);
        let konst = err(r#"{ "type_name": "x", "constants": [ {"name":"c","ty":"arg"} ] }"#);
        assert_eq!(konst.locus, Locus::Constant(0));
        assert!(konst.message.contains("input-only"), "{}", konst.message);
    }

    #[test]
    fn rejects_duplicate_input_and_constant_names() {
        let dup = err(
            r#"{ "type_name": "x", "inputs": [ {"name":"a","ty":"f32_buffer"}, {"name":"a","ty":"f32_buffer"} ] }"#,
        );
        assert_eq!(dup.locus, Locus::Input(1));
        let dup_const = err(
            r#"{ "type_name": "x", "constants": [ {"name":"voices","ty":"i32","i32":{"min":1,"max":32,"default":8}}, {"name":"voices","ty":"i32","i32":{"min":1,"max":4,"default":2}} ] }"#,
        );
        assert_eq!(dup_const.locus, Locus::Constant(1));
    }
}
