# Voicer hosts voice sub-patches; a voice is a standalone instrument referenced by path

## Status

Accepted (2026-06-27). Resolved in a grilling session. Depends on the substrate of
[ADR-0031](0031-float-resolves-to-value-or-signal-by-wiring.md) (Value/Signal port forms, the
per-wire form check, and the change-frame block-slicing). Realizes part of
[ADR-0003](0003-recursive-composition.md)'s "an Instrument is a reusable subgraph with boundary
ports" — but **diverges** from 0003's "nested graphs are inlined at plan-build time, zero runtime
cost": polyphony needs voices that come and go at runtime, so Voicer **hosts** sub-plans and renders
the active ones each block rather than inlining them (see [Decision](#decision) §4 and
[Considered and rejected](#considered-and-rejected)).

Supersedes the **withdrawn** "stub Voicer silent, flip the per-Voice ports anyway, restore polyphony
later under `#99`" ruling (grilling session 4) recorded in
[the 0031 TDD plan](0031-tdd-plan.md). That path deleted/neutered Voicer + `chord_player` +
`tonal_context` + `first_sound` tests for a deferred fix; this ADR does the real rewrite instead.

## Context

Today `Voicer` is the engine's **only** fan-out operator. Its contract declares
`lanes: from_param(voices)`; at instantiate the planner replicates the *entire downstream operator
chain* once per Voice (Lane), and the render loop runs every node once per Lane
(`render.rs` Lane loop). Voicer emits per-Lane `freq`/`gate` **buffers**; downstream
`oscillator.freq`/`sample.freq`/`envelope.gate`/`sample.gate` consume them per Lane.

ADR-0031 wants every gate/trigger/pitch control to be a held **Value** (`f32`), not a per-sample
buffer (`f32_buffer`). The Lane mechanism blocks that for the per-Voice ports, because of two engine
facts:

- **Emission is Lane-0 only** (`render.rs:661`: `if lane == 0 { io.with_emit(..) }`) — a `MsgWriter`
  write from Voice > 0 has no sink.
- **The Value latch is node-global** (`render.rs:608`: one `node.latch[port]`, shared across Lanes)
  — a Value `freq`/`gate` would broadcast one Voice's value to all Voices, collapsing polyphony.

So under the Lane model, making the per-Voice ports Value **breaks polyphony**. ADR-0031 session 4
proposed eating that (stub Voicer silent, defer the fix) — a wide blast radius (deleted/neutered the
whole Voicer test neighbourhood) for a deferred fix, and it would have left `f32_buffer` forced on
the per-Voice spine in the interim. We reject that.

The root cause is the **opaque per-Lane fan-out**: per-Voice state lives in anonymous Lane replicas
the engine spreads across the whole downstream graph, with a node-global latch and Lane-0-only
emission. Remove the fan-out and the constraint disappears.

## Decision

A **Voice is a standalone Instrument patch**, referenced by path, with a declared I/O boundary.
**Voicer hosts N of them**: it instantiates the voice patch `voices` times at its own instantiate,
does note allocation across the instances, drives each with held Values, renders the active ones, and
outputs their summed audio. There is no Lane fan-out.

### 1. The voice patch's `interface` boundary

A voice patch is an ordinary instrument document in every respect except that it declares a named,
**engine-honored** I/O boundary via a new top-level `interface` block:

```json
"interface": {
  "inputs":  { "freq": "<node>/<port>", "gate": "<node>/<port>" },
  "outputs": { "audio": "<node>/<port>", "active": "<node>/<port>" }
}
```

- **in `freq`** — `f32` Value, the resolved frequency in Hz of the note this voice currently holds.
- **in `gate`** — `f32` Value, `1.0` while the voice holds a note, else `0.0`.
- **out `audio`** — `f32_buffer` Signal, the voice's rendered sound.
- **out `active`** — `f32` Value, `1.0` while the voice is still producing sound (through the release
  tail), `0.0` once fully idle. Lets Voicer know a voice is truly finished, not merely gate-off.

`interface` is **distinct from `control`** (ADR-0018): `control` is opaque, engine-*ignored*
control-surface-generation metadata pointing at a player-facing param; `interface` is real wiring the
engine binds and type-checks. The shape is deliberately modeled on `control`'s ergonomics (external
name → internal `node/port`), and the existing `outputs: [PortRef]` tap list is the output half
generalized to carry names. A voice patch may carry **both**: `interface` for Voicer, `control` so it
still loads + plays standalone on a surface for testing.

Inside, the voice is single-Lane, so `freq`/`gate` are ordinary Value inputs and `active` an ordinary
Value output — no broadcast problem, no Lane-0 emission problem. The canonical `active` source is the
amp **`envelope`, which grows an `active` output** (true attack-through-release, false when idle);
Voicer is agnostic to how `active` is computed and only reads the port, so a patch with longer tails
can drive it from a silence-detector node instead.

### 2. The voice patch is loaded as an instrument-kind resource

The path reference reuses the **resource pipeline** (ADR-0016), not a bespoke field. Voicer declares
a resource slot; the instrument JSON maps an id → patch path in the existing `resources` table; the
loader's resolver gains a new **instrument-resource** kind that resolves a path into a built
sub-`Graph` (recursively — a voice patch may itself pull `sample` resources; the resolver already
dedups nested refs). `bind_resources` hands Voicer the sub-Graph. This inherits path resolution,
dedup, nested-resource handling, and the "bound at load, never on the hot path" guarantee. The only
net-new piece is "a resource that is a Graph, not bytes."

### 3. Voicer instantiates N voices at its own instantiate

Given the bound sub-Graph, Voicer instantiates the voice **`voices` times** at Voicer-instantiate
(load time), each into its own `Plan` **plus its own pre-allocated arena** — `Vec<VoiceSlot>` where
`VoiceSlot = { plan, arena, active }`. All N exist up front (memory bounded by `voices` ≤ 32);
allocation off the hot path keeps RT-safety (ADR-0012). `lanes: from_param(voices)` is **removed**;
Voicer becomes a normal single-Lane operator.

### 4. Re-entrant render: `render(plan, arena, frames)` as a free function

The renderer is refactored from a method assuming the one global arena into a reusable **free
function over `(plan, arena, frames)`**. The top level renders the rig with the rig arena; Voicer
calls the *same* function per active voice with that voice's arena. Re-entrancy comes from render
being a pure function of `(plan, arena)`, not nested mutable renderer state.

Per block, `voicer.process()`:

1. Run note allocation (assign / steal) over the incoming `notes`, resolving Degrees through
   `harmony` exactly as today — Voicer keeps the musical brain.
2. For each voice, write its `freq`/`gate` into that voice plan's input latches (mapped via the
   voice's `interface.inputs`) as a **sparse change-list within the block** — the edges at their
   exact frames, not one held value.
3. `render(voice.plan, voice.arena, n)` for each **active** voice (idle voices are skipped). The
   sub-render **block-slices at the freq/gate change frames** — the same change-frame slicing
   ADR-0031 builds for the top-level plan — so note-ons stay **sample-accurate** with no new
   machinery and no 128-frame quantization jitter.
4. Accumulate each active voice's `interface.outputs.audio` into Voicer's audio output, in **fixed
   voice-index order** (determinism). Read each voice's `active` to update its liveness.

### 5. Allocation keys on `active`, not gate

The free pool is voices with **`active == 0.0`** (fully released), not merely gate-off. Assign prefers
a truly-idle voice; Voicer only **steals** (the oldest `active` voice) when all N are still sounding.
A steal writes a new `freq` + a fresh gate rising-edge into an already-active voice, and its envelope
retriggers naturally. This is strictly better than today's gate-based pool — a voice mid-release-tail
is no longer stolen while an idle one exists.

### 6. Output

Voicer outputs a single summed **audio** Signal (the active voices mixed). It no longer outputs
`freq`/`gate` ports at all — those now live *inside* each voice sub-patch.

### Sequencing relative to ADR-0031

The rewrite and 0031's Phase-B flip are entangled: the rewrite needs Value `freq`/`gate` +
change-frame slicing (0031 machinery), and the flip is what breaks the old Lane-fan-out polyphony.
On the 0031 branch, in order:

1. 0031 gate/CV **mono** migration + value-math (`envelope`/`sample`/`euclid`/`clock` → held
   block-sliced edge-detect; author `*_f32_value`). Mono-correct already.
2. Flip `port_kind` `F32 ⇒ Value` (the 0031 barrier). Polyphony is **transiently broken on-branch**
   (Lane fan-out + Value = broadcast) — but this is a private branch, not `main`.
3. **This ADR's rewrite** restores polyphony: re-entrant render, per-voice arenas, `interface`,
   instrument-resource, sub-plan host; delete the Lane fan-out.
4. Merge. `main` never ships a stub, broken polyphony, or an `f32_buffer`-everywhere compromise.

## Considered and rejected

- **Stub Voicer silent, defer the rewrite (ADR-0031 grilling session 4).** Withdrawn: deleting/
  neutering the whole Voicer test neighbourhood for a deferred fix is a wider blast radius than the
  real rewrite, and it leaves `f32_buffer` forced on the per-Voice spine in the meantime.
- **Keep the Lane fan-out; make the latch and emission per-Lane (`#99` original).** Smallest engine
  change, and it would let the per-Voice ports be Value. But it keeps polyphony as an opaque,
  engine-spread Lane replication and delivers none of the "a voice is a reusable, standalone
  instrument patch" authoring win. Rejected in favour of the composition model.
- **Inline the voice subgraph N times at plan-build (ADR-0003's "zero runtime cost").** This is the
  right model for *static* nesting, but voices are *dynamic* — they come and go, idle ones must be
  skipped, and stealing reassigns them at runtime. A fixed build-time inline can't express "render
  only the active voices this block." So Voicer **hosts** at runtime instead. (Static instrument
  nesting can still adopt the 0003 inline path later; this ADR does not preclude it.)
- **Overload `control` for the boundary.** Rejected: `control` is engine-ignored, one-way,
  param-oriented UI metadata. A wiring boundary is engine-honored, bidirectional, and typed.
- **A dedicated `voice:` reference field instead of a resource.** Rejected: it would be a second
  path-resolution mechanism beside `resources`, without nested-resource dedup.
- **Quantize note-ons to the block boundary.** Rejected: ~2.7 ms jitter at 128 frames regresses the
  sample-accurate gate the engine currently guarantees (`voicer::gate_edge_is_sample_accurate`).

## Consequences

- **The Lane fan-out machinery is deleted.** `LaneRule::FromParam`, the per-Lane operator replication
  in `Plan::instantiate`, and the per-Lane render loop existed solely for Voicer. (`LaneRule::Inherit`
  collapses to single-Lane everywhere; confirm nothing else relied on Lane counts.)
- **ADR-0031 Phase B flips uniformly** — all gate/CV/pitch controls become `f32` Value with no
  per-Voice exception, no `f32_buffer`-everywhere compromise, and no silent stub.
- **The renderer becomes re-entrant** (`render(plan, arena, frames)` free fn), which is also the
  primitive ADR-0003 recursive composition needs.
- **New format surface:** the `interface` block (engine-honored boundary ports) and an
  instrument-kind resource. Both need schema + loader + validation work.
- **`envelope` grows an `active` output.** Other tail-producing ops may follow if used inside a voice.
- **Per-instance state isolation is automatic** — each voice is its own `Plan`, so envelope phase,
  filter state, etc. are independent without the Lane-replica bookkeeping.
- **CPU profile is unchanged in the worst case** (N voices sounding = N sub-renders ≈ today's N Lane
  replicas) and **better when voices are idle** (skipped entirely; today's Lane replicas always run).
- **Instruments are re-authored:** the `voicer → osc.freq / env.gate / sample.*` wiring inside every
  polyphonic instrument (`default.json`, `sampler.json`, `chord-player.json`, …) is replaced by a
  voice-patch reference + the inner synth chain moving into that voice patch.
- **Nested polyphony** (a voice patch containing its own Voicer) falls out of the recursion but is
  **not a designed feature** — no guardrail unless a need appears.
</content>
</invoke>
