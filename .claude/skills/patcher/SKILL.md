---
name: patcher
description: Build or modify a reuben Instrument or Rig — the playable JSON graph of Operators. Introspects the live operator set, drafts or edits the graph, and validates it against the real engine load path before declaring done. Use when the user says "build an instrument", "make a synth/pad/bass", "patch up a rig", "add a node", "wire X to Y", "change this instrument", or describes a sound to construct.
---

# patcher

A reuben Instrument is one recursive JSON graph: **Operators** (nodes) wired by **connections**,
with declared **outputs** (CONTEXT.md; ADR-0003). Operator / Instrument / Rig are *scales* of the
same graph, not different file types — this skill authors all three. It grounds itself on the
**live operator set** (`reuben describe`) and proves its work on the **real engine load path**
(`reuben validate`) before finishing (ADR-0020).

It does **not** author new Operators (that is Rust — the `create-operator` skill, ADR-0021), write
`control` blocks (that is the `control-surface` skill, ADR-0018), or edit the schema/core.

## The loop: introspect → draft → validate → report

Run all `reuben` commands from the repo root.

1. **Introspect the operators you need.** Never guess ports/params — ask the binary:
   - `cargo run -q -p reuben-native --bin reuben -- describe --json` — every operator.
   - `cargo run -q -p reuben-native --bin reuben -- describe <op> --json` — one operator's
     ports (`name`+`kind`), params (`min`/`max`/`default`/`unit`/`curve`), resource slots.
   The schema at `crates/reuben-core/schema/instrument.schema.json` is the same data as a
   document shape; `describe` is the per-operator view.

2. **Draft the graph.** Start from a **canonical recipe** (below) or an existing
   `instruments/*.json` (e.g. `good-button.json`) rather than a blank file. Honour the rules the
   loader enforces (so step 3 passes first try):
   - **Every node** has a unique `address` and a registered `type`.
   - **Connections join same-kind ports**: Signal↔Signal, Message↔Message, Context↔Context. A
     Signal→Message wire is a hard error. (Check `kind` from `describe`.)
   - **Signal inputs can't be driven by a Message directly** — convert through a `map`→`m2s`
     front-end (a Good Button), exactly as `good-button.json` feeds the filter's cutoff.
   - `params` override descriptor defaults by name; values out of `[min,max]` are silently
     clamped, so stay in range.
   - A node needing a sample names a `sample` id present in the top-level `resources` table.

3. **Validate — loop until `ok`.**
   `cargo run -q -p reuben-native --bin reuben -- validate <path> --json`
   Returns `{ok, errors:[{node?,port?,message}], warnings:[...]}`. Fix each error (it names the
   offending node/port) and re-run until `ok:true`. This runs the real `load_instrument` +
   `Plan::instantiate`, so it catches unknown type/port/param, duplicate address, kind
   mismatches, **and cycles** — without playing audio.

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

- `voicer` turns `/voicer/note [midi, gate]` into per-Voice `freq` (Signal) + `gate` (Signal).
- `filter` `cutoff`/`resonance` are **Signal** inputs — leave them at descriptor defaults, or
  drive them from a **Good Button**: `map`(public, message in) → `map`(ranged) → `m2s`(smooth to
  Signal) → filter input. See `good-button.json` for the worked fan-out.
- Play it: `reuben play <file>` then send `/voicer/note [60, 1]` (note-on) / `[60, 0]` (off).

**Self-playing** (no external notes): add a `clock` + `sequencer` feeding the voicer, as in
`instruments/sampler-arp.json`.

When unsure of a port or param name, **`describe` it** — don't infer from these sketches.

## Run the tests

The introspection commands are backed by `reuben_native::cli`:
`cargo test -p reuben-native --test cli` — covers validate (accept, unknown-type localization,
cycle, advisory warning) and describe (list-all, one-op fields, unknown-op error).

## Scope

| Thing | Action |
|---|---|
| Instrument/Rig graph — nodes, params, connections, outputs, resources | **author / edit** (validate before done) |
| `control` blocks (player-facing UI metadata) | **never** — that is the `control-surface` skill |
| New Operator types (Rust) | **never** — that is the `create-operator` skill (ADR-0021) |
| `instrument.schema.json` / core crates | **never edit** — read the schema for grounding only |

## Report

End with: which instrument file, what was built or changed (nodes added/rewired, params set),
the final `validate` result (`ok`, any warnings), and how to play it
(`reuben play <file>`, the OSC address to send notes to). If you built a Good Button or other
player-facing control, suggest the `control-surface` skill to generate a TouchOSC UI for it.
