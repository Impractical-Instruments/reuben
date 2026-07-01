//! The pure core of `operator_contract!` (ADR-0025): turn a validated [`OperatorSpec`] into a
//! [`ContractModel`] — every const name, port ordinal, and param index resolved. No tokens,
//! no spans, just data, so the index arithmetic (the old `scaffold::port_consts` hand-logic) is
//! computed **once** here and unit-tested directly.

use reuben_contract::{naming, OperatorSpec, PortSpec};

/// The resolved `f32 { .. }` meta on a `f32` port (ADR-0030), curve normalised to
/// `"linear"`/`"exponential"`.
#[derive(Debug, Clone, PartialEq)]
pub struct F32Model {
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub unit: String,
    pub curve: String,
}

/// The resolved `i32 { .. }` meta on an `i32` port / constant (ADR-0035).
#[derive(Debug, Clone, PartialEq)]
pub struct I32Model {
    pub min: i32,
    pub max: i32,
    pub default: i32,
}

/// One resolved port: its index const (`IN_FREQ`), ordinal, source name, and its
/// [`Arg`](reuben_core::message::Arg) type (ADR-0030). Ports number **sequentially** within
/// inputs/outputs (declaration order). `f32` carries its [`F32Model`]; `enum` carries the
/// shared `vocab` type name.
#[derive(Debug, Clone, PartialEq)]
pub struct PortModel {
    pub const_name: String,
    pub ordinal: usize,
    pub name: String,
    /// The port [`Arg`](reuben_core::message::Arg) type: `f32_buffer` | `f32` | `i32` | `enum` |
    /// `note` | `harmony` | `arg`.
    pub ty: String,
    pub f32: Option<F32Model>,
    /// The resolved `i32 { .. }` meta, for `i32` ports / constants (ADR-0035).
    pub i32: Option<I32Model>,
    /// The shared `vocab` enum type name, for `enum` ports.
    pub vocab: Option<String>,
}

/// A fully-resolved operator contract, ready to render to tokens.
#[derive(Debug, Clone, PartialEq)]
pub struct ContractModel {
    pub type_name: String,
    pub inputs: Vec<PortModel>,
    pub outputs: Vec<PortModel>,
    /// Instantiate-time **`Constant`** ports (ADR-0035), numbered with `C_` index consts. Empty for
    /// the common operator.
    pub constants: Vec<PortModel>,
    pub resources: Vec<String>,
}

/// Number a set of ports sequentially in declaration order (ADR-0030): inputs are indexed `0..`,
/// outputs `0..`, regardless of the port's [`Arg`](reuben_core::message::Arg) type. (The old
/// per-kind ordinal split — the voicer footgun — is gone with the carrier kinds.)
fn port_models(ports: &[PortSpec], prefix: &str) -> Vec<PortModel> {
    ports
        .iter()
        .enumerate()
        .map(|(idx, p)| PortModel {
            const_name: format!("{prefix}_{}", naming::screaming(&p.name)),
            ordinal: idx,
            name: p.name.clone(),
            ty: p.ty.clone(),
            f32: p.f32.as_ref().map(|m| F32Model {
                min: m.min,
                max: m.max,
                default: m.default,
                unit: m.unit.clone(),
                curve: m.curve.clone(),
            }),
            i32: p.i32.as_ref().map(|m| I32Model {
                min: m.min,
                max: m.max,
                default: m.default,
            }),
            vocab: p.vocab.clone(),
        })
        .collect()
}

/// Resolve a validated spec into its const/ordinal model. Assumes the spec already passed
/// [`reuben_contract::validate`] (the macro validates first); curve strings are taken verbatim.
pub fn build(spec: &OperatorSpec) -> ContractModel {
    ContractModel {
        type_name: spec.type_name.clone(),
        inputs: port_models(&spec.inputs, "IN"),
        outputs: port_models(&spec.outputs, "OUT"),
        constants: port_models(&spec.constants, "C"),
        resources: spec.resources.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(json: &str) -> OperatorSpec {
        serde_json::from_str(json).expect("valid spec")
    }

    // One f32_buffer input -> IN_AUDIO at ordinal 0.
    #[test]
    fn single_buffer_input_is_ordinal_zero() {
        let m = build(&spec(
            r#"{ "type_name": "oscillator", "inputs": [ {"name":"audio","ty":"f32_buffer"} ] }"#,
        ));
        assert_eq!(m.inputs[0].const_name, "IN_AUDIO");
        assert_eq!(m.inputs[0].ordinal, 0);
    }

    // Ports number sequentially in declaration order (ADR-0030): the former per-kind split is
    // gone, so a note input and a harmony input are 0 and 1, two outputs are 0 and 1.
    #[test]
    fn ports_number_sequentially() {
        let m = build(&spec(
            r#"{ "type_name": "voicer",
                 "inputs": [ {"name":"notes","ty":"note"}, {"name":"ctx","ty":"harmony"} ],
                 "outputs": [ {"name":"freq","ty":"f32_buffer"}, {"name":"gate","ty":"f32_buffer"} ] }"#,
        ));
        assert_eq!(
            (m.inputs[0].const_name.as_str(), m.inputs[0].ordinal),
            ("IN_NOTES", 0)
        );
        assert_eq!(
            (m.inputs[1].const_name.as_str(), m.inputs[1].ordinal),
            ("IN_CTX", 1)
        );
        assert_eq!(
            (m.outputs[0].const_name.as_str(), m.outputs[0].ordinal),
            ("OUT_FREQ", 0)
        );
        assert_eq!(
            (m.outputs[1].const_name.as_str(), m.outputs[1].ordinal),
            ("OUT_GATE", 1)
        );
    }

    // Constants are ports too (ADR-0035): they number with `C_` consts and keep their i32 meta.
    #[test]
    fn constants_index_sequentially_as_ports() {
        let m = build(&spec(
            r#"{ "type_name": "voicer",
                 "constants": [ {"name":"voices","ty":"i32","i32":{"min":1,"max":32,"default":8}} ] }"#,
        ));
        assert_eq!(m.constants[0].const_name, "C_VOICES");
        assert_eq!(m.constants[0].ordinal, 0);
        assert_eq!(m.constants[0].ty, "i32");
        assert_eq!(m.constants[0].i32.as_ref().map(|m| m.default), Some(8));
    }

    // The full filter port vocabulary: f32_buffer, f32-with-meta, enum naming its vocab type.
    #[test]
    fn resolves_the_filter_ports() {
        let m = build(&spec(
            r#"{ "type_name": "filter",
                 "inputs": [ {"name":"audio","ty":"f32_buffer"},
                             {"name":"cutoff","ty":"f32","f32":{"min":20,"max":20000,"default":1000,"unit":"Hz","curve":"exponential"}},
                             {"name":"mode","ty":"enum","vocab":"FilterMode"} ] }"#,
        ));
        assert_eq!(
            (m.inputs[0].const_name.as_str(), m.inputs[0].ty.as_str()),
            ("IN_AUDIO", "f32_buffer")
        );
        assert_eq!(
            (m.inputs[2].const_name.as_str(), m.inputs[2].ordinal),
            ("IN_MODE", 2)
        );
        assert_eq!(m.inputs[1].ty, "f32");
        assert_eq!(m.inputs[1].f32.as_ref().map(|f| f.default), Some(1000.0));
        assert_eq!(m.inputs[2].ty, "enum");
        assert_eq!(m.inputs[2].vocab.as_deref(), Some("FilterMode"));
    }

    // f32 control inputs keep their curve + unit metadata through the model (the successor to the
    // old `params` profile — runtime controls are inputs now, ADR-0030).
    #[test]
    fn f32_inputs_keep_curves_and_units() {
        let m = build(&spec(
            r#"{ "type_name": "oscillator",
                 "inputs": [
                   {"name":"freq","ty":"f32","f32":{"min":20,"max":20000,"default":440,"unit":"Hz","curve":"exponential"}},
                   {"name":"amp","ty":"f32","f32":{"min":0,"max":1,"default":0,"unit":"","curve":"linear"}} ] }"#,
        ));
        assert_eq!(m.inputs.len(), 2);
        assert_eq!(m.inputs[1].const_name, "IN_AMP");
        assert_eq!(m.inputs[1].ordinal, 1);
        assert_eq!(
            m.inputs[0].f32.as_ref().map(|f| f.curve.as_str()),
            Some("exponential")
        );
        assert_eq!(
            m.inputs[0].f32.as_ref().map(|f| f.unit.as_str()),
            Some("Hz")
        );
    }
}
