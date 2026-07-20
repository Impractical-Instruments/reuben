---
name: control-surface
description: Author and edit a reuben surface doc (`surfaces/<name>.json`) — the durable presentation layer binding an instrument's interface pipes to widgets — and project it to a Hexler TouchOSC layout (.tosc) played over OSC. Use when the user says "make a control surface", "generate a TouchOSC layout", "make a UI for this instrument", "relabel/reorder/regroup the controls", "edit the surface", or wants to play an instrument from a phone or tablet.
---

# control-surface

A playable surface is three layers owned by three files:

| Layer | File | Carries | Lifecycle |
|---|---|---|---|
| **Contract** | the instrument's `interface.inputs` pipes | the *quantity*: `type` / `default` / `min` / `max` / `curve` / `unit` — engine-enforced against every consumer wire | edited by the **`patcher`** skill, never this one |
| **Presentation** | `surfaces/<name>.json` (schema: `surfaces/surface.schema.json`) | `bind` / `label` / `widget` / `group`, selection + order, optional **narrower** `min`/`max` | **durable, editable** — this skill's source of truth |
| **Projection** | `control-surfaces/<name>.tosc` | the TouchOSC rendering | **disposable** (one-shot framing, now scoped to the `.tosc` only) — regenerate, never hand-edit |

Interface input pipes are the **one boundary**: a control bound to pipe `name`
sends OSC to **`/<name>/in`** — the pipe node minted at `/<name>`, its `in` port — the same
address `describe` reports. (That assumes the instrument plays at top level; nested under a host
at `/h`, the same pipe is `/h/<name>/in`.) The resolver merges the pipe's contract at load, so
the doc stores only the pipe *name*: change the pipe's range and every surface follows,
drift-free. A host with its own renderer consumes the same docs directly — the browser player's
JS resolver (private `reuben-web` repo) is this script's twin — so editing a surface doc updates
such a host with no emit step; only the `.tosc` needs regenerating.

**Widget vocabulary** — a superset of what TouchOSC renders, so the web target is never capped:

- **Shipped** (both targets): `fader`, `radial` (rotary fader — same value/OSC model),
  `param-toggle` (button sends its 0/1 straight to the pipe — gate steps), `note-toggle`
  (sends `[note, gate]`, constant note so note-off matches; `velocity` rides along for the web
  target), `chord-button` (sends `[degree, gate]` — a note pipe accepts a degree payload at the
  same `/in` port).
- **Reserved** (format-allowed, web-only, **not built**): `xy-pad`, `grid`, `visualizer`,
  `keyboard`.

**The TouchOSC skip table** — `emit` never fails silently; each dropped control gets one stderr
warning naming it: a reserved/web-only/unknown `widget`; a `bind` naming no pipe; a message
(`note`) pipe bound with no explicit widget (nothing inferable); a `note-toggle` missing `note`
or `chord-button` missing `degree`. An out-of-range `min`/`max` override is *clamped* into the
pipe range with a warning (the control is kept — the subset law). A
`surface_version` other than 1 is a hard error; an instrument-name mismatch is a warning only.

**Surface-doc resolution order** (per target `t`, today `touchosc` only):
`surfaces/<stem>.<t>.json` ?? `surfaces/<stem>.json` ?? **auto-derived default** — one fader per
wireable input pipe, declaration order; channel-bound pipes (device bindings), bare `f32_buffer`
pipes (no range to scale into), and message/enum pipes are skipped with a warning naming each.
Reach for a per-target file only when the control *set* genuinely diverges, not mere geometry.

## The surface doc

`surfaces/strum-harp.json`, complete:

```json
{
  "surface_version": 1,
  "instrument": "strum-harp",
  "controls": [
    { "bind": "strum", "label": "Strum", "widget": "fader" },
    { "bind": "octaves", "label": "Range" },
    { "bind": "key", "label": "Key" },
    { "bind": "brightness", "label": "Brightness", "widget": "fader" }
  ]
}
```

Semantics (full schema in `surfaces/surface.schema.json`):

- `controls` order is render order; several controls may bind one pipe (chord-player puts 7
  `chord-button`s on its one note pipe, degrees 0–6).
- `label` defaults to the pipe name, underscores as spaces, each word's first letter uppercased
  (`kick_step1` → "Kick Step1") — pinned so both resolvers agree byte-for-byte.
- `widget` defaults to the type inference (`f32` or ranged `f32_buffer` → fader); a message pipe
  has no inferable widget, so binding one requires an explicit widget.
- `min`/`max` may only *narrow* the pipe range. The widget rests at the pipe `default` clamped
  into the effective range; a pipe with no default rests at the floor.
- `unit`/`curve` come from the **pipe only** — they describe the quantity, not one rendering.
- `group` packs adjacent same-group controls into one row; `cols` (default 4) is widgets per row
  for ungrouped controls.

## Workflows

Run from the repo root; the script is `gen_surface.py` in this skill's directory (`<skilldir>` =
`.claude/skills/control-surface`).

### 1. Inspect the derived default (no doc yet)

`python3 <skilldir>/gen_surface.py emit instruments/<name>.json` with no `surfaces/<stem>.json`
present resolves the auto-derived default and tells you exactly what surfaced (control count) and
what didn't (one warning per skipped pipe). For the *live-engine* view — metadata read from
`reuben describe --json` rather than the instrument file, which doubles as the drift guard on the
describe contract —

```
python3 <skilldir>/gen_surface.py boundary instruments/patches/space.json
```

(`boundary` runs the binary itself: `--reuben PATH` if it isn't under `target/`, or
`--describe FILE` to feed pre-captured output. It emits one fader per wireable input pipe.)

### 2. Scaffold a new `surfaces/<name>.json`

Start from the derivation, not a blank file:

```
python3 - <<'EOF' > surfaces/<name>.json
import json, sys
sys.path.insert(0, ".claude/skills/control-surface")
import gen_surface as g
inst = json.load(open("instruments/<name>.json"))
doc, warnings = g.derive_surface(inst, inst.get("instrument"))
for w in warnings: print("warning:", w, file=sys.stderr)
print(json.dumps(doc, indent=2))
EOF
```

The warnings are your curation worklist: each skipped message pipe is a candidate for an explicit
`note-toggle`/`chord-button` entry with its payload.

### 3. Edit — the round-trip

The doc is the durable source: edit it with Edit and re-emit. The moves, all plain JSON edits:

- **relabel** — set `label`;
- **reorder / curate** — reorder or delete `controls` entries (order is render order);
- **group** — tag runs of controls with the same `group` string to give each logical unit its
  own row (see *Layout notes*);
- **narrow a range** — set `min`/`max` inside the pipe range (outside clamps, loudly);
- **switch widgets** — e.g. `"widget": "radial"` for knobs (ask the user fader vs knob for the
  continuous controls; the choice is per control now, not global);
- **per-target variant** — copy the doc to `surfaces/<stem>.touchosc.json` and diverge; `emit`
  prefers it for the TouchOSC target while the web target keeps reading `<stem>.json`
  (its own variant is `<stem>.web.json`).

Validate the edit against `surfaces/surface.schema.json` shape by re-emitting: the resolver's
warnings name anything unresolvable.

### 4. Project the `.tosc`

```
python3 <skilldir>/gen_surface.py emit instruments/<name>.json
```

Writes `control-surfaces/<stem>.tosc` (the repo's versioned, shareable surface dir; `--out`
overrides). Other flags: `--surface FILE` to bypass the resolution order, `--cols N` (defaults
to the doc's `cols`, else 4), `--host`/`--port` for the printed reminder. Output is
**deterministic** — the same instrument + doc emits identical bytes, so regenerating a committed
`.tosc` is always safe (an unchanged doc leaves `git status` clean). Read the stderr warnings
against the skip table above: each names a control and why it was dropped; the emit still
succeeds unless *nothing* resolved.

### 5. Verify on device

Have the user open `control-surfaces/<name>.tosc` in TouchOSC and set the OSC **connection
host/port** to the machine running reuben (the connection is one-way, surface → reuben, port
9000). The format is confirmed against a real export (see *Format notes*), so this is a sanity
check — but always confirm a widget *kind* not exercised before.

## Graph edits: delegate to `patcher`

This skill **never edits the instrument graph** — not even `interface` pipes. Anything that
changes the contract is the `patcher` skill's job; come back here once the pipe exists:

- **promoting a control to a pipe** (a knob the surface wants but `interface.inputs` lacks);
- **renaming** a pipe or node (then update `bind` here);
- **rewiring**, or changing a pipe's `type`/`min`/`max`/`default`/`curve`/`unit`.

## Worked example: strum-harp

The contract, from `instruments/strum-harp.json` `interface.inputs`: four `f32` pipes —
`strum` (0–1, the drag bar), `octaves` (1–4), `key` (48–60), `brightness` (0–1). The committed
presentation is the doc quoted above: four faders, two of them relabelled (`octaves` → "Range",
`key` → "Key").

A round-trip relabel:

1. Edit `surfaces/strum-harp.json`: `"label": "Range"` → `"label": "Octaves"`.
2. `python3 <skilldir>/gen_surface.py emit instruments/strum-harp.json`
   → `wrote control-surfaces/strum-harp.tosc — 4 control(s) from surfaces/strum-harp.json`.
3. The new label is in the projection (the `.tosc` is zlib-compressed XML:
   `zlib.decompress(open("control-surfaces/strum-harp.tosc","rb").read())` contains
   `Octaves`). The instrument JSON is untouched throughout.

And the default-derivation view, where the curation gap shows: emitting a copy of
`chord-player.json` under a stem with no `surfaces/` doc yields **2 control(s)** (the `key` and
`brightness` faders) plus `warning: default surface skips 'note' pipe 'chord' (a default
surface cannot guess its payload)` — exactly the gap the committed doc curates with its seven
explicit `chord-button`s.

## Format notes

The emitter hand-builds the format (no external dep, `zlib`-compressed `lexml version="6"`),
**cloned from a known-good editor export**: `fixtures/REUBEN_REF.tosc` (one of each control kind
the editor offers — FADER, RADIAL, BUTTON, LABEL, plus unused types). `FixtureMatchTest` asserts
our per-control property keys still match that fixture, so format drift fails CI. If a future
TouchOSC version changes the format, or you add a new widget type: rebuild the fixture in the
editor (one of each control, distinctive values), replace `fixtures/REUBEN_REF.tosc`, add the
type to `FixtureMatchTest`, and diff to update the property sets in `gen_surface.py` (property
key **order** must match the fixture — the test compares ordered lists).

## Layout notes (widget sizing)

The grid is sized for **faders**: each control fills its full cell. A **RADIAL renders a circle
sized to its frame's bounding box**, so the emitter boxes every radial into the largest centred
square that fits its cell (`RadialTest` locks `w == h`); keep that rule for any future
circular/2-D control. Because the square is capped by cell *height*, a radial-heavy surface with
many rows yields small knobs — **more columns = fewer rows = bigger knobs**: set the doc's `cols`
(or pass `--cols`) to trade width for knob size (25 knobs at 4 cols are 68px; at 7 they grow to
~130px).

**Grouping onto rows.** Consecutive controls sharing a `group` string become one full row
(wrapping past 16), and ungrouped controls flow into the `cols` grid between them. Two uses in
the committed docs: `surfaces/euclidean-drums.json` rows its radials one drum channel per
`group` (kick/snare/tom/hat) with ungrouped tempo on its own row; `surfaces/groovebox.json` tags
each 16-step `param-toggle` lane with its channel's `group`, so every lane lines up as one row —
a lane is *identified* by its shared group, since steps are ordinary pipes now.
Group rows size to their own width, so a 6-knob row and a 1-knob row share the same knob size.

## Run the tests

`cd <skilldir> && python3 -m unittest` — covers the pinned resolver semantics (default labels,
widget inference, range clamping, the skip table), the derived default surface, the doc
resolution order, layout, the zlib/XML round-trip, the structural match against
`fixtures/REUBEN_REF.tosc`, and a **live-engine boundary test** that runs
the real `reuben describe` (via `cargo run`, so it tracks current source) on
`instruments/patches/space.json` — the guard that fails on an interface-format flip instead of
letting the boundary path rot silently. It skips (loudly) if `cargo` isn't on `PATH`.

## Scope

| Thing | Action |
|---|---|
| `surfaces/*.json` surface docs (+ per-target variants) | **author / edit** — the durable presentation source |
| `control-surfaces/*.tosc` | **emit** via `gen_surface.py` — disposable projection; regenerate, never hand-edit |
| instrument `interface` input pipes | **read** only — the contract this skill binds to |
| instrument graph — promote a control to a pipe, rename, rewire, change a pipe's contract | **never** — delegate to the `patcher` skill |
| web renderer (private `reuben-web` repo) | **not in this repo** — it consumes the same docs; its JS resolver is this script's twin |
| two-way OSC feedback; reserved widgets (`xy-pad`, `grid`, `visualizer`, `keyboard`) | **out of scope** (format-allowed, not built) |

## Report

End with: which instrument and which surface doc (authored, edited, per-target variant, or the
derived default), the controls that resolved (bind + widget + range) and every skip warning with
its reason, where the `.tosc` was written, the host/port to set in TouchOSC, the explicit ask to
verify it loads on device — and, if the doc changed, the reminder that a host with its own
renderer picks the edit up with no emit step.
