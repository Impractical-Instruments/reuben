//! Pure introspection for the Patcher skill and every conversational door (ADR-0020,
//! ADR-0044 §3): `describe` the operator set, `describe_patch` a nested instrument's
//! boundary, and `validate` an instrument without touching audio hardware.
//!
//! Descended from `reuben-native`'s CLI module so one implementation serves every consumer:
//! the CLI re-exports this module as `reuben_native::cli`, and the MCP sidecar and the web
//! player call it directly. Everything here is a pure function over [`Registry`] + JSON
//! through the real load/plan code paths, so introspection can never drift from what the
//! engine accepts. `validate` returns the contract [`Report`] every door serializes
//! (ADR-0048 §§4–5); the view types derive `schemars::JsonSchema` behind the same
//! default-off `schemars` feature as the contract types, so rmcp can emit `outputSchema`
//! without the play/CLI build paying for it.

use crate::contract::{Diag, Report};
use crate::describe::{describe_boundary, BoundaryPortDesc};
use crate::descriptor::{Curve, Descriptor, Port, PortType};
use crate::format::{load_instrument, load_instrument_doc, DocValue, NormalizedDoc};
use crate::plan::{Plan, PlanError};
use crate::registry::Registry;
use crate::resources::ResourceResolver;
use crate::AudioConfig;

use serde::Serialize;

/// One operator's self-description, flattened from its [`Descriptor`] for agent grounding.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct OperatorInfo {
    pub type_name: String,
    /// The whole input surface as one list (mirrors [`Descriptor::inputs`] +
    /// [`constants`](Descriptor::constants)): runtime inputs first, then plan-time `Constant` ports
    /// marked `constant: true`. There is no separate `params`/`enums`/`constants` split — a port's
    /// `kind` and its optional metadata already say whether it is a scalar, integer, or enum.
    pub inputs: Vec<PortInfo>,
    pub outputs: Vec<PortInfo>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<String>,
}

/// One port, flattened from its [`Port`] for agent grounding. Inputs and outputs share this shape;
/// a plan-time [`Constant`](Descriptor::constants) (ADR-0035) is just an input with `constant: true`
/// — an immutable port set in a node's `config` block, never wired in `inputs`. Optional metadata
/// appears only where the port's type carries it: `default`/`min`/`max`/`unit`/`curve` for a swept
/// scalar, `default`/`min`/`max` for an integer, `default`/`variants` for an enum.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct PortInfo {
    pub name: String,
    /// The port's [`PortType`] as the glossary's word: `"value"` (a held `f32` Value),
    /// `"signal"` (a dense `f32_buffer` Signal), `"int"`, `"enum"`, `"message"` (Note),
    /// `"harmony"` (Harmony), `"vocab"`, or `"string"`. The two numeric kinds are one wiring
    /// family with a single implicit bridge: `value` → `signal` materializes (ADR-0031);
    /// the reverse is a hard error.
    pub kind: String,
    /// A plan-time `Constant` (ADR-0035): set in the node's `config` block, not wired in `inputs`.
    /// Omitted (false) for an ordinary runtime input or any output.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub constant: bool,
    /// Unwired default: a number for a scalar/integer control, the variant symbol for an enum.
    /// Omitted for a port with no settable default (audio buffers, `Note`/`Harmony`, outputs).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(skip_serializing_if = "str::is_empty")]
    pub unit: String,
    /// `"linear"` or `"exponential"` for a swept scalar; omitted for non-scalar ports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub curve: Option<String>,
    /// The ordered enum choices (ADR-0030); empty for non-enum ports.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<String>,
    /// Boundary-only (ADR-0038 §3): the logical channel a signal pipe binds — the input channel
    /// an input pipe reads, or the master channel an output pipe feeds, when the instrument is
    /// played at top level. Omitted for unbound pipes and operator ports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<usize>,
}

fn port_kind(ty: &PortType) -> &'static str {
    match ty {
        // The glossary's two numeric forms (ADR-0031): a held `f32` is a Value, a dense
        // `f32_buffer` is a Signal. Wire-compatible one way only — a Value source materializes
        // into a Signal input; a Signal into a Value input is a hard plan error.
        PortType::F32 => "value",
        PortType::F32Buffer => "signal",
        PortType::Vocab { name: "Note", .. } => "message",
        PortType::Vocab {
            name: "Harmony", ..
        } => "harmony",
        PortType::Vocab {
            enum_meta: Some(_), ..
        } => "enum",
        PortType::Vocab { .. } => "vocab",
        PortType::I32 { .. } => "int",
        PortType::Str => "string",
        // The type-agnostic pass-through (issue #141) — any Arg, the `osc_out` sink's input.
        PortType::Arg => "arg",
    }
}

/// Widen an `f32` to `f64` without exposing binary-fraction noise: round-trip through the `f32`'s
/// own shortest decimal so `0.2_f32` serializes as `0.2`, not `0.20000000298…` (the naive `as f64`).
fn widen(v: f32) -> f64 {
    v.to_string().parse().unwrap_or(v as f64)
}

fn curve(c: Curve) -> &'static str {
    match c {
        Curve::Linear => "linear",
        Curve::Exponential => "exponential",
    }
}

impl PortInfo {
    /// Flatten one [`Port`] into its agent-facing view, reading whatever metadata its type carries:
    /// the scalar [`F32Meta`](crate::descriptor::F32Meta) on `Port::meta`, or the integer
    /// range / enum variants embedded in the [`PortType`]. `constant` marks a plan-time
    /// [`Constant`](Descriptor::constants) (ADR-0035) so the consumer routes it to `config`.
    fn from_port(p: &Port, constant: bool) -> Self {
        let mut info = PortInfo {
            name: p.name.to_string(),
            kind: port_kind(&p.ty).to_string(),
            constant,
            default: None,
            min: None,
            max: None,
            unit: String::new(),
            curve: None,
            variants: Vec::new(),
            channel: None,
        };
        // Scalar control (ADR-0030): a materialized `f32` input owns its range/curve/default in `meta`.
        if let Some(m) = &p.meta {
            info.default = Some(serde_json::json!(widen(m.default)));
            info.min = Some(widen(m.min));
            info.max = Some(widen(m.max));
            info.unit = m.unit.to_string();
            info.curve = Some(curve(m.curve).to_string());
        }
        // Type-embedded metadata: an integer's range (ADR-0035) or an enum's named choices (ADR-0030).
        match &p.ty {
            PortType::I32 { meta: Some(m) } => {
                info.default = Some(serde_json::json!(m.default));
                info.min = Some(m.min as f64);
                info.max = Some(m.max as f64);
            }
            PortType::Vocab {
                enum_meta: Some(e), ..
            } => {
                info.default = Some(serde_json::json!(e.default_symbol()));
                info.variants = e.variants.iter().map(|v| v.to_string()).collect();
            }
            _ => {}
        }
        info
    }
}

impl OperatorInfo {
    fn from_descriptor(d: &Descriptor) -> Self {
        // One input surface: runtime inputs, then plan-time `Constant` ports (ADR-0035) flagged
        // `constant`. A port's `kind` + metadata already distinguish scalar / integer / enum, so
        // there is no separate `params`/`enums`/`constants` split to keep in sync.
        let mut inputs: Vec<PortInfo> = d
            .inputs
            .iter()
            .map(|p| PortInfo::from_port(p, false))
            .collect();
        inputs.extend(d.constants.iter().map(|p| PortInfo::from_port(p, true)));
        OperatorInfo {
            type_name: d.type_name.to_string(),
            inputs,
            outputs: d
                .outputs
                .iter()
                .map(|p| PortInfo::from_port(p, false))
                .collect(),
            resources: d.resources.iter().map(|r| r.name.to_string()).collect(),
        }
    }
}

/// Describe the operator set: `which = None` lists every registered operator (deterministic
/// order), `Some(name)` returns just that one — erroring if the registry has no such type.
pub fn describe(registry: &Registry, which: Option<&str>) -> Result<Vec<OperatorInfo>, String> {
    match which {
        None => Ok(registry
            .entries()
            .map(|e| OperatorInfo::from_descriptor(&e.descriptor))
            .collect()),
        Some(name) => match registry.get(name) {
            Some(e) => Ok(vec![OperatorInfo::from_descriptor(&e.descriptor)]),
            None => Err(format!("unknown operator type {name:?}")),
        },
    }
}

/// A nested instrument's synthesized boundary (ADR-0034 §4 / ADR-0038 §2), described **as if it
/// were an Operator**: one [`PortInfo`] per `interface` name — an input pipe from its own declared
/// type/range/default, an output pipe inheriting type and metadata from the internal port feeding
/// it plus optional min/max range overrides (a subset of that port's range). Both carry the entry's
/// presentational fields (label/unit/widget). This is the introspection view of the boundary face a
/// `subpatch` node presents (P6, #121).
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct PatchBoundary {
    /// The document's `instrument` name.
    pub instrument: String,
    pub inputs: Vec<PortInfo>,
    pub outputs: Vec<PortInfo>,
    /// Declared boundary ports whose internal target went dark this load (an unavailable
    /// nested child, ADR-0016/0034) — real ports the description can't type.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dark_inputs: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dark_outputs: Vec<String>,
    /// Non-fatal load warnings (unresolved resources etc.), advisory as in `validate` and
    /// localized the same way (ADR-0048 §4): each carries the offending node when the loader
    /// named one.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<Diag>,
}

impl PatchBoundary {
    /// No boundary port of any kind — typed or dark, either direction. The instrument nests but
    /// exposes nothing to wire. Kept here (not at a call site) so a fifth port collection
    /// can't be forgotten by one of the views.
    pub fn is_empty(&self) -> bool {
        self.inputs.is_empty()
            && self.outputs.is_empty()
            && self.dark_inputs.is_empty()
            && self.dark_outputs.is_empty()
    }
}

impl PortInfo {
    /// The introspection view of one core [`BoundaryPortDesc`] (ADR-0034 §4 / ADR-0038 §2):
    /// [`describe_boundary`] already resolved each pipe (an input pipe's declared metadata, an
    /// output pipe's inherited-then-decorated metadata); this only maps its typed fields onto
    /// the flat agent-facing shape shared with operator ports.
    fn from_boundary(b: BoundaryPortDesc) -> Self {
        PortInfo {
            name: b.name,
            kind: port_kind(&b.ty).to_string(),
            constant: false,
            default: b.default.map(|v| match v {
                DocValue::Number(n) => serde_json::json!(n),
                DocValue::Symbol(s) => serde_json::json!(s),
            }),
            min: b.min,
            max: b.max,
            unit: b.unit,
            curve: b.curve.map(|c| curve(c).to_string()),
            variants: b.variants,
            channel: b.channel,
        }
    }
}

/// Describe an instrument document's boundary the way a host instrument will see it (ADR-0034 §4 /
/// ADR-0038 §2): load it through the real engine path (parsed once), let core's
/// [`describe_boundary`] resolve each pipe (an input pipe from its own declared type/range, an
/// output pipe inheriting from the port feeding it plus optional min/max range overrides — both
/// decorated with the entry's `unit`; presentation lives in a surface doc, ADR-0043), and
/// present each port as an operator-style [`PortInfo`]. `kind` is the pipe's `Arg`
/// type — declared on an input pipe, the feeding port's on an output. An instrument with no
/// `interface` yields empty port lists (it nests, but exposes nothing to wire).
pub fn describe_patch(
    json: &str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<PatchBoundary, String> {
    // Mint WITH the resolver (the same way `play` does): a v1 entry re-exporting a nested
    // child's boundary port migrates to the child's real pipe type; a resolver-less mint
    // would fall back to `"f32"` and this description would diverge from what the
    // engine actually loads.
    let doc =
        NormalizedDoc::from_json(json, registry, Some(resolver)).map_err(|e| e.to_string())?;
    let loaded = load_instrument_doc(&doc, registry, resolver).map_err(|e| e.to_string())?;
    let b = describe_boundary(&doc, &loaded);

    Ok(PatchBoundary {
        instrument: doc.into_inner().instrument,
        inputs: b.inputs.into_iter().map(PortInfo::from_boundary).collect(),
        outputs: b.outputs.into_iter().map(PortInfo::from_boundary).collect(),
        dark_inputs: b.dark_inputs,
        dark_outputs: b.dark_outputs,
        warnings: loaded.warnings.iter().map(Diag::from_warning).collect(),
    })
}

/// Validate an instrument the same way the engine would build it — full load (structural +
/// wiring + kind-checking) plus a `Plan::instantiate` to catch cycles — but with a synthetic
/// [`AudioConfig`], so no audio device is opened and nothing renders. The result is the
/// contract [`Report`] every conversational door serializes (ADR-0048 §§4–5, ADR-0052 §5).
pub fn validate(json: &str, registry: &Registry, resolver: &dyn ResourceResolver) -> Report {
    let loaded = match load_instrument(json, registry, resolver) {
        Ok(l) => l,
        Err(e) => {
            return Report {
                ok: false,
                errors: vec![Diag::from_load(&e)],
                warnings: Vec::new(),
            }
        }
    };

    let warnings = loaded.warnings.iter().map(Diag::from_warning).collect();

    match Plan::instantiate(loaded.graph, AudioConfig::default()) {
        Ok(_) => Report {
            ok: true,
            errors: Vec::new(),
            warnings,
        },
        Err(PlanError::Cycle) => Report {
            ok: false,
            errors: vec![Diag {
                node: None,
                port: None,
                message: "graph has a cycle (no valid execution order)".to_string(),
            }],
            warnings,
        },
        Err(PlanError::FormMismatch { src, dst, reason }) => Report {
            ok: false,
            errors: vec![Diag {
                node: None,
                port: None,
                message: format!("wire {src} → {dst}: {reason}"),
            }],
            warnings,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::{MemoryResolver, ResolveError, ResourceResolver, SampleBuffer};
    use crate::Registry;
    use std::path::PathBuf;

    /// Absolute path to the workspace `instruments/` directory, independent of test CWD.
    fn instruments_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../instruments")
    }

    /// Test-only directory resolver: reads instrument-kind resources (JSON text) relative to a
    /// base directory. Samples never resolve — no introspect test needs decoded audio, and
    /// codecs stay out of this crate (ADR-0007/0016); the WAV-decoding `FsResolver` lives in
    /// reuben-native behind the same [`ResourceResolver`] seam.
    struct DirResolver(PathBuf);

    impl DirResolver {
        fn new(base: impl Into<PathBuf>) -> Self {
            Self(base.into())
        }
    }

    impl ResourceResolver for DirResolver {
        fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
            Err(ResolveError::NotFound(source.to_string()))
        }

        fn resolve_text(&self, source: &str) -> Result<String, ResolveError> {
            let path = self.0.join(source);
            std::fs::read_to_string(&path)
                .map_err(|e| ResolveError::NotFound(format!("{}: {e}", path.display())))
        }
    }

    #[test]
    fn validate_accepts_a_worked_instrument() {
        let dir = instruments_dir();
        let json =
            std::fs::read_to_string(dir.join("chord-player.json")).expect("read chord-player.json");
        let report = validate(&json, &Registry::builtin(), &DirResolver::new(&dir));

        assert!(
            report.ok,
            "chord-player.json should validate: {:?}",
            report.errors
        );
        assert!(
            report.errors.is_empty(),
            "no errors expected: {:?}",
            report.errors
        );
    }

    #[test]
    fn validate_accepts_the_mic_space_example() {
        // The live-input demo (ADR-0038): a channel-bound top-level input pipe feeding the nested
        // space patch. Bound pipes are fed by the input master, so validate must be warning-clean
        // (an *unbound* bare signal pipe would warn).
        let dir = instruments_dir();
        let json =
            std::fs::read_to_string(dir.join("mic-space.json")).expect("read mic-space.json");
        let report = validate(&json, &Registry::builtin(), &DirResolver::new(&dir));
        assert!(
            report.ok && report.errors.is_empty(),
            "mic-space.json should validate: {:?}",
            report.errors
        );
        assert!(
            report.warnings.is_empty(),
            "mic-space.json should validate warning-clean: {:?}",
            report.warnings
        );
    }

    #[test]
    fn validate_rejects_unknown_operator_and_names_the_node() {
        let json = r#"{
          "instrument": "typo",
          "nodes": [ { "type": "oscilllator", "address": "/osc" } ],
          "outputs": []
        }"#;
        let report = validate(json, &Registry::builtin(), &MemoryResolver::new());

        assert!(!report.ok, "unknown operator type should fail validation");
        let err = &report.errors[0];
        assert_eq!(
            err.node.as_deref(),
            Some("/osc"),
            "error should localize the node: {err:?}"
        );
        assert!(
            err.message.contains("oscilllator"),
            "message should name the bad type: {}",
            err.message
        );
    }

    #[test]
    fn validate_rejects_a_cycle_that_loads_cleanly() {
        // Two maps feeding each other: every port/kind is legal, so `load` accepts it — only the
        // plan's topological sort catches the loop. This is why validate instantiates a plan.
        let json = r#"{
          "instrument": "loop",
          "nodes": [
            { "type": "map_f32_signal", "address": "/a", "inputs": { "in": { "from": "/b" } } },
            { "type": "map_f32_signal", "address": "/b", "inputs": { "in": { "from": "/a" } } }
          ],
          "outputs": []
        }"#;
        let report = validate(json, &Registry::builtin(), &MemoryResolver::new());

        assert!(!report.ok, "a cyclic graph should fail validation");
        assert!(
            report.errors[0].message.contains("cycle"),
            "message should mention the cycle: {}",
            report.errors[0].message
        );
    }

    #[test]
    fn validate_treats_a_missing_resource_as_advisory_not_invalid() {
        // ADR-0016/0032: a voice resource that doesn't resolve plays silence rather than failing the
        // load. The instrument is still valid (ok), but the unresolved resource surfaces as a warning.
        let json = r#"{
          "instrument": "ghost",
          "resources": { "ghost-voice": "voices/nope.json" },
          "nodes": [
            { "type": "voicer", "address": "/voicer", "voice": "ghost-voice", "config": { "voices": 1 } },
            { "type": "output", "address": "/out", "inputs": { "audio": {"from":"/voicer.audio"} } }
          ],
          "outputs": [ {"node":"/out","port":"audio"} ]
        }"#;
        let report = validate(
            json,
            &Registry::builtin(),
            &DirResolver::new(instruments_dir()),
        );

        assert!(
            report.ok,
            "missing resource is advisory, instrument is still valid"
        );
        assert!(
            report.errors.is_empty(),
            "no hard errors: {:?}",
            report.errors
        );
        assert_eq!(
            report.warnings.len(),
            1,
            "the unresolved sample should warn: {:?}",
            report.warnings
        );
        // ADR-0048 §4 warning-promotion: warnings are Diags, localized like errors, so the agent
        // jumps to the offending node for a warning exactly as for an error.
        assert_eq!(
            report.warnings[0].node.as_deref(),
            Some("/voicer"),
            "the warning should localize the node: {:?}",
            report.warnings
        );
    }

    #[test]
    fn validate_report_serializes_warnings_as_diag_objects() {
        // The CLI's `validate --json` shape (ADR-0048 §4): warnings are Diag *objects* carrying a
        // `message` (plus node/port localization when the loader named one) — never bare strings.
        let json = r#"{
          "instrument": "ghost",
          "resources": { "ghost-voice": "voices/nope.json" },
          "nodes": [
            { "type": "voicer", "address": "/voicer", "voice": "ghost-voice", "config": { "voices": 1 } },
            { "type": "output", "address": "/out", "inputs": { "audio": {"from":"/voicer.audio"} } }
          ],
          "outputs": [ {"node":"/out","port":"audio"} ]
        }"#;
        let report = validate(
            json,
            &Registry::builtin(),
            &DirResolver::new(instruments_dir()),
        );
        let v = serde_json::to_value(&report).expect("serialize report");

        let w = &v["warnings"][0];
        assert!(
            w.is_object(),
            "a warning serializes as a Diag object, not a bare string: {v}"
        );
        assert!(
            w["message"].is_string(),
            "the Diag carries its human message: {v}"
        );
        assert_eq!(
            w["node"],
            serde_json::json!("/voicer"),
            "the Diag localizes the offending node: {v}"
        );
    }

    #[test]
    fn describe_lists_every_registered_operator() {
        let reg = Registry::builtin();
        let ops = describe(&reg, None).expect("describe all");

        let names: Vec<&str> = ops.iter().map(|o| o.type_name.as_str()).collect();
        for expected in [
            "oscillator",
            "filter",
            "voicer",
            "output",
            "map_f32_signal",
            "m2s",
        ] {
            assert!(names.contains(&expected), "missing {expected} in {names:?}");
        }
        assert_eq!(
            ops.len(),
            reg.type_names().count(),
            "describe lists exactly the registry"
        );
    }

    #[test]
    fn describe_one_operator_surfaces_its_ports_and_params() {
        let ops = describe(&Registry::builtin(), Some("oscillator")).expect("describe oscillator");
        assert_eq!(ops.len(), 1);
        let osc = &ops[0];

        // A scalar control input carries its range/curve/default inline (ADR-0030).
        let freq = osc
            .inputs
            .iter()
            .find(|p| p.name == "freq")
            .expect("freq input");
        assert_eq!(freq.kind, "signal", "osc.freq is a dense f32_buffer Signal");
        assert!(freq.default.is_some() && freq.min.is_some() && freq.max.is_some());
        assert_eq!(freq.curve.as_deref(), Some("exponential"));
        assert!(osc
            .outputs
            .iter()
            .any(|p| p.name == "audio" && p.kind == "signal"));
        // `waveform` is an Enum input (ADR-0030) — one input surface, no separate `enums` list; its
        // variants + default symbol ride on the same `PortInfo`.
        let waveform = osc
            .inputs
            .iter()
            .find(|p| p.name == "waveform")
            .expect("waveform input");
        assert_eq!(waveform.kind, "enum");
        assert_eq!(waveform.variants, ["Sine", "Saw"]);
        assert_eq!(waveform.default, Some(serde_json::json!("Sine")));
    }

    #[test]
    fn describe_speaks_the_glossary_for_the_two_numeric_forms() {
        // Issue #176: a held `f32` is a Value, a dense `f32_buffer` is a Signal (ADR-0031,
        // CONTEXT.md) — `describe` must not collapse both into `"signal"`. The envelope has both:
        // its gate/ADSR inputs are held Values, its `cv` output a per-sample Signal.
        let ops = describe(&Registry::builtin(), Some("envelope")).expect("describe envelope");
        let env = &ops[0];

        for held in ["gate", "attack", "decay", "sustain", "release"] {
            let p = env.inputs.iter().find(|p| p.name == held).expect(held);
            assert_eq!(p.kind, "value", "envelope.{held} is a held f32 Value");
        }
        let cv = env.outputs.iter().find(|p| p.name == "cv").expect("cv");
        assert_eq!(
            cv.kind, "signal",
            "envelope.cv is a dense f32_buffer Signal"
        );
        let active = env
            .outputs
            .iter()
            .find(|p| p.name == "active")
            .expect("active");
        assert_eq!(active.kind, "value", "envelope.active is a held f32 Value");

        // The clock splits the same way: a knob (Value) vs a per-sample ramp (Signal).
        let ops = describe(&Registry::builtin(), Some("clock")).expect("describe clock");
        let clock = &ops[0];
        let tempo = clock
            .inputs
            .iter()
            .find(|p| p.name == "tempo")
            .expect("tempo");
        assert_eq!(tempo.kind, "value", "clock.tempo is a block-rate knob");
        let phase = clock
            .outputs
            .iter()
            .find(|p| p.name == "phase")
            .expect("phase");
        assert_eq!(phase.kind, "signal", "clock.phase is a per-sample ramp");
    }

    #[test]
    fn describe_unknown_operator_errors() {
        let err = describe(&Registry::builtin(), Some("nope")).unwrap_err();
        assert!(
            err.contains("nope"),
            "error should name the missing type: {err}"
        );
    }

    #[test]
    fn describe_patch_surfaces_the_boundary_with_inherited_metadata() {
        // ADR-0034 §4 (P6): a voice patch's `interface` describes as operator-style ports, each
        // inheriting the inner port's type + metadata (default-voice's `freq` targets the
        // oscillator's swept-Hz control, so its range/unit/curve come through).
        let dir = instruments_dir().join("voices");
        let json = std::fs::read_to_string(dir.join("default-voice.json")).expect("read voice");
        let b =
            describe_patch(&json, &Registry::builtin(), &DirResolver::new(&dir)).expect("describe");

        assert_eq!(b.instrument, "default-voice");
        let freq = b.inputs.iter().find(|p| p.name == "freq").expect("freq");
        assert_eq!(freq.kind, "signal", "type inherited from /osc.freq");
        assert_eq!(freq.unit, "Hz", "unit inherited from the inner port");
        assert!(
            freq.min.is_some() && freq.max.is_some() && freq.default.is_some(),
            "range/default inherited: {freq:?}"
        );
        let gate = b.inputs.iter().find(|p| p.name == "gate").expect("gate");
        assert_eq!(
            gate.kind, "value",
            "gate inherits the envelope's held f32 Value, not a Signal (#176)"
        );
        assert!(
            b.outputs.iter().any(|p| p.name == "audio"),
            "boundary outputs surface: {:?}",
            b.outputs
        );
    }

    #[test]
    fn describe_patch_applies_interface_overrides_but_never_the_type() {
        // ADR-0034 §4: quantity overrides (`unit`) decorate the inherited port; the Arg type
        // (`kind`) stays the inner port's truth — there is no way to override it. v1's
        // `label`/`widget` are retired presentation (ADR-0043) — drained with a warning, never
        // described. The v1 range override is validated against the engine-enforced [20..20000]
        // (override law) but NOT migrated onto the pipe: a v2 pipe range is engine-enforced, and
        // v1's was display-only — so `describe` publishes the inner port's range, exactly what
        // the engine clamps to (nothing advertised that the engine wouldn't honor, and vice versa).
        let json = r#"{
          "instrument": "shimmer",
          "interface": {
            "inputs": {
              "brightness": { "target": "/filter.cutoff", "label": "Brightness", "unit": "hertz",
                              "min": 200, "max": 8000, "widget": "knob" }
            },
            "outputs": { "audio": "/filter.audio" }
          },
          "nodes": [ { "type": "filter", "address": "/filter", "inputs": { "cutoff": 2000 } } ]
        }"#;
        let b =
            describe_patch(json, &Registry::builtin(), &MemoryResolver::new()).expect("describe");

        let p = &b.inputs[0];
        assert_eq!(p.name, "brightness");
        assert_eq!(
            p.kind, "signal",
            "kind is the inner cutoff's, not overridable"
        );
        assert_eq!(p.unit, "hertz", "unit override replaces the inner Hz");
        assert_eq!(
            (p.min, p.max),
            (Some(20.0), Some(20000.0)),
            "the engine-enforced (inner-port) range, not the v1 display narrowing"
        );
        assert_eq!(
            p.curve.as_deref(),
            Some("exponential"),
            "un-overridden fields stay inherited"
        );
        assert_eq!(
            p.default,
            Some(serde_json::json!(2000.0)),
            "the default is the effective unwired value — the child's literal, not the descriptor"
        );
    }

    #[test]
    fn describe_patch_refuses_a_range_the_engine_would_not_honor() {
        // Review F1's poster child: presenting a Hz port as a 0..100 "%" knob. The engine would
        // reinterpret those values as raw Hz and clamp to [20..20000] — the advertised contract is
        // a lie, so the loader rejects it and `describe` surfaces the boundary-named error.
        let json = r#"{
          "instrument": "shimmer",
          "interface": {
            "inputs": {
              "brightness": { "target": "/filter.cutoff", "unit": "%", "min": 0, "max": 100 }
            }
          },
          "nodes": [ { "type": "filter", "address": "/filter" } ]
        }"#;
        let err = describe_patch(json, &Registry::builtin(), &MemoryResolver::new())
            .expect_err("lying range must not describe");
        assert!(err.contains("brightness"), "boundary-named: {err}");
        assert!(err.contains("engine-enforced range"), "{err}");
    }

    #[test]
    fn describe_patch_drops_an_internally_driven_v1_boundary_input() {
        // ADR-0038: v1 could expose an input whose inner Signal port the child drove internally —
        // a port a host could see but never wire (the old `driven` flag). The flip cannot express
        // that state, so migration drops the entry: the boundary lists only wireable pipes.
        let json = r#"{
          "instrument": "self-fed",
          "interface": { "inputs": { "in": "/filter.audio", "tone": "/filter.cutoff" } },
          "nodes": [
            { "type": "oscillator", "address": "/osc" },
            { "type": "filter", "address": "/filter", "inputs": { "audio": { "from": "/osc.audio" } } }
          ]
        }"#;
        let b =
            describe_patch(json, &Registry::builtin(), &MemoryResolver::new()).expect("describe");
        let names: Vec<&str> = b.inputs.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            ["tone"],
            "the internally-driven `in` entry is dropped by migration; `tone` stays wireable"
        );
    }

    #[test]
    fn describe_patch_without_interface_yields_an_empty_boundary() {
        let json = r#"{ "instrument": "plain",
          "nodes": [ { "type": "oscillator", "address": "/osc" } ] }"#;
        let b =
            describe_patch(json, &Registry::builtin(), &MemoryResolver::new()).expect("describe");
        assert!(b.is_empty());
    }

    /// ADR-0044 §3 / ADR-0048 §3: rmcp derives tool `outputSchema`s from these view types via
    /// schemars, under the same default-off feature as the contract types. Run with
    /// `--features schemars`.
    #[cfg(feature = "schemars")]
    #[test]
    fn view_types_expose_json_schemas_under_the_feature() {
        let schema = serde_json::to_value(schemars::schema_for!(OperatorInfo)).expect("schema");
        let props = schema["properties"]
            .as_object()
            .expect("OperatorInfo schema has properties");
        for field in ["type_name", "inputs", "outputs", "resources"] {
            assert!(props.contains_key(field), "missing {field}: {schema}");
        }
        // The port lists reference the shared PortInfo shape.
        let port = &schema["$defs"]["PortInfo"]["properties"];
        for field in ["name", "kind", "default", "min", "max", "variants"] {
            assert!(
                port.as_object().is_some_and(|p| p.contains_key(field)),
                "PortInfo schema missing {field}: {schema}"
            );
        }

        let schema = serde_json::to_value(schemars::schema_for!(PatchBoundary)).expect("schema");
        let props = schema["properties"]
            .as_object()
            .expect("PatchBoundary schema has properties");
        for field in ["instrument", "inputs", "outputs", "warnings"] {
            assert!(props.contains_key(field), "missing {field}: {schema}");
        }
    }

    #[test]
    fn a_boundary_with_only_dark_ports_is_not_empty() {
        // Review A: the empty-boundary banner once checked three of the four port collections, so a
        // patch whose only entries went dark (unavailable nested child) printed "exposes nothing to
        // wire" and then listed the dark outputs it just denied. `is_empty` owns the definition.
        let json = r#"{
          "instrument": "dark-out",
          "resources": { "v": "missing-child.json" },
          "interface": { "outputs": { "out": "/sub.audio" } },
          "nodes": [ { "type": "subpatch", "address": "/sub", "patch": "v" } ]
        }"#;
        let b =
            describe_patch(json, &Registry::builtin(), &MemoryResolver::new()).expect("describe");
        assert_eq!(b.dark_outputs, vec!["out".to_string()]);
        assert!(!b.is_empty(), "dark-only boundary still exposes ports");
    }
}
