//! The contract serde types every conversational door serializes (one schema,
//! two doors): [`Diag`]/[`Report`], the swap [`DiffSummary`] and [`SwapReport`],
//! and the [`content_hash`] over a document's canonical bytes.
//!
//! These live OS-free in core — not in reuben-native or reuben-mcp — so the wasm lane reuses
//! the exact types the native lane serializes. Every type derives serde both ways, plus
//! `schemars::JsonSchema` behind the default-off `schemars` feature so rmcp can emit
//! `outputSchema` without the play/CLI build paying for it (feature fencing).

use serde::{Deserialize, Serialize};

use crate::format::{LoadError, LoadWarning, NormalizedDoc};

/// The content identity of a normalized document: a hash over the canonical
/// [`to_json_pretty`](crate::format::InstrumentDoc::to_json_pretty) bytes — the exact bytes
/// a save writes — so two equal [`NormalizedDoc`]s hash equal regardless of how their source
/// text was formatted. Every `SwapReport`/`get_document` response carries it; a swap's
/// `expect` guard compares it; a future store may dedup by it — but a store
/// deduping by it must byte-verify: the hash is not cryptographic, so equal tokens are a
/// candidate match, not proof of identical content.
///
/// The string is an **opaque token**: compare it for equality, never parse it. The algorithm
/// is deliberately unspecified in the contract (the mechanism is left epic-level) and
/// carries no cryptographic claim — it guards against accident (a stale `expect`), not attack.
pub fn content_hash(doc: &NormalizedDoc) -> String {
    format!("{:016x}", fnv1a_64(doc.to_json_pretty().as_bytes()))
}

/// FNV-1a, 64-bit: a tiny, well-known, platform-stable content fingerprint. Chosen over a
/// crypto hash because core stays dependency-free and the contract
/// needs accident-detection, not adversary-resistance; chosen over `std`'s `DefaultHasher`
/// because that one is documented to vary across releases, and this token may outlive a
/// process (expect-guards across conversations, dedup in a future store).
fn fnv1a_64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    bytes.iter().fold(OFFSET_BASIS, |hash, &b| {
        (hash ^ u64::from(b)).wrapping_mul(PRIME)
    })
}

/// One diagnostic — an error or a warning — with the offending node/port when the loader
/// localized it, so an agent can jump straight to the offending node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct Diag {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<String>,
    pub message: String,
}

impl Diag {
    /// Carry the loader's human message verbatim, but pull the node/port the loader already
    /// localized into structured fields so an agent can jump straight to the offending node.
    pub fn from_load(e: &LoadError) -> Self {
        let (node, port) = match e {
            LoadError::UnknownType { address, .. } => (Some(address.clone()), None),
            LoadError::DuplicateAddress(a) | LoadError::UnknownNode(a) => (Some(a.clone()), None),
            LoadError::UnknownPort { node, port } => (Some(node.clone()), Some(port.clone())),
            LoadError::UnknownInput { node, input } => (Some(node.clone()), Some(input.clone())),
            LoadError::BadInputValue { node, input, .. } => {
                (Some(node.clone()), Some(input.clone()))
            }
            LoadError::UnknownConfig { node, .. }
            | LoadError::ConstantInInputs { node, .. }
            | LoadError::AmbiguousWire { node, .. }
            | LoadError::UnknownResource { node, .. } => (Some(node.clone()), None),
            // A boundary-named problem: the offending "node" is the interface entry itself.
            LoadError::InterfaceOverride { name, .. } | LoadError::InterfacePipe { name, .. } => {
                (None, Some(name.clone()))
            }
            LoadError::TypeMismatch { .. }
            | LoadError::Json(_)
            | LoadError::CyclicResource { .. }
            | LoadError::UnsupportedVersion { .. }
            | LoadError::AnonymousOutputs => (None, None),
        };
        Diag {
            node,
            port,
            message: e.to_string(),
        }
    }

    /// Promote a non-fatal [`LoadWarning`] to a localized `Diag`: the warning
    /// variants already carry the offending node or boundary-entry name, so a warning jumps
    /// to its node exactly as an error does. The human message is the warning's `Display`,
    /// verbatim. As in [`from_load`](Self::from_load), a boundary-named problem localizes on
    /// `port` — the interface entry is the offending "node". A [`Nested`](LoadWarning::Nested)
    /// warning localizes on the referencing parent node: that is the address that exists in
    /// *this* document (the inner warning's addresses are child-relative), and the message
    /// already carries the inner story.
    pub fn from_warning(w: &LoadWarning) -> Self {
        let (node, port) = match w {
            LoadWarning::MissingResource { node, .. }
            | LoadWarning::Nested { node, .. }
            | LoadWarning::NoPatchRef { node }
            | LoadWarning::DeprecatedControlBlock { node } => (Some(node.clone()), None),
            LoadWarning::UnwiredPipe { node, name } => (Some(node.clone()), Some(name.clone())),
            // Boundary-named: the offending "node" is the interface entry itself.
            LoadWarning::DarkInterfaceEntry { name, .. }
            | LoadWarning::Migration { name, .. }
            | LoadWarning::UnboundInputPipe { name }
            | LoadWarning::InertChannelBinding { name }
            | LoadWarning::DeprecatedPipePresentation { name, .. } => (None, Some(name.clone())),
            LoadWarning::ResolveFailed { .. } => (None, None),
        };
        Diag {
            node,
            port,
            message: w.to_string(),
        }
    }
}

/// Outcome of validating (or swap-validating) an instrument document:
/// loadable + cycle-free means `ok`. Resource problems are advisory `warnings`
/// and do not flip `ok`; a `{ok: false}` report is a tool *working*, not a tool failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct Report {
    pub ok: bool,
    pub errors: Vec<Diag>,
    pub warnings: Vec<Diag>,
}

/// The swap diff summary (keyed by the survivor fingerprint):
/// what happened to the sounding graph, *announced* rather than discovered by ear.
/// `state_reset` lists addresses present in both documents whose node did **not** survive
/// (a type change or an instantiate-time fingerprint change); `added`/`removed` catch
/// whole-document re-emission accidents — a param tweak reporting `removed: ["/voice1"]`
/// is a typo'd address caught while still fixable. The native lane's gapless swap fills in
/// real survivor stats; the web lane's restart-swap rebuilds every node cold,
/// reported honestly as `survived: 0` behind this same shape.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct DiffSummary {
    pub survived: usize,
    pub state_reset: Vec<String>,
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

/// What a `swap` returns: the validation [`Report`], the
/// **installed** document's [`content_hash`] (on `ok: false` nothing installed — the hash
/// still names what keeps playing), and, on success, the [`DiffSummary`]. The `Report`
/// flattens so the wire shape is one flat object — this one serde type is both the structure
/// channel's response and the MCP tool's `structuredContent` (shapes must not
/// drift).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct SwapReport {
    #[serde(flatten)]
    pub report: Report,
    pub content_hash: String,
    /// Present on a successful install; a rejected swap (`ok: false`) has no old-vs-new to
    /// summarize.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<DiffSummary>,
}

impl SwapReport {
    /// The nothing-was-installed report, defined once.
    ///
    /// A door that rejects a swap *before* the loader runs — the structure channel's `expect`
    /// guard, which answers with a [`Conflict`](crate::coordinator::Conflict) rather than a report
    /// — still owes its caller a `SwapReport`, because the tool surface advertises one flattened
    /// `outputSchema` spanning the install, validation-failure, and guard-miss cases. This
    /// constructor is that report: `ok: false` with **no diagnostics of its own** (the `Conflict`
    /// names the cause, so duplicating it as a `Diag` would say the same thing twice) and no diff
    /// (nothing changed to summarize).
    ///
    /// `content_hash` is **what keeps playing** — the conflict's `actual`, never the `expected` the
    /// client asked for. That contract is the whole reason this lives here instead of being spelled
    /// inline at each door.
    pub fn rejected(content_hash: String) -> Self {
        Self {
            report: Report {
                ok: false,
                errors: vec![],
                warnings: vec![],
            },
            content_hash,
            diff: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Registry;

    /// A minimal current-shape document with one tweakable node input.
    fn doc_json(freq: f64) -> String {
        format!(
            r#"{{ "format_version": 3, "instrument": "t",
                 "nodes": [ {{ "type": "oscillator", "address": "/osc",
                               "inputs": {{ "freq": {freq} }} }} ] }}"#
        )
    }

    fn mint(json: &str) -> NormalizedDoc {
        NormalizedDoc::from_json(json, &Registry::builtin(), None).expect("mint")
    }

    #[test]
    fn diag_from_missing_resource_carries_node() {
        // Warning-promotion: `LoadWarning` already carries the offending node
        // (`MissingResource { node, slot, id }`); the Diag must surface it structured, with
        // the loader's human message verbatim.
        let w = LoadWarning::MissingResource {
            node: "/voicer".to_string(),
            slot: "voice",
            id: "ghost-voice".to_string(),
        };
        let d = Diag::from_warning(&w);
        assert_eq!(d.node.as_deref(), Some("/voicer"));
        assert_eq!(d.port, None);
        assert_eq!(d.message, w.to_string());
    }

    #[test]
    fn report_round_trips_serde() {
        let report = Report {
            ok: false,
            errors: vec![Diag {
                node: Some("/osc".to_string()),
                port: Some("freq".to_string()),
                message: "bad wire".to_string(),
            }],
            warnings: vec![Diag {
                node: None,
                port: None,
                message: "advisory".to_string(),
            }],
        };
        let json = serde_json::to_string(&report).expect("serialize");
        let back: Report = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, report);

        // The wire shape is the contract, not an implementation detail:
        // { ok, errors: Diag[], warnings: Diag[] }, absent node/port omitted.
        let v: serde_json::Value = serde_json::from_str(&json).expect("as value");
        assert_eq!(v["ok"], serde_json::json!(false));
        assert_eq!(v["errors"][0]["node"], serde_json::json!("/osc"));
        assert_eq!(v["errors"][0]["port"], serde_json::json!("freq"));
        assert_eq!(v["warnings"][0]["message"], serde_json::json!("advisory"));
        assert!(
            v["warnings"][0]
                .as_object()
                .is_some_and(|w| !w.contains_key("node")),
            "an unlocalized warning omits node: {v}"
        );
    }

    #[test]
    fn swap_report_speaks_the_adr_0048_wire_shape() {
        // `swap` → Report plus `content_hash` plus, on success, a diff summary
        // { survived, state_reset, added, removed }. One flat object — the channel and the
        // tool serialize the same type.
        let report = SwapReport {
            report: Report {
                ok: true,
                errors: vec![],
                warnings: vec![],
            },
            content_hash: "00c0ffee".to_string(),
            diff: Some(DiffSummary {
                survived: 0,
                state_reset: vec!["/osc".to_string()],
                added: vec!["/delay".to_string()],
                removed: vec![],
            }),
        };
        let v = serde_json::to_value(&report).expect("serialize");
        assert_eq!(
            v,
            serde_json::json!({
                "ok": true,
                "errors": [],
                "warnings": [],
                "content_hash": "00c0ffee",
                "diff": {
                    "survived": 0,
                    "state_reset": ["/osc"],
                    "added": ["/delay"],
                    "removed": []
                }
            })
        );
        let back: SwapReport = serde_json::from_value(v).expect("deserialize");
        assert_eq!(back, report);
    }

    #[test]
    fn a_rejected_swap_report_omits_the_diff() {
        // The diff summary rides "on success" — `ok: false` means nothing
        // installed, so there is no old-vs-new to summarize.
        let report = SwapReport {
            report: Report {
                ok: false,
                errors: vec![Diag {
                    node: Some("/osc".to_string()),
                    port: None,
                    message: "unknown operator type".to_string(),
                }],
                warnings: vec![],
            },
            content_hash: "00c0ffee".to_string(),
            diff: None,
        };
        let v = serde_json::to_value(&report).expect("serialize");
        assert!(
            v.as_object().is_some_and(|o| !o.contains_key("diff")),
            "no diff key on a rejected swap: {v}"
        );
        let back: SwapReport = serde_json::from_value(v).expect("deserialize");
        assert_eq!(back, report);
    }

    /// rmcp derives each tool's `outputSchema` from these types via schemars,
    /// so contract drift is a compile-time concern. Run with `--features schemars`.
    #[cfg(feature = "schemars")]
    #[test]
    fn report_schema_has_ok_errors_warnings() {
        let schema = serde_json::to_value(schemars::schema_for!(Report)).expect("schema");
        let props = schema["properties"]
            .as_object()
            .expect("Report schema has properties");
        for field in ["ok", "errors", "warnings"] {
            assert!(props.contains_key(field), "missing {field}: {schema}");
        }
        let required = schema["required"].as_array().expect("required list");
        for field in ["ok", "errors", "warnings"] {
            assert!(
                required.contains(&serde_json::json!(field)),
                "{field} must be required: {schema}"
            );
        }
        // The Diag items localize on optional node/port.
        let diag = &schema["$defs"]["Diag"]["properties"];
        for field in ["node", "port", "message"] {
            assert!(
                diag.as_object().is_some_and(|p| p.contains_key(field)),
                "Diag schema missing {field}: {schema}"
            );
        }
    }

    #[test]
    fn content_hash_is_stable_across_reserialization_of_an_equal_doc() {
        // The hash is over the canonical to_json_pretty() bytes, so source-text
        // formatting never leaks into identity. Mint, save (re-serialize), re-mint: the docs
        // are equal, and so are their hashes.
        let doc = mint(&doc_json(440.0));
        let resaved = mint(&doc.to_json_pretty());
        assert_eq!(doc, resaved, "a save round-trip preserves the document");
        assert_eq!(content_hash(&doc), content_hash(&resaved));
    }

    #[test]
    fn content_hash_differs_on_a_changed_node() {
        let before = mint(&doc_json(440.0));
        let after = mint(&doc_json(220.0));
        assert_ne!(before, after);
        assert_ne!(
            content_hash(&before),
            content_hash(&after),
            "a changed node must change the content identity"
        );
    }
}
