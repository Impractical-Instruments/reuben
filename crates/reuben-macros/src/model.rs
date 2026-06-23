//! The pure core of `operator_contract!` (ADR-0025): turn a validated [`OperatorSpec`] into a
//! [`ContractModel`] — every const name, per-kind ordinal, and param index resolved. No tokens,
//! no spans, just data, so the index arithmetic (the old `scaffold::port_consts` hand-logic) is
//! computed **once** here and unit-tested directly.

use reuben_contract::{naming, LaneSpec, OperatorSpec, PortSpec};

/// One resolved port: its index const (`IN_FREQ`), its per-kind ordinal, and its source name/kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortModel {
    pub const_name: String,
    pub ordinal: usize,
    pub name: String,
    pub kind: String,
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

/// Number a set of ports **within each kind** (ADR-0010): a message and a context input both
/// start at ordinal 0, two signal outputs are 0 and 1. This is the single home of that counting
/// — it used to live, hand-written, in `scaffold::port_consts`.
fn port_models(ports: &[PortSpec], prefix: &str) -> Vec<PortModel> {
    let (mut sig, mut msg, mut ctx) = (0usize, 0usize, 0usize);
    ports
        .iter()
        .map(|p| {
            let ordinal = match p.kind.as_str() {
                "message" => {
                    let i = msg;
                    msg += 1;
                    i
                }
                "context" => {
                    let i = ctx;
                    ctx += 1;
                    i
                }
                _ => {
                    let i = sig;
                    sig += 1;
                    i
                }
            };
            PortModel {
                const_name: format!("{prefix}_{}", naming::screaming(&p.name)),
                ordinal,
                name: p.name.clone(),
                kind: p.kind.clone(),
            }
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

    // Tracer step 1: one signal input -> IN_FREQ at ordinal 0.
    #[test]
    fn single_signal_input_is_ordinal_zero() {
        let m = build(&spec(
            r#"{ "type_name": "oscillator", "inputs": [ {"name":"freq","kind":"signal"} ] }"#,
        ));
        assert_eq!(m.inputs[0].const_name, "IN_FREQ");
        assert_eq!(m.inputs[0].ordinal, 0);
    }

    // Tracer step 2: per-kind ordinals — the voicer footgun. A message input and a context input
    // BOTH land at ordinal 0 (separate index spaces), two signal outputs are 0 and 1.
    #[test]
    fn ports_are_numbered_per_kind() {
        let m = build(&spec(
            r#"{ "type_name": "voicer",
                 "inputs": [ {"name":"notes","kind":"message"}, {"name":"ctx","kind":"context"} ],
                 "outputs": [ {"name":"freq","kind":"signal"}, {"name":"gate","kind":"signal"} ] }"#,
        ));
        assert_eq!(
            (m.inputs[0].const_name.as_str(), m.inputs[0].ordinal),
            ("IN_NOTES", 0)
        );
        assert_eq!(
            (m.inputs[1].const_name.as_str(), m.inputs[1].ordinal),
            ("IN_CTX", 0)
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

    // Tracer step 3: params index sequentially and keep their metadata; FromParam resolves to the
    // param's const name.
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

    // Tracer step 3 (cont): a six-param shape (the `map` profile) with mixed curves and units.
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
