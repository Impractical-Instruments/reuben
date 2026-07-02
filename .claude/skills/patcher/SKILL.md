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

It does **not** author new Operators (that is Rust — the `create-operator` skill, ADR-0021), write
`control` blocks (that is the `control-surface` skill, ADR-0018), or edit the schema/core.

## The loop: introspect → draft → validate → report

Run all `reuben` commands from the repo root.

1. **Introspect the operators you need.** Never guess ports/inputs — ask the binary:
   - `cargo run -q -p reuben-native --bin reuben -- describe --json` — every operator.
   - `cargo run -q -p reuben-native --bin reuben -- describe <op> --json` — one operator's
     ports (`name`+`kind`, where `kind` is the Arg-type word: `signal`=a dense per-sample buffer
     (`Signal`), `enum`=a shared `vocab` enum, `message`=a `Note` stream, `context`=`Harmony`),
     settable inputs (`min`/`max`/`default`/`unit`/`curve`), enum inputs (`variants`+`default`),
     resource slots.
   - `cargo run -q -p reuben-native --bin reuben -- describe <patch.json> --json` — a nested
     instrument's **boundary** (ADR-0034): its `interface` ports as if they were operator ports,
     with metadata inherited from the inner ports (effective defaults included) and any
     presentational overrides applied. This is what a `subpatch` node referencing that file
     exposes — wire against these names, never the child's internals.
   The schema at `crates/reuben-core/schema/instrument.schema.json` is the same data as a
   document shape; `describe` is the per-operator view.

2. **Draft the graph.** Start from a **canonical recipe** (below) or an existing
   `instruments/*.json` (e.g. `good-button.json`) rather than a blank file. The format (ADR-0030):
   each node carries an **`inputs`** map and an optional **`config`** block — there is **no
   top-level `connections` array** and **no per-node `params` map**. Honour the rules the loader
   enforces (so step 3 passes first try):
   - **Every node** has a unique `address` and a registered `type`.
   - **An `inputs` value is a literal or a wire-ref.** A literal sets a numeric (`signal`) default
     (`"cutoff": 1500`) or an `enum` by symbol (`"mode": "Hp"`). A wire-ref connects an upstream
     output: `"audio": { "from": "/osc.audio" }`, or the sole-output sugar `{ "from": "/osc" }`
     when the source has exactly one output. `"cutoff": 1500` and `"cutoff": {"from":"/lfo"}` target
     the same slot.
   - **A wire must join matching Arg types.** `signal`→`signal`, `message`→`message`,
     `context`→`context`. A `signal` wired into a `message` input (or a symbol into an audio input)
     is a `TypeMismatch` error — the one implicit bridge is an `F32`/`signal` source into a
     `signal` (`Buffer`) input, which ZOH-materializes. There is **no Message-vs-Signal carrier**
     anymore: a `signal` input takes a literal, a wire, OR live OSC directly — you do **not** need a
     `map`→`m2s` front-end just to drive a filter's `cutoff`.
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

- `voicer` turns `/voicer/notes [midi, gate]` into per-Voice `freq` + `gate` (`signal` outputs);
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
  node dissolves — zero runtime cost. Boundary wires type-check against the **inner** ports the
  interface names; errors name the subpatch address + external name.
- **Child side**: a nestable patch declares `interface { inputs, outputs }` mapping external
  names to internal `/node.port` targets. An entry can be a bare string or an object adding
  **presentational overrides** — `label`/`unit`/`widget`/`min`/`max`, inherited from the inner
  port, per-field overridable; the **Arg type is never overridable** (no field exists for it):

  ```json
  "tone": { "target": "/filter.cutoff", "label": "Tone", "widget": "knob", "min": 200, "max": 8000 }
  ```
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
| Instrument/Rig graph — nodes, `inputs` (literals + wire-refs), `config`, outputs, resources | **author / edit** (validate before done) |
| `control` blocks (player-facing UI metadata) | **never** — that is the `control-surface` skill |
| New Operator types (Rust) | **never** — that is the `create-operator` skill (ADR-0021) |
| `instrument.schema.json` / core crates | **never edit** — read the schema for grounding only |

## Report

End with: which instrument file, what was built or changed (nodes added/rewired, params set),
the final `validate` result (`ok`, any warnings), and how to play it
(`reuben play <file>`, the OSC address to send notes to). If you built a Good Button or other
player-facing control, suggest the `control-surface` skill to generate a TouchOSC UI for it.
