//! The installed-Plan **manifest** and the swap **survivor key** — per node, its fully-qualified
//! address, operator type, and instantiate-time identity fingerprint (a content hash over its
//! normalized `config` plus everything it resolved at Instantiate) — and every input latch a live
//! message can set, with what it holds at rest. [`Manifest::diff`] turns two manifests into the
//! migration table the render side transplants by box swap.
//!
//! This is off-thread load-time work (no RT constraint) and, like the rest of core, OS-free.

use std::collections::{BTreeMap, BTreeSet};

use crate::format::{NodeDoc, NormalizedDoc, ResourceSource};
use crate::resources::ResourceResolver;
use crate::DiffSummary;
use reuben_core::coordinator::MigrationTable;
use reuben_core::descriptor::{Descriptor, PortType};
use reuben_core::plan::{InterfaceAt, LatchRoute, LiveCarry, LiveSlot, Plan};
use reuben_core::registry::Registry;
use reuben_core::resources::SampleBuffer;

/// One installed node's survivor identity: its fully-qualified address, operator
/// type, and instantiate-time fingerprint. Position in [`Manifest::nodes`] is the node's Plan
/// index, which the migration table pairs old↔new.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeIdentity {
    /// Fully-qualified address (a spliced subpatch child carries its prefix), the
    /// same address the Plan node holds.
    pub address: String,
    /// Operator type name ([`Descriptor::type_name`]).
    pub type_name: String,
    /// Instantiate-time content fingerprint — `Some` for every node the manifest walk covered;
    /// `None` is the conservative fallback (a node whose fingerprint could not be computed **never
    /// survives**, so an unmodelled shape resets rather than silently transplanting).
    pub fingerprint: Option<u64>,
}

/// The installed Plan's per-node survivor identities, in Plan execution order (index = Plan node
/// index). Built by [`build_manifest`]; diffed against a new Plan's manifest by [`Manifest::diff`]
/// to precompute the migration table.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Manifest {
    pub nodes: Vec<NodeIdentity>,
    /// Every unwired input latch, by the address a live message names it with. Read from the Plan
    /// before it renders ([`Plan::live_slots`]), so each `seed` is the document's value.
    pub slots: Vec<LiveSlot>,
}

impl Manifest {
    /// Diff the installed (`self`, old) manifest against a freshly built `new` one, producing the
    /// migration table plus the [`DiffSummary`] the swap report carries. A node **survives** iff a
    /// node at the **same address** exists in both, with the **same
    /// operator type** and the **same instantiate-time fingerprint** (both present). Everything
    /// else at a shared address is a `state_reset` (a type change or a fingerprint change — the
    /// edit always wins); addresses only in `new` are `added`, only in `old` are `removed`.
    ///
    /// Live values carry by address, in two kinds:
    ///
    /// - An **interface input** nothing feeds keeps its value wherever each Plan keeps it — a
    ///   pipe node of its own or a dissolved pipe, whatever it feeds — while its seed (the
    ///   document's default) and its type are unchanged. The render side clamps a numeric value
    ///   to the fresh range, so a range edit keeps the value inside it.
    /// - An **unwired node input** keeps its value while its node survives (same address, type and
    ///   instantiate-time identity, so the same port) and its seed is unchanged.
    ///
    /// Everything else takes the new document's value, including a value that compares unequal
    /// to itself (a NaN seed).
    pub fn diff(&self, new: &Manifest) -> (MigrationTable, DiffSummary) {
        // Addresses are unique within a Plan (the loader rejects duplicates), so an address keys a
        // node unambiguously. Old side maps address -> (plan index, identity).
        let old_by_addr: BTreeMap<&str, (usize, &NodeIdentity)> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.address.as_str(), (i, n)))
            .collect();
        let new_addrs: BTreeMap<&str, ()> =
            new.nodes.iter().map(|n| (n.address.as_str(), ())).collect();

        let mut survivors = Vec::new();
        let mut state_reset = Vec::new();
        let mut added = Vec::new();
        for (new_idx, nn) in new.nodes.iter().enumerate() {
            match old_by_addr.get(nn.address.as_str()) {
                Some(&(old_idx, on)) => {
                    let survives = on.type_name == nn.type_name
                        && on.fingerprint.is_some()
                        && on.fingerprint == nn.fingerprint;
                    if survives {
                        survivors.push((old_idx, new_idx));
                    } else {
                        state_reset.push(nn.address.clone());
                    }
                }
                None => added.push(nn.address.clone()),
            }
        }
        let mut removed: Vec<String> = self
            .nodes
            .iter()
            .filter(|n| !new_addrs.contains_key(n.address.as_str()))
            .map(|n| n.address.clone())
            .collect();

        // Deterministic order for the reported lists (the migration table stays in new-Plan order).
        state_reset.sort();
        added.sort();
        removed.sort();

        let diff = DiffSummary {
            survived: survivors.len(),
            state_reset,
            added,
            removed,
        };
        let live_values = self.carried(new, &survivors);
        (MigrationTable::new(survivors, live_values), diff)
    }

    /// The addresses of this (new) manifest's interface inputs that `carried` lands no live value
    /// in, sorted.
    pub fn interface_resets(&self, carried: &[LiveCarry]) -> Vec<String> {
        let landed: BTreeSet<InterfaceAt> = (carried.iter())
            .filter_map(|c| match *c {
                LiveCarry::Interface { to, .. } => Some(to),
                LiveCarry::Latch { .. } => None,
            })
            .collect();
        let mut reset: Vec<String> = (self.slots.iter())
            .filter(|s| matches!(s.route, LatchRoute::Interface(at) if !landed.contains(&at)))
            .map(|s| s.address.clone())
            .collect();
        reset.sort();
        reset
    }

    /// The live values [`Manifest::diff`] carries (see there for the rule). An address names one
    /// place per side and pairs once, so no place is written twice.
    fn carried(&self, new: &Manifest, survivors: &[(usize, usize)]) -> Vec<LiveCarry> {
        let old_by_addr: BTreeMap<&str, &LiveSlot> =
            self.slots.iter().map(|s| (s.address.as_str(), s)).collect();
        let survivors: BTreeSet<(usize, usize)> = survivors.iter().copied().collect();
        let mut carried = Vec::new();
        for ns in &new.slots {
            let Some(os) = old_by_addr.get(ns.address.as_str()) else {
                continue;
            };
            if os.seed != ns.seed {
                continue;
            }
            match (os.route, ns.route) {
                // A survivor shares its operator type, so its port is the same port.
                (
                    LatchRoute::Node { node: on, port: op },
                    LatchRoute::Node { node: nn, port: np },
                ) if survivors.contains(&(on, nn)) => {
                    carried.push(LiveCarry::Latch {
                        from: (on, op),
                        to: (nn, np),
                    });
                }
                (LatchRoute::Interface(from), LatchRoute::Interface(to))
                    if same_type(&os.typed_by.ty, &ns.typed_by.ty) =>
                {
                    carried.push(LiveCarry::Interface { from, to });
                }
                _ => {}
            }
        }
        carried
    }
}

/// Whether a value of type `old` is still a value of type `new`: the same type, an enum keeping
/// its type and variants. An integer's range and either's default do not count; the render side
/// clamps into the range, and the default is compared as the seed.
fn same_type(old: &PortType, new: &PortType) -> bool {
    match (old, new) {
        (PortType::I32 { .. }, PortType::I32 { .. }) => true,
        (
            PortType::Vocab {
                name: an,
                is_event: ae,
                enum_meta: am,
            },
            PortType::Vocab {
                name: bn,
                is_event: be,
                enum_meta: bm,
            },
        ) => {
            an == bn
                && ae == be
                && match (am, bm) {
                    (Some(a), Some(b)) => a.type_name == b.type_name && a.variants == b.variants,
                    (None, None) => true,
                    _ => false,
                }
        }
        _ => old == new,
    }
}

/// Build the manifest for a freshly built (`doc`, `plan`) pair. The Plan is the authority for the
/// node set — its addresses, types, and indices — while the fingerprint of each node is computed
/// by re-walking `doc` with the resolver (the Plan's operator boxes no longer expose the config /
/// resolved content they were built from). A Plan address the walk did not cover keeps a `None`
/// fingerprint (conservative: it will never be judged a survivor).
pub fn build_manifest(
    doc: &NormalizedDoc,
    plan: &Plan,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Manifest {
    // address -> fingerprint, mirroring how the loader names Plan nodes: top-level nodes keep
    // their address; a subpatch's children are spliced under the subpatch address as a prefix
    //, so the walk recurses with that prefix; a voicer's hosted voice document folds
    // into the one voicer node's fingerprint (voices are sub-plans inside the box).
    let mut fingerprints: BTreeMap<String, u64> = BTreeMap::new();
    walk_document(doc, "", None, registry, resolver, &mut fingerprints, 0);

    let nodes = plan
        .nodes()
        .iter()
        .map(|node| NodeIdentity {
            address: node.address.to_string(),
            type_name: node.descriptor.type_name.to_string(),
            fingerprint: fingerprints.get(&*node.address).copied(),
        })
        .collect();
    Manifest {
        nodes,
        slots: plan.live_slots(),
    }
}

/// Depth cap for the recursive content walk. A document that successfully built has no resource
/// cycle (the loader rejects them as fatal), so this is a pure safety net against a
/// pathological input reaching the fingerprint path some other way.
const MAX_DEPTH: usize = 32;

/// Content-identity sentinels for a resource that could not be resolved — fixed so two equally
/// missing/failed resolutions fingerprint equal (a swap that leaves a broken sample broken must
/// not spuriously reset the node), yet distinct from any real content hash by construction.
const UNRESOLVED_MISSING: u64 = 0x6d69_7373_696e_6721; // "missing!"
const UNRESOLVED_FAILED: u64 = 0x6661_696c_6564_6721; // "failed!"

/// Walk `doc`'s nodes, emitting `fully_qualified_address -> fingerprint` for every node that
/// becomes a Plan node. `prefix` is the splice prefix (empty at top level); `referrer` is the
/// canonical id of `doc` (`None` at top level), threaded so nested references resolve relative to
/// their own document.
fn walk_document(
    doc: &NormalizedDoc,
    prefix: &str,
    referrer: Option<&str>,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
    out: &mut BTreeMap<String, u64>,
    depth: usize,
) {
    if depth > MAX_DEPTH {
        return;
    }
    for node in &doc.nodes {
        let Some(descriptor) = registry.get(&node.type_name).map(|e| &e.descriptor) else {
            // An unknown type never built into a Plan node; nothing to fingerprint.
            continue;
        };
        let address = format!("{prefix}{}", node.address);
        if descriptor.has_resource("patch") {
            // A subpatch dissolves: its own address is not a Plan node, and its
            // child's nodes splice in under this address as a prefix. Recurse so those children
            // get their fully-qualified fingerprints; a dark (unavailable) child contributes no
            // Plan nodes, so it contributes none here either.
            if let Some((child, canon)) = resolve_child_doc(doc, node, referrer, registry, resolver)
            {
                walk_document(
                    &child,
                    &address,
                    Some(&canon),
                    registry,
                    resolver,
                    out,
                    depth + 1,
                );
            }
        } else {
            out.insert(
                address,
                node_fingerprint(doc, node, descriptor, referrer, registry, resolver, depth),
            );
        }
    }
}

/// One node's instantiate-time fingerprint: its normalized `config` block, plus the
/// content identity of what it resolved at Instantiate — a `sample`'s decoded bytes, a `voice`'s
/// hosted document (recursively). Runtime `inputs` are deliberately excluded: they are diffed
/// per latch in [`Manifest::diff`], so a param edit leaves the node a survivor.
fn node_fingerprint(
    doc: &NormalizedDoc,
    node: &NodeDoc,
    descriptor: &Descriptor,
    referrer: Option<&str>,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
    depth: usize,
) -> u64 {
    let mut h = Fnv::new();

    // The normalized `config` block (canonical bytes: `config` is a BTreeMap, so serialization is
    // key-sorted and stable). This is where a voicer's `voices` pool size lives — the constant
    // whose change must reset the node.
    let config = serde_json::to_vec(&node.config).unwrap_or_default();
    h.tagged(b"config", &config);

    // Resolved resource bytes — a sample player's decoded audio. Its identity is the
    // decoded content, not the path, so a re-upload at the same path resets the node.
    if descriptor.has_resource("sample") {
        if let Some(id) = &node.sample {
            h.tag(b"sample");
            h.write_u64(sample_identity(doc, id, referrer, resolver));
        }
    }

    // Hosted voice document — folded in recursively, so changing content *inside* a
    // hosted voice patch changes this (host) node's fingerprint.
    if descriptor.has_resource("voice") {
        if let Some(id) = &node.voice {
            h.tag(b"voice");
            h.write_u64(hosted_doc_identity(
                doc, id, None, referrer, registry, resolver, depth,
            ));
        }
    }

    h.finish()
}

/// The content identity of a `sample` resource: its decoded bytes (channels + sample rate),
/// resolved through the loader's own lookup — a binding a hosting `subpatch` placed on `doc`
/// included. A missing id or a resolve/decode failure hashes to a fixed sentinel (so two
/// equally-broken resolutions match).
fn sample_identity(
    doc: &NormalizedDoc,
    id: &str,
    referrer: Option<&str>,
    resolver: &dyn ResourceResolver,
) -> u64 {
    let ResourceSource::Canonical(canon) = doc.source_of(id, resolver, referrer) else {
        return UNRESOLVED_MISSING;
    };
    match resolver.resolve(&canon) {
        Ok(buffer) => buffer_identity(&buffer),
        Err(_) => UNRESOLVED_FAILED,
    }
}

/// Hash a decoded [`SampleBuffer`]'s content: every channel's samples (as raw `f32` bits) plus the
/// native sample rate. O(channels × frames), off-thread — fine for an authoring-time fingerprint.
fn buffer_identity(buffer: &SampleBuffer) -> u64 {
    let mut h = Fnv::new();
    h.write_u64(buffer.sample_rate().to_bits() as u64);
    let chans = buffer.channel_count();
    let frames = buffer.frame_count();
    h.write_u64(chans as u64);
    h.write_u64(frames as u64);
    for c in 0..chans {
        for f in 0..frames {
            h.write_u32(buffer.sample(c, f).to_bits());
        }
    }
    h.finish()
}

/// The recursive content identity of a hosted document (a `voice` or `patch` source):
/// the child's canonical JSON bytes **plus** the content identity of everything *it* resolves —
/// samples, and its own hosted documents, recursively. This is what makes the parent's fingerprint
/// sensitive to a change buried inside a hosted voice patch. `nest` is the `subpatch` node making
/// the reference, whose `bind` the child is built under.
fn hosted_doc_identity(
    doc: &NormalizedDoc,
    id: &str,
    nest: Option<&NodeDoc>,
    referrer: Option<&str>,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
    depth: usize,
) -> u64 {
    if depth > MAX_DEPTH {
        return UNRESOLVED_FAILED;
    }
    let ResourceSource::Canonical(canon) = doc.source_of(id, resolver, referrer) else {
        return UNRESOLVED_MISSING;
    };
    let Ok(text) = resolver.resolve_text(&canon) else {
        return UNRESOLVED_FAILED;
    };
    match NormalizedDoc::from_json(&text, registry, Some(resolver)) {
        Ok(child) => {
            let child = bound_child(doc, child, nest, referrer, resolver);
            doc_content_identity(&child, &canon, registry, resolver, depth + 1)
        }
        // A resolved-but-malformed child: the build would have failed too, but the fingerprint
        // must still be a deterministic function of the content.
        Err(_) => {
            let mut h = Fnv::new();
            h.tagged(b"raw", text.as_bytes());
            h.finish()
        }
    }
}

/// `child` as the loader builds it under `nest`'s `bind`, each value looked up in `host`.
fn bound_child(
    host: &NormalizedDoc,
    child: NormalizedDoc,
    nest: Option<&NodeDoc>,
    referrer: Option<&str>,
    resolver: &dyn ResourceResolver,
) -> NormalizedDoc {
    match nest.filter(|n| !n.bind.is_empty()) {
        Some(n) => child.with_bound(host.bindings_of(n, resolver, referrer)),
        None => child,
    }
}

/// The content identity of a parsed document: the canonical (re-serialized) JSON less its
/// editor-only `layout`, plus each node's resolved sample/voice/patch content, recursively.
/// Re-serializing through [`NormalizedDoc`] makes the identity insensitive to source-text
/// formatting (matching [`crate::content_hash`]). The resolved content is read through `child`'s
/// own bindings, so a bound source's bytes count rather than the default's.
fn doc_content_identity(
    child: &NormalizedDoc,
    canon: &str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
    depth: usize,
) -> u64 {
    let mut h = Fnv::new();
    h.tagged(b"doc", child.engine_json().as_bytes());
    for node in &child.nodes {
        let Some(descriptor) = registry.get(&node.type_name).map(|e| &e.descriptor) else {
            continue;
        };
        if descriptor.has_resource("sample") {
            if let Some(id) = &node.sample {
                h.tag(b"s");
                h.write_u64(sample_identity(child, id, Some(canon), resolver));
            }
        }
        if descriptor.has_resource("voice") {
            if let Some(id) = &node.voice {
                h.tag(b"v");
                h.write_u64(hosted_doc_identity(
                    child,
                    id,
                    None,
                    Some(canon),
                    registry,
                    resolver,
                    depth,
                ));
            }
        }
        if descriptor.has_resource("patch") {
            if let Some(id) = &node.patch {
                h.tag(b"p");
                h.write_u64(hosted_doc_identity(
                    child,
                    id,
                    Some(node),
                    Some(canon),
                    registry,
                    resolver,
                    depth,
                ));
            }
        }
    }
    h.finish()
}

/// Resolve a subpatch node's child document (its `patch` source) to a parsed [`NormalizedDoc`]
/// plus its canonical id, mirroring the loader's subpatch pass (resolve → canonicalize → parse).
/// `None` on any availability failure (no patch ref, missing id, unreadable/malformed text) — a
/// dark subpatch, which contributes no Plan nodes. The child comes back under `node`'s `bind`, as
/// the loader builds it.
fn resolve_child_doc(
    doc: &NormalizedDoc,
    node: &NodeDoc,
    referrer: Option<&str>,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Option<(NormalizedDoc, String)> {
    let id = node.patch.as_ref()?;
    let ResourceSource::Canonical(canon) = doc.source_of(id, resolver, referrer) else {
        return None;
    };
    let text = resolver.resolve_text(&canon).ok()?;
    let child = NormalizedDoc::from_json(&text, registry, Some(resolver)).ok()?;
    let child = bound_child(doc, child, Some(node), referrer, resolver);
    Some((child, canon))
}

/// FNV-1a, 64-bit — the same tiny, platform-stable content hash [`crate::content_hash`] uses, in
/// an incremental form so a node's config, resolved bytes, and nested identities fold into one
/// value. Not cryptographic: it guards against accidental transplant of a changed instantiation,
/// not against an adversary.
struct Fnv(u64);

impl Fnv {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    fn new() -> Self {
        Fnv(Self::OFFSET_BASIS)
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(Self::PRIME);
        }
    }

    fn write_u32(&mut self, v: u32) {
        self.write(&v.to_le_bytes());
    }

    fn write_u64(&mut self, v: u64) {
        self.write(&v.to_le_bytes());
    }

    /// Fold in a domain tag, so adjacent fields cannot alias (a `config` of `x` and a `sample`
    /// hashing to the same bytes stay distinguishable).
    fn tag(&mut self, tag: &[u8]) {
        self.write(tag);
    }

    /// A tag followed by length-prefixed data — the length pins field boundaries so two different
    /// splits of the same bytes cannot collide.
    fn tagged(&mut self, tag: &[u8], data: &[u8]) {
        self.write(tag);
        self.write_u64(data.len() as u64);
        self.write(data);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::load_instrument_doc;
    use crate::resources::MemoryResolver;
    use reuben_core::config::AudioConfig;
    use reuben_core::descriptor::Port;
    use reuben_core::resources::SampleBuffer;

    const DEFAULT_VOICE_JSON: &str =
        include_str!("../../../../instruments/voices/default-voice.json");

    /// A voicer hosting `voices` copies of a voice patch (the hosted-document case).
    fn voicer_doc(voices: u32) -> String {
        format!(
            r#"{{ "format_version": 3, "instrument": "top",
                 "resources": {{ "dv": "voices/default-voice.json" }},
                 "interface": {{ "outputs": {{ "out": {{ "from": "/out.audio" }} }} }},
                 "nodes": [
                   {{ "type": "voicer", "address": "/voicer", "config": {{ "voices": {voices} }},
                      "voice": "dv" }},
                   {{ "type": "output", "address": "/out",
                      "inputs": {{ "audio": {{ "from": "/voicer.audio" }} }} }} ] }}"#
        )
    }

    /// A bare envelope whose `attack` — a pure runtime `inputs` param — a caller varies. The
    /// envelope has no `config` and resolves nothing, so `attack` is entirely outside the
    /// instantiate-time fingerprint: changing it must leave the fingerprint equal.
    fn envelope_doc(attack: f32) -> String {
        format!(
            r#"{{ "format_version": 3, "instrument": "eg",
                 "interface": {{ "outputs": {{ "out": {{ "from": "/out.audio" }} }} }},
                 "nodes": [
                   {{ "type": "envelope", "address": "/env",
                      "inputs": {{ "gate": 1.0, "attack": {attack}, "decay": 0.01,
                                   "sustain": 0.8, "release": 0.5 }} }},
                   {{ "type": "output", "address": "/out",
                      "inputs": {{ "audio": {{ "from": "/env.cv" }} }} }} ] }}"#
        )
    }

    /// A sample player bound to `kick.wav` (the resolved-bytes case).
    const SAMPLE_DOC: &str = r#"{ "format_version": 3, "instrument": "samp",
        "resources": { "kick": "kick.wav" },
        "interface": { "outputs": { "out": { "from": "/samp.audio" } } },
        "nodes": [
          { "type": "sample", "address": "/samp", "sample": "kick" },
          { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/samp.audio" } } } ] }"#;

    fn voice_resolver() -> MemoryResolver {
        let mut r = MemoryResolver::new();
        r.insert_text("voices/default-voice.json", DEFAULT_VOICE_JSON);
        r
    }

    /// A sample resolver serving one buffer at `kick.wav`.
    fn sample_resolver(buffer: SampleBuffer) -> MemoryResolver {
        let mut r = MemoryResolver::new();
        r.insert_sample("kick.wav", buffer);
        r
    }

    fn manifest_for(json: &str, resolver: &dyn ResourceResolver) -> Manifest {
        let registry = Registry::builtin();
        let doc = NormalizedDoc::from_json(json, &registry, Some(resolver)).expect("mint");
        let loaded = load_instrument_doc(&doc, &registry, resolver).expect("load");
        let plan =
            Plan::instantiate(loaded.graph, AudioConfig::new(48_000.0, 128)).expect("instantiate");
        build_manifest(&doc, &plan, &registry, resolver)
    }

    fn fingerprint(manifest: &Manifest, address: &str) -> Option<u64> {
        manifest
            .nodes
            .iter()
            .find(|n| n.address == address)
            .and_then(|n| n.fingerprint)
    }

    #[test]
    fn equal_config_and_content_fingerprint_equal() {
        // The same config + the same resolved content is the same instantiation, so
        // the fingerprints match node-for-node — the property that lets an unchanged node survive.
        let resolver = voice_resolver();
        let a = manifest_for(&voicer_doc(4), &resolver);
        let b = manifest_for(&voicer_doc(4), &resolver);
        assert_eq!(fingerprint(&a, "/voicer"), fingerprint(&b, "/voicer"));
        assert!(
            fingerprint(&a, "/voicer").is_some(),
            "voicer is fingerprinted"
        );
        assert_eq!(fingerprint(&a, "/out"), fingerprint(&b, "/out"));
    }

    #[test]
    fn changed_config_constant_changes_the_fingerprint() {
        // Bumping the voicer's `voices` pool size (a `config` Constant) is a different
        // instantiation — the box would carry the old pool — so the fingerprint must differ.
        let resolver = voice_resolver();
        let four = manifest_for(&voicer_doc(4), &resolver);
        let eight = manifest_for(&voicer_doc(8), &resolver);
        assert_ne!(
            fingerprint(&four, "/voicer"),
            fingerprint(&eight, "/voicer"),
            "a changed config constant must change the fingerprint"
        );
        // A node the constant did not touch is unchanged — the reset is scoped to the edit.
        assert_eq!(fingerprint(&four, "/out"), fingerprint(&eight, "/out"));
    }

    #[test]
    fn changed_runtime_input_keeps_the_fingerprint() {
        // The survivor half of the asymmetry, the exact counterpart to
        // `changed_config_constant_changes_the_fingerprint`: the fingerprint covers only
        // instantiate-time inputs (`config` + resolved content), never a node's runtime `inputs`.
        // Two documents identical but for the envelope's `attack` — a pure runtime param — must
        // fingerprint node-for-node equal, so the edited node stays a survivor (its box transplants
        // and the new Plan's latch supplies the new value; the edit is not undone).
        let resolver = MemoryResolver::new();
        let slow = envelope_doc(0.5);
        let fast = envelope_doc(0.05);
        assert_ne!(slow, fast, "the two documents really differ in the param");
        let a = manifest_for(&slow, &resolver);
        let b = manifest_for(&fast, &resolver);
        assert!(
            fingerprint(&a, "/env").is_some(),
            "the envelope is fingerprinted"
        );
        assert_eq!(
            fingerprint(&a, "/env"),
            fingerprint(&b, "/env"),
            "a changed runtime input must NOT change the fingerprint"
        );
        // The untouched node matches too — nothing about the edit perturbs any fingerprint.
        assert_eq!(fingerprint(&a, "/out"), fingerprint(&b, "/out"));
    }

    #[test]
    fn changed_resolved_bytes_change_the_fingerprint() {
        // The sample's identity is its decoded content, not its path — a
        // re-upload of different bytes at the same path is a different instantiation.
        let x = manifest_for(
            SAMPLE_DOC,
            &sample_resolver(SampleBuffer::new(vec![vec![0.1, 0.2, 0.3]], 48_000.0)),
        );
        let y = manifest_for(
            SAMPLE_DOC,
            &sample_resolver(SampleBuffer::new(vec![vec![0.9, -0.4, 0.7]], 48_000.0)),
        );
        assert_ne!(
            fingerprint(&x, "/samp"),
            fingerprint(&y, "/samp"),
            "changed resolved sample bytes must change the fingerprint"
        );
    }

    #[test]
    fn equal_resolved_bytes_keep_the_fingerprint() {
        // The converse: the same document + the same bytes at the same path fingerprint equal, so
        // a re-swap of an unchanged sample survives rather than resetting.
        let buffer = SampleBuffer::new(vec![vec![0.1, 0.2, 0.3]], 48_000.0);
        let a = manifest_for(SAMPLE_DOC, &sample_resolver(buffer.clone()));
        let b = manifest_for(SAMPLE_DOC, &sample_resolver(buffer));
        assert_eq!(fingerprint(&a, "/samp"), fingerprint(&b, "/samp"));
    }

    #[test]
    fn changed_content_inside_a_hosted_voice_doc_changes_the_parent_fingerprint() {
        // Prove the recursion: the top document is byte-identical, but the hosted
        // voice patch's own content changes — a constant buried inside it. The parent voicer's
        // fingerprint must follow, because the voice document is part of what the voicer resolved
        // at Instantiate.
        let mut altered = MemoryResolver::new();
        // Same voice, one inner constant changed (the filter cutoff) — different bytes, same shape.
        let tweaked = DEFAULT_VOICE_JSON.replace("3000.0", "1200.0");
        assert_ne!(
            tweaked, DEFAULT_VOICE_JSON,
            "fixture actually changed content"
        );
        altered.insert_text("voices/default-voice.json", tweaked);

        let original = manifest_for(&voicer_doc(4), &voice_resolver());
        let changed = manifest_for(&voicer_doc(4), &altered);
        assert_ne!(
            fingerprint(&original, "/voicer"),
            fingerprint(&changed, "/voicer"),
            "a change inside the hosted voice document must change the parent's fingerprint"
        );
    }

    /// A sampled child whose own `sample` row defaults to `lib/default` — the bindable case.
    const BOUND_CHILD: &str = r#"{ "format_version": 4, "instrument": "voice",
        "resources": { "sample": "lib/default" },
        "interface": { "outputs": { "audio": { "from": "/s.audio" } } },
        "nodes": [ { "type": "sample", "address": "/s", "sample": "sample" } ] }"#;

    /// A host nesting [`BOUND_CHILD`] at `/a` with `sample` bound to the host id `bound`, whose
    /// row points at `source`.
    fn bound_host(bound: &str, source: &str) -> String {
        format!(
            r#"{{ "format_version": 4, "instrument": "host",
                 "resources": {{ "voice": "lib/voice", "{bound}": "{source}" }},
                 "interface": {{ "outputs": {{ "out": {{ "from": "/a.audio" }} }} }},
                 "nodes": [ {{ "type": "subpatch", "address": "/a", "patch": "voice",
                               "bind": {{ "sample": "{bound}" }} }} ] }}"#
        )
    }

    /// [`BOUND_CHILD`] plus samples whose bytes all differ.
    fn bound_resolver() -> MemoryResolver {
        let mut r = MemoryResolver::new();
        r.insert_text("lib/voice", BOUND_CHILD);
        for (key, level) in [
            ("lib/default", 0.1),
            ("lib/scrape", 0.25),
            ("lib/hiss", 0.5),
        ] {
            r.insert_sample(key, SampleBuffer::new(vec![vec![level; 4]], 48_000.0));
        }
        r
    }

    #[test]
    fn rebinding_a_subpatch_to_another_host_id_resets_the_bound_node() {
        let resolver = bound_resolver();
        let old = manifest_for(&bound_host("scrape", "lib/scrape"), &resolver);
        let new = manifest_for(&bound_host("hiss", "lib/hiss"), &resolver);
        assert_ne!(fingerprint(&old, "/a/s"), fingerprint(&new, "/a/s"));
        let (_, diff) = old.diff(&new);
        assert_eq!(diff.state_reset, vec!["/a/s".to_string()], "{diff:?}");
    }

    #[test]
    fn repointing_the_bound_host_row_resets_the_bound_node() {
        let resolver = bound_resolver();
        let old = manifest_for(&bound_host("scrape", "lib/scrape"), &resolver);
        let new = manifest_for(&bound_host("scrape", "lib/hiss"), &resolver);
        let (_, diff) = old.diff(&new);
        assert_eq!(diff.state_reset, vec!["/a/s".to_string()], "{diff:?}");
    }

    #[test]
    fn an_unchanged_binding_keeps_the_bound_node_a_survivor() {
        let resolver = bound_resolver();
        let old = manifest_for(&bound_host("scrape", "lib/scrape"), &resolver);
        let new = manifest_for(&bound_host("scrape", "lib/scrape"), &resolver);
        let (_, diff) = old.diff(&new);
        assert_eq!(diff.survived, 1, "{diff:?}");
        assert!(diff.state_reset.is_empty(), "{diff:?}");
    }

    #[test]
    fn a_bound_sample_inside_a_hosted_voice_reaches_the_voicer_fingerprint() {
        // A voice patch nesting the bindable child: what the voicer hosts includes the bound
        // sample's bytes, not the child's default.
        let voice = |bound: &str| {
            format!(
                r#"{{ "format_version": 4, "instrument": "v",
                     "resources": {{ "child": "lib/voice", "b": "{bound}" }},
                     "interface": {{ "outputs": {{ "audio": {{ "from": "/c.audio" }} }} }},
                     "nodes": [ {{ "type": "subpatch", "address": "/c", "patch": "child",
                                   "bind": {{ "sample": "b" }} }} ] }}"#
            )
        };
        let host = r#"{ "format_version": 4, "instrument": "top",
            "resources": { "v": "lib/v" },
            "interface": { "outputs": { "out": { "from": "/vo.audio" } } },
            "nodes": [ { "type": "voicer", "address": "/vo", "voice": "v" } ] }"#;
        let mut scrape = bound_resolver();
        scrape.insert_text("lib/v", voice("lib/scrape"));
        let mut hiss = bound_resolver();
        hiss.insert_text("lib/v", voice("lib/hiss"));
        // The voice text unchanged, only the bytes behind its bound row.
        let mut hiss_bytes = bound_resolver();
        hiss_bytes.insert_text("lib/v", voice("lib/scrape"));
        hiss_bytes.insert_sample(
            "lib/scrape",
            SampleBuffer::new(vec![vec![0.5; 4]], 48_000.0),
        );
        let a = manifest_for(host, &scrape);
        assert_ne!(
            fingerprint(&a, "/vo"),
            fingerprint(&manifest_for(host, &hiss), "/vo")
        );
        assert_ne!(
            fingerprint(&a, "/vo"),
            fingerprint(&manifest_for(host, &hiss_bytes), "/vo"),
            "new bytes behind an unchanged bound row must move the voicer's fingerprint"
        );
    }

    #[test]
    fn diff_pairs_survivors_and_lists_resets_added_removed() {
        // The migration table + DiffSummary over two manifests: an unchanged node survives (paired
        // old→new by index), a config change resets, a new address is added, a dropped one removed.
        let resolver = voice_resolver();
        let old = manifest_for(&voicer_doc(4), &resolver);
        let new = manifest_for(&voicer_doc(8), &resolver);
        let (table, diff) = old.diff(&new);

        // `/out` survives (identical), `/voicer` resets (voices changed).
        assert_eq!(diff.survived, 1);
        assert_eq!(diff.state_reset, vec!["/voicer".to_string()]);
        assert!(diff.added.is_empty() && diff.removed.is_empty());
        assert_eq!(table.len(), 1);
        // The survivor pair maps `/out`'s old index to its new index.
        let out_old = old.nodes.iter().position(|n| n.address == "/out").unwrap();
        let out_new = new.nodes.iter().position(|n| n.address == "/out").unwrap();
        assert_eq!(table.survivors(), &[(out_old, out_new)]);
    }

    /// `/m.a` is fed `a`, `/m.b` is wired from `/k` when `b_wired`, else the constant `1.0`.
    fn product_doc(a: f32, b_wired: bool) -> String {
        let b = if b_wired {
            r#"{ "from": "/k.out" }"#.to_string()
        } else {
            "1.0".to_string()
        };
        format!(
            r#"{{ "format_version": 3, "instrument": "p",
                 "interface": {{ "outputs": {{ "out": {{ "from": "/out.audio" }} }} }},
                 "nodes": [
                   {{ "type": "mul_f32_signal", "address": "/k", "inputs": {{ "a": 1.0 }} }},
                   {{ "type": "mul_f32_signal", "address": "/m", "inputs": {{ "a": {a}, "b": {b} }} }},
                   {{ "type": "output", "address": "/out",
                      "inputs": {{ "audio": {{ "from": "/m.out" }} }} }} ] }}"#
        )
    }

    /// The ports of `address` whose latch the table carries onto the same port.
    fn carried(old: &Manifest, new: &Manifest, address: &str) -> Vec<usize> {
        let (table, _) = old.diff(new);
        let old_idx = old.nodes.iter().position(|n| n.address == address).unwrap();
        let new_idx = new.nodes.iter().position(|n| n.address == address).unwrap();
        (table.live_values().iter())
            .filter_map(|c| match *c {
                LiveCarry::Latch {
                    from: (o, op),
                    to: (n, np),
                } if (o, n) == (old_idx, new_idx) && op == np => Some(np),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_survivor_carries_exactly_the_inputs_whose_document_value_held() {
        let resolver = MemoryResolver::new();
        let base = manifest_for(&product_doc(0.3, false), &resolver);
        // `mul_f32_signal` declares `a` (port 0) then `b` (port 1).
        assert_eq!(carried(&base, &base, "/m"), vec![0, 1], "nothing edited");
        let a_edited = manifest_for(&product_doc(0.5, false), &resolver);
        assert_eq!(carried(&base, &a_edited, "/m"), vec![1], "a edited");
        let b_wired = manifest_for(&product_doc(0.3, true), &resolver);
        assert_eq!(carried(&base, &b_wired, "/m"), vec![0], "b gained a wire");
        assert_eq!(carried(&b_wired, &base, "/m"), vec![0], "b lost its wire");
        assert_eq!(
            carried(&b_wired, &b_wired, "/m"),
            vec![0],
            "a wired input carries nothing"
        );
    }

    #[test]
    fn a_node_input_and_an_interface_input_never_carry_into_each_other() {
        // One address, one surviving node, equal values: only the route differs.
        let slot = |route: LatchRoute| LiveSlot {
            address: "/level/in".to_string(),
            seed: reuben_core::message::Arg::F32(0.3),
            typed_by: Port::f32_buffer("in"),
            route,
        };
        let manifest = |route: LatchRoute| Manifest {
            nodes: vec![NodeIdentity {
                address: "/level".to_string(),
                type_name: "m2s".to_string(),
                fingerprint: Some(1),
            }],
            slots: vec![slot(route)],
        };
        let node = LatchRoute::Node { node: 0, port: 0 };
        let pipe = LatchRoute::Interface(InterfaceAt::Pipe(0));
        let alias = LatchRoute::Interface(InterfaceAt::Alias(0));
        let carries = |old: LatchRoute, new: LatchRoute| {
            !(manifest(old).diff(&manifest(new)).0)
                .live_values()
                .is_empty()
        };
        for (old, new, expected) in [
            (node, node, true),
            (pipe, alias, true),
            (alias, pipe, true),
            (node, pipe, false),
            (alias, node, false),
        ] {
            assert_eq!(carries(old, new), expected, "{old:?} -> {new:?}");
        }
    }
}
