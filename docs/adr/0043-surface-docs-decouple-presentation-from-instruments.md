# ADR-0043: Surface docs — presentation decoupled from instruments, pipes as the one boundary

## Status

Accepted (2026-07-09). The design gate of the decouple-UI-from-instruments epic
([#247](https://github.com/Impractical-Instruments/reuben/issues/247)); decisions were put to
and confirmed by the repo owner in the grilling session recorded there.

**Supersedes** [ADR-0018](0018-control-surface-generation.md)'s `control`-block decision: the
per-node `control` block, the `NodeDoc.control` passthrough, and the infer→write-back
generator workflow all retire. (ADR-0018's TouchOSC target choice, the `.tosc` emit machinery,
the `map.default` resting position, and the one-way/port-9000 connection story live on under
this model.)
**Amends** [ADR-0038](0038-interface-pipes-and-the-device-layer.md) §2 — interface pipes no
longer carry `label`/`widget`; a pipe keeps only the *quantity* contract
(`type`/`default`/`min`/`max`/`curve`/`unit`) — and
[ADR-0041](0041-web-player-app-in-repo.md) — the web player's auto-UI renders from interface
pipes + surface docs, not from `control` blocks.
Rides on [ADR-0036](0036-instrument-library-and-format-versioning.md) §4's `format_version`
machinery (the second breaking bump: **v3**) and applies
[ADR-0026](0026-v1-finish-line-osc-out-and-stereo.md)'s bit-identical migration discipline.

## Context

The tree carries **two competing playable-surface mechanisms**:

- **Inline `control` blocks** (ADR-0018) — scattered per node across 7 instruments, read by
  both the web player's auto-UI (`crates/reuben-web/js/surface/widget-model.mjs`) and the
  TouchOSC generator (`.claude/skills/control-surface/gen_surface.py emit`).
- **`interface` input pipes** (ADR-0038) — the named, engine-enforced boundary, read by
  TouchOSC's `boundary` subcommand but **not** by the web player.

Worse, `widget-model.mjs` is a hand-ported duplicate of the Python inference — corrections and
all — so "a system for generating 2D UIs" half-exists twice, in two languages, drifting. Both
copies reverse-engineer ranges from `map` instance literals and sniff sequencer gate steps,
because `control` blocks carry no contract of their own.

The enabling fact: `NodeDoc.control` is an opaque passthrough the engine never reads
(`format.rs` documents this; the only Rust production access writes `None`). Removing it is
render-safe by construction. The real work is *promoting* public controls to interface pipes
so decoupled surfaces have something honest to bind to, and giving presentation a home of its
own.

## Decision

### 1. Interface pipes are the one boundary

Every player-facing control is an `interface` **input pipe** (ADR-0038): a named entry with a
declared type, engine-enforced against every consumer wire. Surfaces reference pipes **by
name**; the instrument's `interface` block *is* the contract. A control bound to pipe `name`
sends OSC to **`/<name>/in`** (the pipe node minted at `/<name>`, its `in` port) — the same
address `describe` and the `boundary` generator use today.

Pipe types cover the whole widget set: `f32`/`f32_buffer` pipes back faders and toggles;
**`note` pipes** back note-toggles and chord-buttons (the OSC boundary already converts a
degree/note payload at a note port — today's `/chord/set` path, unchanged).

### 2. The split line: quantity on the pipe, presentation on the surface

The pipe (contract) carries `type` / `min` / `max` / `default` / `unit` / `curve` — `unit`
and `curve` describe the *quantity*, so every surface of that instrument inherits them. The
surface (presentation) carries `bind` (pipe name) / `label` / `widget` / `group`, ordering and
selection, and an optional **narrower** `min`/`max` (the ADR-0034 §4 subset law; a resolver
clamps an out-of-range override to the pipe range and warns). Only `label` and `widget` are
stripped off today's pipes; nothing engine-enforced moves.

**Considered and rejected:** moving `unit`/`curve` to the surface too — they describe the
quantity, not one rendering of it; two surfaces of the same instrument must not disagree on
what Hz means.

### 3. Auto-derived default surface

With no surface file, the renderer synthesizes a default from the pipes — exactly what
TouchOSC's `boundary` subcommand does today: **one widget per wireable input pipe, declaration
order**, widget inferred from type (`f32`, ranged `f32_buffer` → fader; enum, `note`,
`harmony`, and channel-bound signal pipes are skipped — with a warning naming each, since a
default cannot guess their payloads). A surface file is optional curation/override/variant. A
new Toy is instantly playable with zero config.

### 4. Reference-based surface doc

The surface doc stores the pipe *name*; the resolver merges the pipe's inline contract at
load. Drift-free: change the pipe's range and every surface follows. The web player **drops
its `schema.json` fetch** — the instrument's `interface` block already carries the resolved
contract. (The headless check harness keeps reading the committed schema for its
registry-count pin; that use is unrelated to surfaces.)

### 5. One format, superset widget vocabulary, two targets

One shared surface format + one resolver semantics, projected to two targets:

- The **widget vocabulary is a superset of TouchOSC's**, so the web player is never capped by
  TouchOSC's ceiling. Shipped kinds (this epic): `fader`, `radial`, `param-toggle`,
  `note-toggle`, `chord-button`. Reserved (format-allowed, **not built** — see §10):
  `xy-pad`, `grid`, `visualizer`, `keyboard`.
- The web renderer supports all shipped kinds. The TouchOSC emitter supports its subset and
  **skips web-only/reserved widgets loudly** — a warning naming each skipped control, as
  `gen_surface.py` skips enum inputs today.
- **File resolution order** (per target `t` ∈ `web`, `touchosc`):
  `surfaces/<instrument>.<t>.json` ?? `surfaces/<instrument>.json` ?? auto-derived (§3).
  A per-target file is reached for only when the control *set* genuinely diverges, not merely
  the geometry.

### 6. Array/lane controls are ordinary pipes

A sequencer's N gate steps become **N ordinary interface pipes** (`kick_step1..16`, each
`f32`, `min` 0 / `max` 1, `default` = the old inline literal so the rest state is unchanged),
each wired to its step input. No new lane/indexed-pipe machinery; zero engine additions. This
relocates pre-existing control-block bloat into the honest, discoverable, engine-validated
place. Groovebox's 48 is far under ADR-0038 §3's 4096 bound. (An indexed "lane pipe" stays on
the shelf as optional future sugar.)

### 7. Format v3, ignore-with-warning migration

The `control` block and pipe `label`/`widget` are a breaking shape change under ADR-0036 §4:
**`format_version: 3`**, with a parse-time migration:

- The loader **ignores** leftover `control` blocks and pipe `label`/`widget`, emitting a
  `LoadWarning` naming each — sound is unaffected (the engine never read them). This applies
  to v2 documents *and* to a v3 document that still carries them: ignore-with-warning, never
  fatal, never silent.
- Save writes v3; migrated documents never save back under v2 (ADR-0036 discipline).
- A **one-shot repo rewrite** promotes the 7 control-block instruments' controls to pipes,
  authors their `surfaces/*.json`, and strips `label`/`widget` off existing pipes. Migrated
  and rewritten instruments render **bit-identically** — asserted in tests (the
  ADR-0026/0038 discipline). Old/external v2 docs keep playing; they only lose
  auto-UI-from-control-blocks.
- **Node renames during promotion** (a pipe minting an address an internal node already
  holds, e.g. good-button's `/brightness` m2s) follow ADR-0017's rename discipline as a
  JSON-structural sweep: every `{"from": ...}` ref re-pointed, `doc` prose flagged and
  updated. External OSC senders cannot be reached — the rename is **warned in the change
  record** (this ADR and the PR), per ADR-0017's guard list. The durable list of retired
  external addresses from the one-shot rewrite (knob addresses like `/brightness/in`,
  `/kick_filter/in`, `/kick_vol/in` are **unchanged** — the pipe re-mints them):

  | Retired address | Send instead |
  | --- | --- |
  | `/clock/tempo` (groovebox, euclidean-drums, djfilter-demo) | `/tempo/in` |
  | `/kick/step1..16`, `/snare/step1..16`, `/hat/step1..16` (groovebox) | `/kick_step1/in` … `/hat_step16/in` |
  | `/voicer/notes` (good-button) | `/notes/in` |
  | `/chord/set` (chord-player) | `/chord/in` |
  | `/harmony/root` (chord-player, strum-harp) | `/key/in` |
  | `/strum/position`, `/strum/octaves` (strum-harp) | `/strum/in`, `/octaves/in` |
  | `/filterpos/in`, `/djfilter/resonance` (djfilter-demo) | `/filter/in`, `/resonance/in` |
  | `/<ch>_eu/{pulses,steps,rotation}`, `/<ch>_env/decay` (euclidean-drums) | `/<ch>_pulses/in`, `/<ch>_steps/in`, `/<ch>_rotation/in`, `/<ch>_decay/in` |
  | `/grain/{position,grain_size,pitch,density,spray,gain}` (granulator-demo) | `/position/in`, `/grain_size/in`, `/pitch/in`, `/density/in`, `/spray/in`, `/gain/in` |

### 8. One evolved skill: generate + edit

The `control-surface` skill is repurposed: derive a default surface from pipes, **scaffold
and edit** `surfaces/*.json` (round-trip — relabel, reorder, group, narrow a range, add a
variant), then project to both targets (the web player consumes the doc live; `.tosc` is
emitted, web-only widgets skipped loudly). The surface **doc** is a durable, editable source;
the `.tosc` stays a disposable projection (ADR-0018's framing, now scoped to the projection
only). **Graph edits** — promoting a control to a pipe — delegate to the `patcher` skill; the
surface skill owns presentation, never the graph.

### 9. The duplication dissolves

Because pipes are self-describing, "resolve a control" collapses to: read
`instrument.interface.inputs[bind]`, merge the surface doc's overrides, pick a widget. The
gate-step sniffing and `map`-literal range archaeology in both resolvers go away. Each target
keeps a *tiny* native resolver (JS for the web, Python for TouchOSC), guarded by a **shared
cross-implementation fixture** asserting both resolve the same instrument + surface to the
same widget list (the role `expected-widgets.json` plays today, now target-neutral).
`NodeDoc.control` is removed from the format.

### 10. Bounded widget scope

This epic ships the vocabulary plus the **existing** widget kinds only. Web-rich widgets
(XY pad, live grid, visualizer) are format-allowed future work — the reserved names in §5
keep the format stable when they land, but nothing here builds them.

## The surface-doc format (v1)

Committed schema: [`surfaces/surface.schema.json`](../../surfaces/surface.schema.json)
(hand-authored; the engine never reads surface docs, so it is not generated from Rust).

```json
{
  "surface_version": 1,
  "instrument": "good-button",
  "cols": 4,
  "controls": [
    { "bind": "brightness", "label": "Brightness", "widget": "fader" },
    { "bind": "notes", "label": "Play C", "widget": "note-toggle", "note": 60 }
  ]
}
```

- `surface_version` (required, int): 1. Same forward-compat stance as ADR-0036: a resolver
  refuses a newer major it does not know.
- `instrument` (required, string): the instrument the doc presents; resolvers warn on
  mismatch with the loaded document's `instrument` name.
- `cols` (optional, int, default 4): coarse layout hint — widgets per row.
- `controls` (required, ordered array): the curated selection. Order is render order. Each:
  - `bind` (required): interface input pipe name. A bind naming no pipe is a warning; the
    control is skipped (dark-degrade, ADR-0016 philosophy).
  - `label` (optional): display name; defaults to the pipe name with underscores as spaces
    and each word's first letter uppercased (`kick_step1` → "Kick Step1") — pinned so both
    resolvers agree byte-for-byte.
  - `widget` (optional): vocabulary kind; defaults to the §3 inference for the pipe type. A
    message (`note`) pipe has no inferable widget — a control binding one without an explicit
    widget is a warning + skip. A widget kind the target cannot render (the reserved names,
    or an unknown) is skipped loudly by that target.
  - `group` (optional): adjacent same-group controls pack into one row (the grouped-row
    layout the auto-UI already has).
  - `min` / `max` (optional): narrower presentation range, §2 subset law (an override
    outside the pipe range clamps with a warning). The widget's rest value is the pipe
    `default` clamped into the effective range; a pipe with no declared default rests at
    the range floor.
  - Payload fields by widget: `note` (note-toggle, required, int note number) and optional
    `velocity` (default 1.0); `degree` (chord-button, required, int scale degree). Several
    controls may bind the same pipe (seven chord-buttons on one `note` pipe).

## Consequences

- **Core (`crates/reuben-core`):** `NodeDoc.control` removed; `label`/`widget` removed from
  interface pipe docs; v2→v3 normalize migration with new `LoadWarning`s; `FORMAT_VERSION = 3`;
  schema regenerated; bit-identical render tests for migrated and rewritten documents
  (`format_v2.rs` discipline, new `format_v3.rs`).
- **Instruments:** the 7 control-block instruments rewritten to pipes + `surfaces/*.json`
  authored; `space.json`/`mic-space.json` pipes stripped of `label`/`widget` (their labels
  move to surface docs).
- **Web (`crates/reuben-web/js/surface`, `web/`):** `widget-model.mjs` becomes the thin pipe
  resolver; `schema.json` fetch dropped (staging + PWA precache updated in lockstep);
  `main.js` resolves `surfaces/<id>.web.json ?? surfaces/<id>.json ?? auto`.
- **Skill (`.claude/skills/control-surface`):** `infer`/write-back retire; surface-doc
  scaffold/edit + `.tosc` projection with loud skips; graph edits delegate to `patcher`.
- **Docs:** ADR-0018 carries a superseded note; ADR-0038/0041 carry amended notes;
  ARCHITECTURE/README/authoring.md swept.
- **Terminology:** *surface doc* = the presentation-only JSON binding pipes to widgets;
  *superset widget vocabulary* = the shared widget name set of which each target renders a
  subset; *surface pipe promotion* = rewriting an inline control as an interface input pipe.
