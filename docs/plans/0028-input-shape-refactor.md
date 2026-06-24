# Refactor plan — ADR-0028 "One `Input`, one axis: `shape`"

Implements [ADR-0028](../adr/0028-one-input-shape.md). Engine-wide breaking change: collapses
`Signal port / Message port / Context port / param / unwired-default` into one `Input` described
by a `shape` (`Float | Enum | Harmony | Note`) plus a `Constant` carve-out, and retires the
`Signal/Message/Context` carrier (`PortKind`).

The phases are dependency-ordered. Each ends at a **green gate** (named below) before the next
starts. Phases 0–3 are sequential; the operator sweep (Phase 2) and instrument migration (Phase
4) are the bulk and can fan out internally once the gate before them is green.

---

## Phase 0 — Engine core: shapes, materialize, the new `Io`

The foundation everything else compiles against. No operator behavior changes yet.

1. **`Shape`** enum in `descriptor.rs`: `Float | Enum | Harmony | Note`. `Constant` shapes:
   `Int | Enum`. Replace `PortKind` on `Port`/descriptor with `shape`. Add `Constant`
   (instantiate-time config) to `Descriptor` distinct from `inputs`/`outputs`.
2. **`Float` delivery — materialize.** Engine holds, per `Float` input, a latched current scalar.
   Add a materialize pass: held Floats → a scratch buffer filled from the latch, with mid-block
   message changes written at their frame; dense sources pass through; **cache** unchanged held
   values (refill only on change). Scratch lives in the Signal arena (ADR-0001). Carry a
   `varying: bool` per buffer.
3. **Block-slicing split.** Retain sub-block slicing for `Enum`/`Harmony`/`Note` reads; **remove**
   process re-slicing for `Float` param changes (materialize replaces it). This is the riskiest
   engine change — see Risks.
4. **`Io` accessors.** Add `signal(IN) -> &[f32]` (+ `varying`), `value(IN) -> f32`,
   `enum(IN) -> E`, `harmony(IN) -> Harmony`, `events() -> &[Event]`, `signal_mut(OUT)`,
   `set_value(OUT, x)`. Remove `input`/`param`/`context`/`publish_context` (fold the last into
   `Harmony` output via `set_*`). Keep `Harmony`'s `Copy` resolver struct (rename from `Context`).
5. **`Constant`/config plumbing.** Instantiate-time application of `config` values;
   `LaneRule::FromParam` reads a `Constant` (`voices`) instead of a param slot.

**Gate 0:** core compiles; a single hand-migrated operator (oscillator) processes correctly
through the new `Io`; `rt_safe` allocation check passes (materialize must not allocate on the
audio thread — pre-size scratch at instantiate).

## Phase 1 — Contract macro (ADR-0025)

6. **New `operator_contract!` surface.** `inputs`/`outputs` carry `name: shape { range, default,
   unit, curve }`; `config { name: enum { A, B } | int { ..range } }`. Generates `IN_*`/`OUT_*`
   consts, the `descriptor()` impl, and the `Enum` types. Single-source preserved.
7. **`Enum` ↔ OSC.** Define enum-by-name binding: `/filt/mode "Hp"` (symbol) resolves to the
   descriptor's variant; decide symbol-vs-index on the wire (recommend **symbol**, with int index
   accepted as fallback) and document it.

**Gate 1:** macro emits a valid descriptor for oscillator + filter; golden descriptor snapshot
machinery updated (not yet re-blessed wholesale).

## Phase 2 — Operator sweep

The bulk. One operator at a time, test-first (create-operator skill / TDD). Each: reclassify
every input/output to a `shape`, move structural values to `config`, rewrite `process` to the
single read path, update its tests.

8. **Inventory + reclassification table.** Enumerate every operator in `operators/`; for each,
   list each port/param → `{shape, Input|Constant, default}`. Land this table first so the sweep
   is mechanical. Known calls:
   - `oscillator`: `freq` → `Float` Input (one decl); `waveform` → `Enum` Input (live-switchable).
   - `filter`: `cutoff`/`resonance` → `Float`; `mode` → `Enum` Input.
   - `voicer`: `notes` → `Note`; `ctx` → `Harmony`; `voices` → **`Constant`**.
   - `clock`: `phase`/`gate` → `Float` outputs; `tempo`/`division` → `Float` Inputs (read via
     `io.value`, block-rate).
   - `context` op → publishes `Harmony`; rename type/terms.
   - `envelope`: `gate` → `Float` Input; `cv` → `Float` output.
   - `power`, `mul`, `add`, `map` → `Float` in/out.
9. **`m2s` → `slew`/`glide`.** Remove the carrier bridge; `snap` becomes the engine default
   (materialize). Reimplement `slew`/`smooth`/`glide` as `Float → Float` shaper ops.
10. **`differentiate`/`integrate`.** Reframe as per-sample `Float` calculus. Note in code/docs
    that event-rate gesture-velocity is now an explicit construction (sample-and-hold the
    derivative at events), not the default. Decide whether any current instrument needs it.

**Gate 2:** every operator compiles + unit tests green; no operator references `io.input`/`param`/
`PortKind`.

## Phase 3 — Patch format + loader

11. **New instrument JSON.** One `inputs` map per node (value = literal `0.4` **or** wire-ref
    `{ "from": "/node.port" }`); a `config` block; **remove** the top-level `connections` array.
    Decide default-output-port sugar (`"/osc"` ≡ `"/osc.<sole-output>"`); recommend allowing the
    short form only when the source has exactly one output.
12. **Loader + validation.** Resolve wire-refs to arena indices; apply literals as held
    Floats / enum values / constants; apply `config` at instantiate. Replace `PortKindMismatch`
    with **shape-mismatch** validation (`"audio": "Hp"` and `Float`→`Note` are errors) and a
    Constant-at-runtime error.

**Gate 3:** loader round-trips a hand-written new-format instrument; validation rejects the
illegal cases with clear errors.

## Phase 4 — Instrument migration + re-bless

13. **Migrate `instruments/*.json`** to `inputs`/`config`. `m2s`(snap) nodes drop;
    `m2s`(slew/smooth/glide) → the shaper op. Enum params → named (`"Hp"`).
14. **Re-bless** the golden descriptor snapshot; **regenerate** the instrument schema.

**Gate 4:** all bundled instruments load + pass integration tests + `rt_safe`; schema regenerated;
a human OSC walkthrough on one playable instrument (e.g. groovebox) confirms live `mode`/`waveform`
switching and audio-rate `cutoff` modulation.

## Phase 5 — Docs + tooling

15. **Docs:** `authoring.md` (the `Input { shape, default }` + `Constant` model, the two read
    views, cross-shape converters); mark ADR-0017 superseded-in-part, ADR-0011 / ADR-0015 amended;
    run the sync-docs currency pass.
16. **Tooling:** create-operator skill (new contract surface), patcher skill (new JSON), and the
    control-surface generator (ADR-0018) — wire the "audio vs control" intent as tooling metadata
    now that it's no longer a type.

**Gate 5:** docs sync clean; skills updated; schema in docs matches generated.

---

## Risks / watch items

- **Materialize replacing block-slicing (Phase 0.3)** is the load-bearing change. Verify
  sample-accuracy of a mid-block `Float` change via a regression test (param step at frame N →
  buffer reflects it at N). Verify no per-sample `process()` calls for audio-rate `Float`
  modulation (one `process` per block).
- **No audio-thread allocation.** Materialize scratch must be pre-sized at instantiate; assert via
  `rt_safe`.
- **`varying` correctness.** Must be `true` when a held value changed *this block* (else
  const-folding ops miss the change). Test: held cutoff changed mid-block → filter recomputes.
- **Enum-over-OSC** (Phase 1.7) is a small but public contract — lock symbol-vs-index before the
  sweep so operators don't disagree.
- **`differentiate`/`integrate` semantic shift** — confirm no shipped instrument silently depends
  on event-rate behavior before changing it.

## Suggested PR breakdown

`P0` engine core+`Io` (one operator migrated to prove it) · `P1` macro · `P2a..n` operator sweep
(batchable, ~1 PR per few operators) · `P2x` `m2s`→shapers · `P3` loader+format · `P4`
instruments+re-bless · `P5` docs+tooling. P0→P1→P3 are the serial spine; P2 and P4 fan out behind
their gates.

## Test strategy

Per-operator unit tests (TDD) · golden descriptor snapshot · generated-schema check · per-instrument
integration tests · `rt_safe` allocation check · the materialize/`varying`/block-accuracy
regressions above · one human OSC walkthrough at Gate 4.
