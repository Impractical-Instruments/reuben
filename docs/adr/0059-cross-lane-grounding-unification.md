# ADR-0059: Cross-lane grounding unification — shared input handling, host-owned output filtering

## Status

Accepted (2026-07-16). The cross-lane grounding unification & prompt-architecture decision of
the patch-pipeline streamlining effort — wayfinder ticket
[Patch-pipeline/J (reuben-web#102)](https://github.com/Impractical-Instruments/reuben-web/issues/102)
on map [reuben-web#81](https://github.com/Impractical-Instruments/reuben-web/issues/81),
grounded on [the grounding audit (reuben-web#83)](https://github.com/Impractical-Instruments/reuben-web/issues/83)
and the 2026-07-16 three-lane survey (comment on reuben-web#81). **Rides on**
[ADR-0051](0051-authoring-grounding-single-source.md) (single-source grounding, gist-and-point),
[ADR-0052](0052-web-parity-contract-not-protocol.md) (parity as contract),
[ADR-0054](0054-web-chat-agent-host.md) (the web chat agent host),
[ADR-0057](0057-instrument-reuse-interface-makes-the-role.md) (recipes are instruments),
[ADR-0058](0058-intent-vocabulary-word-to-move-table.md) (the word→move table, three delivery
lanes). **Amends** [ADR-0048](0048-mcp-tool-surface-and-contracts.md) §7 (resource surface:
adds the library index, removes `reuben://schema/instrument`) and
[ADR-0040](0040-raw-c-abi-worklet-boundary.md) §4's guard mechanism (the committed-schema
registry pin is replaced by same-commit parity).

## Context

- The **verb layer is unified**: web wasm bridge, MCP tools, and CLI all descend to
  `reuben_core::introspect` / `schema::generate` (ADR-0052 held). The **prose/policy layer is
  not**: the web persona (~4.3k tok) is hardcoded web-repo-side with no engine home; MCP's
  `instructions` is six lines; the skills carry their own workflow prose; the web lane serves
  **no authoring-guide grounding at all** (reuben-web#83).
- Un-unified lanes make the pipeline effort's measurements noisy: a layer's token/latency win
  can't be trusted when each lane serves different sauce.
- Three delivery lanes (ADR-0058): repo **skills** (checkout, pointers work), **MCP** clients
  (in-band resources are their only grounding), **web** chat (no checkout, no resources —
  gist-complete or nothing).
- The web system prompt mixes voice rules ("talk about the sound, not the machine", register
  ratchet, failure posture) with page-tool wiring (`suggest`/`point_to_keep`, Keep, turn-one
  shapes) and restated authoring mechanics.

## Decision

### 1. The axis: input handling is shared sauce; output filtering is host flavor

The portable/lane-specific boundary is **not** a persona split and not a register mechanism —
it is the direction of the language:

- **Input handling** — interpreting musical/mood/abstract language as patching moves — is
  identical in every lane: a dev patching in the repo says "warmer" too. It is the base sauce
  all three lanes consume: the word→move table (ADR-0058) plus the edge conduct around
  imperfect mappings (§2).
- **Output filtering** — what the person is shown: the sound-not-machine subject rule, hidden
  failures and diagnostics, silent tool planning, the plain→theory register ratchet, the
  naming table, tone — is **zero at skills/MCP and maximal at web**. "Persona" *means* the
  output filter. It stays web-repo-side (`proxy/system-prompt.mjs`) as a **composable host
  module**, flavored for its consumer; a future musician-facing desktop host imports it, a
  dev harness never sees it.
- **MCP adds nothing over skills** — delivery only (in-band resources + a gist pointer); its
  differences are subtractions (no recompile verbs, degraded filesystem gestures). Furniture
  (page gestures, Keep, FTUE shapes) belongs to **hosts**, not lanes.

Every lane host reduces to **transport bindings + lane furniture + the base sauce**: web
bundles the sauce at build, MCP serves it in-band, skills point at it.

**Considered and rejected:** a third "structural" register tier with host-set bounds (models
dev-vs-musician as depth when it is actually direction — and forces one doc to carry
contradictory absolutes); an engine-side home for the persona (it is product voice for one
host, not lane sauce); patcher consuming a named subset of the persona (a subset boundary is
a standing drift seam).

### 2. Factoring the existing prose

- **Edge conduct** (ambiguous ask → pick the most-likely reading, act, offer alternative
  readings; unsatisfiable → offer the nearest achievable move) moves from the web persona into
  the **vocabulary artifact's curated source** as a preamble section, rendered into the same
  view (ADR-0058's delivery, staleness CI, and human-locked curation apply unchanged).
  Offering alternatives is base conduct; only the *tappable chip* wiring is web furniture.
- **Loop conduct** (validate proves *legal*, not *audible* — check generator→output reach
  before reporting; when unsure of a port, `describe` it — never infer) moves from the
  `patcher` skill into the **guide's authoring-loop section**.
- **Recipes**: the patcher's ~70 lines of canonical-recipe prose are superseded by ADR-0057's
  seed recipe instruments + generated library index; the skill's recipe section thins to a
  pointer once they land.
- **`patcher` keeps** only what is genuinely skills-lane: CLI transport strings, the
  skill-routing scope table, the report format, the tests pointer.
- **Plugin posture** (named principle): registry-varying content is **generated or CI-keyed,
  never hand-fixed in prose**; curated artifacts must stay composable at generation time. An
  operator the table doesn't know degrades to the direction-only block + live `describe` — by
  design.

### 3. Delivery: push what earns its keep every session; pull the rest

**Push** = bundled into context, paid every session (web's only channel today). **Pull** =
pointer or resource, free until followed (skills, MCP). Per artifact:

| Artifact | Skills (pull) | MCP (pull) | Web (push) |
|---|---|---|---|
| Guide (+ loop conduct) | pointer | `reuben://guide/authoring` | bundled: full **minus checkout-only sections**, cut by `lanes:` section tags at build |
| Vocabulary view (+ edge conduct) | pointer | `reuben://guide/vocabulary` | bundled |
| Library index (ADR-0057) | pointer | new resource URI (ADR-0048 §7 amendment) | **seed recipes + index bundled at build** — the app already bundles instrument JSON; swap must resolve a doc's reference to a bundled recipe |
| Compact describe projection | CLI flag | tool param | bundled |
| Output filter | — | — | web module |
| Instrument JSON Schema | **deleted** | **deleted** | **deleted** (§4) |

The compact describe projection is a **mode of the verb** — one projection in
`reuben_core::introspect` off the same `Descriptor` — not a fourth prose artifact. Full
`describe` remains an in-session zoom tool in every lane.

Bundling the seeds resolves the *repo-curated* increment of the available-set question;
user-generated / Vault-era available-sets (server-fetched? staged?) remain open on the map.
If web ever grows a pull channel, omitted artifacts become pullable with no re-litigation.

**Considered and rejected:** hand-curated web guide slice (the lane-tag cut is mechanical and
CI-checkable); bundling the schema (~21k tok for grounding the `validate` tool already
provides mechanically); leaving the guide off web (its absence is measured — repair rounds
each costing a full document copy, reuben-web#83).

### 4. The schema is deleted — same-commit parity replaces its one real job

The instrument JSON Schema has **no grounding role in any lane** (agents need prose rules +
ports + a validator loop, not a JSON Schema to eyeball). Its two real consumers need not the
schema but *a registry truth independent of the artifact under test*, and both get a stronger
source:

- `check.mjs`'s registry pin (ADR-0040 §4's silent ctor-drop tripwire) becomes **same-commit
  parity**: native `describe --json` output ≡ wasm introspection output, compared
  structurally at CI time — descriptor-set equality instead of a bare count, fresh-vs-fresh
  so nothing can go stale (ADR-0052's posture applied to the registry).
- The web live-eval's `describeOperators` stub reads fresh native describe output.

Then delete outright: the committed `instrument.schema.json`, the `gen_schema` example, the
staleness test, the MCP drift test, the `instrument_schema` key in the web tool-schema
artifact, and the `reuben://schema/instrument` resource (ADR-0048 §7 amended). A future
constrained-decoding experiment regenerates from `Descriptor` — the source survives; the
rendering does not deserve permanent residency.

> **Superseded (#498):** the generator source (`crates/reuben-core/src/schema.rs`) was
> retired too. Product call: the constrained-decoding experiment is YAGNI, so it is not worth
> carrying 410 lines of dead-but-regenerable code plus its hand-maintained `$defs`↔serde parity
> test. `InputPipeDoc`/`OutputPipeDoc` are now the sole field authority; a future experiment
> re-derives `Descriptor → JSON-Schema` from scratch.

**Considered and rejected:** keeping the file as the pin's carrier (any committed witness
recreates the staleness-guard machinery for less value than a fresh comparison); re-homing
the pin onto the compact describe artifact (workable, but couples the deletion to that
artifact's timing for no gain).

### 5. The web prefix

Composition order: **output filter → guide (web cut) → vocabulary → compact describe → seed
index**, plus the declared tool schemas. Budget ~21–22k tok stable prefix (vs ~8.5k today,
reuben-web#96 corrected figures), byte-stable per deploy, cached at ~0.1× after round one; it
retires the ~9.9k full-registry `describe` that today lands tail-priced in session history.

### 6. MCP posture

`INSTRUCTIONS` stays gist-and-point with pointer edits only: the schema sentence dies; one
sentence each points at the vocabulary and library-index resources. A bring-your-own-harness
client gets, by default, the gist + pointers — nothing pushed, nothing filtered; an MCP
client is presumed dev-shaped unless its *host* says otherwise. Hosts differentiate; the lane
does not.

### 7. Known drift retirement (routing)

- Web's dangling `instrument_schema` tool-description pointer → rides the §4 deletion slice
  (descriptions reword to lean on grounding that actually ships).
- MCP `swap` description + guide still describing the M1 restart-swap →
  [reuben#456](https://github.com/Impractical-Instruments/reuben/issues/456), independent;
  the guide-edit slice references it to avoid a double edit.
- `patcher` vestiges (the "read the schema for grounding" line; the nesting example's overlap
  with the guide) → ride the patcher-thinning slice.
- `sync-docs` scope table: schema regeneration leaves the sweep; vocabulary, index, and
  compact describe join it.

### 8. Validation contract: three tiers, content proven once

Artifact **content** is proven once, at the lowest tier, in engine CI; each lane tests only
its own **delivery glue**; the live model is reserved for behavior only a model can exhibit.

- **Tier 1 — mechanical, zero tokens**: artifact staleness (registry-keyed); seed recipes
  validate + the euclid re-expression is bit-identical (ADR-0057's acceptance); registry
  parity native ≡ wasm; the guide lane-tag cut (checkout-only sections dropped, every section
  tagged); prefix composes byte-deterministically; **no dangling references** (every artifact
  or tool the prose names actually ships — the `instrument_schema` disease, now a test
  class); char-count budget ceiling with reuben-web#96's correction factor; skills pointers
  resolve; the existing plumbing-passthrough evals and prompt-teaches-the-rules lints.
- **Tier 2 — scripted stdio, zero tokens**: resources served byte-equal to the checkout;
  schema resource absent; `INSTRUCTIONS` points only at URIs that exist.
- **Tier 3 — live model only**: the model *obeys* the output filter; edge-conduct adherence;
  the directional token/round win vs baseline. Existing `live-eval` harness, pinned model,
  as merge gate — no new API surface. (Register/conduct cannot be proven in another lane's
  scaffold: the filter is web-only sauce, and a Claude-Code-hosted session has a competing
  system prompt, a different tool surface, and no turn envelope — it validates a composition
  that never ships.)

## Consequences

- The prose/policy layer joins the verb layer: **authored once, delivered per lane** — web
  becomes measurable against the other lanes, unblocking the pipeline effort's build slicing
  (Patch-pipeline/H).
- The guide gains `lanes:` section tags and the loop-conduct additions; the vocabulary
  source gains the edge-conduct preamble; `authoring.md`'s web cut becomes a build artifact.
- The schema machinery (file, generator example, two guard tests, artifact key, resource) is
  deleted; ADR-0048 §7's surface is guide + vocabulary + library index; ADR-0040 §4's guard
  is the parity comparison.
- `proxy/system-prompt.mjs` becomes a **composer** (filter + bundled artifacts), no longer an
  author of authoring mechanics.
- `sync-docs` scope: − schema regeneration, + vocabulary/index/compact-describe.
- H slices build tickets directly from §§2–8; the §8 table is the test-ticket spec.
- Glossary: **Delivery lane**, **Input handling**, **Output filter**, **Push/pull delivery**
  (CONTEXT.md).
