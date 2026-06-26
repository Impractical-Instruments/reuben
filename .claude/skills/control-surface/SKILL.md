---
name: control-surface
description: Generate a Hexler TouchOSC control surface (.tosc) for a reuben instrument, so you can play it from a phone or tablet over OSC. Infers player-facing controls, writes `control` blocks into the instrument, and emits the layout. Use when the user says "make a control surface", "generate a TouchOSC layout", "make a UI for this instrument", or wants to play an instrument from a touch controller.
---

# control-surface

A reuben instrument is a graph; a *control surface* is the curated set of player-facing knobs
(ADR-0017's Good Buttons + a few musical params). This skill generates a Hexler **TouchOSC**
layout (`.tosc`) that sends OSC straight to reuben's node addresses — the fastest path from a
new instrument to a touchable UI. It is a **one-shot, disposable** generator (ADR-0018): the
instrument JSON is the source of truth; regenerate when it changes.

It is **not** an auto-UI system and **not** a round-trip editor — it emits a scratch surface to
play with.

## Vocabulary (where each control's metadata comes from)

| Control | OSC address the widget sends to | Range / unit / default |
|---|---|---|
| **Good Button** — a `map` whose input is not wired from another node | the node address, e.g. `/brightness` | the map's `in_min`/`in_max` instance inputs; default = `map`'s `default` |
| **Direct input** — a settable numeric (`signal`) input on a node | `/<node>/<input>`, e.g. `/clock/tempo` or `/filter/cutoff` | the input's schema metadata (min/max/unit/default) |

A numeric (`signal`) input (e.g. a filter's `cutoff`) is now **directly controllable** over OSC at
`/<node>/<input>` (ADR-0030) — no `map`/`m2s` front-end is required to reach it. A **Good Button**
remains the right pattern for a *curated, ranged* player face (one knob fanned to several inputs
over musical ranges), exactly as `good-button.json` does; a direct input is the raw,
full-range alternative. `enum` inputs (filter `mode`, osc `waveform`) are settable too but aren't
emitted as faders yet — they need a selector/toggle widget (out of scope today).

## Workflow

Run from the repo root. The script is `gen_surface.py` in this skill's directory.

1. **Pick the instrument and host.** Ask which `instruments/*.json` and the host running reuben
   (default `localhost`, port `9000`).

2. **Discover candidates** (read-only):
   `python3 <skilldir>/gen_surface.py infer instruments/<name>.json`
   This prints Good Buttons (high-confidence public controls) and every node param with
   resolved address + metadata.

3. **Curate and annotate.** If the instrument has no `control` blocks yet, propose a tight set —
   **Good Buttons first**, then only the params that are genuinely musical to play (tempo,
   sequence steps, voice count); skip structural/internal params. Show the user the proposed
   labels + ranges, get confirmation, then **write `control` blocks into the instrument JSON
   with Edit** (preserve the file's inline-node formatting). Forms:
   - Good Button: `"control": { "label": "Brightness", "unit": "%" }`
   - one param: `"control": { "label": "Tempo", "param": "tempo" }`
   - many params on one node: `"control": [ { "label": "Step 1", "param": "step1" }, ... ]`
   - play toggle: `"control": { "label": "Play C", "widget": "note-toggle", "port": "note", "note": 60 }`
   `label` is required; `unit`/`widget`/`min`/`max`/`default` are optional overrides (otherwise
   inferred). `widget` ∈ `fader` (default), `note-toggle`. A `note-toggle` plays `<node>/<port>
   [note, gate]` with a constant `note` so note-off matches (TouchOSC can't share a value
   between two controls without scripting, so a separate slider + gate isn't possible natively).

4. **Emit the surface:**
   `python3 <skilldir>/gen_surface.py emit instruments/<name>.json --host <host>`
   Writes `control-surfaces/<name>.tosc` (the repo's versioned, shareable surface dir;
   override with `--out`). Faders send real values (0..1 scaled to range) and init to the
   resting default; the connection is one-way (surface → reuben). A `note-toggle` control emits
   a toggle button that plays a fixed `note` through a message port, e.g. `/voicer/note`.

5. **Open + verify on device.** Have the user open `control-surfaces/<name>.tosc` in TouchOSC,
   set the OSC **connection host/port** to the machine running reuben, and play. The format is
   confirmed against a real export (see *Format notes*), so this is a sanity check, not a
   debugging round — but always confirm new control *kinds* (a widget type not yet exercised).

## Format notes

The emitter hand-builds the format (no external dep, `zlib`-compressed `lexml version="6"`),
**cloned from a known-good editor export**: `fixtures/REUBEN_REF.tosc`. The test
`FixtureMatchTest` asserts our per-control property keys still match that fixture, so format
drift fails CI. If a future TouchOSC version changes the format, or you add a new widget type:
rebuild the fixture in the editor (one of each control, distinctive values), replace
`fixtures/REUBEN_REF.tosc`, and diff to update the property sets in `gen_surface.py`.

## Run the tests

`cd <skilldir> && python3 -m unittest` — covers metadata resolution, inference, OSC addressing,
the zlib/XML round-trip, and the structural match against `fixtures/REUBEN_REF.tosc`.

## Scope

| Thing | Action |
|---|---|
| `control` blocks in the instrument JSON | **write** (curated, confirmed, via Edit) |
| `.tosc` surface file | **emit** via `gen_surface.py emit` |
| instrument graph / operators | **never edit** — surface metadata only |
| two-way OSC feedback, grouped layouts | **out of scope** (ADR-0018 deferred) |

## Report

End with: which instrument, the controls chosen (address + range), where the `.tosc` was
written, the host/port to set in TouchOSC, and the explicit ask to verify it loads on device.
