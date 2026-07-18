# Authoring: Instruments and Rigs <!-- lanes: skills,mcp -->

The instrument-authoring guide — the contract an **authoring agent** builds against, whether
it drives the repo skills (`patcher`) or the reuben MCP server (this file is served in-band
as `reuben://guide/authoring` — [ADR-0048](../adr/0048-mcp-tool-surface-and-contracts.md) §7).
It owns the type system and wiring rules, the instrument JSON format, addressing, and the
authoring loop's semantics. The conceptual narrative lives in
[ARCHITECTURE.md](../ARCHITECTURE.md); capitalized terms (Operator, Voice, Plan…) are defined
in [CONTEXT.md](../../CONTEXT.md). The ADRs are the source of truth; this doc tells you where
the contract lives and how to author against it.

Developing a new **Operator in Rust** — the `Operator` trait, the `operator_contract!` macro,
registration, `OpDriver` testing — is the builder's job, not the authoring agent's: that
contract lives in [operator-dev.md](operator-dev.md).

## The recursive model <!-- lanes: skills,mcp,web -->

One concept at every scale ([ADR-0003](../adr/0003-recursive-composition.md)): a graph of
nodes with typed ports.

- **Operator** — the smallest unit of behavior; does one simple thing.
- **Instrument** — a named subgraph of Operators exposing boundary ports; reusable inside
  another Instrument *as if it were an Operator*.
- **Rig** — a full playable system: Instruments wired with routing.

Nesting is an authoring concept only; at runtime everything inlines into one flat graph.

## The authoring loop: the document is truth, `send` is audition <!-- lanes: skills,mcp,web -->

A conversational edit works on one thing: the **instrument document**. Its semantics
([ADR-0045](../adr/0045-whole-document-edit-contract.md) §5):

- **The document is durable truth.** The unit of edit is the whole document
  (ADR-0045 §1): edit the JSON, validate it (the engine's own load path is the single
  validation authority), and swap it in. What the document says is what plays after the next
  Swap — and what saves, shares, and reloads.
- **`send` is ephemeral audition.** A live control message (`/filt/cutoff 1500`) changes
  render state only — sweep a cutoff, try a tempo. The next Swap re-reads inputs from the
  installed document, so an un-folded tweak is **clobbered by design**.
- **Try, then commit.** `send` to explore; when a value is a keeper, fold it into the
  document and swap. Never let the sound and the document drift apart — the document is the
  save source of truth.
- **Renames reset state.** At Swap a node keeps its state iff a node with the same
  fully-qualified address *and* the same operator type exists on both sides (ADR-0045 §2).
  Renaming an address, or changing the operator type at an address, is a remove + add;
  everything else — rewired inputs, changed params, new neighbors — leaves a survivor a
  survivor.

Two pieces of loop conduct, in every lane
([ADR-0059](../adr/0059-cross-lane-grounding-unification.md) §2):

- **Sanity-check that it's audible.** `validate` proves the graph is *legal*, **not that it
  makes sound** — a disconnected oscillator or an unfed output pipe validates clean and
  renders silence. Before reporting a reshape done, check generator→output reach: is there a
  path from a sound source to a declared `interface` output, and are the voicer's
  `freq`/`gate` reaching the voice chain? Warnings (an unresolvable sample, a dark subpatch)
  are advisory — the document is still valid.
- **When unsure of a port, `describe` it — never infer.** Port, input, and param names come
  from the live operator set, not from memory or a sketch; a guessed name costs a repair
  round.

One more piece of authoring conduct that rides the same whole-document re-emission
([ADR-0057](../adr/0057-instrument-reuse-interface-makes-the-role.md) §3):

- **Keep `doc` true when you reshape.** An instrument's reuse story — its **recipe-role** —
  is the first sentence of its own top-level `doc` field: what it is, and when to reach for
  it. Under the whole-document contract every reshape re-emits `doc` too, mechanically
  re-presenting the role line for revision on every edit — keep it true: revise it whenever
  the reshape changes what the instrument is or when it earns reaching for. The role is
  trusted for **selection only**; the `interface` block stays the mechanically-enforced
  face, so a stale role line can cost a bad-sounding pick, never a mis-wired document.

Two swap rules of thumb ([ADR-0050](../adr/0050-swap-sonic-rudeness-ramp.md)):

- **A swap ducks the output for ~20ms.** The engine ramps the master to silence, installs
  the new Plan at zero, and ramps back up — a brief duck, never a click. Don't chase it as a
  dropout. A node at the same address with the same operator type keeps its live state across
  the swap; only the changed nodes rebuild cold. (The **web** lane's swap is ruder by design —
  every node rebuilds cold, `survived: 0` — because its single-threaded worklet can't run the
  off-thread mailbox install, [ADR-0052](../adr/0052-web-parity-contract-not-protocol.md) §2.)
- **A note-off racing a swap can hang a note — re-send the off.** Pending messages are
  dropped at install, so an off landing in that window (≤ one block plus the down-ramp) is
  lost and a surviving voice's gate stays high. Recoverable in-band: re-send the off (or
  re-trigger and release, or let voice stealing claim it). When notes were sounding, follow
  a swap with a corrective `send`.

<a id="type-system"></a>
## One `Input`, one `Arg` type ([ADR-0030](../adr/0030-osc-as-all-data-one-message-type.md)) <!-- lanes: skills,mcp,web -->

Every functional input an operator consumes is **one `Input`**, declared once, carrying one
piece of typed data — its **`Arg`** type, drawn from one closed, central enum. How the value is
read follows from the `Arg` type plus the read verb; whether a numeric port is a held **Value**
(`f32`) or a dense **Signal** (`f32_buffer`) follows from which keyword it declares (ADR-0031).
Outputs carry an `Arg` the same way. (The ADR-0028 **`shape`** axis is **retired** — the axes now
are the port's `Arg` type and its declared form.)

The read/write surface is **two verbs over typed handles** ([ADR-0037](../adr/0037-typed-port-handles.md),
extending ADR-0031): `io.read(IN_X)` and `io.write(OUT_X)`. The contract macro emits each port's
const as a typed handle — `In<form>` / `Out<form>` — whose *type* fixes the read/write shape and
whose value carries the declared default, so a wrong-form read **does not compile** and a held
read's fallback **is** the contract default (no second literal to drift).

| `Arg` type (form) | what it is | `io.read(IN)` / `io.write(OUT)` |
|---|---|---|
| **`f32_buffer`** (a *Signal*, `In<SignalF32>`) | dense per-sample audio / CV / control — the one buffer payload | read: `&[f32]`, **always exactly `io.frames()` samples** (the buffer-presence invariant — index directly; + `io.varying(IN)`) · write: `&mut [f32]` |
| **`f32`** (a held *Value*, `In<Held<f32>>`) | a number — freq, cutoff, amp, a contour; owns a default, latched and read once per (sub)block | read: `f32`, defaulted to the declared default · write: `MsgWriter` (`.set(frame, v)`) |
| **enum** (a *vocab* type, a Value, `In<Held<E>>`) | a named discrete choice — `FilterMode`, `Waveform` | read: the real Rust enum (not an index), defaulted to its `#[default]` variant |
| **`Harmony`** (vocab struct, a Value, `In<Held<Harmony>>`) | the tonal-context struct: `root`/`scale`/`chord` + resolvers `hz()`/`snap()`/`chord_tone()` | read: `Harmony`, defaulted to C-major 12-TET (`Harmony::DEFAULT`) · write: `MsgWriter` (`.set(frame, h)`) |
| **`Note`** (vocab struct, an Event, `In<Event<Note>>`) | a pitch/velocity event | read: `EventStream<Note>` of `Stamped<Note>` (`.frame`, `.payload`) · write: `EventWriter` (`.emit(frame, note)`) |

A port's **form** is one of three — **Signal** (`f32_buffer`), **Value** (`f32`/enum/`Harmony`, a
held latch read once per slice), **Event** (`Note`, a sparse frame-stamped stream) — and follows from
the declared `Arg` type (`PortKind` in `plan.rs`). Reading older code: **Signal** = `f32_buffer`;
**param** = an `f32` Value or held enum; **Context** = `Harmony`; **Message events** = `Note` (the
ADR-0028 `shape`/temporality axis is gone). A runtime integer is a rounded `f32` or an enum; `I32` is
an OSC primitive `Arg`, but no operator declares an `Int` port.

### Form is declared, not inferred: `f32` is a held Value, `f32_buffer` is a Signal <!-- lanes: skills,mcp,web -->

The author picks a numeric port's form by which keyword it declares
([ADR-0031](../adr/0031-float-resolves-to-value-or-signal-by-wiring.md)):

- **`f32` → a held Value.** A latched scalar read once per block-slice with `io.read(IN)`. The
  engine block-slices at each change frame, so the read is sample-accurate without a buffer. It
  seeds its latch from override-or-default; unwired it reads the declared default (carried by the
  handle itself, ADR-0037). No buffer allocated.
- **`f32_buffer` → a Signal.** A dense per-sample buffer read with `io.read(IN)` — audio, CV,
  or any *swept* control (a filter cutoff an LFO modulates). A meta default materializes a constant
  buffer when unwired/knob-set; an unwired *bare* buffer materializes **silence** — so the read is
  a real length-n buffer in every case (the **buffer-presence invariant**, ADR-0037): index
  `io.read(IN)[i]` directly, no `.get(i).unwrap_or(..)` guard.

A cheap **`varying: bool`** rides alongside a Signal read (`io.varying(IN)`): `false` when a
materialized input held unchanged this block, `true` when dense or changed. A const-folding op (a
filter recomputing biquad coefficients only when `cutoff` moves) opts in; a naive op ignores it and
reads `io.read(IN)[i]` — always correct.

So form follows the processing model: per-sample DSP (osc, filter, `mul_f32_signal`, the envelope's
`cv`) declares `f32_buffer` and reads a slice; block-rate controls (a clock's `tempo`, a sequencer's
`length`, a gate edge) declare `f32` and read the held value.

### Wiring across forms: one legal coercion, the rest hard errors <!-- lanes: skills,mcp,web -->

Each wire is checked **locally** at Instantiate against the two ports' forms (`check_wire_forms` in
`plan.rs`, surfacing `PlanError::FormMismatch`). The rules:

- **like → like** (`Signal→Signal`, `Value→Value`, `Event→Event`) connects directly.
- **`Value → Signal`** is the **one implicit coercion**: the held Value materializes (ZOH) into a
  buffer at the Signal input, a mid-block change written at its frame (sample-accurate, one
  `process()`). This is the canonical `voice.freq`(Value) → `osc.freq`(`f32_buffer`) bridge.
- **`Signal → Value`** is a **hard error** — there is no implicit sample-and-hold, and the
  explicit converter that would bridge it (an envelope follower / quantizer) **doesn't ship
  yet**: reshape the graph so the consumer reads a Signal, or shape on the Value side (the
  `m2s` gap-filling modes, below). Into an enum Value it's equally illegal (an enum takes a
  discrete choice, not a per-sample signal).
- **`Event` mismatched** against a Signal/Value is an error (needs an explicit latch / change-detect).

Every cross-*type* crossing still needs an operator: `f32 → enum` is a quantizer; `f32 → Note` is a
threshold/trigger; `slew`/`glide` are `f32 → f32` shapers (the `m2s` gap-filling modes).

### `Constant` — instantiate-time configuration, not an `Input` <!-- lanes: skills,mcp,web -->

A **`Constant`** configures an operator *instance* at instantiate time and never changes on the
data path. The boundary is precise: **a value is a `Constant` iff changing it would rebuild the
graph.** The canonical (and today only) case is the Voicer's `voices` — it sets the voice-pool size,
hence how many voice sub-patches are instantiated, so it can't be a runtime value. A `Constant` is
declared with the contract's **`constant: <param>`** keyword (ADR-0032) and lives in the patch's
`config` block, not `inputs`.

**`Arg` type does not decide `Constant`-vs-`Input`.** `mode` (Lp/Hp/Bp) and `waveform` (Sine/Saw)
are enums, but changing them rebuilds nothing — only which coefficients run — so they are **runtime
enum inputs**, switchable live over OSC. Only genuinely topology-fixing values are `Constant`s.

## The Instrument format (`crates/reuben-core/src/format/`) <!-- lanes: skills,mcp,web -->

An Instrument is plain JSON data ([ADR-0028](../adr/0028-one-input-shape.md); **format v3**
since [ADR-0043](../adr/0043-surface-docs-decouple-presentation-from-instruments.md)). At the top
level it **requires a `instrument` name field** (a string — the human-facing name/id) alongside
`nodes`; a document that omits `instrument` is rejected (the loader denies unknown fields, so the
name cannot be misspelled or dropped). It also carries `nodes` (operator `type` + `address`, plus
an `inputs` map, an optional `config` block, and optional `doc`) and an optional **`interface`**
block — the graph's one boundary, everything that crosses its edge (see below). There is **no
top-level `connections` array** and **no per-node `params` map** (both fold into `inputs`), and
**no anonymous master `outputs` array** (v1-only — it dissolved into named `interface.outputs`
entries; the loader migrates old documents).

**Creating an instrument from scratch? Start with `scaffold-instrument`, not a blank file** — every
lane offers it (`reuben scaffold-instrument` on the CLI, the `scaffold_instrument` tool on the MCP/web
surfaces). It returns a guaranteed-valid minimal document (`{ "format_version": 3, "instrument":
<name>, "nodes": [] }`), which you then edit and swap — turning first-creation into the
reshape-from-template path, so you never stall guessing the required top-level shape
([#146](https://github.com/Impractical-Instruments/reuben/issues/146)).

Each entry in a node's **`inputs`** map is one of:

- a **literal** — `"resonance": 0.4` (an `F32` control default) or `"mode": "Hp"` (an enum by symbol);
- a **wire-ref** — `{ "from": "/osc.audio" }`, or the sole-output sugar `{ "from": "/osc" }` when
  the source has exactly one output.

`"cutoff": 1000` and `"cutoff": { "from": "/lfo.audio" }` target the **same slot**. A node's
**`config`** block holds its `Constant`s (`{ "voices": 8 }`).

```json
{
  "type": "filter", "address": "/filt",
  "inputs": {
    "audio":     { "from": "/osc.audio" },
    "cutoff":    { "from": "/lfo.audio" },
    "resonance": 0.4,
    "mode":      "Hp"
  }
}
```

`format::load` resolves types via a `Registry`, applies literals/config, resolves wire-refs to
edges (checking `Arg` types), and returns a `Graph`. Loading is an authoring step — portable core,
never the audio thread. Every node needs a registered `type` and a unique `address` — a
duplicate is the fatal `DuplicateAddress`. An out-of-range numeric literal — an input default
or a `config` constant — is **clamped** into the port's declared range, never a load error.
Other errors are specific: `UnknownInput`, `BadInputValue`, `TypeMismatch`,
`ConstantInInputs` (a `Constant` placed in `inputs`), `UnknownConfig`, `AmbiguousWire`. See
`instruments/*.json` for worked examples.

### The `interface` block: named pipes at the boundary ([ADR-0038](../adr/0038-interface-pipes-and-the-device-layer.md)) <!-- lanes: skills,mcp,web -->

`interface.inputs` / `interface.outputs` entries are **named pipes** — the single boundary
mechanism at every graph level, with the wiring direction **flipped** relative to the v1
target-pointing form (no entry points inward anymore):

- An **input pipe mints an address in the flat node namespace** (entry `in` → node `/in`; a
  collision with a real node is the fatal `DuplicateAddress`) and behaves like a source node:
  internal consumers wire from it with ordinary wire-refs (`"audio": { "from": "/in" }`),
  fan-out free. Because nothing is pointed at, **the entry declares its own `Arg` type** —
  `"type"`: `"f32_buffer"`, `"f32"`, `"note"`, `"harmony"`, or a vocab enum name — enforced
  against every consumer wire by the ordinary pass-2 wire check. A numeric pipe owns
  engine-enforced `default`/`min`/`max`/`curve` plus a display `unit` — the pipe's whole
  *quantity* contract; presentation (`label`/`widget`) lives in a surface doc, not on the pipe
  ([ADR-0043](../adr/0043-surface-docs-decouple-presentation-from-instruments.md)). A
  defaulted pipe unfed materializes its default —
  a knob at rest, message-drivable at **`/<name>/in`** over OSC; an unfed *bare* signal pipe
  renders silence (and warns at top level, where nothing can ever feed it).
- An **output pipe is fed from an internal port**: `"main_l": { "from": "/pan.left" }`.
  Signal output pipes drive the logical master channels.
- A **signal** pipe may carry an optional logical **`channel: <int>`** binding — **honored
  only on the graph actually played at top level**: an input pipe with `channel: k` reads
  logical input channel `k` (real device audio via the input stream); an output pipe with
  `channel: k` feeds logical output channel `k`. A channel-bound pipe keeps its declared
  `default` as the unfed fallback. `channel` on a **message** pipe is a load error. An output
  pipe with `channel` omitted **broadcasts** to all logical output channels (the v1 default,
  unchanged). Logical widths derive from the played top graph: output = max bound output
  channel + 1 (floor 2), input = max bound input channel + 1 (0 if none — a patch with no
  input pipes pays nothing).
- **Nested or Voicer-hosted, `channel` is inert** — the parent feeds the pipe through the
  synthesized face like any boundary wire; a nest never reaches the hardware on its own.
  Patches never name *device* channels at all: binding logical channels to a real rig is the
  device profile's job (`play --io-map`, [docs/device-profile.md](../device-profile.md)).

```json
"interface": {
  "inputs": {
    "in":   { "type": "f32_buffer" },
    "mic":  { "type": "f32_buffer", "channel": 0 },
    "tone": { "type": "f32_buffer", "default": 4000.0, "min": 20.0, "max": 20000.0,
              "curve": "exp", "unit": "Hz" }
  },
  "outputs": {
    "main_l": { "from": "/pan.left",  "channel": 0 },
    "main_r": { "from": "/pan.right", "channel": 1 }
  }
}
```

Worked examples: `instruments/patches/space.json` (a nestable effect's typed pipes) and
`instruments/mic-space.json` (a channel-bound live-input pipe feeding a nested patch). The
multichannel-output demos left the library in the cull and live on as frozen test fixtures:
`crates/reuben-native/tests/fixtures/stereo-sub.json` + `stereo-sub.io-map.json` (three bound
output channels + the device profile that maps them) and `stereo-autopan.json` (stereo
channel-pinned outputs).

A document may also carry a top-level `resources` table (logical id → source path) that
resource-bearing nodes reference by a `sample` field
([ADR-0016](../adr/0016-sample-player-and-resource-store.md)). Resolving + decoding those needs a
`ResourceResolver`, so use `format::load_instrument(json, registry, resolver)` — it returns the
`Graph` plus any non-fatal `LoadWarning`s (a missing/undecodable sample degrades to silence).
`crates/reuben-native/tests/fixtures/sampler.json` is the worked example (frozen as a test
fixture since the library cull); `reuben-native` supplies a filesystem WAV resolver.
A source path resolves **relative to the document that names it** (a nested patch's own resources
live next to *it*, not next to the top-level instrument), falling back to a configurable library
root (`reuben --instrument-root <DIR>` or `REUBEN_INSTRUMENT_ROOT`); the resolver canonicalizes
identity, so `a.json` and `./a.json` are one cycle-guard/dedup key. For embedded hosts and tests,
core's in-memory `MemoryResolver` serves patches and samples by exact key with no filesystem.
A document may declare a `format_version` ([ADR-0036](../adr/0036-instrument-library-and-format-versioning.md));
absent means 1, and a version newer than the engine understands refuses to load. The current
version is **3** — [ADR-0043](../adr/0043-surface-docs-decouple-presentation-from-instruments.md)'s
presentation strip, the second breaking bump after ADR-0038's v2 interface-pipe direction
flip. Old documents keep loading forever, migrated at parse: v1's target-form `interface`
entries flip to pipes + consumer wire-refs (deriving each pipe's type/range/default from the
old target port; the anonymous `outputs` array becomes named `interface.outputs` entries), and
a leftover per-node `control` block or pipe `label`/`widget` — v2's retired presentation — is
**ignored with a `LoadWarning` naming it** (`DeprecatedControlBlock` /
`DeprecatedPipePresentation`): never fatal, never silent, and sound is unaffected (the engine
never read them; re-saving strips them). Migrated-vs-native renders are **bit-identical**
(asserted in `crates/reuben-core/tests/format_v2.rs` and `format_v3.rs`). Save writes v3 — a
migrated document never saves back under its old number. The whole normalize pipeline —
version gate, migrations, stamp — lives in `format/normalize.rs` behind the **`NormalizedDoc`**
type ([ADR-0047](../adr/0047-normalization-is-a-type.md)): `NormalizedDoc::from_json` is the
one mint (a hand-deserialized `InstrumentDoc` enters via `NormalizedDoc::from_doc`), building a
`Graph` is `NormalizedDoc::build`, so a document past the gate is current-shaped and migrated
exactly once — held by the compiler, not re-checks. To **save**, serialize
the `InstrumentDoc` (nested references survive; a `NormalizedDoc` derefs to one, or take
`into_inner()`); `NormalizedDoc::from_graph` is the explicit
flatten/export path — a built graph's spliced subpatches appear as their inlined nodes.

A Voicer node references a **voice sub-patch** the same way, by a **`voice`** field naming a standalone
instrument JSON ([ADR-0032](../adr/0032-voicer-hosts-voice-subpatches.md)); the loader resolves it
(nested `sample` resources resolve recursively), builds it `voices` times, and binds the graphs. A
voice patch declares its **`interface`** like any graph (pipes, ADR-0038): input pipes
(`freq`/`gate`) its internal nodes consume, output pipes (`audio`/`active`) fed from internal
ports — so the host Voicer can drive and tap it through the boundary. Hosted this way, any
`channel` binding on a voice's pipes is inert, exactly as for a nested subpatch.
(`interface` is real wiring the engine type-checks — the contract a surface doc binds to,
never surface metadata itself.) See `instruments/default.json` + `instruments/voices/default-voice.json`.

### Nesting: a `subpatch` node inlined at build ([ADR-0034](../adr/0034-instrument-nesting.md)) <!-- lanes: skills,mcp,web -->

A **`subpatch`** node references a nested instrument the same way, by a **`patch`** field naming an
instrument JSON in `resources`. At build the child is resolved recursively and **inlined**: its
nodes splice into the parent under the node's address prefix (child `/filter` inside `/space`
becomes `/space/filter` — still OSC-reachable; a post-prefix collision is a fatal
`DuplicateAddress`), every parent wire onto a boundary port is rewired straight to the inner
target, and the `subpatch` node **dissolves** — nesting costs nothing at runtime. Two uses of one
patch get disjoint prefixes, so per-reuse state isolation is automatic. Cyclic references are a
fatal `CyclicResource`; availability problems (missing id, unreadable source) degrade to a
`LoadWarning` (the node goes *dark* — references to it drop with warnings); a
resolved-but-malformed child document is fatal.

The node's ports are the child's **`interface` names** — a synthesized **boundary face**, one port
per name, each carrying the **entry's declared `Arg` type** (ADR-0038 §2, amending ADR-0034 §4's
inherit-from-the-inner-port rule: a pipe points at no inner port, so there is nothing to inherit
from and the entry declares its type itself; the ordinary pass-2 wire check covers boundary wires,
and errors speak in boundary terms — the subpatch address and external name). Wire or set literals
on those names exactly as on operator ports: `"tone": 2500` validates against the pipe's declared
type and range. Inlined this way, a child pipe's `channel` binding is **inert** (ADR-0038 §3): the
host's wiring feeds the pipe through the face; an unwired nested pipe falls back to its declared
default (silence for a bare signal pipe) with a `LoadWarning` — a nest never reaches the hardware
on its own.

A pipe entry carries its **quantity contract** alongside the declared type — for numeric
pipes the engine-enforced `default`/`min`/`max`/`curve` plus a display `unit` (ADR-0038 §2 as
amended by [ADR-0043](../adr/0043-surface-docs-decouple-presentation-from-instruments.md):
the entry owns this metadata outright, and `unit`/`curve` describe the *quantity*, so every
surface of the instrument inherits them). Presentation — `label`, `widget`, grouping, order —
lives apart in a **surface doc** (`surfaces/<name>.json`, schema
`surfaces/surface.schema.json`) that binds pipes by name; the `control-surface` skill authors
it, and the TouchOSC emitter and any host-side renderer read from it. The declared `type` is what flows
(see `instruments/patches/space.json`):

```json
"interface": {
  "inputs": {
    "in":   { "type": "f32_buffer" },
    "tone": { "type": "f32_buffer", "default": 4000.0, "min": 20.0, "max": 20000.0,
              "curve": "exp", "unit": "Hz" }
  },
  "outputs": { "out": { "from": "/verb.audio" } }
}
```

`reuben describe <patch.json>` prints the boundary a host wires against — each pipe with its
declared type, range, default, and unit.
`instruments/patches/space.json` (nestable effect) + `instruments/mic-space.json` (its host,
behind a live-input pipe) are the worked pair.

The per-node **`control`** block ([ADR-0018](../adr/0018-control-surface-generation.md)) is
**retired** ([ADR-0043](../adr/0043-surface-docs-decouple-presentation-from-instruments.md)):
a v2 document (or a v3 one still carrying leftovers) parses, but the block is dropped with a
`LoadWarning::DeprecatedControlBlock` — the engine never read it, so sound is unchanged, and
re-saving strips it. Player-facing controls are **interface input pipes** now; their
presentation lives in a surface doc read by the
[`control-surface` skill](../../.claude/skills/control-surface/SKILL.md) and by any host-side
renderer (`instruments/groovebox.json` + `surfaces/groovebox.json` are the worked pair).

## Instrument reuse: the recipe-role and the library index ([ADR-0057](../adr/0057-instrument-reuse-interface-makes-the-role.md)) <!-- lanes: skills,mcp,web -->

There is one noun — **instrument**. A "recipe" is not a second kind of file: it is a role an
instrument plays *while referenced* by another via `subpatch` (above) — same format, same
loader, same `validate`, nothing extra to keep true.

- **The recipe-role lives in the document itself.** The first sentence of an instrument's
  top-level `doc` field is **trusted for selection only** — the `interface` block is always
  the mechanically-enforced face; no consumer may take wiring facts from prose.
- **Discovery is a generated index, not a curated list.** Check `instruments/index.md` for a
  close-enough child before drafting a chain from scratch, and reference it by id through a
  `subpatch` node rather than re-authoring its shape inline. Fetch the full document only when
  a role line seems off — reference id + face is the whole contract; no internals are needed
  to reuse one.

## Recipe authoring: canonical naming, non-nestable idioms, and index regeneration <!-- lanes: skills,mcp -->

Depth for whoever drafts or maintains a reusable recipe rather than just referencing one — the
web chat lane consumes seeds via the essentials above and doesn't author them, so this detail
stays checkout-side.

- **The recipe-role lives in the document itself, in full.** The first sentence of an
  instrument's top-level `doc` field states what it is and when to reach for it, in the domain
  language — authored once, at creation, then kept true by re-emission (the loop-conduct
  bullet above). It is trusted for selection only: the `interface` block is always the
  mechanically-enforced face (pipe names, `Arg` types, defaults, outputs) — no consumer may
  take wiring facts from prose.
- **Canonical pipe naming** is a recipe-authoring guideline
  ([ADR-0058](../adr/0058-intent-vocabulary-word-to-move-table.md) §2): give a reusable
  instrument's face pipes the same names the intent vocabulary's moves target (`cutoff`,
  `tone`, `decay`, `drive`, …) wherever the pipe proxies that move, so the vocabulary's
  type-keyed rows transfer to the face by name instead of needing a second,
  instrument-specific mapping.
- **Not every idiom is a library entry.** Clock scaffolds and poly scaffolds compose *around*
  a voice rather than presenting a reusable face, so they stay prompt-side material, never a
  library entry (ADR-0057 §1) — there is no index line for them, and none is coming. The
  worked self-playing idiom: add a `clock` + `sequencer` feeding the voicer, one sequencer per
  voice on a shared clock, as in `instruments/groovebox.json`.
- **The index is generated, never hand-kept.** `instruments/index.md` is one signature line
  per instrument in the available-set — name, role line, face signature (`(inputs) →
  outputs`) — projected mechanically from the documents themselves; regenerate with
  `cargo run -p reuben-native --example gen_library_index`. A staleness test fails the build if
  it drifts, so it is never hand-kept.

## The sample workflow: "use this sample" is a filesystem gesture <!-- lanes: skills,mcp -->

No resource bytes cross the tool surface — there is no upload tool, by decision
([ADR-0049](../adr/0049-no-resource-bytes-over-mcp.md)). The agent handles the bytes itself:

1. **Write the bytes yourself, next to the instrument.** Copy, move, or synthesize the WAV
   **sibling to the instrument document** with your own file tools. Sibling-first resolution
   ([ADR-0036](../adr/0036-instrument-library-and-format-versioning.md)) makes
   next-to-the-document the blessed location.
2. **Reference it by logical id + relative path.** Add a `resources` entry mapping a logical
   id to the file's path relative to the document (`"resources": { "pluck": "pluck.wav" }`),
   and point the node at the id through its `sample` field. Resolution semantics — relative
   to the naming document, library-root fallback — are in the `resources` paragraph above.
3. **Missing = silence + a node-localized warning.** A missing or unreadable resource is
   never fatal: the node degrades to silence and `validate` (which stats the file) reports a
   warning naming the node — a wrong path is diagnosed from the report, not by ear. An
   undecodable file surfaces the same way in the swap report as the dark-degrade warning:
   announced, not discovered.

## "Audio vs control" is boundary metadata, not a type <!-- lanes: skills,mcp,web -->

Collapsing audio, CV, and control into one `f32_buffer` Signal means the engine treats every
`f32_buffer` alike. The authoring *intent* — "this is an audio/CV cable" vs "this is a control knob" —
that the surface resolvers and patcher care about lives at the **graph boundary** (a knob is an
interface input pipe with a declared range and default; a surface doc binds it to a widget),
never as a runtime type.

## Addressing <!-- lanes: skills,mcp,web -->

Every node has an OSC **address**, derived from graph structure by default. A Message targets a
node by address prefix and an **input port by name** — always addressed explicitly as
`/<node>/<input>` (ADR-0030 routes by port name; there is no whole-node sugar). An `F32` control
input takes a scalar (`/filt/cutoff 1500`). An enum input takes a **symbol** — its variant name
(`/filt/mode "Hp"`; the JSON literal `"mode": "Hp"` is the same form) — with a bare in-range
integer index accepted as a fallback; an unknown symbol or out-of-range index is an **error**,
never a silent snap to the default. A `Note` input takes its args (`/voicer/notes [69.0, 1.0]`).
Full wildcard dispatch (`/drums/*/decay`) is designed but not built — today a Message targets at
most one node ([ADR-0005](../adr/0005-osc-namespace-and-wildcards.md)).

## Invariants you must not break <!-- lanes: skills,mcp,web -->

- **Determinism** — output is bit-identical regardless of executor or thread interleaving
  ([ADR-0001](../adr/0001-unified-block-graph-execution.md)). No wall-clock, no RNG without
  a seeded, plan-owned source. **Live audio input is the one sanctioned nondeterministic
  boundary** ([ADR-0038](../adr/0038-interface-pipes-and-the-device-layer.md) §10, the same
  category as OSC-in): a patch with no input pipes gains no new nondeterminism, and offline
  render / `OpDriver` injects known buffers into input pipes, so injected-input renders stay
  bit-reproducible.
- <a id="rt-safe-render"></a>**RT-safe Render** — code that runs on the audio render
  thread(s) — the **hot** path — never allocates, locks, blocks, or panics; `render_block` is
  allocation-free after warmup, asserted by `crates/reuben-core/tests/rt_safe.rs`. How this
  binds an *operator author* — the hot/cold boundary, hot-path totality, the preallocation
  idioms — lives in [operator-dev.md](operator-dev.md#rt-safe-render).
- **OSC-only core** — the core speaks only OSC-shaped Messages. MIDI, Ableton Link, tempo
  sync, etc. are removable boundary adapters that convert to/from OSC in the native layer
  ([ADR-0007](../adr/0007-osc-only-core.md)).
- **Single-writer boundary** — the Coordinator is the only writer of graph structure;
  Render only ever reads an immutable Plan
  ([ADR-0012](../adr/0012-boundary-and-threading.md)).

## ADR index <!-- lanes: skills,mcp -->

The decisions and reasoning behind all of the above live in [docs/adr/](../adr/) — start
there when a contract's *why* is unclear.
[ADR-0030](../adr/0030-osc-as-all-data-one-message-type.md) is the one-`Message`/`Arg` data model
this doc is built on (superseding the ADR-0028 shape model).
