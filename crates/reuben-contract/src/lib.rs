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

/// How a control responds across its range — the good-button curve. The **one** definition of
/// the curve axis (issue #217): the macro grammar's `lin`/`exp` keywords, the scaffold JSON's
/// `"linear"`/`"exponential"` strings, and the runtime descriptor all resolve to this enum, so an
/// unknown curve is unrepresentable past the parse/deserialize boundary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Curve {
    #[default]
    Linear,
    /// Perceptually-even for frequency-like controls.
    Exponential,
}

/// The `{ min, max, default, unit, curve }` block on a `f32` port (ADR-0030): its unwired
/// default, range, and display metadata. Required on a `f32` port (a bare per-sample wire is
/// `f32_buffer`, not `f32`). The **one** definition (issue #217): the macro AST, the model layer,
/// and the runtime descriptor all use this type — the owning
/// [`Port`](reuben_core::descriptor::Port) carries the name, so a control's name lives in
/// exactly one place.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct F32Meta {
    pub min: f32,
    pub max: f32,
    pub default: f32,
    /// Display unit, e.g. "Hz", "dB", "s".
    #[serde(default)]
    pub unit: String,
    #[serde(default)]
    pub curve: Curve,
}

impl F32Meta {
    pub fn clamp(&self, v: f32) -> f32 {
        v.clamp(self.min, self.max)
    }
}

/// The `{ min, max, default }` block on an `i32` port (ADR-0035): a bounded integer control /
/// constant (a count like `voices`). No unit/curve — a count is not a swept knob. Like
/// [`F32Meta`], the one definition (issue #217), nameless: the owning port carries the name.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct I32Meta {
    pub min: i32,
    pub max: i32,
    pub default: i32,
}

impl I32Meta {
    pub fn clamp(&self, v: i32) -> i32 {
        v.clamp(self.min, self.max)
    }
}

/// A port's [`Arg`](reuben_core::message::Arg) type (ADR-0030, ADR-0035) — **payload-carrying**
/// (issue #217): the type and the meta it takes are one datum, so "meta iff type" is
/// unrepresentable rather than validated. The one authoring-side taxonomy: the proc-macro's
/// grammar parses into it, the scaffold's JSON deserializes into it (via the flat wire shape —
/// see [`PortSpec`]'s `Deserialize`), and both render/emit from it.
#[derive(Debug, Clone, PartialEq)]
pub enum PortTy {
    /// `f32_buffer` — a dense per-sample signal (audio / control buffer). The optional meta
    /// (ADR-0031 decision (a)) gives a Signal port a scalar default + knob range
    /// (`oscillator.freq`): unwired/knob-set it materializes from the default, yet a Signal
    /// source still wires straight in.
    F32Buffer(Option<F32Meta>),
    /// `f32 { .. }` — a materialized scalar control with its (required) default/range meta.
    F32(F32Meta),
    /// `i32 { .. }` — a bounded integer control / constant (ADR-0035).
    I32(I32Meta),
    /// `enum(VocabType)` — a held vocab enum, naming its shared `vocab` type (PascalCase, e.g.
    /// `"FilterMode"`); the descriptor reads its `VARIANTS`/default from `Type::enum_meta(name)`.
    Enum(String),
    /// `note` — a `Note` event port.
    Note,
    /// `harmony` — a `Harmony` held port.
    Harmony,
    /// `arg` — a type-agnostic pass-through carrying any `Arg` (issue #141). Input-only
    /// ([`validate`] enforces it — legality needs list context).
    Arg,
}

/// The legal `ty` words of the flat wire shape, for the deserialize error message — the same
/// set the old stringly `PORT_TYPES` const enumerated, now owned by [`PortTy`].
const PORT_TY_WORDS: [&str; 7] = ["f32_buffer", "f32", "i32", "enum", "note", "harmony", "arg"];

/// A port in the contract — its name plus its payload-carrying [`PortTy`] (issue #217).
///
/// Deserializes from the **flat wire shape** the scaffold JSON has always used —
/// `{"name":"cutoff","ty":"f32","f32":{..}}` — via [`PortSpecFlat`]: a shape-invalid port (an
/// unknown `ty`, a missing or stray meta block, a stray key) fails at parse time with the
/// reason in the error text, before [`validate`] ever runs.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(try_from = "PortSpecFlat")]
pub struct PortSpec {
    pub name: String,
    pub ty: PortTy,
}

/// The flat JSON wire shape of a port — the scaffold's authoring format, unchanged from the
/// stringly era (issue #217). Private: it exists only to give [`PortSpec`]'s `Deserialize` the
/// old surface (`deny_unknown_fields` keeps it closed) while the in-memory shape is [`PortTy`].
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PortSpecFlat {
    name: String,
    ty: String,
    #[serde(default)]
    f32: Option<F32Meta>,
    #[serde(default)]
    i32: Option<I32Meta>,
    #[serde(default)]
    vocab: Option<String>,
}

impl TryFrom<PortSpecFlat> for PortSpec {
    type Error = String;

    /// Fold the flat wire fields into the payload-carrying [`PortTy`], rejecting every
    /// shape mismatch the old `validate_port` caught — same guarantees and the same precedence
    /// (unknown type first, then a missing block, then strays), earlier seam.
    fn try_from(flat: PortSpecFlat) -> Result<Self, String> {
        let PortSpecFlat {
            name,
            ty,
            mut f32,
            mut i32,
            mut vocab,
        } = flat;
        // Each arm takes the block(s) its type consumes; whatever is left afterwards is a stray.
        let ty = match ty.as_str() {
            "f32_buffer" => PortTy::F32Buffer(f32.take()),
            "f32" => PortTy::F32(f32.take().ok_or_else(|| {
                format!("port {name:?}: a `f32` port needs a {{ .. }} meta block")
            })?),
            "i32" => PortTy::I32(i32.take().ok_or_else(|| {
                format!("port {name:?}: an `i32` port needs a {{ .. }} meta block")
            })?),
            "enum" => PortTy::Enum(vocab.take().ok_or_else(|| {
                format!(
                    "port {name:?}: an `enum` port must name its vocab type, e.g. `enum(FilterMode)`"
                )
            })?),
            "note" => PortTy::Note,
            "harmony" => PortTy::Harmony,
            "arg" => PortTy::Arg,
            other => {
                return Err(format!(
                    "port {name:?}: type {other:?} must be one of {PORT_TY_WORDS:?}"
                ))
            }
        };
        if f32.is_some() {
            return Err(format!(
                "port {name:?}: only a `f32` or `f32_buffer` port carries a {{ .. }} meta block"
            ));
        }
        if i32.is_some() {
            return Err(format!(
                "port {name:?}: only an `i32` port carries an integer {{ .. }} meta block"
            ));
        }
        if vocab.is_some() {
            return Err(format!(
                "port {name:?}: only an `enum` port names a vocab type"
            ));
        }
        Ok(PortSpec { name, ty })
    }
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

/// Validate one port's **data** rules (ADR-0030): coherent ranges, in-range defaults, an
/// identifier-shaped vocab type, `arg` input-only. Shape rules ("meta iff type") no longer live
/// here — the payload-carrying [`PortTy`] makes a shape mismatch unrepresentable (issue #217),
/// rejected at the macro's parse or the scaffold JSON's deserialize.
fn validate_port(at: Locus, label: &str, p: &PortSpec) -> Result<(), ContractError> {
    // One range rule for both numeric metas — `f32` and `i32` share the check and the message
    // shape, differing only in the scalar type.
    fn range<T: PartialOrd + Copy + std::fmt::Display>(
        at: Locus,
        label: &str,
        name: &str,
        (min, max, default): (T, T, T),
    ) -> Result<(), ContractError> {
        if min > max {
            return Err(ContractError::new(
                at,
                format!("{label} {name:?}: min {min} > max {max}"),
            ));
        }
        if default < min || default > max {
            return Err(ContractError::new(
                at,
                format!("{label} {name:?}: default {default} outside [{min}, {max}]"),
            ));
        }
        Ok(())
    }
    match &p.ty {
        PortTy::F32(m) | PortTy::F32Buffer(Some(m)) => {
            range(at, label, &p.name, (m.min, m.max, m.default))?
        }
        PortTy::I32(m) => range(at, label, &p.name, (m.min, m.max, m.default))?,
        PortTy::Enum(v) => {
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
        // `arg` is **input-only** (issue #141): it is legal only where the operator treats the
        // payload as opaque — a pure carrier's inbound port. An `arg` output would put an untyped
        // source on the graph, and a typed input downstream of it would need plan-time type flow
        // *through* the carrier to recover the true source type — machinery no operator has
        // earned. Fail closed here — it needs list context (which block the port sits in), so it
        // can't move to the deserialize seam.
        PortTy::Arg => {
            if label != "input" {
                return Err(ContractError::new(
                    at,
                    format!(
                        "{label} {:?}: `arg` is input-only (the pass-through carries whatever its \
                         wired source declares; an `arg` {label} would have no type authority at \
                         all)",
                        p.name
                    ),
                ));
            }
        }
        PortTy::F32Buffer(None) | PortTy::Note | PortTy::Harmony => {}
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
    fn rejects_the_reserved_pipe_type_name() {
        // Interface pipes are loader-built (ADR-0038): the name is reserved so a scaffolded
        // `pipe` operator fails before any code is generated.
        let e = err(r#"{ "type_name": "pipe" }"#);
        assert_eq!(e.locus, Locus::TypeName);
        assert!(e.message.contains("reserved"), "{}", e.message);
    }

    /// The deserialize-time rejection of a shape-invalid port JSON (issue #217): the payload-
    /// carrying [`PortTy`] makes "meta iff type" unrepresentable, so the scaffold's JSON path
    /// fails at parse — before `validate()` — with the reason in the error text.
    fn de_err(json: &str) -> String {
        serde_json::from_str::<OperatorSpec>(json)
            .expect_err("shape-invalid port JSON must fail to deserialize")
            .to_string()
    }

    #[test]
    fn rejects_unknown_port_type_at_deserialize() {
        let msg = de_err(r#"{ "type_name": "x", "inputs": [ {"name":"a","ty":"audio"} ] }"#);
        assert!(msg.contains("audio"), "{msg}");
        // The error names the legal set, like the old validate() message did.
        assert!(
            msg.contains("f32_buffer") && msg.contains("harmony"),
            "{msg}"
        );
        // Precedence matches the old validator: an unknown type wins over a stray meta block —
        // the type is the more fundamental mistake.
        let both = de_err(
            r#"{ "type_name": "x", "inputs": [ {"name":"a","ty":"audio","f32":{"min":0,"max":1,"default":0}} ] }"#,
        );
        assert!(both.contains("must be one of"), "{both}");
    }

    #[test]
    fn rejects_shape_invalid_ports_at_deserialize() {
        // `f32` needs a meta block.
        let bare_float = de_err(r#"{ "type_name": "x", "inputs": [ {"name":"a","ty":"f32"} ] }"#);
        assert!(bare_float.contains("meta"), "{bare_float}");

        // `i32` needs its integer meta block.
        let bare_int =
            de_err(r#"{ "type_name": "x", "constants": [ {"name":"voices","ty":"i32"} ] }"#);
        assert!(bare_int.contains("meta"), "{bare_int}");

        // `enum` must name its vocab type.
        let no_vocab = de_err(r#"{ "type_name": "x", "inputs": [ {"name":"a","ty":"enum"} ] }"#);
        assert!(no_vocab.contains("vocab"), "{no_vocab}");

        // A port that is neither `f32` nor `f32_buffer` can't carry f32 meta (ADR-0031
        // decision (a) extended the optional meta block to `f32_buffer`).
        let stray_meta = de_err(
            r#"{ "type_name": "x", "inputs": [ {"name":"a","ty":"note","f32":{"min":0,"max":1,"default":0}} ] }"#,
        );
        assert!(stray_meta.contains("f32"), "{stray_meta}");

        // Only an `i32` port carries the integer meta block.
        let stray_int = de_err(
            r#"{ "type_name": "x", "inputs": [ {"name":"a","ty":"note","i32":{"min":0,"max":1,"default":0}} ] }"#,
        );
        assert!(stray_int.contains("i32"), "{stray_int}");

        // Only an `enum` port names a vocab type.
        let stray_vocab = de_err(
            r#"{ "type_name": "x", "inputs": [ {"name":"a","ty":"note","vocab":"FilterMode"} ] }"#,
        );
        assert!(stray_vocab.contains("vocab"), "{stray_vocab}");
    }

    #[test]
    fn rejects_unknown_port_fields_at_deserialize() {
        // The flat wire shape is closed: a typo'd or stray key is an error, not silently dropped.
        let msg =
            de_err(r#"{ "type_name": "x", "inputs": [ {"name":"a","ty":"note","meta":1} ] }"#);
        assert!(msg.contains("meta"), "{msg}");
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
        // (A bare `i32` with no meta block is a *shape* error now — rejected at deserialize;
        // see `rejects_shape_invalid_ports_at_deserialize`.)
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
    fn rejects_bad_ranges_on_f32_ports() {
        // (The shape rules — meta iff type, vocab iff enum — are deserialize-time now; see
        // `rejects_shape_invalid_ports_at_deserialize`. validate() keeps the data rules.)
        // An `f32_buffer` *may* carry a meta block (a signal control with a scalar default), and
        // a bad range in it is still validated.
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

    // The curve axis is an enum (issue #217): a curve that is neither "linear" nor
    // "exponential" is unrepresentable, so the scaffold's JSON path fails at deserialize time
    // (the macro's `lin`/`exp` keywords map to [`Curve`] at parse and never reach here).
    #[test]
    fn unknown_curve_fails_to_deserialize() {
        let e = serde_json::from_str::<OperatorSpec>(
            r#"{ "type_name": "x", "inputs": [ {"name":"a","ty":"f32","f32":{"min":0,"max":1,"default":0,"curve":"log"}} ] }"#,
        )
        .expect_err("unknown curve must fail at deserialize");
        let msg = e.to_string();
        assert!(
            msg.contains("linear") && msg.contains("exponential"),
            "error must name the legal curves: {msg}"
        );
    }

    // The deserialized curve is the enum, with an omitted curve defaulting to linear — the same
    // default the macro grammar applies to an omitted curve keyword.
    #[test]
    fn curve_deserializes_to_the_enum_and_defaults_linear() {
        let s = spec(
            r#"{ "type_name": "x", "inputs": [
                 {"name":"a","ty":"f32","f32":{"min":0,"max":1,"default":0,"curve":"exponential"}},
                 {"name":"b","ty":"f32","f32":{"min":0,"max":1,"default":0}} ] }"#,
        );
        let meta = |i: usize| match &s.inputs[i].ty {
            PortTy::F32(m) => m,
            other => panic!("expected a f32 port, got {other:?}"),
        };
        assert_eq!(meta(0).curve, Curve::Exponential);
        assert_eq!(meta(1).curve, Curve::Linear);
        // An omitted unit is the empty string.
        assert_eq!(meta(1).unit, "");
    }

    // The flat wire shape folds into the payload-carrying [`PortTy`] (issue #217): the meta and
    // the vocab name ride inside the type, so "meta iff type" holds by construction.
    #[test]
    fn ports_deserialize_to_the_payload_carrying_ty() {
        let s = spec(
            r#"{ "type_name": "filter",
                 "inputs": [
                   {"name":"audio","ty":"f32_buffer"},
                   {"name":"freq","ty":"f32_buffer","f32":{"min":20,"max":20000,"default":440}},
                   {"name":"cutoff","ty":"f32","f32":{"min":20,"max":20000,"default":1000}},
                   {"name":"mode","ty":"enum","vocab":"FilterMode"},
                   {"name":"notes","ty":"note"},
                   {"name":"ctx","ty":"harmony"},
                   {"name":"in","ty":"arg"} ],
                 "constants": [ {"name":"voices","ty":"i32","i32":{"min":1,"max":32,"default":8}} ] }"#,
        );
        assert!(matches!(s.inputs[0].ty, PortTy::F32Buffer(None)));
        assert!(matches!(&s.inputs[1].ty, PortTy::F32Buffer(Some(m)) if m.default == 440.0));
        assert!(matches!(&s.inputs[2].ty, PortTy::F32(m) if m.default == 1000.0));
        assert!(matches!(&s.inputs[3].ty, PortTy::Enum(v) if v == "FilterMode"));
        assert!(matches!(s.inputs[4].ty, PortTy::Note));
        assert!(matches!(s.inputs[5].ty, PortTy::Harmony));
        assert!(matches!(s.inputs[6].ty, PortTy::Arg));
        assert!(matches!(&s.constants[0].ty, PortTy::I32(m) if m.default == 8));
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
