//! Pure introspection for the Patcher skill and every conversational door (ADR-0020,
//! ADR-0044 §3): `describe` the operator set (full port objects, or the compact
//! signature-line mode `describe_compact`, ADR-0059 §3), `describe_patch` a nested
//! instrument's boundary, `validate` an instrument without touching audio hardware, and
//! project an instrument's `library_index_line` — the generated library index's
//! signature line (ADR-0057 §4).
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
use crate::format::{load_instrument, load_instrument_doc, DocValue, NormalizedDoc, PipeDefault};
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

impl PortInfo {
    /// This input's fragment of a compact [`signature`](OperatorInfo::signature) line
    /// (ADR-0059 §3): `name:kind`, then only the metadata the port actually carries —
    /// `[variants]` for an enum, unit, `exp` for an exponential curve (linear is unmarked),
    /// `lo..hi`, `=default`. The `±1e6` "effectively unbounded" sentinel range
    /// ([`reuben_contract::NUMBER_MIN`]/[`NUMBER_MAX`](reuben_contract::NUMBER_MAX)) is
    /// suppressed — it states no authoring intent, and it would swamp the listing (every bare
    /// math operand carries it). Everything is read off the same flattened fields the full
    /// JSON view serializes, so the two modes cannot disagree about a port.
    fn signature_fragment(&self) -> String {
        let mut s = format!("{}:{}", self.name, self.kind);
        if !self.variants.is_empty() {
            s.push_str(&format!("[{}]", self.variants.join(",")));
        }
        if !self.unit.is_empty() {
            s.push_str(&format!(" {}", self.unit));
        }
        if self.curve.as_deref() == Some("exponential") {
            s.push_str(" exp");
        }
        if let (Some(min), Some(max)) = (self.min, self.max) {
            let unbounded = min == f64::from(reuben_contract::NUMBER_MIN)
                && max == f64::from(reuben_contract::NUMBER_MAX);
            if !unbounded {
                s.push_str(&format!(" {min}..{max}"));
            }
        }
        if let Some(d) = &self.default {
            match d {
                // An enum default is its variant symbol — render it bare, not `"quoted"`.
                serde_json::Value::String(symbol) => s.push_str(&format!("={symbol}")),
                // A numeric default renders shortest (`440`, not JSON's `440.0`).
                other => match other.as_f64() {
                    Some(f) => s.push_str(&format!("={f}")),
                    None => s.push_str(&format!("={other}")),
                },
            }
        }
        s
    }
}

impl OperatorInfo {
    /// This operator's compact one-line signature (ADR-0059 §3; grounding-audit option 2a), e.g.
    /// `filter(audio:signal, cutoff:signal Hz exp 20..20000=1000, …) -> audio:signal`. Runtime
    /// inputs come first; plan-time constants group under `config:` (they are set in the node's
    /// `config` block, never wired); resource slots group under `res:`; a pure sink renders with
    /// no arrow. Outputs render bare `name:kind` — what wiring needs; an output's range/default
    /// metadata is not an authoring lever, and full describe stays the zoom for it. Notation is
    /// keyed by [`COMPACT_DESCRIBE_LEGEND`]. Rendered from the same flattened view as the full
    /// JSON mode — one source, two projections (ADR-0051).
    pub fn signature(&self) -> String {
        let group = |constant: bool| -> Vec<String> {
            self.inputs
                .iter()
                .filter(|p| p.constant == constant)
                .map(PortInfo::signature_fragment)
                .collect()
        };
        let mut groups: Vec<String> = Vec::new();
        let inputs = group(false);
        if !inputs.is_empty() {
            groups.push(inputs.join(", "));
        }
        let constants = group(true);
        if !constants.is_empty() {
            groups.push(format!("config: {}", constants.join(", ")));
        }
        if !self.resources.is_empty() {
            groups.push(format!("res: {}", self.resources.join(", ")));
        }
        let mut s = format!("{}({})", self.type_name, groups.join("; "));
        if !self.outputs.is_empty() {
            let outputs: Vec<String> = self
                .outputs
                .iter()
                .map(|p| format!("{}:{}", p.name, p.kind))
                .collect();
            s.push_str(&format!(" -> {}", outputs.join(", ")));
        }
        s
    }

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

/// The one-line notation key for the compact describe listing (ADR-0059 §3). Consumers that ship
/// the listing as standalone grounding (the web build's bundled prefix, the CLI's human view)
/// prepend this line so the notation is self-describing; consumers with their own prose home for
/// it (an MCP tool description) may carry the gist there instead.
pub const COMPACT_DESCRIBE_LEGEND: &str = "One line per operator: \
name(inputs; config: constants; res: resource-slots) -> outputs. Each port is name:kind, an enum \
lists [variants], a numeric port appends unit, exp (exponential curve; linear unmarked), lo..hi, \
=default. config: ports are plan-time constants set in the node's `config` block, never wired; \
res: slots name `resources` entries the node binds. Describe one operator by name for full detail.";

/// Compact describe — a generated mode of the verb (ADR-0059 §3; grounding-audit option 2a): the
/// same registry truth as [`describe`], projected to one [`signature`](OperatorInfo::signature)
/// line per operator instead of full port objects. It delegates to [`describe`] and renders its
/// flattened view, so the two modes cannot list different operator sets — a new operator appears
/// in both by construction (never a hand-written digest, ADR-0051). Full describe remains the
/// in-session zoom tool; this is the listing that earns a place in a bundled prefix
/// (~2–3k tokens full-registry vs ~9.9k, reuben-web#96 corrected figures).
pub fn describe_compact(registry: &Registry, which: Option<&str>) -> Result<Vec<String>, String> {
    Ok(describe(registry, which)?
        .iter()
        .map(OperatorInfo::signature)
        .collect())
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

/// One instrument's **library-index signature line** (ADR-0057 §4): name, recipe-role line, and
/// interface face —
///
/// ```text
/// kick-body — pitch-drop kick/tom body; gate-driven. (gate:f32=0, base:f32 Hz=48) → audio, active
/// ```
///
/// Projected mechanically from the document alone, through the real load path (the same
/// projection family as [`describe_patch`]): the role line is the top-level `doc` field's first
/// sentence (ADR-0057 §3 — trusted for selection only, never for wiring), and the face is read
/// off the post-mint `interface` block — each input pipe's declared `type`/`unit`/`default`
/// (min/max/curve/channel stay in the full [`describe_patch`] view, the on-demand fallback),
/// then the output pipe names. A document that fails to load gets no line: the index never
/// vouches for an instrument the engine would refuse.
pub fn library_index_line(
    json: &str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<String, String> {
    // Mint + load exactly like `describe_patch`: migration rewrites v1 interface forms into
    // pipes, and the full load enforces the face the line advertises.
    let doc =
        NormalizedDoc::from_json(json, registry, Some(resolver)).map_err(|e| e.to_string())?;
    load_instrument_doc(&doc, registry, resolver).map_err(|e| e.to_string())?;

    let mut line = format!("{} —", doc.instrument);
    if let Some(role) = doc
        .doc
        .as_deref()
        .map(first_sentence)
        .filter(|s| !s.is_empty())
    {
        line.push(' ');
        line.push_str(&role);
    }

    let mut inputs: Vec<String> = Vec::new();
    let mut outputs: Vec<&str> = Vec::new();
    if let Some(iface) = doc.interface.as_ref() {
        for (name, entry) in &iface.inputs {
            // Post-mint, every input entry is a Pipe (the mint migrates or rejects v1 forms).
            let pipe = entry.pipe().expect("a minted input entry is a pipe");
            let mut part = format!("{name}:{}", pipe.ty);
            if let Some(unit) = pipe.unit.as_deref().filter(|u| !u.is_empty()) {
                part.push(' ');
                part.push_str(unit);
            }
            match &pipe.default {
                // `f64` Display is the shortest round-trip decimal: `48`, `0.4` — never `48.0`.
                Some(PipeDefault::Number(n)) => part.push_str(&format!("={n}")),
                Some(PipeDefault::Symbol(s)) => part.push_str(&format!("={s}")),
                None => {}
            }
            inputs.push(part);
        }
        outputs.extend(iface.outputs.keys().map(String::as_str));
    }

    line.push_str(&format!(" ({})", inputs.join(", ")));
    if !outputs.is_empty() {
        line.push_str(&format!(" → {}", outputs.join(", ")));
    }
    Ok(line)
}

/// The `doc` field's first sentence — the recipe-role line (ADR-0057 §3). Whitespace-normalized
/// (the line format is one instrument per line), ending at the first `.` whose successor is
/// whitespace or end-of-text, so a `.` inside a token (`patches/space.json`, `e.g.`… followed by
/// more of the same sentence) never truncates. A doc with no sentence-ending period is one
/// sentence — returned whole.
fn first_sentence(doc: &str) -> String {
    let text = doc.split_whitespace().collect::<Vec<_>>().join(" ");
    let end = text
        .char_indices()
        .find(|&(i, c)| {
            c == '.'
                && text[i + 1..]
                    .chars()
                    .next()
                    .is_none_or(|next| next.is_whitespace())
        })
        .map(|(i, _)| i);
    match end {
        Some(i) => text[..=i].to_string(),
        None => text,
    }
}

/// Split a `"{node.address}.{port}"` wire-ref — the shape [`PlanError::FormMismatch`] carries in
/// its `src`/`dst` — into the structured `(node, port)` a [`Diag`] localizes on (ADR-0048 §4).
/// Node addresses carry no `.`, so the last `.` separates node from port (the same rule as
/// `format`'s `parse_wire`); a bare node ref with no port degrades to node-only.
fn localize_wire_ref(reference: &str) -> (Option<String>, Option<String>) {
    match reference.rsplit_once('.') {
        Some((node, port)) => (Some(node.to_string()), Some(port.to_string())),
        None => (Some(reference.to_string()), None),
    }
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
        Err(PlanError::FormMismatch { src, dst, reason }) => {
            // Localize to the destination endpoint (ADR-0048 §4): a form mismatch is rejected at
            // the *input* port that can't accept the source's form (Signal→Value, Event→Signal,
            // …), so `dst` is the node an agent must go fix — exactly as every `LoadError` with a
            // known input localizes to its node.
            //
            // This arm is a **defensive backstop**: the load-time wiring check (`format`'s
            // `compatible` gate, ADR-0034 §5) rejects every *reachable* form/type mismatch first,
            // in boundary terms, before `Plan::instantiate` ever runs its form check — see
            // `mistyped_boundary_wire_fails_at_load_not_at_instantiate`. So `validate` only reaches
            // here if some future path hands `instantiate` a form-mismatched graph that skipped
            // load. It still localizes like every other Diag if it ever does.
            let (node, port) = localize_wire_ref(&dst);
            Report {
                ok: false,
                errors: vec![Diag {
                    node,
                    port,
                    message: format!("wire {src} → {dst}: {reason}"),
                }],
                warnings,
            }
        }
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
    fn scaffold_instrument_produces_a_document_validate_accepts() {
        // The #146 fix: the scaffolded minimal document must clear the same `validate` path a
        // first-creation stall fails — proving the emitted `{format_version, instrument, nodes:[]}`
        // is a valid seed the model can edit-then-swap, not a shape it has to guess.
        let doc = crate::format::scaffold_instrument(Some("my-synth"));
        assert_eq!(
            doc,
            serde_json::json!({
                "format_version": 3,
                "instrument": "my-synth",
                "nodes": []
            }),
            "the scaffold emits exactly the minimal required document"
        );
        let json = serde_json::to_string(&doc).expect("serialize scaffold document");
        let report = validate(&json, &Registry::builtin(), &MemoryResolver::new());
        assert!(
            report.ok,
            "the scaffolded document must validate ok: {:?}",
            report.errors
        );
        assert!(
            report.errors.is_empty(),
            "no errors expected from a scaffold: {:?}",
            report.errors
        );

        // A bare scaffold uses the default name and still validates.
        let default = crate::format::scaffold_instrument(None);
        assert_eq!(default["instrument"], serde_json::json!("untitled"));
        let json = serde_json::to_string(&default).expect("serialize default scaffold");
        assert!(validate(&json, &Registry::builtin(), &MemoryResolver::new()).ok);
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
    fn localize_wire_ref_splits_the_offending_node_and_port() {
        // The FormMismatch backstop localizes its Diag by splitting the `dst` wire-ref (ADR-0048
        // §4). A load-shadowed arm has no reachable document to drive it (every form mismatch is a
        // load-time TypeMismatch first — `mistyped_boundary_wire_fails_at_load_not_at_instantiate`
        // in `format`), so the localization logic is pinned here on the pure split it performs.
        assert_eq!(
            localize_wire_ref("/lfo.rate"),
            (Some("/lfo".to_string()), Some("rate".to_string())),
            "a plain node.port ref splits into node + port"
        );
        assert_eq!(
            localize_wire_ref("/sub/inner.freq"),
            (Some("/sub/inner".to_string()), Some("freq".to_string())),
            "slashes belong to the node address; only the last `.` splits"
        );
        assert_eq!(
            localize_wire_ref("/osc"),
            (Some("/osc".to_string()), None),
            "a bare node ref with no port degrades to node-only"
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
    fn describe_compact_lists_exactly_the_registry() {
        // R2's registry-parity acceptance (reuben#459, ADR-0059 §3): compact is generated from
        // the same source as full describe — a new operator appears in both or this fails. The
        // two projections must agree entry-for-entry, in the same deterministic order.
        let reg = Registry::builtin();
        let lines = describe_compact(&reg, None).expect("compact all");
        let ops = describe(&reg, None).expect("describe all");

        assert_eq!(
            lines.len(),
            reg.type_names().count(),
            "compact lists exactly the registry"
        );
        for (line, op) in lines.iter().zip(&ops) {
            assert!(
                line.starts_with(&format!("{}(", op.type_name)),
                "compact line must open with the full view's operator: {} vs {line}",
                op.type_name
            );
        }
    }

    #[test]
    fn describe_compact_one_operator_matches_the_full_listing_line() {
        // One projection, two entry points: the per-name compact line is byte-identical to that
        // operator's line in the full-registry listing — the filter selects, it never re-renders.
        let reg = Registry::builtin();
        let all = describe_compact(&reg, None).expect("compact all");
        let one = describe_compact(&reg, Some("filter")).expect("compact filter");

        assert_eq!(one.len(), 1);
        assert!(
            all.contains(&one[0]),
            "the single-operator line must appear verbatim in the full listing: {}",
            one[0]
        );
    }

    #[test]
    fn describe_compact_unknown_operator_errors() {
        let err = describe_compact(&Registry::builtin(), Some("nope")).unwrap_err();
        assert!(
            err.contains("nope"),
            "error should name the missing type: {err}"
        );
    }

    #[test]
    fn compact_signature_carries_the_wiring_essentials() {
        // The signature line carries what wiring needs (grounding-audit option 2a's shape):
        // port kinds, a swept scalar's unit/curve/range/default, an enum's variants + default,
        // and the named outputs.
        let line = &describe_compact(&Registry::builtin(), Some("filter")).expect("filter")[0];

        assert!(line.contains("audio:signal"), "input kind: {line}");
        assert!(
            line.contains("cutoff:signal Hz exp 20..20000=1000"),
            "unit + exponential curve + range + default: {line}"
        );
        assert!(
            line.contains("mode:enum[Lp,Hp,Bp]=Lp"),
            "enum variants + default symbol: {line}"
        );
        assert!(
            line.ends_with("-> audio:signal"),
            "named, kinded outputs after the arrow: {line}"
        );
    }

    #[test]
    fn compact_signature_groups_constants_and_resources() {
        // A plan-time Constant (ADR-0035) routes to the node's `config` block and a resource
        // slot (ADR-0016) to a `resources` entry — the signature must say so, or the compact
        // grounding teaches un-loadable documents. The voicer carries both.
        let line = &describe_compact(&Registry::builtin(), Some("voicer")).expect("voicer")[0];

        assert!(
            line.contains("config: voices:int"),
            "the voices pool size is a config: constant: {line}"
        );
        assert!(line.contains("res: voice"), "the voice slot: {line}");
        // Constants live inside the parens — part of the authoring surface, not an output.
        let parens = &line[line.find('(').unwrap()..line.find(')').unwrap()];
        assert!(
            parens.contains("config:") && parens.contains("res:"),
            "config/res group inside the parens: {line}"
        );
    }

    #[test]
    fn compact_signature_suppresses_the_unbounded_sentinel_range() {
        // The ±1e6 sentinel (`reuben_contract::NUMBER_MIN/MAX`) means "effectively unbounded" —
        // it states no authoring intent, so the compact projection drops the range (the default
        // stays: it is the unwired value). Every bare math operand would otherwise carry 24
        // chars of noise. A real range (the clamp's own defaults) still renders.
        let reg = Registry::builtin();
        let add = &describe_compact(&reg, Some("add_f32_value")).expect("add")[0];
        assert!(
            add.contains("a:value=0") && !add.contains(".."),
            "sentinel range suppressed, default kept: {add}"
        );

        let osc = &describe_compact(&reg, Some("oscillator")).expect("osc")[0];
        assert!(
            osc.contains("20..20000"),
            "a declared range still renders: {osc}"
        );
    }

    #[test]
    fn compact_signature_of_a_sink_has_no_arrow() {
        // A pure sink (`osc_out`) has nothing after the parens — no dangling `->`.
        let line = &describe_compact(&Registry::builtin(), Some("osc_out")).expect("osc_out")[0];
        assert!(!line.contains("->"), "no arrow on a sink: {line}");
    }

    #[test]
    fn compact_full_registry_fits_the_grounding_budget() {
        // R2's sizing-sanity acceptance (reuben#459), zero-token per the tier-1 rules: the
        // re-baseline correction table (reuben-web#96) measured full-registry describe at
        // ~9.9k tok and calibrated dense text at ≈ chars/2.0–2.2, so ≤6,000 chars keeps the
        // compact listing ≤ ~3k tok at the conservative end of the audit's 2–3k target
        // (5,399 chars ≈ 2.5–2.7k tok when this landed). The relative gate scales with the
        // registry: compact must stay under a third of the full minified view, or it no longer
        // earns the name.
        let reg = Registry::builtin();
        let compact = describe_compact(&reg, None)
            .expect("compact all")
            .join("\n");
        let full = serde_json::to_string(&describe(&reg, None).expect("describe all"))
            .expect("serialize full describe");

        let compact_chars = compact.chars().count();
        assert!(
            compact_chars <= 6_000,
            "compact full-registry listing must stay ≤ ~3k tok (≤6,000 chars at chars/2.0, \
             reuben-web#96); measured {compact_chars} chars"
        );
        assert!(
            compact_chars * 3 <= full.chars().count(),
            "compact must stay under a third of the full minified view: {compact_chars} vs {}",
            full.chars().count()
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

    #[test]
    fn library_index_line_projects_name_role_line_and_face() {
        // ADR-0057 §4: name — role line (the `doc` first sentence) — face (declared input
        // pipes) → output pipe names, through the real load path.
        let dir = instruments_dir().join("voices");
        let json = std::fs::read_to_string(dir.join("kick-voice.json")).expect("read voice");
        let line = library_index_line(&json, &Registry::builtin(), &DirResolver::new(&dir))
            .expect("index line");
        assert_eq!(
            line,
            "kick-voice — Pitch-drop drum body: one gate-fired envelope both \
drops the pitch of a sine body (base + decaying sweep) and shapes its amplitude through the \
nested shaped-vca -- the classic kick/tom 'thump'. (attack:f32 s=0.001, base:f32 Hz=48, \
decay:f32 s=0.1, gate:f32=0, release:f32 s=0.08, sustain:f32=0, sweep:f32 Hz=220) → active, audio"
        );
    }

    #[test]
    fn library_index_line_renders_declared_units_and_symbol_defaults() {
        // The face speaks the interface block's own declarations: `name:type unit=default`,
        // with an enum pipe's default as its variant symbol. min/max/curve/channel stay in the
        // full describe_patch view — the index line is the ~30–60-token selection signature.
        let json = r#"{
          "format_version": 3,
          "instrument": "sig",
          "doc": "Signature fixture.",
          "interface": {
            "inputs": {
              "base": { "type": "f32", "unit": "Hz", "default": 48.0, "min": 20.0, "max": 2000.0 },
              "decay": { "type": "f32", "unit": "s", "default": 0.4, "min": 0.0, "max": 2.0 },
              "mode": { "type": "FilterMode", "default": "Lp" },
              "in": { "type": "f32_buffer" }
            },
            "outputs": { "audio": { "from": "/osc.audio" } }
          },
          "nodes": [
            { "type": "oscillator", "address": "/osc", "inputs": { "freq": { "from": "/base" } } }
          ]
        }"#;
        let line = library_index_line(json, &Registry::builtin(), &MemoryResolver::new())
            .expect("index line");
        assert_eq!(
            line,
            "sig — Signature fixture. \
             (base:f32 Hz=48, decay:f32 s=0.4, in:f32_buffer, mode:FilterMode=Lp) → audio"
        );
    }

    #[test]
    fn library_index_line_role_ends_at_the_sentence_not_inside_a_token() {
        // A `.` inside a token (`patches/space.json`) never truncates the role line — only a
        // `.` followed by whitespace (or end) ends the sentence; whitespace normalizes to
        // single spaces so the artifact stays one instrument per line.
        let json = r#"{
          "format_version": 3,
          "instrument": "nester",
          "doc": "Nests patches/space.json\n   into one demo. Everything after is not the role.",
          "nodes": [ { "type": "oscillator", "address": "/osc" } ]
        }"#;
        let line = library_index_line(json, &Registry::builtin(), &MemoryResolver::new())
            .expect("index line");
        assert_eq!(line, "nester — Nests patches/space.json into one demo. ()");
    }

    #[test]
    fn library_index_line_without_doc_or_interface_degrades_to_the_bare_face() {
        // No `doc` → no role line to project (quality is authoring, ADR-0057 §4 — the index
        // never invents one); no `interface` → an empty face, honestly rendered.
        let json = r#"{ "format_version": 3, "instrument": "plain",
          "nodes": [ { "type": "oscillator", "address": "/osc" } ] }"#;
        let line = library_index_line(json, &Registry::builtin(), &MemoryResolver::new())
            .expect("index line");
        assert_eq!(line, "plain — ()");
    }

    #[test]
    fn library_index_line_refuses_a_document_that_does_not_load() {
        // The index never vouches for an instrument the engine would refuse: a load error is a
        // generation error, not a silently missing or lying line.
        let json = r#"{ "format_version": 3, "instrument": "typo",
          "nodes": [ { "type": "oscilllator", "address": "/osc" } ] }"#;
        let err = library_index_line(json, &Registry::builtin(), &MemoryResolver::new())
            .expect_err("a broken document must not index");
        assert!(err.contains("oscilllator"), "names the bad type: {err}");
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
