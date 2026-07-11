//! The one normalization seam (ADR-0047): **gate + migrate + strip + stamp**, behind
//! [`NormalizedDoc`] — a document proven current-shaped. The version gate refuses the future,
//! the v1→v2 migration engine flips target-form `interface` entries into pipes (ADR-0038 §5),
//! the v2→v3 strip drains retired presentation (ADR-0043 §7), and the stamp writes the current
//! version — all exactly once, at the mint. The newtype's field is private to this module, so
//! the only way to hold a `NormalizedDoc` is to have passed through here: the two-migrations
//! footgun ADR-0036 §4 guarded with prose and re-checks is unrepresentable by type.

use super::*;

/// A document the normalize seam has proven current-shaped: gated, migrated, stripped, and
/// stamped [`FORMAT_VERSION`]. The only type [`build`](Self::build) and the load paths accept —
/// mintable solely by this module ([`from_json`](Self::from_json) for text,
/// [`from_doc`](Self::from_doc) for a hand-deserialized [`InstrumentDoc`],
/// [`from_graph`](Self::from_graph) for a built graph), so normalization runs exactly once per
/// document (ADR-0047). Read access is by [`Deref`](std::ops::Deref); there is deliberately no
/// `DerefMut` — the data model can still represent v1-only shapes, so mutation exits via
/// [`into_inner`](Self::into_inner) and re-enters through the gate.
///
/// The resolver is **not** captured in the type: minting with resolver A and building with
/// resolver B remains the caller's contract, exactly as before (`describe_patch` passes the
/// same resolver to both). What the type makes unrepresentable is per-document double
/// migration.
#[derive(Debug, Clone, PartialEq)]
pub struct NormalizedDoc(InstrumentDoc);

impl std::ops::Deref for NormalizedDoc {
    type Target = InstrumentDoc;
    fn deref(&self) -> &InstrumentDoc {
        &self.0
    }
}

impl NormalizedDoc {
    /// Parse a document from JSON (no operator resolution yet) and normalize it to the current
    /// format version — **the** parse entry, replacing the old resolver-less/resolver-fed
    /// `from_json` pair with one mint. A v1 entry re-exporting a nested boundary port needs the
    /// child document to type its pipe: with `Some(resolver)` it migrates to the child's real
    /// declared type; with `None` it types `"f32"` as the documented fallback (degrades dark at
    /// build, ADR-0016) — behaviorally, `None` is an always-failing resolver, so there is one
    /// migration to reason about, parameterized by resolver, never two entry points to diverge
    /// through.
    pub fn from_json(
        json: &str,
        registry: &Registry,
        resolver: Option<&dyn ResourceResolver>,
    ) -> Result<Self, LoadError> {
        Self::parse_with(json, registry, resolver, &mut LoadCtx::default(), None)
    }

    /// The explicit gate for a document a host already holds — built through the public
    /// `Deserialize`, or edited after [`into_inner`](Self::into_inner). Consumes the raw doc
    /// and normalizes it exactly as [`from_json`](Self::from_json) would have: refuse the
    /// future, migrate the past, strip retired presentation, stamp. This replaces the old
    /// defensive clone-and-re-migrate inside the load path — the hypothetical raw-doc host now
    /// has a visible door instead of a silent re-run.
    pub fn from_doc(
        doc: InstrumentDoc,
        registry: &Registry,
        resolver: Option<&dyn ResourceResolver>,
    ) -> Result<Self, LoadError> {
        Self::normalize(doc, registry, resolver, &mut LoadCtx::default(), None)
    }

    /// Exit the invariant: hand back the plain document for mutation or serialization
    /// elsewhere. Edits re-enter through [`from_doc`](Self::from_doc), visibly re-passing the
    /// gate. (Serialization alone needs no exit — [`InstrumentDoc::to_json_pretty`] is
    /// reachable by deref.)
    pub fn into_inner(self) -> InstrumentDoc {
        self.0
    }

    /// [`from_json`](Self::from_json) with the nested-load machinery threaded through, so a v1
    /// entry re-exporting a nested child's boundary port can parse the child (recursively,
    /// cycle-guarded via `ctx`) and copy its pipe's declared type. `referrer` is the canonical
    /// id of the document being parsed (`None` at top level), so the child's own references
    /// resolve relative to its location.
    pub(super) fn parse_with(
        json: &str,
        registry: &Registry,
        resolver: Option<&dyn ResourceResolver>,
        ctx: &mut LoadCtx,
        referrer: Option<&str>,
    ) -> Result<Self, LoadError> {
        let doc: InstrumentDoc = serde_json::from_str(json).map_err(LoadError::Json)?;
        Self::normalize(doc, registry, resolver, ctx, referrer)
    }

    /// Gate + migrate + strip + stamp (ADR-0036 §4, held by this type per ADR-0047). The
    /// version gate lives at the mint so every load path — top-level, voice, subpatch, raw doc
    /// — refuses a too-new document before touching its shape; an older version migrates to
    /// current here; a current-version document is shape-checked (no v1 forms may hide under a
    /// v2+ stamp). Stamping last is what makes "save always writes the current version" a
    /// mechanism, not a coincidence — a migrated doc never saves back under its old version
    /// number.
    fn normalize(
        mut doc: InstrumentDoc,
        registry: &Registry,
        resolver: Option<&dyn ResourceResolver>,
        ctx: &mut LoadCtx,
        referrer: Option<&str>,
    ) -> Result<Self, LoadError> {
        if doc.format_version > FORMAT_VERSION {
            return Err(LoadError::UnsupportedVersion {
                found: doc.format_version,
                supported: FORMAT_VERSION,
            });
        }
        if doc.format_version < 2 {
            migrate_v1(&mut doc, registry, resolver, ctx, referrer)?;
        } else {
            doc.check_pipe_shape()?;
        }
        // v2→v3 (ADR-0043 §7) is a pure strip, so it runs unconditionally: a v3-stamped
        // document still carrying leftovers degrades identically (ignore-with-warning).
        doc.strip_retired_presentation();
        doc.format_version = FORMAT_VERSION;
        Ok(NormalizedDoc(doc))
    }

    /// Build the [`Graph`] this document describes.
    ///
    /// Two passes: pass 1 creates every node and applies its `config` constants and literal
    /// `inputs`; pass 2 resolves wire-refs (which may name a node declared later) into edges,
    /// type-checking each `Arg` type (ADR-0030). Between them, the subpatch pass (ADR-0034)
    /// inlines nested instruments.
    ///
    /// This path resolves no resources, so a nested reference cannot be loaded: every `subpatch`
    /// node dissolves *dark* — no child is spliced in, and wires/taps touching it are dropped
    /// (the same degradation an unavailable child gets on the full path). Use [`load_instrument`](super::load_instrument)
    /// to resolve and inline nested instruments.
    pub fn build(&self, registry: &Registry) -> Result<Graph, LoadError> {
        Ok(self
            .build_nested(registry, None, &mut LoadCtx::default(), None)?
            .graph)
    }

    /// Derive a document from a built [`Graph`] — the explicit **flatten/export** path, not the
    /// save path (ADR-0036): the document is the source of truth, so saving means serializing
    /// the [`InstrumentDoc`] you loaded/edited (nested references survive via serde), while
    /// `from_graph` of a built graph deliberately emits the flattened equivalent — every
    /// spliced subpatch appears as its inlined nodes, the reference dissolved. Use it to
    /// export a self-contained flat instrument or to materialize a programmatically built
    /// graph; don't round-trip an edited *nested* instrument through it. Nodes are emitted in
    /// a stable order, and within a node `config`/`inputs` keys are sorted (BTreeMap), so output is
    /// deterministic. A `Constant` override goes to `config`; a materialized `Float` override, an
    /// `Enum` choice (as its symbol), and every inbound wire go to `inputs` (ADR-0035).
    pub fn from_graph(graph: &Graph, instrument: impl Into<String>, registry: &Registry) -> Self {
        Self::from_doc(
            InstrumentDoc::from_graph_doc(graph, instrument),
            registry,
            None,
        )
        .expect("a flattened graph is current-shaped by construction")
    }
}

impl InstrumentDoc {
    /// ADR-0043 §7: drain the retired presentation carriers — a node's `control` block and
    /// `label`/`widget` on interface pipes — into ignore-with-warning migration notes,
    /// surfaced on the next build. Sound is unaffected (the engine never read them); save
    /// writes the document clean.
    fn strip_retired_presentation(&mut self) {
        let mut warnings = Vec::new();
        for node in &mut self.nodes {
            if node.control.take().is_some() {
                warnings.push(LoadWarning::DeprecatedControlBlock {
                    node: node.address.clone(),
                });
            }
        }
        if let Some(iface) = &mut self.interface {
            for (name, entry) in iface.inputs.iter_mut().chain(iface.outputs.iter_mut()) {
                let (label, widget) = match entry {
                    InterfaceEntry::Pipe(p) => (p.label.take(), p.widget.take()),
                    InterfaceEntry::Feed(f) => (f.label.take(), f.widget.take()),
                    _ => (None, None),
                };
                for (field, dropped) in [("label", label.is_some()), ("widget", widget.is_some())] {
                    if dropped {
                        warnings.push(LoadWarning::DeprecatedPipePresentation {
                            name: name.clone(),
                            field,
                        });
                    }
                }
            }
        }
        self.migration.warnings.extend(warnings);
    }

    /// Refuse v1-only forms under a v2+ stamp: a target-pointing `interface` entry or the
    /// anonymous top-level `outputs` block. Migration only runs for documents that *declare*
    /// themselves v1, so a v2/v3 document must already speak pipes.
    fn check_pipe_shape(&self) -> Result<(), LoadError> {
        if !self.outputs.is_empty() {
            return Err(LoadError::AnonymousOutputs);
        }
        if let Some(iface) = &self.interface {
            for (name, entry) in &iface.inputs {
                if entry.pipe().is_none() {
                    return Err(LoadError::InterfacePipe {
                        name: name.clone(),
                        reason: "a v2 interface input is a named pipe declaring its type \
                                 ({\"type\": …}); the target-pointing form is v1-only"
                            .to_string(),
                    });
                }
            }
            for (name, entry) in &iface.outputs {
                if entry.feed().is_none() {
                    return Err(LoadError::InterfacePipe {
                        name: name.clone(),
                        reason: "a v2 interface output is fed from an internal port \
                                 ({\"from\": …}); the target-pointing form is v1-only"
                            .to_string(),
                    });
                }
            }
        }
        Ok(())
    }
}

/// The v1→v2 migration (ADR-0036 §4, ADR-0038 §5), run at parse on any document declaring a
/// version below current. Mechanical, in four moves:
///
/// 0. a node whose address an input entry is about to mint (`"filter"` entry, `/filter` node —
///    legal in v1, where entries pointed inward and minted nothing) is **renamed aside** with
///    its references rewritten, so minting never turns a legal v1 document into a fatal
///    `DuplicateAddress`;
/// 1. every `interface.inputs` target entry **flips** into a pipe — its declared type and
///    engine range derived from the old target port — and the old target input gains a
///    consumer wire-ref (`"cutoff": {"from": "/tone"}`); a target literal moves onto the pipe
///    as its `default`;
/// 2. every `interface.outputs` target entry is respelled `{"from": …}` (outputs were already
///    fed-from-inside; only the key changes);
/// 3. the anonymous top-level `outputs` block dissolves into named `interface.outputs`
///    entries, reproducing the **exact v1 tap multiset** (see the pass below).
///
/// v1 shapes the flip cannot express degrade explicitly — dropped with a
/// [`LoadWarning::Migration`] naming the entry and a **dark** boundary name so host references
/// degrade instead of failing (never a silent drop, never fatal):
/// - an entry whose target input the child **already wires internally** (v1 merged host
///   messages with the wire on Value/Event ports; a pipe and an internal wire cannot share the
///   one document input slot);
/// - a second entry **aliasing** a target an earlier entry already flipped;
/// - an entry targeting a port type with no pipe form (`Arg`/`Str`/`I32` — v1 accepted any
///   input port by inheritance).
///
/// An entry re-exporting a **nested** child's boundary port derives its type from the child's
/// own (migrated) pipe; when the child is unavailable (no resolver / missing / unreadable) the
/// pipe falls back to `"f32"` so the document still loads and references degrade dark at
/// build, exactly like every other reference to an unavailable nest (ADR-0016).
fn migrate_v1(
    doc: &mut InstrumentDoc,
    registry: &Registry,
    resolver: Option<&dyn ResourceResolver>,
    ctx: &mut LoadCtx,
    referrer: Option<&str>,
) -> Result<(), LoadError> {
    let v2_in_v1 = |name: &str| LoadError::InterfacePipe {
        name: name.to_string(),
        reason: "a pipe-form entry in a v1 document — declare `format_version: 2`".to_string(),
    };

    // 0: clear the minted namespace. Every input entry mints `/<name>` (ADR-0038 §2); a v1
    // node already holding that address was legal, so a post-mint `DuplicateAddress` would
    // break "v1 documents keep loading forever" (ADR-0036 §4). The **node** steps aside — its
    // address is internal plumbing (every wire/target/tap referencing it is rewritten
    // mechanically; the OSC surface moves, the documented migration cost), while the entry
    // name is the boundary contract hosts already wire by and must keep.
    if let Some(iface) = &doc.interface {
        let minted: BTreeSet<String> = iface.inputs.keys().map(|n| format!("/{n}")).collect();
        let colliding: Vec<usize> = (0..doc.nodes.len())
            .filter(|&i| minted.contains(&doc.nodes[i].address))
            .collect();
        for idx in colliding {
            let old = doc.nodes[idx].address.clone();
            let mut i = 1usize;
            let new = loop {
                i += 1;
                let candidate = format!("{old}_{i}");
                if !minted.contains(&candidate) && !doc.nodes.iter().any(|n| n.address == candidate)
                {
                    break candidate;
                }
            };
            rename_node(doc, idx, &new);
            doc.migration.warnings.push(LoadWarning::Migration {
                name: old.clone(),
                detail: format!(
                    "node renamed to {new:?} (references rewritten): the interface input \
                     {:?} mints {old:?} as its pipe address (ADR-0038 §2) — external OSC \
                     paths under the old node address must follow it",
                    &old[1..]
                ),
            });
        }
    }

    // 1+2: flip the interface block.
    if let Some(iface) = doc.interface.take() {
        let mut inputs: BTreeMap<String, InterfaceEntry> = BTreeMap::new();
        // Ports an earlier entry already flipped, so a later same-target entry reads as an
        // **alias** of that pipe rather than as an internally-wired target.
        let mut flipped: BTreeMap<(usize, String), String> = BTreeMap::new();
        for (name, entry) in iface.inputs {
            let Some(target) = entry.v1_target().map(str::to_string) else {
                return Err(v2_in_v1(&name));
            };
            let meta = entry.v1_meta().cloned();
            if let Some(pipe) = migrate_input_entry(
                doc,
                &name,
                &target,
                meta.as_ref(),
                &mut flipped,
                registry,
                resolver,
                ctx,
                referrer,
            )? {
                inputs.insert(name, InterfaceEntry::Pipe(pipe));
            }
        }
        let mut outputs: BTreeMap<String, InterfaceEntry> = BTreeMap::new();
        for (name, entry) in iface.outputs {
            let Some(target) = entry.v1_target().map(str::to_string) else {
                return Err(v2_in_v1(&name));
            };
            let m = entry.v1_meta();
            outputs.insert(
                name,
                InterfaceEntry::Feed(OutputPipeDoc {
                    from: target,
                    channel: None,
                    label: m.and_then(|m| m.label.clone()),
                    unit: m.and_then(|m| m.unit.clone()),
                    widget: m.and_then(|m| m.widget.clone()),
                    min: m.and_then(|m| m.min),
                    max: m.and_then(|m| m.max),
                }),
            );
        }
        doc.interface = Some(InterfaceDoc { inputs, outputs });
    }

    // 3: the anonymous `outputs` block dissolves into `interface.outputs` (ADR-0038 §4),
    // reproducing the **exact v1 tap multiset**. At build, every channel-less signal output
    // pipe is a broadcast master tap and every channel-bound one a pinned tap, so each v1
    // anonymous tap must map to exactly one entry:
    //
    // - a tap on a port an unclaimed v1 boundary entry already feeds **claims that entry** —
    //   the entry becomes the tap (the voice-patch shape: `audio` declared *and* tapped stays
    //   one tap, as in v1), inheriting the tap's pinned channel when it has one (inert when
    //   the graph is hosted, so the boundary contract is unchanged);
    // - every other tap gets its own generated-name entry, one entry per tap, so duplicated v1
    //   taps (which v1 summed twice) stay duplicated;
    // - a pinned tap only claims an entry provably fed by a Signal port — `channel` on a
    //   message pipe is a load error, and migration must never manufacture one.
    //
    // A v1 boundary entry no tap claims migrates untouched; played at top level it becomes a
    // broadcast tap v1 did not have — the one v1 output shape the consolidated block cannot
    // express (ADR-0038 §4 unified "boundary output" with "master tap"; v1 kept them
    // separate). **Accepted + warned** (decided 2026-07-04, recorded in ADR-0038 §5):
    // hosted/nested behavior — the position such entries were used in — is exact, and each
    // such signal entry gets a `Migration` warning naming it, so the new top-level audibility
    // is never silent.
    let anon = std::mem::take(&mut doc.outputs);
    let InstrumentDoc {
        ref nodes,
        ref mut interface,
        ref mut migration,
        ..
    } = *doc;
    if interface.is_none() && anon.is_empty() {
        return Ok(());
    }
    let iface = interface.get_or_insert_with(InterfaceDoc::default);
    let mut unclaimed: BTreeSet<String> = iface.outputs.keys().cloned().collect();
    for o in anon {
        let claim = unclaimed
            .iter()
            .find(|n| {
                let Some(f) = iface.outputs[n.as_str()].feed() else {
                    return false;
                };
                f.channel.is_none()
                    && feed_names_port(f, &o, nodes, registry)
                    && (o.channel.is_none() || feed_is_signal(f, nodes, registry))
            })
            .cloned();
        if let Some(name) = claim {
            unclaimed.remove(&name);
            if o.channel.is_some() {
                if let Some(InterfaceEntry::Feed(f)) = iface.outputs.get_mut(&name) {
                    f.channel = o.channel;
                }
            }
            continue;
        }
        let name = generated_name(|c| iface.outputs.contains_key(c));
        iface.outputs.insert(
            name,
            InterfaceEntry::Feed(OutputPipeDoc {
                from: format!("{}.{}", o.node, o.port),
                channel: o.channel,
                ..Default::default()
            }),
        );
    }
    // The accepted §4 divergence, surfaced loudly: every v1 boundary-only **signal** output —
    // an entry no anonymous tap claimed — is a master tap in v2, audible at top level where
    // v1 played nothing from it. Provably-signal only: a message-typed feed never taps (no
    // divergence), and a nested re-export's type is unknowable at parse (rare; under-warning
    // there beats mis-warning every voice's `active`).
    for name in unclaimed {
        let Some(f) = iface.outputs[name.as_str()].feed() else {
            continue;
        };
        if feed_is_signal(f, nodes, registry) {
            migration.warnings.push(LoadWarning::Migration {
                name: name.clone(),
                detail: format!(
                    "this v1 interface output was boundary-only; in v2 it is a master tap \
                     (ADR-0038 §4) and is now audible when this instrument plays at top \
                     level, where v1 played nothing from it (hosted/nested behavior is \
                     unchanged) — delete the entry from the migrated document if that tap \
                     is unwanted (feeds {:?})",
                    f.from
                ),
            });
        }
    }
    Ok(())
}

/// Rename `doc.nodes[idx]` to `new`, rewriting every reference — wire-refs in node `inputs`,
/// v1 `interface` targets, anonymous-tap node names — so the rename is behavior-invariant
/// (migration step 0: a node stepping aside for a minted pipe address).
fn rename_node(doc: &mut InstrumentDoc, idx: usize, new: &str) {
    let old = std::mem::replace(&mut doc.nodes[idx].address, new.to_string());
    for n in &mut doc.nodes {
        for v in n.inputs.values_mut() {
            if let InputValue::Wire { from } = v {
                rename_in_ref(from, &old, new);
            }
        }
    }
    if let Some(iface) = &mut doc.interface {
        for entry in iface.inputs.values_mut().chain(iface.outputs.values_mut()) {
            match entry {
                InterfaceEntry::Target(t) => rename_in_ref(t, &old, new),
                InterfaceEntry::Detailed(m) => rename_in_ref(&mut m.target, &old, new),
                _ => {}
            }
        }
    }
    for o in &mut doc.outputs {
        if o.node == old {
            o.node = new.to_string();
        }
    }
}

/// Rewrite one wire-ref/target for a node rename: an exact match (the sole-output sugar form
/// `"/filter"`) or a `"/filter.port"` form follows to the new address; `"/filterbank"` does not.
fn rename_in_ref(reference: &mut String, old: &str, new: &str) {
    if reference.as_str() == old {
        *reference = new.to_string();
    } else if reference
        .strip_prefix(old)
        .is_some_and(|rest| rest.starts_with('.'))
    {
        *reference = format!("{new}{}", &reference[old.len()..]);
    }
}

/// Whether an output feed **provably** reads a Signal (`f32_buffer`) port: the ref names a
/// document node whose descriptor output is a buffer. A nested re-export (a subpatch face
/// port) is unknowable at parse and returns `false` — the migration tap-claiming pass then
/// declines to pin a channel on it rather than risk minting an illegal channel-on-message pipe.
fn feed_is_signal(feed: &OutputPipeDoc, nodes: &[NodeDoc], registry: &Registry) -> bool {
    let (addr, port) = parse_wire(&feed.from);
    let Some(d) = nodes
        .iter()
        .find(|n| n.address == addr)
        .and_then(|n| registry.get(&n.type_name))
        .map(|e| &e.descriptor)
    else {
        return false;
    };
    let p = match port {
        Some(p) => d.outputs.iter().find(|o| o.name == p),
        None if d.outputs.len() == 1 => Some(&d.outputs[0]),
        None => None,
    };
    matches!(p.map(|p| &p.ty), Some(PortType::F32Buffer))
}

/// Whether an already-migrated output pipe feeds from the same port a v1 anonymous tap names —
/// the migration dedup (a voice patch declared `audio: /out.audio` *and* tapped it). Handles
/// the sole-output sugar by resolving the node's descriptor when the ref names no port.
fn feed_names_port(
    feed: &OutputPipeDoc,
    tap: &PortRef,
    nodes: &[NodeDoc],
    registry: &Registry,
) -> bool {
    let (addr, port) = parse_wire(&feed.from);
    if addr != tap.node {
        return false;
    }
    match port {
        Some(p) => p == tap.port,
        None => nodes
            .iter()
            .find(|n| n.address == addr)
            .and_then(|n| registry.get(&n.type_name))
            .map(|e| &e.descriptor.outputs)
            .is_some_and(|outs| outs.len() == 1 && outs[0].name == tap.port),
    }
}

/// Migrate one v1 `interface.inputs` entry (see [`migrate_v1`]): derive the pipe declaration
/// from the target port, apply the v1 presentational overrides, capture the target's literal as
/// the pipe default, and rewrite the target input into a consumer wire-ref. `Ok(None)` drops
/// the entry — an internally-wired or aliased target, or a port type with no pipe form — always
/// with a [`LoadWarning::Migration`] naming it and a dark boundary name (never silent, never
/// fatal: ADR-0036 §4).
#[allow(clippy::too_many_arguments)]
fn migrate_input_entry(
    doc: &mut InstrumentDoc,
    name: &str,
    target: &str,
    meta: Option<&InterfaceMeta>,
    flipped: &mut BTreeMap<(usize, String), String>,
    registry: &Registry,
    resolver: Option<&dyn ResourceResolver>,
    ctx: &mut LoadCtx,
    referrer: Option<&str>,
) -> Result<Option<InputPipeDoc>, LoadError> {
    let (addr, port) = parse_wire(target);
    // v1 rule: an input ref names its port explicitly — no sole-input sugar.
    let port_name = port.ok_or_else(|| LoadError::UnknownPort {
        node: addr.to_string(),
        port: target.to_string(),
    })?;
    let node_idx = doc
        .nodes
        .iter()
        .position(|n| n.address == addr)
        .ok_or_else(|| LoadError::UnknownNode(addr.to_string()))?;
    let entry = registry
        .get(&doc.nodes[node_idx].type_name)
        .ok_or_else(|| LoadError::UnknownType {
            address: addr.to_string(),
            type_name: doc.nodes[node_idx].type_name.clone(),
        })?;
    // Degrade, loudly: warn naming the entry and leave the name **dark** so a host reference
    // to it drops with a warning instead of failing (the ADR-0016 discipline, applied to
    // migration loss).
    let drop_entry = |doc: &mut InstrumentDoc, detail: String| {
        doc.migration.warnings.push(LoadWarning::Migration {
            name: name.to_string(),
            detail,
        });
        doc.migration.dark_inputs.insert(name.to_string());
        Ok(None)
    };

    let existing = doc.nodes[node_idx].inputs.get(port_name).cloned();
    if matches!(existing, Some(InputValue::Wire { .. })) {
        // v1 tolerated a boundary name over a wired port: Value/Event inputs **merged** host
        // messages with the internal wire (only a host wire onto a driven *Signal* port was
        // fatal). Post-flip the port's one document slot holds the consumer wire-ref, so a
        // pipe cannot coexist with the internal wire — the entry drops, warned, and the name
        // goes dark. Distinguish an alias (another entry already flipped this port — order is
        // the deterministic BTreeMap name order) so the author is pointed at the survivor.
        let detail = match flipped.get(&(node_idx, port_name.to_string())) {
            Some(first) => format!(
                "entry dropped: it aliases {target:?}, which entry {first:?} already migrated \
                 — drive the {first:?} pipe instead"
            ),
            None => format!(
                "entry dropped: its target {target:?} takes an internal wire, and a migrated \
                 pipe cannot merge with it (v1 merged host messages into Value/Event ports); \
                 re-author the boundary as a v2 pipe wired through an explicit merge point"
            ),
        };
        return drop_entry(doc, detail);
    }

    let mut pipe = if entry.descriptor.has_resource("patch") {
        // A re-export of a nested child's boundary port: the pipe declaration is the child's
        // own (migrated) pipe. Unavailable child → the `"f32"` fallback (degrades dark at
        // build, ADR-0016).
        child_input_pipe(
            doc, node_idx, addr, port_name, registry, resolver, ctx, referrer,
        )?
    } else {
        let d = &entry.descriptor;
        let pi = d
            .inputs
            .iter()
            .position(|p| p.name == port_name)
            .ok_or_else(|| LoadError::UnknownPort {
                node: addr.to_string(),
                port: port_name.to_string(),
            })?;
        // The v1 override law (ADR-0034 §4) still holds for v1 documents: an override must
        // stay a truthful subset of what the target port enforced (fatal in v1, stays fatal).
        if let Some(m) = meta {
            check_range_override(
                name,
                m.min,
                m.max,
                &d.inputs[pi],
                Some(effective_default(&d.inputs[pi], existing.as_ref())),
            )?;
        }
        match pipe_from_port(&d.inputs[pi]) {
            Some(pipe) => pipe,
            // v1 accepted any input port by inheritance (`Arg`/`Str`/`I32` included); the
            // pipe model cannot carry those types (ADR-0038 §2), so the entry drops, warned.
            None => {
                return drop_entry(
                    doc,
                    format!(
                        "entry dropped: its target {target:?} is a {} port, which has no \
                         pipe form (ADR-0038 §2) — drive the target address directly, or \
                         re-author the boundary around a pipe-typed port",
                        d.inputs[pi].ty
                    ),
                );
            }
        }
    };

    // The target's own literal is what an unwired v1 host got — it becomes the pipe's default,
    // clamped into the pipe's range exactly as the v1 engine clamped the materialized literal.
    match &existing {
        Some(InputValue::Number(v)) => {
            let lo = pipe
                .min
                .unwrap_or_else(|| widen_f32(reuben_contract::NUMBER_MIN));
            let hi = pipe
                .max
                .unwrap_or_else(|| widen_f32(reuben_contract::NUMBER_MAX));
            pipe.default = Some(PipeDefault::Number(v.clamp(lo, hi)));
        }
        Some(InputValue::Symbol(s)) => pipe.default = Some(PipeDefault::Symbol(s.clone())),
        _ => {}
    }
    // The v1 presentational overrides decorate the derived declaration. `min`/`max` are
    // deliberately **not** carried: v1 documented them as presentational — the engine clamped
    // to the *inner* port's range — but on a v2 pipe they would become the engine-enforced
    // range (ADR-0038 §2) and clamp harder than v1 did (a bit-identical break on out-of-range
    // control input). The pipe keeps the inner port's engine range (already derived above);
    // the display-only narrowing has no v2 slot and is dropped, validated first by the v1
    // truthfulness law above.
    if let Some(m) = meta {
        if m.label.is_some() {
            pipe.label = m.label.clone();
        }
        if m.unit.is_some() {
            pipe.unit = m.unit.clone();
        }
        if m.widget.is_some() {
            pipe.widget = m.widget.clone();
        }
    }

    // The mechanical flip: the old target input now consumes the pipe.
    doc.nodes[node_idx].inputs.insert(
        port_name.to_string(),
        InputValue::Wire {
            from: format!("/{name}"),
        },
    );
    flipped.insert((node_idx, port_name.to_string()), name.to_string());
    Ok(Some(pipe))
}

/// The effective unwired value of a v1 target port: the child's own literal beats the
/// descriptor default (`None` for a port with neither — a bare buffer).
fn effective_default(port: &Port, literal: Option<&InputValue>) -> Option<f64> {
    match literal {
        Some(InputValue::Number(v)) => Some(*v),
        _ => port.meta.as_ref().map(|m| widen_f32(m.default)),
    }
}

/// Derive a migrated pipe's declaration from the v1 target [`Port`]: the declared type string,
/// and — for a numeric port — the engine range/default/curve/unit it enforced, so the pipe
/// enforces (and renders) exactly what the v1 boundary did. `None` when the port's type has no
/// pipe form (`Arg`/`Str`/`I32` — the caller degrades the entry, warned).
fn pipe_from_port(port: &Port) -> Option<InputPipeDoc> {
    let mut pipe = InputPipeDoc {
        ty: pipe_type_name(&port.ty)?,
        ..Default::default()
    };
    if matches!(port.ty, PortType::F32Buffer | PortType::F32) {
        if let Some(m) = &port.meta {
            pipe.default = Some(PipeDefault::Number(widen_f32(m.default)));
            pipe.min = Some(widen_f32(m.min));
            pipe.max = Some(widen_f32(m.max));
            if m.curve == Curve::Exponential {
                pipe.curve = Some(CurveDoc::Exp);
            }
            if !m.unit.is_empty() {
                pipe.unit = Some(m.unit.to_string());
            }
        }
    }
    Some(pipe)
}

/// The nested-re-export arm of [`migrate_input_entry`]: parse the referenced child (recursively
/// migrated, cycle-guarded, cached in [`LoadCtx::docs`] so N re-exported entries parse it once)
/// and copy the named boundary pipe's declaration. Any availability failure — no resolver on
/// this path, a missing id/source, unreadable text — falls back to a plain `"f32"` pipe so the
/// document loads and references degrade dark at build (ADR-0016); a child that *parses* but
/// declares no such pipe stays the fatal `UnknownPort` v1 raised.
#[allow(clippy::too_many_arguments)]
fn child_input_pipe(
    doc: &InstrumentDoc,
    node_idx: usize,
    addr: &str,
    port_name: &str,
    registry: &Registry,
    resolver: Option<&dyn ResourceResolver>,
    ctx: &mut LoadCtx,
    referrer: Option<&str>,
) -> Result<InputPipeDoc, LoadError> {
    let fallback = || InputPipeDoc {
        ty: "f32".to_string(),
        ..Default::default()
    };
    let (Some(resolver), Some(id)) = (resolver, &doc.nodes[node_idx].patch) else {
        return Ok(fallback());
    };
    let Some(source) = doc.resources.get(id) else {
        return Ok(fallback());
    };
    let canon = resolver.canonical(source, referrer);
    if !ctx.docs.contains_key(&canon) {
        // A child mid-parse (a re-export chain containing itself) is caught by the guard, not
        // the cache — only fully parsed children are cached.
        let Ok(text) = resolver.resolve_text(&canon) else {
            return Ok(fallback());
        };
        // A resolved-but-malformed child stays fatal (ADR-0034 §1).
        let child = with_cycle_guard(ctx, &canon, |ctx| {
            NormalizedDoc::parse_with(&text, registry, Some(resolver), ctx, Some(&canon))
        })?;
        ctx.docs.insert(canon.clone(), child);
    }
    let pipe = ctx.docs[&canon]
        .interface
        .as_ref()
        .and_then(|i| i.inputs.get(port_name))
        .and_then(|e| e.pipe())
        .cloned()
        .ok_or_else(|| LoadError::UnknownPort {
            node: addr.to_string(),
            port: port_name.to_string(),
        })?;
    Ok(InputPipeDoc {
        // The child's own channel binding is child-local (inert when nested, ADR-0038 §3);
        // a re-export does not inherit it.
        channel: None,
        ..pipe
    })
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use super::*;
    use crate::registry::Registry;
    use crate::resources::{ResourceResolver, SampleBuffer};

    fn reg() -> Registry {
        Registry::builtin()
    }

    /// Serves one fixed child document for any source (the `PatchResolver` idiom of the
    /// format tests, local to the mint's own seams).
    struct ChildResolver(&'static str);
    impl ResourceResolver for ChildResolver {
        fn resolve(&self, source: &str) -> Result<SampleBuffer, crate::resources::ResolveError> {
            Err(crate::resources::ResolveError::NotFound(source.to_string()))
        }
        fn resolve_text(&self, _: &str) -> Result<String, crate::resources::ResolveError> {
            Ok(self.0.to_string())
        }
    }

    /// A current-version child whose boundary pipe type is *not* the fallback `"f32"` — what
    /// a resolver-fed mint must derive and a resolver-less mint cannot.
    const CHILD: &str = r#"{
        "format_version": 3,
        "instrument": "child",
        "interface": {
            "inputs": { "freq": { "type": "f32_buffer", "min": 20, "max": 20000, "default": 440 } },
            "outputs": { "audio": { "from": "/osc.audio" } }
        },
        "nodes": [ { "type": "oscillator", "address": "/osc", "inputs": { "freq": { "from": "/freq" } } } ]
    }"#;

    /// A v1 host whose `interface` entry re-exports the nested child's boundary port — the
    /// migration arm that needs the child document to type the pipe for real.
    const V1_REEXPORT_HOST: &str = r#"{
        "instrument": "host",
        "resources": { "c": "child.json" },
        "interface": { "inputs": { "freq": "/nest.freq" } },
        "nodes": [ { "type": "subpatch", "address": "/nest", "patch": "c" } ]
    }"#;

    fn pipe_type(doc: &NormalizedDoc, name: &str) -> String {
        doc.interface
            .as_ref()
            .and_then(|i| i.inputs.get(name))
            .and_then(|e| e.pipe())
            .map(|p| p.ty.clone())
            .expect("a migrated input entry is a pipe")
    }

    #[test]
    fn the_mint_migrates_a_stale_stamp_and_stamps_current() {
        // ADR-0036 §4 through the one gate: a pre-versioning document is a valid v1, migrates
        // at the mint, and holds (and saves) the current version — never its old number.
        let doc = NormalizedDoc::from_json(r#"{"instrument":"t","nodes":[]}"#, &reg(), None)
            .expect("v1 parses");
        assert_eq!(doc.format_version, FORMAT_VERSION);
        assert!(doc.to_json_pretty().contains("\"format_version\": 3"));
    }

    #[test]
    fn the_mint_refuses_the_future() {
        let err = NormalizedDoc::from_json(
            r#"{"format_version":99,"instrument":"t","nodes":[]}"#,
            &reg(),
            None,
        )
        .expect_err("a future version must not mint");
        assert!(matches!(
            err,
            LoadError::UnsupportedVersion {
                found: 99,
                supported: FORMAT_VERSION
            }
        ));
    }

    #[test]
    fn a_resolverless_mint_falls_back_to_f32_for_a_reexported_child_pipe() {
        // The documented degrade-dark half of the old from_json/from_json_with divergence:
        // with no resolver the child is unreachable, so the re-exported pipe types "f32".
        let doc = NormalizedDoc::from_json(V1_REEXPORT_HOST, &reg(), None).expect("mint");
        assert_eq!(pipe_type(&doc, "freq"), "f32");
    }

    #[test]
    fn a_resolver_fed_mint_types_a_reexported_child_pipe_for_real() {
        // The other half: the same document minted with the resolver derives the child's
        // declared type. One mint entry, two resolver arguments — no second entry point to
        // diverge through.
        let doc = NormalizedDoc::from_json(V1_REEXPORT_HOST, &reg(), Some(&ChildResolver(CHILD)))
            .expect("mint");
        assert_eq!(pipe_type(&doc, "freq"), "f32_buffer");
    }

    #[test]
    fn from_doc_refuses_v1_forms_under_a_current_stamp() {
        // Fail-closed (#189 F8a, respelled from load_doc_guarded's defensive re-migrate): a
        // hand-deserialized doc stamped v2 smuggling the v1-only anonymous `outputs` block
        // must refuse at the gate, not tap twice. The old smuggle routes — handing the raw
        // doc to `load_instrument_doc` or calling `build` on it — are compile errors now;
        // this gate is the one door left.
        let smuggled: InstrumentDoc = serde_json::from_str(
            r#"{"format_version":2,"instrument":"s",
                "interface":{"outputs":{"out":{"from":"/osc.audio"}}},
                "nodes":[{"type":"oscillator","address":"/osc"}],
                "outputs":[{"node":"/osc","port":"audio"}]}"#,
        )
        .expect("raw deserialize does not gate");
        assert!(matches!(
            NormalizedDoc::from_doc(smuggled, &reg(), None),
            Err(LoadError::AnonymousOutputs)
        ));
    }

    #[test]
    fn from_doc_refuses_the_future() {
        // The embedded-host idiom deserializes straight to `InstrumentDoc`; entering typed
        // requires the gate, which refuses a shape it can't trust (ADR-0036 §4).
        let doc: InstrumentDoc =
            serde_json::from_str(r#"{"format_version":99,"instrument":"t","nodes":[]}"#)
                .expect("raw deserialize does not gate");
        assert_eq!(doc.format_version, 99);
        assert!(matches!(
            NormalizedDoc::from_doc(doc, &reg(), None),
            Err(LoadError::UnsupportedVersion { found: 99, .. })
        ));
    }

    #[test]
    fn from_doc_migrates_a_stale_stamp() {
        // A raw v1 doc (target-form interface) entering through the explicit door migrates
        // exactly as the JSON mint would: the entry flips to a pipe typed from its target
        // port, and the doc stamps current.
        let raw: InstrumentDoc = serde_json::from_str(
            r#"{"instrument":"t",
                "interface":{"inputs":{"freq":"/osc.freq"}},
                "nodes":[{"type":"oscillator","address":"/osc"}]}"#,
        )
        .expect("raw deserialize");
        let doc = NormalizedDoc::from_doc(raw, &reg(), None).expect("v1 migrates at the gate");
        assert_eq!(doc.format_version, FORMAT_VERSION);
        assert_eq!(pipe_type(&doc, "freq"), "f32_buffer");
    }

    #[test]
    fn from_doc_strips_retired_presentation_under_a_current_stamp() {
        // ADR-0043 §7 at the gate: a current-stamped doc still carrying pipe presentation is
        // stripped (ignore-with-warning), replacing load_doc_guarded's clone-and-re-migrate.
        let raw: InstrumentDoc = serde_json::from_str(
            r#"{"format_version":3,"instrument":"t",
                "interface":{"inputs":{"freq":{"type":"f32","label":"Frequency"}}},
                "nodes":[]}"#,
        )
        .expect("raw deserialize");
        let doc = NormalizedDoc::from_doc(raw, &reg(), None).expect("leftovers are non-fatal");
        let pipe = doc.interface.as_ref().unwrap().inputs["freq"]
            .pipe()
            .unwrap()
            .clone();
        assert_eq!(pipe.label, None, "the leftover label is drained");
        assert!(
            doc.migration.warnings.iter().any(
                |w| matches!(w, LoadWarning::DeprecatedPipePresentation { name, field }
                    if name == "freq" && field == &"label")
            ),
            "the strip is loud: {:?}",
            doc.migration.warnings
        );
    }

    #[test]
    fn edits_exit_via_into_inner_and_reenter_through_the_gate() {
        // No DerefMut: mutation leaves the type (into_inner) and visibly re-passes the gate
        // (from_doc), where an edit that broke the invariant is refused.
        let doc = NormalizedDoc::from_json(r#"{"instrument":"t","nodes":[]}"#, &reg(), None)
            .expect("mint");
        let mut raw = doc.into_inner();
        raw.format_version = 99;
        assert!(matches!(
            NormalizedDoc::from_doc(raw, &reg(), None),
            Err(LoadError::UnsupportedVersion { found: 99, .. })
        ));
    }
}
