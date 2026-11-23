# MIDI input via a MIDI surface doc — design

Status: design · 2026-11-23

## Intent

A player plugs in a MIDI controller and plays an Instrument with it: keys drive the Voicer, knobs
and faders move parameters. The mapping from MIDI to the Instrument's interface inputs travels with
the Instrument, not with the hardware: swap a Keystep for an MPC 2000, and as long as it sends the
same notes and CCs, the Instrument plays the same.

The Instrument itself stays **input-protocol-agnostic**. Its interface inputs are the one boundary,
and they look like OSC (`/<name>/in`). MIDI is a kind of UI, so the MIDI mapping lives in the
surface layer — beside the on-screen surface doc, never in the Instrument document — following the
split #247 made: the Instrument carries the contract, a surface doc carries one way to play it. An
Instrument with no MIDI map plays exactly as it does today, and no Instrument requires MIDI.

## Goals

- `reuben play` reads MIDI from the devices the device profile names and converts each MIDI message
  to an OSC-shaped message on a fixed address scheme (`/midi/<ch>/cc/<n>`, …), identity in the
  address and only the value in the payload.
- A **MIDI surface doc**, `surfaces/<stem>.midi.json`, binds interface inputs to MIDI with one
  OSC 1.0 address pattern per control: `{ "bind": "bass", "midi": "/midi/[4-8]/note" }`.
- Each MIDI value is scaled into the bound input's range, so a CC's travel spans the input's
  `min`..`max` along its `curve`, optionally narrowed per control.
- MVP message kinds: note on/off, control change, pitch bend, channel pressure, program change.
- A binding can be exercised without hardware, by injecting a translated `/midi/...` message.

## Non-goals (MVP)

- **Editing MIDI maps (and surfaces) from the editor or MCP.** Planned next: server verbs for
  surface docs, then editor and MCP clients. In the MVP the MIDI map is edited as a file, like
  today's surface docs (by hand or by the `control-surface` skill).
- **Per-note expression** — MPE, polyphonic aftertouch. Needs the Voicer to route expression into
  the voice playing the note; voices receive only `freq`/`gate` today.
- **MIDI clock, transport, song position** — belongs with clock sync (#54).
- SysEx, NRPN/RPN, 14-bit CC (MSB/LSB pairs), MIDI output.
- Numeric ranges in patterns beyond one digit (CC 30–70, channels 10–16). OSC 1.0 spells them
  awkwardly (`{3[0-9],4[0-9],5[0-9],6[0-9],70}`); a range extension waits for real need.
- Choosing among several MIDI maps per Instrument (`--midi-map <name>`); per-device address
  prefixes. All devices share one `/midi/...` space and the one map.
- Pattern matching for UDP OSC. Plain OSC keeps sending to `/<name>/in`, unchanged.
- Sample-accurate MIDI timing; delivery is block-quantized like OSC input today.
- In-graph MIDI types, or filter/route Operators.

## Anti-goals

- Any change to the Instrument document format. No `midi`, `listen` or `filter` field on inputs,
  no reserved names.
- Raw MIDI crossing into `reuben-core`; any new work on the render thread. Every invariant in
  `docs/agents/authoring.md#invariants-you-must-not-break` holds.
- A client reading Instrument document bytes to learn the inputs. The MIDI router runs inside the
  `play` server and reads the interface inputs' types, ranges and curves through `reuben-api`
  (the structured interface the authoring half already returns,
  `crates/reuben-api/src/authoring/result.rs`), never the document text.
- A silently ignored mapping. Every control the router cannot use is reported, naming it.

## Constraints (as the code stands)

- A send to `/<name>/in` **clamps** to the input's `min`/`max`; it never rescales, and nothing in
  the engine applies `curve` (`osc_in_arg`, `crates/reuben-core/src/boundary.rs`; `held_arg` /
  `clamp_to`, `crates/reuben-core/src/render.rs`). Scaling MIDI is therefore new work, done before
  the message is sent.
- Inbound control traffic reaches the render side through per-producer control rings
  (`crates/reuben-native/src/control.rs`). `ControlProducer` is single-owner; the UDP OSC threads and
  the engine channel's `send` each own one (`start_with`, `crates/reuben-native/src/engine.rs`).
- `play` already locates an Instrument's surface doc — `surfaces/<stem>.json` in the Instrument's
  directory or up to two ancestors (`locate_surface`,
  `crates/reuben-native/src/engine_channel.rs`) — and serves it to the editor through the engine
  channel's `surface` verb. The editor only reads `<stem>.json`; the TouchOSC emitter's per-target
  lookup only reads `<stem>.touchosc.json`. Neither will pick up `<stem>.midi.json`.
- The editor's and the TouchOSC emitter's surface-doc readers ignore unknown keys, but
  `surfaces/surface.schema.json` sets `additionalProperties: false` on a control, so the schema
  must gain `midi` before any MIDI map validates against it. The only Rust surface-doc parser is in
  `reuben-editor` (outside `default-members`); `reuben-native` needs one too — move the parser to a
  crate both can use rather than writing it twice.
- A scalar sent to an `f32_buffer` input that declares a range is accepted and clamped
  (`osc_in_arg`), which is how on-screen faders reach signal controls today.
- An `f32`/`i32` input that declares no `min`/`max` has the type-wide, effectively unbounded range.
- The `note` OSC form reads an `f32` pitch as `Pitch::Absolute` and velocity `<= 0` as note-off;
  `note` inputs are Event ports, so several notes in one block do not collapse.

## Approaches considered

1. **Selector fields on Instrument inputs** (`"midi": { "cc": 74, "channel": [1,2] }`). Simple,
   numeric ranges free. Rejected: puts a protocol into the Instrument, which should stay
   protocol-agnostic.
2. **Address-pattern subscriptions on Instrument inputs** (`"listen": "/midi/2/cc/74"`) matching
   all inbound OSC. Rejected for the same reason, and because names-as-addresses already give
   plain OSC its route; a second address per input mixed shapes and ranges on one input.
3. **Routes in the device profile** (`"/midi/[4-8]/note": "/bass/in"`). Keeps the Instrument clean,
   but the mapping then belongs to the rig, not the Instrument — swapping controllers would mean
   rewriting routes. Rejected: the mapping must travel with the Instrument.
4. **MIDI map as a surface doc** — *chosen*. The surface layer already exists to hold "one way to
   play this Instrument", is optional, references inputs by name, and travels with the Instrument
   by convention. A separate `<stem>.midi.json` rather than a `midi` key on the existing surface
   doc: the on-screen panel and the MIDI map change independently, and a MIDI-only entry in
   today's `controls` would appear as an on-screen control to existing readers.
5. **Matching in `reuben-core`**. Rejected: breaks the OSC-only core invariant.

Identity goes in the **address**, not the payload (`/midi/2/cc/40 [..]`, not
`/midi [2, CC, 40, ..]`): patterns match addresses only, and the payload then already has the
shape the destination input decodes (`[60, 0.8]` *is* a `note`).

## Design

### 1. MIDI → OSC translation (`reuben-native`)

A MIDI thread per opened device decodes each message to one translated message. Channels are 1–16,
as printed on hardware. Other MIDI messages are dropped (and counted).

| MIDI message | Address | Translated args |
| --- | --- | --- |
| Note on, vel > 0 | `/midi/<ch>/note` | `[pitch f32, velocity f32]`, velocity = v/127 |
| Note off, or note on vel 0 | `/midi/<ch>/note` | `[pitch f32, 0.0]` |
| Control change | `/midi/<ch>/cc/<n>` | `[value f32]`, v/127 |
| Pitch bend | `/midi/<ch>/bend` | `[value f32]`, −1..1 (14-bit, 8192 → 0, 0 → −1, 16383 → +1) |
| Channel pressure | `/midi/<ch>/pressure` | `[value f32]`, v/127 |
| Program change | `/midi/<ch>/program` | `[program i32]`, 0..127 |

The translated message is never sent to the engine as-is. It is the input to the router (§3).

### 2. The MIDI surface doc

`surfaces/<stem>.midi.json`, located exactly as `locate_surface` locates `<stem>.json` (same
directories, same depth). Same surface-doc format and `surface_version: 1`; each control carries
`bind` and `midi`, and optionally a narrower `min`/`max`. `label`, `widget`, `group`, `cols` are
allowed by the shared format and ignored here.

```json
{
  "surface_version": 1,
  "instrument": "acid-techno",
  "controls": [
    { "bind": "notes",  "midi": "/midi/1/note" },
    { "bind": "bass",   "midi": "/midi/[4-8]/note" },
    { "bind": "cutoff", "midi": "/midi/{1,2,5,9}/cc/74", "min": 200, "max": 8000 },
    { "bind": "drive",  "midi": "/midi/1/cc/1" },
    { "bind": "bend",   "midi": "/midi/1/bend" },
    { "bind": "patch",  "midi": "/midi/16/program" }
  ]
}
```

`surfaces/surface.schema.json` gains the optional `midi` string on a control.

**Naming.** `<stem>.midi.json` follows the existing per-target convention (`<stem>.touchosc.json`),
and shares its one ambiguity: an Instrument whose own stem is `foo.midi` would have its on-screen
surface at the same path as `foo`'s MIDI map. No reservation is added; the docs state the
convention, and `play` treats `surfaces/<stem>.midi.json` as the MIDI map of `<stem>`.

**Matching.** `midi` is one OSC 1.0 address pattern (`*`, `?`, `[..]`, `[!..]`, `{a,b}`), matched
against translated addresses. A translated message goes to **every** control whose pattern matches
it (fan-out); several controls may bind one input (merge — CCs from several channels or numbers
moving one input is intended, consistent with notes merging across channels).

**Checks, per control**, against the played Instrument's interface inputs. A failing control is
**skipped with a warning naming it and the reason**; the other controls still work:

- the pattern is well-formed OSC 1.0 and starts with `/midi/`;
- it can match only **one message kind** (`note`, `cc`, `bend`, `pressure`, `program`) — `/midi/1/*`
  or `/midi/1/{note,cc}` is refused, so one input always receives one shape and one source range;
- `bind` names an interface input of the top-level Instrument;
- the kind fits the input's type: `note` → `note`; `cc`, `bend`, `pressure` → `f32`, `i32`, or an
  `f32_buffer` input that declares a range (a signal control, reachable as an on-screen fader
  reaches it); `program` → `i32`. Anything else (a bare `f32_buffer`, `str`, enums, `harmony`,
  `pitch`) is refused;
- a `cc`, `bend` or `pressure` control has a finite range to scale into: the input declares
  `min`/`max`, or the control narrows it. An input with neither is refused — spreading a knob's
  travel across an unbounded range is never what was meant;
- a narrowing `min`/`max` lies inside the input's range (else clamped into it, with a warning —
  the surface doc's existing subset rule).

A doc that does not parse, or has another `surface_version`, is not used at all, with a warning.
An `instrument` name that differs from the played Instrument's is a warning only (as for surfaces).

### 3. The router (`reuben-native`, off the render thread)

- **Which map applies:** the one beside the Instrument `play` is currently playing — after
  `open_instrument` switches it, the router follows, exactly as the engine channel's `surface` verb
  resolves against the open Instrument. Whatever is played at top level (an Instrument or a Rig) is
  the one whose stem names the map.
- The router holds a **route table**: compiled pattern, message kind, target `/<bind>/in`, and the
  scaling the target needs (its type, effective `min`/`max`, `curve`). It is built inside `play`
  from the parsed Instrument it already holds plus the MIDI surface doc, and rebuilt after every
  Swap and whenever the MIDI surface doc changes on disk (the same file following `play` already
  does for Instrument files). Building happens off the MIDI threads; they pick up the new table by
  a non-blocking pointer swap.
- For each translated message, the router finds every matching route, scales the value (§4), and
  pushes `/<bind>/in [value]` onto its own control-ring producer. The render side is unchanged: it
  sees ordinary `/<name>/in` sends and applies the existing decode and clamp.
- A message routed against a table one Swap old, naming an input the new Instrument no longer has,
  drops and counts like any unroutable send.
- **Held notes.** The router remembers, per note route, which (channel, pitch) it has sent a
  note-on for. When the table is rebuilt and a route with held notes changes target or disappears,
  or a device is lost, the router sends a note-off for each held note to the target it sent the
  note-on to, so nothing hangs. A note-off arriving later for a note it already released is
  dropped.
- **Devices lost mid-play** (unplugged) are a warning and a diagnostics counter, not an error;
  `play` keeps running. Reconnecting (hot-plug) is out of the MVP — restart `play`.
- Multiple devices may share one router and producer, or each own one; left to the builder.

### 4. Scaling

Every MIDI value is first normalized to `u` in 0..1: CC, pressure `v/127` (so CC 64 is
0.504, not exactly the middle — no split mapping); bend `(b + 1) / 2` (centre exactly 0.5). The effective range is the control's `min`/`max` if given, else the input's.

- `f32` input, or an `f32_buffer` input with a range: `u` mapped across the effective range along the input's `curve`, the same curve
  math the engine documents for that curve (reuse an existing implementation — e.g. the `map`
  Operator's — rather than writing a second one; a builder decision).
- `i32` input: the same, then rounded to the nearest integer.
- `program` → `i32` input: **not scaled**; the program number 0..127 is sent as-is and clamps into
  the input's range. (Program numbers are identities, not positions.)
- `note` → `note` input: not scaled; `[pitch, velocity]` as translated.

Example: CC 74 = 64 into `cutoff` (20–20000 Hz, `exp`) with the control's 200–8000 →
200 · 40^0.504 ≈ 1284 Hz.

### 5. Devices (device profile)

The device profile (`docs/device-profile.md`,
`crates/reuben-native/schema/device-profile.schema.json`) gains:

```jsonc
"midi": { "inputs": ["Keystep", "MPC"] }   // case-insensitive name or substring, like audio devices
```

Each named port is opened when `play` starts. A name matching no port is a startup error, as
for audio devices; it also lists the available MIDI ports (new — the audio errors do not list
devices today). No `midi` section opens nothing. With no MIDI surface
doc for the played Instrument, open devices are read and counted but route nowhere.

### 6. Injection without hardware

`reuben` gains a way to inject a translated message into the router, e.g.
`reuben send-midi /midi/1/cc/74 0.5`, carried over the engine channel to `play` (the server) as its
own verb — not through `send`, so `/midi/...` never needs to be a reserved node address. Exact CLI
spelling is the builder's. MCP exposure comes with the later MCP surface work.

### 7. Diagnostics

Engine diagnostics gain MIDI counters: messages received per device, translated, dropped
(unsupported kind), routed, unmatched; and the skipped-control warnings from §2, surfaced at
startup, at every rebuild, and in `get_engine_diagnostics`. The existing OSC logging flag (or a
sibling) logs translated MIDI messages and their routes, so "is my keyboard arriving, and where is
it going?" has an answer.

## Decisions made in the conversation

1. MIDI is converted to OSC-shaped messages at the native boundary; core never sees MIDI.
2. Identity in the address, value in the payload.
3. The mapping lives in the surface layer, in a separate `surfaces/<stem>.midi.json`; the Instrument
   document is unchanged and protocol-agnostic.
4. One pattern per control; patterns are OSC 1.0 address patterns over the `/midi/...` space.
5. Merging several channels or CC numbers into one input is intended.
6. Channels 1–16 in addresses.
7. Per-note expression, clock sync, and editor/MCP editing of surfaces are out of the MVP; editor
   and MCP editing of surfaces and MIDI maps is the planned follow-up.

## Calls made while writing this (check these)

- A control's pattern must select one MIDI kind, and the kind must fit the input type (§2).
- Values are scaled into the input's range along its `curve`; `program` is not scaled (§4).
- A bad control is skipped with a warning, not a load failure, matching surface-doc behavior (§2).
- Ranged `f32_buffer` inputs accept CC/bend/pressure; an input with no finite range needs the
  control to narrow it, else the control is skipped (§2).
- The map follows the Instrument `play` is currently playing (§3).
- The router releases held notes when their route changes or vanishes, or a device is lost;
  no hot-plug in the MVP (§3).
- CC normalizes as `v/127`, no split mapping (§4).
- The `<stem>.midi.json` name collision is documented, not reserved (§2).
- Injection is its own engine-channel verb, so the Instrument format reserves nothing (§6).
- UDP OSC gets no pattern matching in the MVP.

## Left to the builder

- Pattern matcher: borrow one with OSC 1.0 semantics or write one; test against the spec's rules.
- MIDI library (`midir` expected); confirm Linux/macOS/Windows coverage.
- How the route table is shared with the MIDI threads; producer ownership per device.
- How the "one message kind" check is computed (e.g. match the pattern's third segment against the
  five kind names).
- Counter names, log flag, CLI spelling for injection.
- PR split. A natural order: (1) shared surface-doc parser + schema `midi` field + MIDI surface doc
  checks + route table + injection verb, tested with injected messages; (2) MIDI device input +
  translation + device profile + held-note release; (3) diagnostics and docs
  (`docs/device-profile.md`, the `control-surface` skill).
