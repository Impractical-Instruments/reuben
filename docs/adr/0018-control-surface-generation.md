# Generated control surfaces: the `control` block, a `map` resting default, and a TouchOSC skill

## Context

[ADR-0017](0017-playable-surface-and-control-domain.md) built the *playable surface* — the
operators (math family, M→S converter) and the composition pattern (Good Buttons) that let an
Instrument expose good controls. It deliberately **deferred the surface's public metadata**:
"until nesting, a Good Button's metadata just lives on its `map` params," and the
*surface → synthesized `Descriptor`* step waits on the ungrilled nesting/contract thread.

That leaves a practical gap. We make instruments faster than we can hand-build UIs to play
them. The fastest path to a touchable UI is a **generated control surface** for an external OSC
controller — and reuben is already OSC-in over UDP ([`osc.rs`](../../crates/reuben-native/src/osc.rs),
[ADR-0007](0007-osc-only-core.md)), so a surface needs no engine I/O work, only a *generator*
and a *little metadata to generate from*. This ADR records that generator's design and the two
small engine additions it needs. It **partially un-defers** ADR-0017's surface-metadata thread:
not the full Descriptor synthesis (that still waits on nesting), but the minimum per-node
metadata a UI generator must read.

Two existing facts framed the tree:

- **A surface controller speaks OSC to node addresses.** A widget sends a value to a node's
  address (e.g. `/brightness`, `/filter/cutoff`); reuben routes it exactly as today. No new
  control path — the generator's whole job is to emit a controller layout whose widgets target
  the right addresses with the right value ranges.
- **There is no public-control boundary yet.** ADR-0017 ships Good Buttons as *composition*
  with no declaration of which nodes are player-facing. A generator must be told which nodes go
  on the surface, and with what label/range — metadata that today exists only implicitly (a
  direct param's `ParamMeta`, or a Good Button's `map` instance params, which carry **no unit
  and no label**).

This ADR settles: where that metadata lives, how the generator discovers and reads it, the one
engine change that gives Good Buttons a coherent resting position, and the shape of the skill.

## Decision

### Choose Hexler TouchOSC as the first (and only, for now) target

The generator emits a Hexler **TouchOSC** layout (`.tosc`, gzipped XML) — the current,
cross-platform, OSC-native controller product. PureData and the legacy TouchOSC Mk1 format are
rejected: Pd is a DSP patching environment, not a quick-UI tool, and Mk1 is EOL. TouchOSC ↔ OSC
↔ reuben is a native match with no glue layer.

This is a **one-shot, disposable generator**, not a living artifact: the instrument JSON is the
source of truth; a `.tosc` is a scratch playing surface you regenerate when the instrument
changes. No `schema_is_in_sync`-style staleness machinery until drift actually hurts. The
**auto-UI system** (an app that reactively builds UI from an instrument) is explicitly *not*
this — it is the larger build this generator de-risks by getting us playing first.

### The public-control boundary: an opt-in `control` block per node

A node declares itself player-facing with an optional `control` block — the minimum
un-deferral of ADR-0017's surface metadata, **not** the full Descriptor synthesis:

```json
{ "type": "map", "address": "/brightness",
  "params": { "in_min": 0, "in_max": 100, "default": 50 },
  "control": { "label": "Brightness", "unit": "%" } }
```

- **`label`** — required. The one thing inference cannot supply (a `map` carries no name).
- **`unit`** — optional display string. For a direct param it defaults from `ParamMeta.unit`
  ("Hz") and may be overridden; for a `map` front-end (no intrinsic unit) it is the only source.
- **`widget`** and **range** — optional overrides; otherwise inferred (below).

The block lives **inline** in the instrument JSON, with a passthrough field on `NodeDoc` so it
survives load → round-trip → re-serialize (serde silently *drops* unknown fields today, and
`InstrumentDoc::from_graph` would erase it). One source of truth; the literal seed for
ADR-0017's deferred surface-boundary work. A sidecar file was rejected (drift, two files to
align); inline-but-loader-ignored was rejected as a silent data-loss trap.

### Real values from metadata; source depends on binding

A widget shows and sends the **real value the user feeds into that node** — clearer than a
normalized `0..1` for both direct params (see actual Hz) and Good Buttons (see whatever range we
chose). The range/unit/default source differs by what the control binds to:

| Binding | Range | Unit | Default |
| --- | --- | --- | --- |
| Direct param (`/filter/cutoff`) | descriptor `ParamMeta.min/max` | `ParamMeta.unit` | `ParamMeta.default` |
| `map` front-end (`/brightness`) | node's `in_min`/`in_max` **instance param values** | `control.unit` (none intrinsic) | `map.default` (below) |

A `map`'s *descriptor* min/max are validation bounds (`±1,000,000`), useless as a widget range —
the generator reads the node **instance's** `in_min`/`in_max` param values. Widget type is
inferred (Signal/param → fader; small-int discrete param → stepper/radio; gate/trigger →
button) with `control.widget` as the override.

### A resting `default` on the `map` operator

ADR-0017's `m2s` already holds a resting Signal value (`m2s.default`), so a Good Button chain is
not silent at rest. But that default is **output-domain and per-converter** — a brightness knob
feeding `m2s_cutoff` and `m2s_res` rests at two *independent* values with **no single knob
position** tying them together, and the UI knob has **no defined initial value at all**.

`map` gains a `default` param so a Good Button has one coherent resting position:

1. **Domain:** input-side (the resting knob position, in the same units the widget shows). One
   number serves both the UI's initial value and the runtime.
2. **Emit-on-init:** `map` emits `remap(default)` once at frame 0 of its first block (before any
   event), so the whole downstream chain converges to the position the knob visibly shows —
   sound and UI agree at rest. Stateful flag, reset on `spawn()` (same pattern as
   `differentiate`/`integrate`).
3. **Default-of-default:** `default = in_min`. This **changes behavior** — a `map` now emits
   `in_min` at startup where it previously emitted nothing. Accepted for the defined resting
   state; existing instruments should set `default` explicitly to taste.

### Bootstrapping: the skill infers, proposes, and writes back

Every existing instrument has zero `control` blocks. On a node with none, the skill **infers
candidates** (top-level `map`s, sequencer steps, clock tempo, unwired Signal params), proposes a
control set, and on confirmation **writes `control` blocks back into the JSON** *and* emits the
`.tosc`. First run is the annotation pass, not a chore; subsequent runs are deterministic from
the stored blocks. (Error-only and write-nothing variants rejected — the write-back is where the
skill earns its keep over hand-authoring.)

### Connection: one-way, host-arg, port 9000

The emitted layout targets `host:9000` (reuben's default), `host` a skill argument defaulting to
`localhost`. **One-way** (surface → reuben); widgets initialize locally to the param/`map`
default. Two-way feedback (reuben echoing current values) needs an OSC *sender* in the engine —
reuben is OSC-in only — so it is a future engine feature, not a surface-generator concern.

## Consequences

- **Engine changes (PR 1, independently useful):**
  - `NodeDoc.control: Option<serde_json::Value>` passthrough
    ([`format.rs`](../../crates/reuben-core/src/format.rs)) — `#[serde(default,
    skip_serializing_if = "Option::is_none")]`; round-trip-safe.
  - `map` gains a `default` param ([`operators/math.rs`](../../crates/reuben-core/src/operators/math.rs)):
    input-domain, emit-on-init at frame 0, `spawn()`-reset seed flag, descriptor default
    `= in_min` (`0.0`). **Behavior change** — covered by tests and the `rt_safe` allocation check.
  - `control`-block schema (object: `label` required; `widget`/`unit`/range optional) added to
    the generator and `instrument.schema.json` regenerated (`cargo run -p reuben-core --example
    gen_schema`); `schema_is_in_sync` kept green.
- **The TouchOSC generator skill (PR 2, on the settled schema):** reads an instrument, infers +
  proposes + writes back `control` blocks on first run, then emits a Hexler `.tosc` — uniform
  grid, declaration order, tablet-landscape, one-way OSC to `host:9000`. Real values from
  metadata per the binding table; widget type inferred with `control.widget` override.
- **Partially un-defers ADR-0017's surface-metadata thread:** the `control` block is the minimum
  per-node public metadata. Full *surface → synthesized `Descriptor`*, a stable node `id`, and
  encapsulating surfaces **remain deferred to the nesting/contract thread** — the `control` block
  is designed to feed that synthesis later, not replace it.
- **Not built:** the reactive auto-UI system (this generator de-risks it); two-way OSC feedback
  (needs an engine OSC sender); grouped/author-positioned layouts (want the section model
  nesting hasn't built); PureData / legacy TouchOSC targets.
