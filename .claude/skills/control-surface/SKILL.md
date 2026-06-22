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
| **Good Button** — a `map` whose message input has no incoming connection | the node address, e.g. `/brightness` | the map's `in_min`/`in_max` instance params; default = `map`'s `default` |
| **Direct param** — a Message param on a node | `/<node>/<param>`, e.g. `/clock/tempo` | the param's schema metadata (min/max/unit/default) |

Signal inputs (e.g. a filter's `cutoff`) are **not** directly controllable — drive them through
a `map`/`m2s` front-end (a Good Button), exactly as `good-button.json` does.

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
   with Edit** (preserve the file's inline-node formatting). Shapes:
   - Good Button: `"control": { "label": "Brightness", "unit": "%" }`
   - one param: `"control": { "label": "Tempo", "param": "tempo" }`
   - many params on one node: `"control": [ { "label": "Step 1", "param": "step1" }, ... ]`
   `label` is required; `unit`/`widget`/`min`/`max`/`default` are optional overrides (otherwise
   inferred). `widget` ∈ `fader` (default), `button`, `label`.

4. **Emit the surface:**
   `python3 <skilldir>/gen_surface.py emit instruments/<name>.json --host <host> --out <name>.tosc`
   Widgets send real values (the fader's 0..1 is scaled to the control's range); faders init to
   the resting default; the connection is one-way (surface → reuben).

5. **Verify on device** (this is required — the `.tosc` format has not been device-tested from
   the generator yet). Have the user: open the `.tosc` in TouchOSC, set the OSC **connection
   host/port** to the machine running reuben, and move a control while reuben listens. If it
   won't open or controls don't move anything, see *Format notes* and iterate.

## Format notes (for the verify loop)

The emitter hand-builds the format (no external dep). Two details are reverse-engineered and are
the first suspects if a file won't load:
- **`r`/`c` properties** (frame, colour) are written as nested `<value><x>..</x>…</value>`. Some
  references use attributes (`<property type="r" x=".."/>`) instead — flip `_prop` in
  `gen_surface.py` if needed.
- The leading `<?xml …?>` declaration and the `<values>`/`<messages>` block shapes may need
  tweaks per TouchOSC version. The payload is **zlib**-compressed (not gzip).

## Run the tests

`cd <skilldir> && python3 -m unittest` — covers metadata resolution, inference, OSC addressing,
and the zlib/XML round-trip (not on-device loading).

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
