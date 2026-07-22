//! PROTOTYPE — the pure half. This is the bit worth keeping; the TUI in `main.rs` is not.
//!
//! **The question** (issue #576): `set-input` is proposed as a pure transform
//! `(document, node, input, value) -> { document, report }` so an agent changes
//! `cutoff: 1000 -> 800` without re-emitting the whole document. The execution plan claims the
//! win is that "corruption of the rest of the document becomes structurally impossible", proven
//! by a test asserting `doc_after == doc_before` *modulo* `nodes[X].inputs[Y]`.
//!
//! That claim is about the *transform*, but it can only be checked against a concrete
//! **serialization strategy**, and the plan picks one implicitly (parse `InstrumentDoc` ->
//! mutate -> reserialize). So this module implements the same signature three ways and lets the
//! TUI show what each one does to the other 99% of the document:
//!
//! - [`Strategy::Typed`]  — the plan's step 1: `InstrumentDoc` round-trip.
//! - [`Strategy::Value`]  — `serde_json::Value` round-trip (schema-blind).
//! - [`Strategy::Splice`] — replace the value's bytes in the original text, nothing else.
//!
//! Everything below the node lookup reuses the real loader's `Diag` (via `introspect::validate`),
//! per the plan's "fail honestly through the real loader" decision. Node lookup is this
//! transform's own localized `Diag`. Semantics are write-iff-valid: an edit that fails to
//! validate returns the *original* document.

use std::collections::BTreeMap;

use reuben_core::contract::{Diag, Report};
use reuben_core::format::{InputValue, InstrumentDoc};
use reuben_core::introspect::validate;
use reuben_core::resources::ResourceResolver;
use reuben_core::Registry;

/// How the edited document is turned back into text. The transform's signature is identical
/// across all three; only the collateral damage differs.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Strategy {
    /// Parse `InstrumentDoc` -> set the input -> `to_json_pretty`. The plan's step 1.
    Typed,
    /// Parse `serde_json::Value` -> set the input -> `to_string_pretty`. Schema-blind, so
    /// nothing the format struct drops can be dropped here.
    Value,
    /// Locate the existing literal's byte span in the original text and replace exactly those
    /// bytes. Every other byte of the document is provably untouched.
    Splice,
}

impl Strategy {
    pub const ALL: [Strategy; 3] = [Strategy::Typed, Strategy::Value, Strategy::Splice];

    pub fn label(self) -> &'static str {
        match self {
            Strategy::Typed => "TYPED  (InstrumentDoc round-trip — the plan's step 1)",
            Strategy::Value => "VALUE  (serde_json::Value round-trip)",
            Strategy::Splice => "SPLICE (byte-span replace in the original text)",
        }
    }
}

/// The knobs this prototype grew while being driven — bundled because the loose-argument form
/// crossed clippy's 7-argument line, which is itself a signal about the real API's shape.
#[derive(Copy, Clone)]
pub struct Policy {
    pub strategy: Strategy,
    /// Refuse a set whose target slot currently holds a wire-ref.
    pub guard_wires: bool,
}

/// What one `set-input` call produced. `document` is always a whole document the caller swaps
/// whole — the pure-transform shape ADR-0045 sanctions, never a stateful edit against the
/// playing graph.
pub struct Edit {
    pub document: String,
    pub report: Report,
    /// False when the document came back untouched (bad target, or the edit failed to validate).
    pub changed: bool,
}

/// Set one scalar literal on one node's `inputs`, then validate through the real load path.
///
/// Write-iff-valid: `changed` is false and `document` is the input document verbatim whenever
/// the target can't be found or the result doesn't validate.
///
/// `policy.guard_wires` is the prototype's answer to what driving it turned up: an `inputs` slot holds
/// **either** a literal **or** the wire that drives it, so an unguarded set silently severs a
/// modulation and still validates. With the guard on, a wired target is a localized rejection
/// instead.
pub fn set_input(
    json: &str,
    node: &str,
    input: &str,
    value: f64,
    policy: Policy,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Edit {
    if policy.guard_wires {
        if let Some(source) = wire_source(json, node, input) {
            return Edit {
                document: json.to_string(),
                report: Report {
                    ok: false,
                    errors: vec![Diag {
                        node: Some(node.to_string()),
                        port: Some(input.to_string()),
                        message: format!(
                            "`{input}` on `{node}` is driven by the wire `{source}` — setting a \
                             literal here would sever it. Rewire it or clear the wire first."
                        ),
                    }],
                    warnings: Vec::new(),
                },
                changed: false,
            };
        }
    }

    let edited = match policy.strategy {
        Strategy::Typed => set_typed(json, node, input, value),
        Strategy::Value => set_value(json, node, input, value),
        Strategy::Splice => set_splice(json, node, input, value),
    };

    let edited = match edited {
        Ok(text) => text,
        Err(diag) => {
            return Edit {
                document: json.to_string(),
                report: Report {
                    ok: false,
                    errors: vec![diag],
                    warnings: Vec::new(),
                },
                changed: false,
            }
        }
    };

    let report = validate(&edited, registry, resolver);
    if report.ok {
        Edit {
            document: edited,
            report,
            changed: true,
        }
    } else {
        Edit {
            document: json.to_string(),
            report,
            changed: false,
        }
    }
}

/// The wire currently driving `node.input`, if the slot holds a wire-ref rather than a literal.
/// The `inputs` map is a single slot per input — "a `Float` input and the wire that drives it now
/// target the same slot" (`format`) — so this is the one thing a value edit can destroy.
pub fn wire_source(json: &str, node: &str, input: &str) -> Option<String> {
    let doc: serde_json::Value = serde_json::from_str(json).ok()?;
    doc["nodes"]
        .as_array()?
        .iter()
        .find(|n| n["address"].as_str() == Some(node))?
        .get("inputs")?
        .get(input)?
        .get("from")?
        .as_str()
        .map(str::to_string)
}

/// The transform's own localized `Diag` — the one rejection the loader can't produce for us,
/// because a document with a missing node is a document we never build.
fn unknown_node(node: &str) -> Diag {
    Diag {
        node: Some(node.to_string()),
        port: None,
        message: format!("no node in this instrument has the address `{node}`"),
    }
}

fn json_diag(e: serde_json::Error) -> Diag {
    Diag {
        node: None,
        port: None,
        message: format!("document is not valid JSON: {e}"),
    }
}

// ---------------------------------------------------------------------------------------------
// Strategy 1 — typed round-trip.
// ---------------------------------------------------------------------------------------------

fn set_typed(json: &str, node: &str, input: &str, value: f64) -> Result<String, Diag> {
    let mut doc: InstrumentDoc = serde_json::from_str(json).map_err(json_diag)?;
    let target = doc
        .nodes
        .iter_mut()
        .find(|n| n.address == node)
        .ok_or_else(|| unknown_node(node))?;
    target
        .inputs
        .insert(input.to_string(), InputValue::Number(value));
    Ok(format!("{}\n", doc.to_json_pretty()))
}

// ---------------------------------------------------------------------------------------------
// Strategy 2 — `serde_json::Value` round-trip.
// ---------------------------------------------------------------------------------------------

fn set_value(json: &str, node: &str, input: &str, value: f64) -> Result<String, Diag> {
    let mut doc: serde_json::Value = serde_json::from_str(json).map_err(json_diag)?;
    let nodes = doc
        .get_mut("nodes")
        .and_then(|n| n.as_array_mut())
        .ok_or_else(|| Diag {
            node: None,
            port: None,
            message: "document has no `nodes` array".to_string(),
        })?;
    let target = nodes
        .iter_mut()
        .find(|n| n.get("address").and_then(|a| a.as_str()) == Some(node))
        .ok_or_else(|| unknown_node(node))?;
    let obj = target.as_object_mut().ok_or_else(|| unknown_node(node))?;
    obj.entry("inputs")
        .or_insert_with(|| serde_json::Value::Object(Default::default()))
        .as_object_mut()
        .ok_or_else(|| Diag {
            node: Some(node.to_string()),
            port: None,
            message: "`inputs` is not an object".to_string(),
        })?
        .insert(input.to_string(), serde_json::json!(value));
    Ok(format!(
        "{}\n",
        serde_json::to_string_pretty(&doc).expect("Value serializes")
    ))
}

// ---------------------------------------------------------------------------------------------
// Strategy 3 — byte-span splice.
// ---------------------------------------------------------------------------------------------

fn set_splice(json: &str, node: &str, input: &str, value: f64) -> Result<String, Diag> {
    let b = json.as_bytes();

    let nodes_span = object_member(b, object_start(b, 0)?, "nodes").ok_or_else(|| Diag {
        node: None,
        port: None,
        message: "document has no `nodes` array".to_string(),
    })?;

    let node_span = array_elements(b, nodes_span.0)
        .into_iter()
        .find(|&(s, _)| {
            object_member(b, s, "address")
                .map(|(vs, ve)| json[vs..ve] == format!("\"{node}\""))
                .unwrap_or(false)
        })
        .ok_or_else(|| unknown_node(node))?;

    let inputs_span = object_member(b, node_span.0, "inputs").ok_or_else(|| Diag {
        node: Some(node.to_string()),
        port: Some(input.to_string()),
        message: format!(
            "node `{node}` has no `inputs` block — a splice can replace an existing literal but \
             has no insertion point for a new key"
        ),
    })?;

    let (vs, ve) = object_member(b, inputs_span.0, input).ok_or_else(|| Diag {
        node: Some(node.to_string()),
        port: Some(input.to_string()),
        message: format!(
            "`{input}` is not set on `{node}` (it rides the descriptor default) — a splice can \
             replace an existing literal but has no insertion point for a new key"
        ),
    })?;

    // `f64` Display is the shortest round-trip decimal — `800`, `0.4`, never `800.0`.
    Ok(format!("{}{value}{}", &json[..vs], &json[ve..]))
}

/// Index of the `{` opening the object that starts at or after `from`.
fn object_start(b: &[u8], from: usize) -> Result<usize, Diag> {
    let i = skip_ws(b, from);
    if b.get(i) == Some(&b'{') {
        Ok(i)
    } else {
        Err(Diag {
            node: None,
            port: None,
            message: "document is not a JSON object".to_string(),
        })
    }
}

/// Byte span of `key`'s value inside the object opening at `obj` (`b[obj] == b'{'`).
fn object_member(b: &[u8], obj: usize, key: &str) -> Option<(usize, usize)> {
    let mut i = skip_ws(b, obj + 1);
    loop {
        if b.get(i)? == &b'}' {
            return None;
        }
        let key_end = skip_string(b, i)?;
        let found = &b[i + 1..key_end - 1] == key.as_bytes();
        i = skip_ws(b, key_end);
        if b.get(i)? != &b':' {
            return None;
        }
        let value_start = skip_ws(b, i + 1);
        let value_end = skip_value(b, value_start)?;
        if found {
            return Some((value_start, value_end));
        }
        i = skip_ws(b, value_end);
        if b.get(i)? == &b',' {
            i = skip_ws(b, i + 1);
        }
    }
}

/// Byte spans of every element of the array opening at `arr` (`b[arr] == b'['`).
fn array_elements(b: &[u8], arr: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = skip_ws(b, arr + 1);
    while b.get(i).is_some_and(|&c| c != b']') {
        let Some(end) = skip_value(b, i) else { break };
        out.push((i, end));
        i = skip_ws(b, end);
        if b.get(i) == Some(&b',') {
            i = skip_ws(b, i + 1);
        }
    }
    out
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while b.get(i).is_some_and(|c| c.is_ascii_whitespace()) {
        i += 1;
    }
    i
}

/// Index one past the closing quote of the string starting at `i`.
fn skip_string(b: &[u8], i: usize) -> Option<usize> {
    if b.get(i)? != &b'"' {
        return None;
    }
    let mut j = i + 1;
    loop {
        match b.get(j)? {
            b'\\' => j += 2,
            b'"' => return Some(j + 1),
            _ => j += 1,
        }
    }
}

/// Index one past the JSON value starting at `i`.
fn skip_value(b: &[u8], i: usize) -> Option<usize> {
    match b.get(i)? {
        b'"' => skip_string(b, i),
        b'{' | b'[' => {
            let mut depth = 0usize;
            let mut j = i;
            loop {
                match b.get(j)? {
                    b'"' => {
                        j = skip_string(b, j)?;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(j + 1);
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
        }
        _ => {
            let mut j = i;
            while b
                .get(j)
                .is_some_and(|&c| !matches!(c, b',' | b'}' | b']') && !c.is_ascii_whitespace())
            {
                j += 1;
            }
            Some(j)
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The yardstick (#574): what the model had to emit, and what moved in the document.
// ---------------------------------------------------------------------------------------------

/// Line-level churn between two documents, as multisets — no alignment, so a reordered key
/// counts as one removal + one addition, which is exactly the "unrelated churn" we're hunting.
pub struct Churn {
    pub removed: Vec<String>,
    pub added: Vec<String>,
}

impl Churn {
    pub fn between(before: &str, after: &str) -> Churn {
        let mut counts: BTreeMap<&str, i64> = BTreeMap::new();
        for l in before.lines() {
            *counts.entry(l).or_default() += 1;
        }
        for l in after.lines() {
            *counts.entry(l).or_default() -= 1;
        }
        let mut removed = Vec::new();
        let mut added = Vec::new();
        for (line, n) in counts {
            for _ in 0..n.max(0) {
                removed.push(line.to_string());
            }
            for _ in 0..(-n).max(0) {
                added.push(line.to_string());
            }
        }
        Churn { removed, added }
    }

    /// Lines that moved for reasons other than the value the caller asked to change.
    /// A one-value edit should score 1 removed / 1 added; anything more is collateral.
    pub fn collateral(&self) -> usize {
        self.removed.len().saturating_sub(1) + self.added.len().saturating_sub(1)
    }
}
