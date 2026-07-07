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
| **Direct input** — a settable numeric (`value`/`signal`) input on a node | `/<node>/<input>`, e.g. `/clock/tempo` or `/filter/cutoff` | the input's schema metadata (min/max/unit/default) |

A numeric input — a held `f32` Value (`kind: "value"`, e.g. `/clock/tempo`) or a dense
`f32_buffer` Signal (`kind: "signal"`, e.g. a filter's `cutoff`) — is now **directly
controllable** over OSC at
`/<node>/<input>` (ADR-0030) — no `map`/`m2s` front-end is required to reach it. A **Good Button**
remains the right pattern for a *curated, ranged* player face (one knob fanned to several inputs
over musical ranges), exactly as `good-button.json` does; a direct input is the raw,
full-range alternative. `enum` inputs (filter `mode`, osc `waveform`) are settable too but aren't
emitted as faders yet — they need a selector/toggle widget (out of scope today).

### Interface pipes: the `interface` boundary *is* the surface

An instrument whose `interface.inputs` declares control **pipes** (ADR-0038 §2) already carries a
curated boundary — each input pipe declares its own `Arg` type and **owns** its presentation
metadata (label/unit/widget/min/max/curve/default). That boundary needs no hand-authored `control`
blocks: it *is* the curated set. The `boundary` subcommand emits **one fader per wireable interface
input pipe** straight from it.

| Control | OSC address the widget sends to | Range / unit / default |
|---|---|---|
| **Boundary input pipe** — a name in `interface.inputs` | the pipe's `/<name>/in` port (ADR-0038: an input pipe mints its own address `/<name>` and takes control on its single `in` port; it fans out to every internal consumer, so the fader drives the pipe, not an inner port) | the pipe's own declared metadata from `describe --json` |

Metadata comes from `reuben describe <instrument>.json --json` — each pipe reports its declared
type, range, default, unit, curve, label, and widget, so the script re-implements nothing. Inputs
the host can't drive from a fader are skipped: an **`enum`**/message/harmony pipe (needs a non-fader
widget) and a **bare audio** pipe (a `signal` kind with no range to scale into). The address
assumes the surfaced instrument is played at top level; nested under a host at `/h`, the same pipe
is `/h/<name>/in`.

## Workflow

Run from the repo root. The script is `gen_surface.py` in this skill's directory.

**Shortcut for an instrument with interface input pipes** (`interface.inputs` declares ranged
control pipes, ADR-0038 §2): skip the infer/curate steps — the boundary is already the curated
set. Just
`python3 <skilldir>/gen_surface.py boundary instruments/<name>.json --host <host>`
(runs `reuben describe --json` itself; pass `--reuben PATH` if the binary isn't under `target/`,
or `--describe FILE` to feed pre-captured output). It writes `control-surfaces/<name>.tosc` with
one fader per wireable input pipe. Then jump to step 5 (verify on device). Use the full
`infer`→curate→`emit` flow below for an instrument with no control pipes, or when you want to
hand-curate Good Buttons / toggles beyond the boundary.

1. **Pick the instrument and host.** Ask which `instruments/*.json` and the host running reuben
   (default `localhost`, port `9000`).

2. **Discover candidates** (read-only):
   `python3 <skilldir>/gen_surface.py infer instruments/<name>.json`
   This prints Good Buttons (high-confidence public controls) and every node param with
   resolved address + metadata.

3. **Curate and annotate.** If the instrument has no `control` blocks yet, propose a tight set —
   **Good Buttons first**, then only the params that are genuinely musical to play (tempo,
   sequence steps, voice count); skip structural/internal params. Show the user the proposed
   labels + ranges. **Also ask how the user wants the continuous (float) controls rendered —
   linear `fader`s (default) or rotary `radial` knobs** (the choice applies to every fader-kind
   control; toggles/buttons are unaffected). Get confirmation, then **write `control` blocks into
   the instrument JSON with Edit** (preserve the file's inline-node formatting). Forms:
   - Good Button: `"control": { "label": "Brightness", "unit": "%" }`
   - one param: `"control": { "label": "Tempo", "param": "tempo" }`
   - many params on one node: `"control": [ { "label": "Step 1", "param": "step1" }, ... ]`
   - radial knob: `"control": { "label": "Decay", "widget": "radial", "param": "decay" }`
   - play toggle: `"control": { "label": "Play C", "widget": "note-toggle", "port": "note", "note": 60 }`
   `label` is required; `unit`/`widget`/`min`/`max`/`default`/`group` are optional (otherwise
   inferred). `group` is a layout hint (any string): consecutive controls sharing it pack onto one
   row — e.g. tag each drum channel's knobs `"group": "kick"` / `"snare"` / … to get one channel
   per row (see *Layout notes*). `widget` ∈ `fader` (default), `radial`, `note-toggle`. A `radial` is a rotary knob —
   identical value/OSC model to a `fader` (one `x` scaled to the control's range), just rendered
   as a dial; use it for any continuous param when the user prefers knobs. A `note-toggle` plays
   `<node>/<port> [note, gate]` with a constant `note` so note-off matches (TouchOSC can't share a
   value between two controls without scripting, so a separate slider + gate isn't possible natively).

4. **Emit the surface:**
   `python3 <skilldir>/gen_surface.py emit instruments/<name>.json --host <host>`
   Writes `control-surfaces/<name>.tosc` (the repo's versioned, shareable surface dir;
   override with `--out`). Faders and radials send real values (0..1 scaled to range) and init to
   the resting default; the connection is one-way (surface → reuben). A `note-toggle` control emits
   a toggle button that plays a fixed `note` through a message port, e.g. `/voicer/notes`.

5. **Open + verify on device.** Have the user open `control-surfaces/<name>.tosc` in TouchOSC,
   set the OSC **connection host/port** to the machine running reuben, and play. The format is
   confirmed against a real export (see *Format notes*), so this is a sanity check, not a
   debugging round — but always confirm new control *kinds* (a widget type not yet exercised).

## Format notes

The emitter hand-builds the format (no external dep, `zlib`-compressed `lexml version="6"`),
**cloned from a known-good editor export**: `fixtures/REUBEN_REF.tosc` (which carries one of each
control kind the editor offers — FADER, RADIAL, BUTTON, LABEL, plus unused types). The test
`FixtureMatchTest` asserts our per-control property keys still match that fixture, so format
drift fails CI. If a future TouchOSC version changes the format, or you add a new widget type:
rebuild the fixture in the editor (one of each control, distinctive values), replace
`fixtures/REUBEN_REF.tosc`, add the type to `FixtureMatchTest`, and diff to update the property
sets in `gen_surface.py` (note property key **order** must match the fixture — the test compares
ordered lists).

## Layout notes (widget sizing)

The grid is sized for **faders**: each control fills its full cell — a tall/wide rectangle. A
**RADIAL renders a circle sized to its frame's bounding box**, so handing it the same wide, short
fader cell makes the knob overflow into the neighbouring rows (and the lone control of a short
last row, which a fader stretches full-width, becomes a giant circle). The emitter therefore boxes
every radial into the largest **centred square** that fits its cell (`build_tosc`); `RadialTest`
locks `w == h`. Keep this rule for any future circular/2-D control (XY pad, radar) — frame it
square, don't let it fill a fader cell.

Because the square is capped by the cell's *height*, a radial-heavy surface with many rows yields
small knobs (the vertical budget is split across all rows). Knobs are square and there's usually
spare horizontal room, so **more columns = fewer rows = bigger knobs**: pass `--cols N` to `emit`
to trade width for knob size (e.g. 25 knobs at `--cols 4` are 68px; at `--cols 7` they grow to
~130px). Tune `--cols` to the channel/group structure when the params group naturally.

**Grouping params onto rows.** For a channel-structured instrument, a uniform `--cols` grid splits
a channel awkwardly across rows. Instead tag each logical group with a `group` string on its
control specs (in declaration order): consecutive same-`group` controls become one full row
(wrapping past `STEP_COLS`=16), and ungrouped controls flow into the `--cols` grid in between. The
euclidean-drums surface uses this — tempo (ungrouped) on row 1, then the kick/snare/tom/hat knobs
each `"group"`-tagged onto their own row, giving 5 even rows of square knobs. Group rows size to
their own width, so a 6-knob channel row and a 1-knob tempo row still share the same knob size.

## Run the tests

`cd <skilldir> && python3 -m unittest` — covers metadata resolution, inference, OSC addressing,
the zlib/XML round-trip, the structural match against `fixtures/REUBEN_REF.tosc`, and a
**live-engine boundary test** that runs the real `reuben describe` (via `cargo run`, so it tracks
current source) on `instruments/patches/space.json` and asserts the emitted pipe surface — this is
the guard that fails on an ADR-0038-style interface-format flip instead of letting the boundary
path rot silently. It skips (loudly) if `cargo` isn't on `PATH`.

## Scope

| Thing | Action |
|---|---|
| `control` blocks in the instrument JSON | **write** (curated, confirmed, via Edit) |
| `.tosc` surface file | **emit** via `gen_surface.py emit` (control blocks) or `boundary` (a nested instrument's `interface`) |
| instrument's `interface` input pipes | **read** via `describe --json`; never edit — a `boundary` surface authors no `control` blocks |
| instrument graph / operators | **never edit** — surface metadata only |
| two-way OSC feedback, grouped layouts | **out of scope** (ADR-0018 deferred) |

## Report

End with: which instrument, the controls chosen (address + range), where the `.tosc` was
written, the host/port to set in TouchOSC, and the explicit ask to verify it loads on device.
