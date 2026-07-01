# General instrument-as-operator nesting: a `subpatch` node inlined at plan-build

## Status

Accepted (2026-06-28). Resolved in a grilling session (the design half of [#64](https://github.com/Impractical-Instruments/reuben/issues/64); P1 of the nesting epic [#123](https://github.com/Impractical-Instruments/reuben/issues/123)). This is the **design gate** — it produces the semantics only; no engine/loader code lands here (that is P3–P6: [#118](https://github.com/Impractical-Instruments/reuben/issues/118)/[#119](https://github.com/Impractical-Instruments/reuben/issues/119)/[#120](https://github.com/Impractical-Instruments/reuben/issues/120)/[#121](https://github.com/Impractical-Instruments/reuben/issues/121)).

Realizes the general case of [ADR-0003](0003-recursive-composition.md) ("an Instrument is a reusable subgraph with boundary ports, used as if it were an Operator, inlined into the flat schedule at zero runtime cost"). Builds directly on the boundary + resource machinery [ADR-0032](0032-voicer-hosts-voice-subpatches.md) introduced for the Voicer — the `interface` block, the instrument-kind resource, `resolve_instrument` — but takes the **inline** path 0032 deliberately left for static nesting, where the Voicer takes the **host** path. Discharges the surface/contract items [ADR-0017](0017-playable-surface-and-control-domain.md) deferred to "the nesting / contract thread."

## Context

reuben's composition model is recursive ([ADR-0003](0003-recursive-composition.md)): an Instrument is a named subgraph exposing boundary ports, reusable inside another Instrument *as if it were an Operator*. The whole graph is flattened into one topological schedule at plan-build ([ADR-0001](0001-unified-block-graph-execution.md)), so nesting is an authoring concept that should cost nothing at runtime.

Today the **only** nesting that works is the Voicer hosting voice sub-patches ([ADR-0032](0032-voicer-hosts-voice-subpatches.md)) — a specialized, polyphony-shaped path. General instrument-in-instrument nesting was designed-but-unbuilt (the ungrilled thread [#64](https://github.com/Impractical-Instruments/reuben/issues/64)). Three pieces of the foundation already exist and are reused wholesale:

- The **`interface` block** ([`InterfaceDoc`](../../crates/reuben-core/src/format.rs) → [`Interface`](../../crates/reuben-core/src/graph.rs), resolved at `build()`) — an engine-honored, direction-checked boundary mapping external names → internal `(node, port)`.
- The **instrument-kind resource** and **`resolve_instrument`** (`format.rs`) — reads a referenced patch and builds it to a `Graph` recursively, resources and all.
- The **resource table + `ResourceResolver` seam** ([ADR-0016](0016-sample-player-and-resource-store.md)) — the reference-by-id pattern a nested instrument reuses.

What is missing is the **general** path: a generic node that references a sub-instrument, and **plan-build inlining** outside the Voicer. This ADR settles the semantics so that path can be built.

It pins: how a node names a sub-instrument; inline-dissolve vs. runtime-host and where the line is; what happens to internal addresses on inline; the synthesized boundary face and its metadata; where cross-boundary type-checking lives; and whether a stable node `id` distinct from `address` is needed.

## Decision

### 1. A reserved `subpatch` node type with a `patch` resource slot

A nested instrument is referenced by a **built-in `subpatch` node** that declares a single `patch` **resource slot** (ADR-0016), resolved through the existing `resources` table:

```json
"resources": { "myreverb": "instruments/reverb.json" },
"nodes": [
  { "type": "subpatch", "address": "/reverb", "patch": "myreverb",
    "inputs": { "in": { "from": "/dry.audio" }, "wet": 0.3 } }
]
```

This is the exact shape of the Voicer's `voice` slot, generalized: a logical id into `resources`, resolved by `resolve_instrument` into a built sub-`Graph`. The node's **ports are not registered** — they are **synthesized** from the resolved patch's `interface` (§4): each `interface.inputs` name becomes a `subpatch` input port, each `interface.outputs` name an output port. So `/reverb` above presents inputs `in`, `wet` and whatever outputs `reverb.json` exposes, even though no operator named `reverb` exists in the registry.

The invariant **`type` = a registered operator** is preserved: `subpatch` *is* a registered (built-in) type; the patch reference rides in the `patch` field, not in `type`.

**Failure taxonomy — the ADR-0016 split lands at the fetch seam.** *Availability* problems (the id missing from `resources`, a `resolve_text` failure) degrade to a `LoadWarning` and leave the node with no sub-graph. Everything after the text is in hand is a **structural error in the referenced patch and stays fatal** (`LoadError`) — including JSON that fails to parse. A resolved-but-malformed child is a broken document the author must fix, not a missing resource to play through; treating it as ADR-0016's "bad decode → warn" would misread that clause, which classifies *sample* decoding behind the resolver seam, where no document was ever promised.

**Considered and rejected:**

- **`type` names the resource id** (`"type": "my_reverb"`). Reads most like ADR-0003's "as if it were an Operator," but it breaks the `type` = registered-operator invariant and forces the loader into a registry-miss → resource-fallthrough lookup, blurring the line between an operator and a patch at the one place authors and tooling rely on it being sharp. The single extra word (`"type": "subpatch"` + `"patch": "id"`) is a cheap price for keeping that line crisp, and it matches the precedent the Voicer already set.
- **An inline patch object** embedded in the node. Rejected for the same reasons [ADR-0016](0016-sample-player-and-resource-store.md)/[ADR-0032](0032-voicer-hosts-voice-subpatches.md) rejected inline blobs for samples and voices: it scatters the library concern across nodes, bloats diffs, defeats the resolve/dedup/versioning the `resources` table exists to centralize, and gives a reused sub-instrument no single home.

### 2. Inline at plan-build (dissolve), not host; the Voicer stays the exception

A `subpatch` is **inlined**: at build, its child nodes are spliced into the parent `Graph`, the child addresses are namespace-prefixed (§3), and every parent wire that targeted a boundary port is **rewired to point straight at the inner target `(node, port)`** the interface names. After the splice the `subpatch` node **is gone** — it dissolves into ordinary parent nodes and edges. The `Plan` and renderer never see a "sub-instrument operator"; there is **no sub-`Plan`, no sub-arena, no re-entrant render call** for a static nest. This is the literal realization of ADR-0003's "inlined into the single flat topological schedule, zero runtime cost."

This is a **different mechanism** from the Voicer ([ADR-0032](0032-voicer-hosts-voice-subpatches.md) §3–4), which keeps each voice as its own `Plan` + arena and calls the re-entrant `render(plan, arena, frames)` per active voice. The two are split by a clean line:

> **Fixed-at-build cardinality → inline (dissolve). Runtime-varying cardinality → host.**

The Voicer hosts because voices *come and go*: idle ones are skipped, stealing reassigns them mid-block — a fixed build-time splice cannot express "render only the active voices this block." A static nest has exactly one instance, always rendered, so it inlines. The Voicer remains the **sole** host; it is not a precedent for general nesting and general nesting is not a precedent for it.

**Considered and rejected:**

- **Host static nests too** (one mechanism — always keep a sub-`Plan` and call the re-entrant renderer). Uniform, and it reuses 0032's machinery directly. Rejected: it re-introduces the per-instance arena + render-call overhead ADR-0003 exists to avoid, on the common case (static composition) that has none of the dynamism that justifies it for voices. Inline is strictly cheaper and is the model 0003 already chose.
- **Inline the Voicer's voices too** (one mechanism — always dissolve). Rejected by [ADR-0032](0032-voicer-hosts-voice-subpatches.md) already: a build-time splice can't express runtime voice allocation/stealing/idle-skip. The two mechanisms coexist by design.

### 3. Encapsulation by namespace-prefix; internals stay reachable; collisions are fatal

On inline, each child node's address is **prefixed by the `subpatch` node's address**: child `/filter` inside `/reverb` becomes `/reverb/filter`. Nesting compounds by repeated prefixing (`/outer/inner/filter`). Prefixing is the per-reuse identity ADR-0003 requires — two uses of the same patch (`/reverb`, `/reverb2`) yield disjoint address sets and therefore disjoint state, automatically.

The prefixed internal addresses **remain externally addressable** (OSC can reach `/reverb/filter`). Encapsulation here is a **namespace, not a seal**: it scopes and disambiguates, it does not hide. This matches the flat-inlined-graph reality — after dissolve the nodes genuinely exist as first-class parent nodes — and ADR-0017's "additive" surface (curated/boundary names are *aliases*; structural addresses still resolve because the runtime is one flat graph).

Crucially, **prefixing cannot break wiring.** Connections are resolved to `NodeKey`s at build (`Connection { src: NodeKey, … }`, `graph.rs`), not addresses; an address is a routing/OSC name, not a runtime wire reference. So rewriting addresses on inline is a pure naming transform over already-resolved edges — the scary-looking part of inlining is a non-event.

A child address that, **after prefixing**, collides with an existing parent address is a fatal `DuplicateAddress` load error — the existing uniqueness check (`format.rs` build pass 1) simply runs again over the post-inline address set.

**Considered and rejected:**

- **Sealed boundary (only `interface` ports public).** True encapsulation — internals get opaque/generated addresses unreachable by OSC. Cleaner as an abstraction, but it throws away the ad-hoc reach (debugging, automation, wildcard dispatch) that a flat addressable graph gives for free, and it contradicts the "additive" half of ADR-0017's "additive-then-encapsulating." The boundary is still *the contract* (authoring should wire through it), but reachability is not sacrificed to enforce that.
- **Sealed-by-default with opt-in re-export.** Most flexible, but it is format surface invented before any consumer needs it; deferred until a concrete need for hiding appears. Today reachability has no cost worth a new mechanism.

### 4. The synthesized boundary face: an owned-string artifact, type inherited and locked, presentation inherited and overridable

The `subpatch` node's ports are a **synthesized boundary descriptor** computed from the resolved child's `interface`. One port per interface name; for each, the port's properties come from the **inner port** the interface entry points at:

- **`Arg` type is inherited and *not* overridable.** The synthesized input/output port carries the inner port's type verbatim. Overriding it would let the boundary lie to the type-checker (§5) and to a downstream wire — structurally forbidden.
- **Presentational metadata** (label, unit, range, widget) is inherited from the inner port and **overridable per-field** in the `interface` block (the additive-then-encapsulating, "metadata inherited from the internal param, overridable" surface from [#64](https://github.com/Impractical-Instruments/reuben/issues/64)/[ADR-0017](0017-playable-surface-and-control-domain.md)). An override decorates how a control presents; it never changes what type flows.

This synthesized face is a **separate, owned-string artifact**, *not* the engine [`Descriptor`](../../crates/reuben-core/src/descriptor.rs). The engine `Descriptor`/`Port` are `&'static str` end to end because operators are compile-time-registered ([ADR-0024](0024-compile-time-operator-registration.md)/[ADR-0025](0025-single-source-operator-contract.md)); a nested face is computed at load from runtime-owned strings and cannot inhabit that type. The synthesized face exists only long enough to (a) type-check the parent's boundary wires at build and (b) feed introspection/schema/docs (P6, [#121](https://github.com/Impractical-Instruments/reuben/issues/121)). Once §2's inline dissolves the node, **the runtime holds no synthesized descriptor at all** — which is what keeps "zero runtime cost" honest.

**Considered and rejected:**

- **Make the engine `Descriptor` own its strings** (`Cow<'static, str>` or `String`) so a synthesized face *is* a real `Descriptor`. Rejected: it churns every operator, the registry, and the contract macro to carry runtime ownership that all but the nested case never use, to model a thing that dissolves before render anyway. A purpose-built owned artifact is the smaller, truer change.
- **Type override allowed** (an interface entry may re-declare a port's type). Rejected: it is a type system hole with no musical payoff — cross-domain change is what the explicit converter operators are for ([ADR-0017](0017-playable-surface-and-control-domain.md)/[ADR-0031](0031-float-resolves-to-value-or-signal-by-wiring.md)), authored *inside* the patch, visible and type-checked, not smuggled through the boundary.

### 5. Cross-boundary type-checking falls out of the existing pass-2 wire check

There is **no new type-checker.** Build pass 2 already `Arg`-type-checks every wire as it resolves wire-refs to edges ([ADR-0030](0030-osc-as-all-data-one-message-type.md), `format.rs` build). Because the synthesized boundary face (§4) presents its ports with the **inner ports' real `Arg` types**, a parent wire into `/reverb.wet` is checked against `wet`'s true inner type by the same pass, unchanged. Inlining (§2) then rewires that edge to the inner target — which already passed the same check inside the child build — so the splice introduces no untyped edge.

P5 ([#120](https://github.com/Impractical-Instruments/reuben/issues/120)) is therefore **"make the synthesized port types faithful to the inner ports," not "write a cross-boundary checker."** The check is faithfulness of §4's synthesis, exercised end to end.

**Considered and rejected:**

- **A dedicated boundary type-check pass.** Redundant once the synthesized face is faithful: it would re-implement, at the boundary, the per-wire `Arg` check that already guards every other wire. Faithful synthesis reuses one checker instead of maintaining two.

### 6. No stable node `id`: the interface name and namespacing already carry it

This ADR introduces **no `id` distinct from `address`.** The refactor-safety `id` was meant to buy is already provided, from two directions:

- **Across documents** — the `interface` name *is* the stable public handle. A parent references `/reverb.wet`; the child may rename its internal `/mix` → `/wetdry` freely (sweeping its own document), and the public name `wet` never moves. The `interface` block is itself the indirection layer that decouples the public contract from the internal address; a stable `id` would be a second, redundant one.
- **Within a document** — internal address renames are the **address-rename refactor tool**'s job ([ADR-0017](0017-playable-surface-and-control-domain.md)), an atomic JSON-structural sweep of all refs at once. `id` is an optimization on top of that tool, not a primitive nesting needs.

And `id` cannot help the one place address-fragility genuinely bites — **external OSC senders** key on the address string, so a rename breaks them regardless ([ADR-0017](0017-playable-surface-and-control-domain.md) already rules these "can't reach — warn"). Namespacing (§3) supplies per-reuse identity. So every job `id` was nominated for is covered; minting it would add format surface (and ADR-0017's "id-default trap": auto-pinning `id` = old address before a rename) for no uncovered consumer.

This resolves the design question P2/[#117](https://github.com/Impractical-Instruments/reuben/issues/117) was created to answer by **declining its premise.** P2 is closed; any residual "unify connections + resource refs under one id namespace" idea is format-library hygiene that belongs to [#65](https://github.com/Impractical-Instruments/reuben/issues/65), explicitly **not** a dependency of the nesting build (the epic's minimal vertical slice is already P1→P3→P4→P5, P2 excluded).

**Considered and rejected:**

- **A stable `id`, defaulting to address, opt-in** (the [ADR-0017](0017-playable-surface-and-control-domain.md) sketch). Rejected here because, as above, the `interface` name covers cross-document stability and the rename tool covers intra-document stability, leaving `id` with no consumer the two don't already serve — while it drags its own complexity (the id-default trap, a second identity namespace).
- **Reserve `id` in the ADR without wiring it.** Rejected: an ADR decision with no enforced consequence is a standing invitation to build the wart later "because the ADR said so." If a real consumer appears, it earns its own ADR then.

### Resolution ordering (a note for P3)

A `subpatch` node, unlike every registered operator and unlike the Voicer, has **no static face** — its ports are unknown until the child patch is resolved. So the child must be resolved (via `resolve_instrument`) at **parent-build time**, *earlier* than ADR-0016's pipeline, which resolves resources *after* the `Graph` is built (`parse → build Graph → resolve refs → bind_resources`). The build path must therefore have the `ResourceResolver` in hand (thread it into `build`, or run inlining as a post-resolve graph-surgery pass holding both registry and resolver). This is P3/P4 mechanics ([#118](https://github.com/Impractical-Instruments/reuben/issues/118)/[#119](https://github.com/Impractical-Instruments/reuben/issues/119)); it is flagged here so the inversion is a designed decision, not a surprise.

## Consequences

- **New format surface:** a built-in **`subpatch`** node type declaring a **`patch`** resource slot (a third `(slot, ref)` entry alongside `sample`/`voice` in `NodeDoc::resource_refs`). No other JSON section is added; nesting reuses `resources` + `interface` as-is.
- **The loader gains a build-time inline pass** (P4): resolve child → synthesize boundary face → type-check parent boundary wires → splice nodes, prefix addresses, rewire boundary edges to inner targets, re-run the duplicate-address check → dissolve the node. Recurses for nested `subpatch`es.
- **Resolution moves earlier for `subpatch`** — the resolver must be available during build (see the ordering note). `sample`/`voice` resolution is unaffected.
- **A synthesized-boundary artifact** (owned strings) is added for build-time type-checking and introspection (P6); it is not the engine `Descriptor` and does not survive into render.
- **Cross-boundary type-checking is the existing pass-2 wire check** over faithful synthesized types — no new checker (P5 = faithfulness).
- **Internal addresses are namespace-prefixed on inline** and stay OSC-reachable; per-reuse state isolation is automatic via disjoint prefixes; post-prefix address collisions are fatal.
- **No node `id` is introduced.** **P2/[#117](https://github.com/Impractical-Instruments/reuben/issues/117) is closed** (premise declined); the id-unification residue, if any, folds into the format/library thread [#65](https://github.com/Impractical-Instruments/reuben/issues/65) and is not a nesting dependency.
- **The Voicer is untouched** and remains the sole runtime-host; this ADR adds the parallel inline path 0032 anticipated.
- **Unblocks** P3 (sub-instrument node ref + recursive load, [#118](https://github.com/Impractical-Instruments/reuben/issues/118)), P4 (plan-build inlining, [#119](https://github.com/Impractical-Instruments/reuben/issues/119)), P5 (cross-boundary type-check, [#120](https://github.com/Impractical-Instruments/reuben/issues/120)), P6 (schema + introspection + docs, [#121](https://github.com/Impractical-Instruments/reuben/issues/121)). P7 (instrument library, [#122](https://github.com/Impractical-Instruments/reuben/issues/122)) trails independently.
- **Terminology:** *subpatch* = a nested instrument referenced as a node; *inline / dissolve* = the build-time splice that flattens a subpatch into the parent graph; *host* = the Voicer's runtime sub-plan path (the dynamic-cardinality exception); *boundary face* = the synthesized, owned-string port set a subpatch presents from its child's `interface`.
