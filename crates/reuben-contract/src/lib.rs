//! reuben-contract — the single source of an operator's port/param contract (ADR-0025).
//!
//! Every operator declares its ports and params **once**. Two consumers turn that one
//! declaration into code: the [`operator_contract!`](../reuben_macros) proc-macro (which emits
//! the `IN_`/`OUT_`/`P_` index consts and the `Descriptor`) and the
//! [`scaffold`](../reuben_native) (which emits the macro call for a brand-new operator). Both
//! must agree on what a *valid* contract is and how names map to consts, so that shared logic —
//! the spec types, the naming rules, and [`validate`] — lives here, in a crate both depend on.
//! Putting it anywhere else would re-create the very drift this layer exists to remove.

use serde::Deserialize;

pub mod naming;

/// A port in the contract: a name and a kind (`signal` | `message` | `context`). Kept as a
/// `String` kind (not an enum) so the same struct round-trips from the scaffold's JSON spec and
/// from the proc-macro's parsed `name: kind` tokens with no conversion.
#[derive(Debug, Clone, Deserialize)]
pub struct PortSpec {
    pub name: String,
    pub kind: String,
}

/// One parameter's metadata, mirroring [`reuben_core::descriptor::ParamMeta`].
#[derive(Debug, Clone, Deserialize)]
pub struct ParamSpec {
    pub name: String,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    #[serde(default)]
    pub unit: String,
    #[serde(default = "default_curve")]
    pub curve: String,
}

fn default_curve() -> String {
    "linear".to_string()
}

/// How the operator sets its output Lane count (mirrors [`reuben_core::descriptor::LaneRule`]).
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LaneSpec {
    #[default]
    Inherit,
    /// Expand to as many Lanes as the named param's value (the Voicer pattern).
    FromParam(String),
}

/// The contract for an Operator — one declaration of its ports, params, resources, and Lane rule.
/// The scaffold hand-authors / deserializes it; the proc-macro parses it from `operator_contract!`
/// syntax. Mirrors a [`reuben_core::descriptor::Descriptor`].
#[derive(Debug, Clone, Deserialize)]
pub struct OperatorSpec {
    pub type_name: String,
    #[serde(default)]
    pub inputs: Vec<PortSpec>,
    #[serde(default)]
    pub outputs: Vec<PortSpec>,
    #[serde(default)]
    pub params: Vec<ParamSpec>,
    #[serde(default)]
    pub resources: Vec<String>,
    #[serde(default)]
    pub lanes: LaneSpec,
}

/// The three legal port kinds. Centralised so both the scaffold and the macro reject the same set.
pub const PORT_KINDS: [&str; 3] = ["signal", "message", "context"];

/// Where in the spec a validation error sits, so the proc-macro can attach a source span to the
/// offending token. The scaffold ignores the locus and just formats the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locus {
    TypeName,
    Input(usize),
    Output(usize),
    Param(usize),
    Lanes,
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

/// Reject a malformed contract before any code is generated. A bad spec would otherwise emit
/// source that fails to compile (duplicate consts, dangling lane param) far from its cause. This
/// is the **one** validator: the macro runs it at expansion time (turning each error into a
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

    let mut seen_param = std::collections::BTreeSet::new();
    for (i, p) in spec.params.iter().enumerate() {
        let at = Locus::Param(i);
        if !is_snake_case(&p.name) {
            return Err(ContractError::new(
                at,
                format!(
                    "param name {:?} must be snake_case: a lowercase letter then [a-z0-9_]",
                    p.name
                ),
            ));
        }
        if !seen_param.insert(p.name.as_str()) {
            return Err(ContractError::new(
                at,
                format!("duplicate param name {:?}", p.name),
            ));
        }
        if p.min > p.max {
            return Err(ContractError::new(
                at,
                format!("param {:?}: min {} > max {}", p.name, p.min, p.max),
            ));
        }
        if p.default < p.min || p.default > p.max {
            return Err(ContractError::new(
                at,
                format!(
                    "param {:?}: default {} outside [{}, {}]",
                    p.name, p.default, p.min, p.max
                ),
            ));
        }
        if !matches!(p.curve.as_str(), "linear" | "exponential") {
            return Err(ContractError::new(
                at,
                format!(
                    "param {:?}: curve {:?} must be \"linear\" or \"exponential\"",
                    p.name, p.curve
                ),
            ));
        }
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
            if !PORT_KINDS.contains(&p.kind.as_str()) {
                return Err(ContractError::new(
                    at,
                    format!(
                        "{label} {:?}: kind {:?} must be \"signal\", \"message\", or \"context\"",
                        p.name, p.kind
                    ),
                ));
            }
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
        }
    }

    if let LaneSpec::FromParam(param) = &spec.lanes {
        if !spec.params.iter().any(|p| &p.name == param) {
            return Err(ContractError::new(
                Locus::Lanes,
                format!("lanes.from_param {param:?} names no declared param"),
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
    fn rejects_bad_port_kind_at_that_port() {
        let e = err(r#"{ "type_name": "x", "inputs": [ {"name":"a","kind":"audio"} ] }"#);
        assert_eq!(e.locus, Locus::Input(0));
        assert!(e.message.contains("kind"), "{}", e.message);
    }

    #[test]
    fn rejects_bad_curve_and_ranges_at_that_param() {
        let curve = err(
            r#"{ "type_name": "x", "params": [ {"name":"a","min":0,"max":1,"default":0,"curve":"log"} ] }"#,
        );
        assert_eq!(curve.locus, Locus::Param(0));
        let inverted =
            err(r#"{ "type_name": "x", "params": [ {"name":"a","min":1,"max":0,"default":0} ] }"#);
        assert!(inverted.message.contains("min"), "{}", inverted.message);
        let oob =
            err(r#"{ "type_name": "x", "params": [ {"name":"a","min":0,"max":1,"default":5} ] }"#);
        assert!(oob.message.contains("outside"), "{}", oob.message);
    }

    #[test]
    fn rejects_non_snake_case_port_name_at_that_port() {
        for bad in ["in gain", "2x", "Freq"] {
            let json = format!(
                r#"{{ "type_name": "x", "inputs": [ {{"name":{bad:?},"kind":"signal"}} ] }}"#
            );
            let e = err(&json);
            assert_eq!(e.locus, Locus::Input(0), "{}", e.message);
            assert!(e.message.contains("snake_case"), "{}", e.message);
        }
    }

    #[test]
    fn rejects_non_snake_case_param_name_at_that_param() {
        // `Freq` would otherwise screaming-collide with `freq` into one `P_FREQ` const.
        let e = err(
            r#"{ "type_name": "x", "params": [ {"name":"Freq","min":0,"max":1,"default":0} ] }"#,
        );
        assert_eq!(e.locus, Locus::Param(0), "{}", e.message);
        assert!(e.message.contains("snake_case"), "{}", e.message);
    }

    #[test]
    fn rejects_duplicates_and_dangling_lane_param() {
        let dup = err(
            r#"{ "type_name": "x", "inputs": [ {"name":"a","kind":"signal"}, {"name":"a","kind":"signal"} ] }"#,
        );
        assert_eq!(dup.locus, Locus::Input(1));
        let dangling = err(r#"{ "type_name": "x", "lanes": { "from_param": "voices" } }"#);
        assert_eq!(dangling.locus, Locus::Lanes);
    }
}
