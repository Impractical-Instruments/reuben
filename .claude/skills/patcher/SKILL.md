---
name: patcher
description: Build or modify a reuben Instrument or Rig — the playable JSON graph of Operators. Introspects the live operator set, drafts or edits the graph, and validates it against the real engine load path before declaring done. Use when the user says "build an instrument", "make a synth/pad/bass", "patch up a rig", "add a node", "wire X to Y", "change this instrument", or describes a sound to construct.
---

# patcher

A reuben Instrument is one recursive JSON graph: **Operators** (nodes), each with an **`inputs`**
map (literals + wire-refs) and an optional **`config`** block, with declared **outputs**
(CONTEXT.md; ADR-0003, ADR-0030). Operator / Instrument / Rig are *scales* of the same graph, not
different file types — this skill authors all three. It grounds itself on the **live operator set**
(`reuben describe`) and proves its work on the **real engine load path** (`reuben validate`) before
finishing (ADR-0020).

It does **not** author new Operators (that is Rust — the `create-operator` skill, ADR-0021), author
surface docs (`surfaces/*.json` presentation — the `control-surface` skill, ADR-0043), or edit the
schema/core.

## The loop: introspect → draft → validate → report

Run all `reuben` commands from the repo root.

1. **Introspect the operators you need.** Never guess ports/inputs — ask the binary:
   - `cargo run -q -p reuben-native --bin reuben -- describe --json` — every operator.
   - `cargo run -q -p reuben-native --bin reuben -- describe <op> --json` — one operator's
     ports (`name`+`kind`, where `kind` is the glossary word: `value`=a held `f32` Value (a knob,
     a gate, a latched pitch), `signal`=a dense `f32_buffer` Signal (audio, a per-sample sweep),
     `enum`=a shared `vocab` enum, `message`=a `Note` stream, `harmony`=`Harmony`),
     settable inputs (`min`/`max`/`default`/`unit`/`curve`), enum inputs (`variants`+`default`),
     resource slots.
   - `cargo run -q -p reuben-native --bin reuben -- describe <patch.json> --json` — a nested
     instrument's **boundary** (ADR-0034/0038): its `interface` pipes as if they were operator
     ports, each with its **declared** `Arg` type, range, default, and unit.
     This is what a `subpatch` node referencing that file exposes — wire against these names,
     never the child's internals.
   The schema at `crates/reuben-core/schema/instrument.schema.json` is the same data as a
   document shape; `describe` is the per-operator view.

2. **Draft the graph.** Start from a **canonical recipe** (below) or an existing
   `instruments/*.json` (e.g. `good-button.json`) rather than a blank file. The format (ADR-0030):
   each node carries an **`inputs`** map and an optional **`config`** block — there is **no
   top-level `connections` array** and **no per-node `params` map**. Honour the rules the loader
   enforces (so step 3 passes first try):
   - **Every node** has a unique `address` and a registered `type`.
   - **An `inputs` value is a literal or a wire-ref.** A literal sets a numeric (`value`/`signal`) default
     (`"cutoff": 1500`) or an `enum` by symbol (`"mode": "Hp"`). A wire-ref connects an upstream
     output: `"audio": { "from": "/osc.audio" }`, or the sole-output sugar `{ "from": "/osc" }`
     when the source has exactly one output. `"cutoff": 1500` and `"cutoff": {"from":"/lfo"}` target
     the same slot.
   - **A wire must join matching Arg types.** `value`→`value`, `signal`→`signal`,
     `message`→`message`, `harmony`→`harmony`. A numeric wired into a `message` input (or a
     symbol into an audio input) is a `TypeMismatch` error. The two numeric kinds are one wiring
     family with exactly one implicit bridge (ADR-0031): a `value` source into a `signal` input
     ZOH-materializes (a constant `cutoff` still works). The reverse — a `signal` source into a
     `value` input, e.g. an envelope's `cv` into a gate — is a **hard plan error**: there is no
     implicit sample-and-hold, and the explicit sig→val converter ops don't exist yet. There is
     **no Message-vs-Signal carrier** anymore: a `value` or `signal` input takes a literal, a
     wire, OR live OSC directly — you do **not** need a `map`→`m2s` front-end just to drive a
     filter's `cutoff`.
   - **`Constant`s go in `config`, not `inputs`.** Today that's the Voicer's `voices`
     (`"config": { "voices": 8 }`). Putting it in `inputs` is a `ConstantInInputs` error.
   - Out-of-range numeric literals are clamped; an unknown `enum` symbol or out-of-range index is
     an error (it never snaps to a default).
   - A node needing a sample names a `sample` id present in the top-level `resources` table.

3. **Validate — loop until `ok`.**
   `cargo run -q -p reuben-native --bin reuben -- validate <path> --json`
   Returns `{ok, errors:[{node?,port?,message}], warnings:[...]}`. Fix each error (it names the
   offending node/input) and re-run until `ok:true`. This runs the real `load_instrument` +
   `Plan::instantiate`, so it catches unknown type/input, duplicate address, type mismatches,
   constants-in-inputs, **and cycles** — without playing audio.

4. **Sanity-check that it's audible.** `validate` proves the graph is *legal*, **not that it
   makes sound** — a disconnected oscillator or a missing `output` validates clean and is silent.
   Before reporting, eyeball: is there a path from a generator to a declared `output`? Are the
   voicer's `freq`/`gate` reaching the voice chain? Warnings (e.g. an unresolved sample) are
   advisory — the instrument is still valid.

## Canonical recipes

**Basic playable voice** (saw → filter → ADSR → out), the spine of `good-button.json`:

```
voicer ─freq→ oscillator ─audio→ filter ─audio→ envelope ─audio→ output
voicer ─gate───────────────────────────────────→ envelope(gate)
```

- `voicer` turns `/voicer/notes [midi, gate]` into per-Voice `freq` + `gate` (held `value` outputs);
  wire them into the chain via `"freq": {"from":"/voicer.freq"}` etc.
- `filter` `cutoff`/`resonance` are `signal` inputs — leave them at their literal defaults
  (`"cutoff": 1200`), wire a modulator (`"cutoff": {"from":"/lfo.audio"}`), or drive them live over
  OSC (`/filter/cutoff 1500`). `mode` is an `enum` (`"mode": "Hp"`). A **Good Button** is still a
  nicety for a curated player control — `map`(public) → `map`(ranged) → filter input, optionally
  with an `m2s`/`slew` shaper for zipper-free smoothing — but it is no longer *required* to reach a
  `signal` input. See `good-button.json` for the worked fan-out.
- Play it: `reuben play <file>` then send `/voicer/notes [60, 1]` (note-on) / `[60, 0]` (off).

**Self-playing** (no external notes): add a `clock` + `sequencer` feeding the voicer, as in
`instruments/sampler-arp.json`.

**Nesting an instrument as a node** (ADR-0034) — the worked pair is
`instruments/patches/space.json` (the nestable child) + `instruments/nested-space.json` (the host):

- **Host side**: add the child to `resources` and reference it from a `subpatch` node's `patch`
  field. The node's ports are the child's `interface` names — `describe <patch.json>` first, then
  wire/set them like any operator's (`"in": {"from":"/voicer.audio"}, "tone": 2500`):

  ```json
  "resources": { "space": "patches/space.json" },
  "nodes": [ { "type": "subpatch", "address": "/space", "patch": "space",
               "inputs": { "in": { "from": "/voicer.audio" }, "tone": 2500 } } ]
  ```

  At build the child inlines under the node's address (`/space/filter`…, OSC-reachable) and the
  node dissolves — zero runtime cost. Boundary wires type-check against each interface pipe's
  **declared type** (ADR-0038 §2); errors name the subpatch address + external name.
- **Child side**: a nestable instrument declares `interface { inputs, outputs }` as **named
  pipes** (ADR-0038 flipped the wiring direction from v1's target-pointing entries — no entry
  points inward anymore). An **input pipe** mints an address in the flat node namespace (entry
  `tone` → `/tone`) that internal nodes consume with an ordinary wire-ref, and — pointing at no
  inner port — **declares its own `type`** (`f32`, `f32_buffer`, `note`, `harmony`, or a vocab
  enum name) plus the quantity contract it now **owns**: `default`/`min`/`max`/`curve`/`unit`
  (range engine-enforced for a numeric pipe). Presentation — `label`/`widget` — does **not**
  belong on a pipe (ADR-0043): it lives in a surface doc (`surfaces/*.json`, the
  `control-surface` skill's territory). An **output pipe** is fed **from** an internal port:

  ```json
  "interface": {
    "inputs":  { "in":   { "type": "f32_buffer" },
                 "tone": { "type": "f32_buffer", "default": 4000.0, "min": 20.0, "max": 20000.0,
                           "curve": "exp", "unit": "Hz" } },
    "outputs": { "out": { "from": "/verb.audio" } }
  }
  ```

  Internal nodes wire from an input pipe by its minted address — the filter's
  `"audio": { "from": "/in" }`, `"cutoff": { "from": "/tone" }` (see `patches/space.json`). A
  **signal** pipe may carry an optional `channel: <int>` binding to a logical hardware channel,
  honored only when the graph is played at top level and **inert when nested** (ADR-0038 §3).
  The declared `Arg` type is enforced against every consumer wire by the ordinary wire check.
  (v1 documents that spell interface entries as `{ "target": "/node.port" }` still load — the
  loader migrates them to pipes at parse — but author new **instruments** in the pipe form.)
- A cyclic patch reference is a fatal error; a missing/unreadable child degrades to a warning
  (the node goes dark). Validate the child standalone too — it's a full instrument.

When unsure of a port or param name, **`describe` it** — don't infer from these sketches.

## Run the tests

The introspection commands are backed by `reuben_native::cli`:
`cargo test -p reuben-native --test cli` — covers validate (accept, unknown-type localization,
cycle, advisory warning) and describe (list-all, one-op fields, unknown-op error).

## Scope

| Thing | Action |
|---|---|
| Instrument/Rig graph — nodes, `inputs` (literals + wire-refs), `config`, `interface` pipes (including promoting a player-facing control to an input pipe), resources | **author / edit** (validate before done) |
| Surface docs (`surfaces/*.json` — presentation binding pipes to widgets, ADR-0043) | **never** — that is the `control-surface` skill; it delegates graph edits back here |
| New Operator types (Rust) | **never** — that is the `create-operator` skill (ADR-0021) |
| `instrument.schema.json` / core crates | **never edit** — read the schema for grounding only |

## Report

End with: which instrument file, what was built or changed (nodes added/rewired, params set),
the final `validate` result (`ok`, any warnings), and how to play it
(`reuben play <file>`, the OSC address to send notes to). If you built a Good Button or promoted
a player-facing control to an interface pipe, suggest the `control-surface` skill to author its
surface doc (the web player and TouchOSC render from it).
