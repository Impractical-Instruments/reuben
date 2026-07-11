# ADR-0038: Interface pipes, logical channel binding, and the device layer

> **Amended by [ADR-0043](0043-surface-docs-decouple-presentation-from-instruments.md).**
> §2's "presentation metadata (label/widget/…) lives on entries" is narrowed: pipes keep the
> quantity contract (`type`/`default`/`min`/`max`/`curve`/`unit`); `label`/`widget` move to
> decoupled surface docs (format v3).

## Status

Accepted (2026-07-04). Resolved in a grilling session — the design gate of the explicit
mono-signal ↔ hardware channel I/O epic
([#185](https://github.com/Impractical-Instruments/reuben/issues/185); this record is P1,
[#178](https://github.com/Impractical-Instruments/reuben/issues/178)). Like
[ADR-0034](0034-instrument-nesting.md), this ADR produces semantics only; no engine/loader
code lands here (that is P2–P7:
[#179](https://github.com/Impractical-Instruments/reuben/issues/179)/[#180](https://github.com/Impractical-Instruments/reuben/issues/180)/[#181](https://github.com/Impractical-Instruments/reuben/issues/181)/[#182](https://github.com/Impractical-Instruments/reuben/issues/182)/[#183](https://github.com/Impractical-Instruments/reuben/issues/183)/[#184](https://github.com/Impractical-Instruments/reuben/issues/184)).

**Supersedes** the open design threads of
[#55](https://github.com/Impractical-Instruments/reuben/issues/55) (audio/device layer:
n-channel, negotiation, xrun policy) and
[#174](https://github.com/Impractical-Instruments/reuben/issues/174) (audio inputs into
(sub)graphs) — every question both carried is answered below.
**Amends** [ADR-0026](0026-v1-finish-line-osc-out-and-stereo.md) (the top-level anonymous
`outputs` block dissolves into `interface.outputs`; the implicit logical→device policy becomes
explicitly overridable via a device profile) and [ADR-0034](0034-instrument-nesting.md) §4
(interface entries declare their `Arg` type instead of inheriting it from an inner port).
Rides on [ADR-0036](0036-instrument-library-and-format-versioning.md) §4's `format_version`
machinery (this is the first breaking bump: v2).

## Context

reuben's channel handling is output-only and half-implicit. Patches tap *logical* master
channels ([ADR-0026](0026-v1-finish-line-osc-out-and-stereo.md): a tap's optional `channel`
index; omitted = broadcast), and `audio.rs`'s `map_frame` silently maps logical→device
(interleave / mono-downmix / zero-fill) with no way to say "logical 1 goes to device
channel 3". Audio **input does not exist at all** — no input stream, no input master, no way
for a patch to name an input ([#174](https://github.com/Impractical-Instruments/reuben/issues/174)'s
inventory). [#55](https://github.com/Impractical-Instruments/reuben/issues/55) additionally
proposed an N-channel Signal, which would touch every operator's `process` and every buffer.

Meanwhile the boundary story grew a second seam. [ADR-0032](0032-voicer-hosts-voice-subpatches.md)/[ADR-0034](0034-instrument-nesting.md)
gave graphs an `interface` block whose entries *point at* inner ports (`"in": "/filter.audio"`,
`"tone": {"target": "/filter.cutoff"}`), while the top level kept its own anonymous `outputs`
array from [ADR-0026](0026-v1-finish-line-osc-out-and-stereo.md). So "how audio leaves a
graph" had two spellings, and "how audio enters one" had none.

The grilling resolved the whole tree at once: what a boundary entry *is*, how it binds to
hardware, where device knowledge lives, what the clock anchor is, and what happens when
reality (devices, rates, deadlines) disagrees with the patch.

## Decision

### 1. The Signal stays mono, permanently

The multi-channel Signal — [#55](https://github.com/Impractical-Instruments/reuben/issues/55)'s
headline, "touching every operator" — is **rejected**, not re-deferred. N-channel I/O is
**N mono pipes with explicit channel bindings** (§3). One channel per edge is the modular-synth
precedent [ADR-0026](0026-v1-finish-line-osc-out-and-stereo.md) already cites, and it is what
keeps all existing operators' `process` untouched forever: width lives at the graph boundary,
never inside the Signal.

**Considered and rejected:**

- **N-channel Signal.** Every operator, every buffer, every arena allocation grows a channel
  dimension; per-op channel semantics (does a filter share state across channels?) multiply
  the contract surface; and the payoff — fewer edges in a quad patch — is authoring sugar the
  pipe model already provides legibly. Rejected on the merits, permanently, so the question
  stops re-opening.

### 2. One boundary concept at every level: interface pipes (the direction flip)

`interface.inputs` / `interface.outputs` entries become **named pipes** — the *single*
boundary mechanism at every graph level — with the wiring direction **flipped** relative to
today's target-pointing entries:

- An interface **input** is a named pipe that **mints an address in the flat node namespace**
  (entry `in` → address `/in`; a post-mint collision is the existing fatal
  `DuplicateAddress`). Internal nodes consume it with ordinary wire-refs:
  `{"from": "/in"}`. Fan-out is free — the pipe behaves like a source node.
- An interface **output** is a named pipe **fed from an internal port**:
  `"main_l": {"from": "/pan.left", "channel": 0}`.
- **Everything flips — message/control entries too.** space.json's
  `"tone": {"target": "/filter.cutoff"}` becomes a pipe the filter consumes:
  the filter's `"cutoff": {"from": "/tone"}`. One direction rule for the whole block; no
  entry points inward anymore.
- Because an entry no longer points at an inner port, there is nothing to inherit a type
  from: **the entry declares its `Arg` type**, alongside the presentation metadata
  (label/widget/min/max…) that already lives on entries. This **amends
  [ADR-0034](0034-instrument-nesting.md) §4** ("type inherited from the inner port and
  locked"). The declared type is enforced by the **existing pass-2 wire check** against every
  consumer wire — no new checker, exactly the §5 discipline of ADR-0034. Subpatch face
  synthesis reads the declared type.

**Considered and rejected:**

- **A new top-level `inputs` block separate from `interface`.** Mints a third boundary
  concept next to the two we're consolidating; every consumer (subpatch face synthesis,
  describe, control-surface generation) would have to read both. The `interface` block is
  already the boundary — inputs belong in it.
- **Target-pointing entries retained** (inputs keep `{"target": ...}`, only hardware I/O
  gets pipes). Keeps two wiring directions alive in one block, and a target-pointing input
  cannot fan out without a distribution node — the pipe form gets fan-out for free by being
  a source in the namespace. The flip is breaking either way we extend it; better to land on
  the one-rule form once (§5 pays the migration).

### 3. Hardware binding: an optional logical `channel` on signal pipes, honored only at top level

A **signal** pipe (input or output) may carry an optional **`channel: <int>`** — a *logical*
channel binding, [ADR-0026](0026-v1-finish-line-osc-out-and-stereo.md)'s indexed-not-named
convention, now on both sides:

- Honored **only on the graph actually played at top level**: an input pipe with
  `channel: k` reads logical input channel `k`; an output pipe with `channel: k` feeds
  logical output channel `k`.
- **Nested** (subpatch-inlined, ADR-0034) or **Voicer-hosted** (ADR-0032), the binding is
  **inert** — the parent's edge feeds the pipe through the synthesized face exactly as any
  boundary wire. An unwired nested/hosted pipe renders **silence + `LoadWarning`**. *Inputs
  of a graph are pipes — never a magic hardware connection from inside a nest.*
- `channel` on a **message** pipe is a **load error** (hardware channels carry signals).
- A channel-bound pipe **keeps its declared `default` as the unfed fallback**: while no
  input stream supplies the channel, the pipe materializes its default and stays
  message-drivable — exactly the control `describe` advertises (a bare pipe's fallback is
  silence). When the channel is supplied, device audio drives the pipe for the block.
- A logical `channel` index is **bounded** (4096, both sides): the derived width sizes
  real per-channel buffers, so past the bound the document is structurally broken — a
  load error, not a degrade (§7 degrades reality mismatches, not broken documents).
- Omitted `channel` on a top-level **output** pipe keeps today's broadcast-to-all-logical-
  channels meaning ([ADR-0026](0026-v1-finish-line-osc-out-and-stereo.md), unchanged).
  Logical widths derive from the played top graph: output width = max bound output channel
  + 1 (floor 2, as today); input width = max bound input channel + 1 (0 if none — a patch
  that uses no inputs pays nothing).

**Considered and rejected:**

- **Nested hardware fallback binding** ("if nobody wires the nested pipe, its `channel`
  reaches through to hardware"). Action-at-a-distance: whether a nest touches the mic would
  depend on the host's wiring omissions. Silence + warning is honest; the host that wants
  the mic wires its own top-level pipe in.
- **Patch carries device channels** (bind pipes straight to hardware channel numbers).
  Destroys device portability — the same patch should play on any rig; the logical/device
  split is the whole point of §6's profile.

### 4. The top-level anonymous `outputs` block dissolves into `interface.outputs`

Full input/output symmetry: the anonymous top-level `outputs` array
([ADR-0026](0026-v1-finish-line-osc-out-and-stereo.md)) is **v1-only**. Migration (§5) moves
its entries into `interface.outputs` under generated names. Master-tap plumbing
(`Graph::outputs`, `tap_output_channel`) is fed from the consolidated block. One boundary
block describes everything that crosses a graph's edge, at every level.

### 5. Format v2, with in-loader auto-migration

The direction flip is a breaking shape change, so this is the first
[ADR-0036](0036-instrument-library-and-format-versioning.md) §4 version bump:
**`format_version: 2`**. Per 0036's bump rules the loader ships a parse-time migration —
v1 documents keep loading forever; only the future is unreadable:

- `target`-form interface entries → pipe + consumer wire-ref (mechanical rewrite; the
  entry's declared type is derived from the old target port during migration);
- anonymous top-level `outputs` → named `interface.outputs` entries (generated names);
- save writes v2 (0036: a migrated document never saves back under its old number).

All shipped documents (`instruments/`, `instruments/patches/`, `instruments/voices/`) are
rewritten to native v2, and the schema is regenerated. **Migrated and rewritten instruments
render bit-identically — asserted in tests** (the ADR-0026 discipline).

**Scope note — the one accepted divergence (decided 2026-07-04 with the repo owner, during
the [#189](https://github.com/Impractical-Instruments/reuben/pull/189) review).** §4's
unification means a channel-less **signal** `interface.outputs` pipe at top level *is* a
broadcast master tap; v1 kept boundary outputs and master taps separate, so a v1
**boundary-only** signal output — declared in `interface.outputs` but never anonymously
tapped — made no top-level sound. The consolidated block has no way to spell "signal boundary
output, not a tap", and the unification **stands** (no opt-out marker is added). Migration
keeps such entries: hosted/nested behavior — the position they were authored for — stays
bit-identical, and the entry becomes **audible when the migrated document is played at top
level**, by design. The divergence is never silent: migration emits a `LoadWarning` naming
each such entry and the consequence; deleting the entry from the migrated document retires
both the tap and the warning. Every other v1 shape keeps the full bit-identical guarantee
(anonymous taps reproduce exactly, multiplicity and channels included).

### 6. Logical↔device mapping lives outside the patch: the device profile (`--io-map`)

Patches speak *logical* channels only; a small **device-profile JSON**, loaded with
**`--io-map <file>`** on `play`, binds them to a rig:

- `output.map` — logical→device channel pairs (e.g. `{"0": 2, "1": 3}`);
- `input.map` — device→logical pairs;
- `output.device` / `input.device` — device selection by name substring (today: default
  device only);
- `sample_rate` / `buffer_size` — **preferences**: requested against the device's supported
  configs; the engine **adopts what the device grants** and logs the outcome. Request →
  grant → adopt discharges [#55](https://github.com/Impractical-Instruments/reuben/issues/55)'s
  "negotiation" — reuben never fights the device.

**No profile (or fields omitted) → identity map + today's implicit policy** (broadcast /
mono-downmix / zero-fill), bit-identical for existing instruments (asserted). The profile
makes [ADR-0026](0026-v1-finish-line-osc-out-and-stereo.md)'s "audio.rs owns the
logical→device mapping" *explicitly overridable* without patches ever learning device
geography.

**Considered and rejected:**

- **Mapping in the patch** — see §3 (portability).
- **Mapping via CLI flags only** (`--out-map 0:2,1:3`). A rig description is a thing you
  keep, name, and check in; flags don't compose across the six fields above and rot into
  shell history. The file is the artifact; the flag just points at it.

### 7. Mismatch policy: warn + zeros (dark-degrade), never fatal

Matching the missing-resource philosophy ([ADR-0016](0016-sample-player-and-resource-store.md)):
a logical input channel the device/map can't supply renders **silence with a startup
warning**; an unmappable logical output channel is **dropped with a warning**. A patch never
fails to play because the rig is smaller than the author's studio. (Structural errors in the
profile itself — malformed JSON, `channel` on a message pipe — remain load errors; degrade is
for *reality* mismatches, not broken documents.)

P5 ([#182](https://github.com/Impractical-Instruments/reuben/issues/182)) records one
deliberate carve-out: an instrument that explicitly binds input channels **fails fast** at
`play` when no input device exists at all (or none matches the profile's `input.device`) —
consistent with the output side's no-device precedent; dark-degrade covers mismatches on a
device that opened, not the absence of any device.

### 8. Clock anchor: the output device rate; input is resampled from day one

The engine stays hosted **in the output callback**, rendering at the output device's rate,
exactly as today — zero added output latency, output stays bit-reproducible. The input
stream crosses a thread boundary via a **lock-free SPSC ring** (the primitive
[ADR-0002](0002-rust-core.md) anticipated) and is **resampled / drift-compensated into the
engine rate from day one**. Mismatched-rate and dual-device (separate in/out devices) input
work in the first implementation slice; there is **no same-device-only MVP**. Resampler
choice is the implementer's (RT-safe, alloc-free in `process`; quality can start modest) —
recorded in the P5 PR.

**Considered and rejected:**

- **Fixed engine rate with output resampling.** Adds a resampler (latency + coloration) to
  the *output* path every patch pays for, to buy a constant the determinism story doesn't
  need — offline render already pins its own rate.
- **Same-device no-resample MVP.** Ships the illusion that input is easy, then the first
  USB mic (own clock, even at "the same" nominal rate) drifts into clicks. The ring +
  resampler *is* the feature; deferring it defers the feature.

### 9. Xrun/ring policy: fixed and observable, not configurable

- Input ring **empty → zeros**; ring **full → drop oldest**; output render **deadline miss →
  the device's own silence**, detected and counted.
- All three are surfaced as **counters through one shared diagnostics surface** (P6,
  [#183](https://github.com/Impractical-Instruments/reuben/issues/183)): periodic/exit
  logging now; an OSC diagnostic endpoint explicitly later. *(Amended by
  [ADR-0047](0047-mcp-tool-surface-and-contracts.md) §6: the deferred endpoint is the MCP
  `get_diagnostics` tool over the structure channel, not OSC.)*
- **No configurability and no recovery mode** (no block-skipping, no degraded render).
  reuben's job on an xrun is to *know and say*, not to improvise. Configurability only if a
  real need appears — recorded here so the temptation has to argue with an ADR.

### 10. Determinism carve-out: live input is a sanctioned nondeterministic boundary

Live audio input joins OSC-in ([ADR-0010](0010-single-lane-operators.md)/[ADR-0012](0012-boundary-and-threading.md))
as a **sanctioned nondeterministic boundary**: same graph + same control input no longer
implies same output *when a live device feeds an input pipe* — that is the point of a mic.
The **OpDriver/offline path injects known buffers into input pipes**, so offline render with
injected input is bit-reproducible, and all existing output-determinism tests are unchanged
(a patch with no input pipes has no new nondeterminism).

### 11. One ADR

The author-facing format (pipes, flip, channel binding, v2) and the device/RT layer
(profile, ring, resampling, xrun policy) are **one decision set** — each half constrains the
other (pipes are only honest if the device layer honors logical channels; the device layer
only stays out of patches because pipes carry the binding). One record, this one, so future
readers get the contract whole.

## Consequences — touch-point map for P2–P7

- **P2 (format v2, [#179](https://github.com/Impractical-Instruments/reuben/issues/179)):**
  `format.rs` (`InterfaceDoc`/`InterfaceEntry` → pipe form, declared types, `channel`;
  address minting + `DuplicateAddress`; v1→v2 migration in `from_json`; anonymous `outputs`
  dissolution feeding `Graph::outputs`/`tap_output_channel`), subpatch face synthesis +
  `PatchBoundary`/`describe`/`validate`, schema regen, all shipped instruments rewritten,
  bit-identical render tests. Pure core/loader — input pipes are valid format that renders
  silence until P3.
- **P3 (core input master, [#180](https://github.com/Impractical-Instruments/reuben/issues/180)):**
  `config.rs` (`AudioConfig` input channel count), `render.rs`/`plan.rs`/`engine.rs`
  (`render_block` input parameter, `fill` scratch — the dual of ADR-0026's 1→N output
  change), top-level pipe→channel binding (fan-out at the master allowed; unbound/unsupplied
  → zeros + warning), nested-inert rule enforcement, `op_driver.rs` injection seam +
  determinism tests. No allocation in the render path (scratch at plan build).
- **P4 (device profile, [#181](https://github.com/Impractical-Instruments/reuben/issues/181)):**
  reuben-native: profile parse/validate, `--io-map` on `play`, output map applied in
  `audio.rs` (`map_frame` override), device selection, rate/buffer request-grant-adopt,
  no-profile bit-identical test. Input half parse+validate only. Parallel with P2/P3.
- **P5 (input stream, [#182](https://github.com/Impractical-Instruments/reuben/issues/182)):**
  reuben-native: `cpal::build_input_stream` (opened only when the played patch binds input
  channels), SPSC ring, resampler + drift compensation, device→logical input map (the
  `map_frame` dual), `AudioError` input variants, counters into P6's surface, manual smoke
  (headless CI can't prove device I/O — ADR-0026's Windows precedent).
- **P6 (xrun observability, [#183](https://github.com/Impractical-Instruments/reuben/issues/183)):**
  render-deadline measurement in the output callback (atomic counters, no
  syscalls/allocation), the shared diagnostics counter surface P5 also feeds;
  order-independent with P2–P5.
- **P7 (demos/docs, [#184](https://github.com/Impractical-Instruments/reuben/issues/184)):**
  live-input + multichannel-out demo rigs with an example profile, ARCHITECTURE/README/
  authoring.md currency pass, schema/describe/control-surface verification,
  ROADMAP/OPEN-QUESTIONS strike the superseded #55/#174 threads.
- **Terminology:** *interface pipe* = a named boundary entry that mints (input) or is fed
  from (output) an address in the flat namespace; *logical channel* = the device-independent
  index a pipe binds; *device profile* = the rig-side JSON binding logical channels to a
  device; *dark-degrade* = warn + zeros/drop, never fatal.
