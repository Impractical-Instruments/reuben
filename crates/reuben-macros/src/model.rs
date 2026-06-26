//! The pure core of `operator_contract!` (ADR-0025): turn a validated [`OperatorSpec`] into a
//! [`ContractModel`] — every const name, port ordinal, and param index resolved. No tokens,
//! no spans, just data, so the index arithmetic (the old `scaffold::port_consts` hand-logic) is
//! computed **once** here and unit-tested directly.

use reuben_contract::{naming, LaneSpec, OperatorSpec, PortSpec};

/// The resolved `float { .. }` meta on a `float` port (ADR-0030), curve normalised to
/// `"linear"`/`"exponential"`. Mirrors [`ParamModel`] minus the const/index.
#[derive(Debug, Clone, PartialEq)]
pub struct FloatModel {
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub unit: String,
    pub curve: String,
}

/// One resolved port: its index const (`IN_FREQ`), ordinal, source name, and its
/// [`Arg`](reuben_core::message::Arg) type (ADR-0030). Ports number **sequentially** within
/// inputs/outputs (declaration order). `float` carries its [`FloatModel`]; `enum` carries the
/// shared `vocab` type name.
#[derive(Debug, Clone, PartialEq)]
pub struct PortModel {
    pub const_name: String,
    pub ordinal: usize,
    pub name: String,
    /// The port [`Arg`](reuben_core::message::Arg) type: `buffer` | `float` | `enum` | `note` |
    /// `harmony`.
    pub ty: String,
    pub float: Option<FloatModel>,
    /// The shared `vocab` enum type name, for `enum` ports.
    pub vocab: Option<String>,
}

/// One resolved param: its index const (`P_FREQ`), slot index, and metadata. `curve` is already
/// normalised to `"linear"` / `"exponential"`.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamModel {
    pub const_name: String,
    pub index: usize,
    pub name: String,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub unit: String,
    pub curve: String,
}

/// The resolved Lane rule. `FromParam` carries the **param const name** (`P_VOICES`) it expands
/// against, so the emitted `LaneRule::FromParam(P_VOICES)` references the const the macro plants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaneModel {
    Inherit,
    FromParam(String),
}

/// A fully-resolved operator contract, ready to render to tokens.
#[derive(Debug, Clone, PartialEq)]
pub struct ContractModel {
    pub type_name: String,
    pub inputs: Vec<PortModel>,
    pub outputs: Vec<PortModel>,
    pub params: Vec<ParamModel>,
    pub resources: Vec<String>,
    pub lanes: LaneModel,
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
            float: p.float.as_ref().map(|m| FloatModel {
                min: m.min,
                max: m.max,
                default: m.default,
                unit: m.unit.clone(),
                curve: m.curve.clone(),
            }),
            vocab: p.vocab.clone(),
        })
        .collect()
}

/// Resolve a validated spec into its const/ordinal model. Assumes the spec already passed
/// [`reuben_contract::validate`] (the macro validates first); curve strings are taken verbatim.
pub fn build(spec: &OperatorSpec) -> ContractModel {
    let params = spec
        .params
        .iter()
        .enumerate()
        .map(|(i, p)| ParamModel {
            const_name: format!("P_{}", naming::screaming(&p.name)),
            index: i,
            name: p.name.clone(),
            min: p.min,
            max: p.max,
            default: p.default,
            unit: p.unit.clone(),
            curve: p.curve.clone(),
        })
        .collect();
    let lanes = match &spec.lanes {
        LaneSpec::Inherit => LaneModel::Inherit,
        LaneSpec::FromParam(name) => LaneModel::FromParam(format!("P_{}", naming::screaming(name))),
    };
    ContractModel {
        type_name: spec.type_name.clone(),
        inputs: port_models(&spec.inputs, "IN"),
        outputs: port_models(&spec.outputs, "OUT"),
        params,
        resources: spec.resources.clone(),
        lanes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(json: &str) -> OperatorSpec {
        serde_json::from_str(json).expect("valid spec")
    }

    // One buffer input -> IN_AUDIO at ordinal 0.
    #[test]
    fn single_buffer_input_is_ordinal_zero() {
        let m = build(&spec(
            r#"{ "type_name": "oscillator", "inputs": [ {"name":"audio","ty":"buffer"} ] }"#,
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
                 "outputs": [ {"name":"freq","ty":"buffer"}, {"name":"gate","ty":"buffer"} ] }"#,
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

    // Params index sequentially and keep their metadata; FromParam resolves to the param's const.
    #[test]
    fn params_index_sequentially_and_lane_resolves_to_const() {
        let m = build(&spec(
            r#"{ "type_name": "voicer",
                 "params": [ {"name":"voices","min":1,"max":32,"default":8,"curve":"linear"} ],
                 "lanes": { "from_param": "voices" } }"#,
        ));
        assert_eq!(m.params[0].const_name, "P_VOICES");
        assert_eq!(m.params[0].index, 0);
        assert_eq!(m.lanes, LaneModel::FromParam("P_VOICES".to_string()));
    }

    // The full filter port vocabulary: buffer, float-with-meta, enum naming its vocab type.
    #[test]
    fn resolves_the_filter_ports() {
        let m = build(&spec(
            r#"{ "type_name": "filter",
                 "inputs": [ {"name":"audio","ty":"buffer"},
                             {"name":"cutoff","ty":"float","float":{"min":20,"max":20000,"default":1000,"unit":"Hz","curve":"exponential"}},
                             {"name":"mode","ty":"enum","vocab":"FilterMode"} ] }"#,
        ));
        assert_eq!(
            (m.inputs[0].const_name.as_str(), m.inputs[0].ty.as_str()),
            ("IN_AUDIO", "buffer")
        );
        assert_eq!(
            (m.inputs[2].const_name.as_str(), m.inputs[2].ordinal),
            ("IN_MODE", 2)
        );
        assert_eq!(m.inputs[1].ty, "float");
        assert_eq!(m.inputs[1].float.as_ref().map(|f| f.default), Some(1000.0));
        assert_eq!(m.inputs[2].ty, "enum");
        assert_eq!(m.inputs[2].vocab.as_deref(), Some("FilterMode"));
    }

    // A six-param profile with mixed curves and units.
    #[test]
    fn many_params_with_curves_and_units() {
        let m = build(&spec(
            r#"{ "type_name": "oscillator",
                 "params": [
                   {"name":"freq","min":20,"max":20000,"default":440,"unit":"Hz","curve":"exponential"},
                   {"name":"waveform","min":0,"max":1,"default":0,"unit":"","curve":"linear"} ] }"#,
        ));
        assert_eq!(m.params.len(), 2);
        assert_eq!(m.params[1].const_name, "P_WAVEFORM");
        assert_eq!(m.params[1].index, 1);
        assert_eq!(m.params[0].curve, "exponential");
        assert_eq!(m.params[0].unit, "Hz");
    }
}
