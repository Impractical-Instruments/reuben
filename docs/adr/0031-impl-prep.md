# ADR-0031 Implementation Prep / Handoff

Companion to [0031](0031-float-resolves-to-value-or-signal-by-wiring.md), written to its **revised**
(declared-forms) model — there is **no propagation pass**. Three artifacts the 8-step plan needs
before /tdd + parallel migration: (1) the test oracle, (2) the wire-coercion fixture table = the
red-test suite for the planner's per-wire check, (3) the per-operator migration map.

> **Model recap (the thing that changed).** A port's form is **declared**, not resolved: `f32` =
> Value (sparse/held), `f32_buffer` = Signal (dense/per-sample). The planner's only form job is a
> **local per-wire check**: Value→Signal input **materializes**, Signal→Value input is a **hard
> error**, like→like is direct. Math ops ship as explicit **value-math (`f32`) / signal-math
> (`f32_buffer`)** variants. No topological solver, no denseness tags, no `F32In`/`F32Out` sum type,
> no feedback back-edge rule (cycles stay `PlanError::Cycle`).

---

## 1. Test oracle (build first — does NOT exist; infra does)

No buffer-count / form probe today. Hooks that exist:

- `Plan.num_buffers` — total arena slots (`plan.rs`).
- `PlanNode.materialize: Vec<(input_port, arena_idx)>` — which inputs got a materialized buffer.
- `PlanNode.input_kinds: Vec<PortKind>` — per-input classification (becomes the assert target).
- `Plan.materialize_scratch_mask` — scratch vs edge buffers.
- `OpDriver` (`op_driver.rs`) — instantiates a **one-node** Plan, `plan.nodes[0]` + `num_buffers`
  public. Good for single-op asserts; **insufficient for graph fixtures** (need multi-node wires).

**Oracle = test-infra step 0 (precedes ADR step 1):**

1. `fn port_form(plan, node, port) -> PortKind` — getter over `input_kinds` / a declared-output form.
2. `fn signal_buffer_count(plan) -> usize` — count buffers = declared-Signal ports + materialized
   Value→Signal edges.
3. **Multi-node fixture builder** — OpDriver is 1-node. Either extend it to accept a small wired
   graph, or load a tiny instrument JSON through `Plan::instantiate`. Pick one; every fixture below
   needs it, and the coercion fixtures (G/H/I) need to assert a **plan error**, so the builder must
   surface `Result<Plan, PlanError>`, not panic.

Assertions are three kinds: **form** (`port_form == Value|Signal|Event`), **cost**
(`signal_buffer_count == N`), and **error** (`instantiate == Err(PlanError::FormMismatch{..})`).

---

## 2. Wire-coercion fixture table (red tests for the per-wire checker — ADR steps 1 & 6)

Forms are declared, so these test the **wire check**, not a solver. Write them RED first.

| # | Graph (declared forms) | Expect | Buffers | Proves |
|---|---|---|---|---|
| A | `cutoff` const → `filter.cutoff` (`f32_buffer`) | **materialize** | 1 (constant buffer) | Value→Signal at a Signal input is legal |
| B | `lfo`(`f32_buffer`) → `filter.cutoff` (`f32_buffer`) | direct, no coercion | 1 (lfo edge) | Signal→Signal is a plain wire |
| C | `tempo` const → `clock.tempo` (`f32`) | direct, **Value** | 0 | a held knob never materializes |
| D | `voicer.freq` (`f32` Value) → `oscillator.freq` (`f32_buffer`) | **materialize** | 1 | the canonical Value→Signal bridge (sparse freq → per-sample) |
| E | `euclid.gate` (`f32`) → `envelope.gate` (`f32`) | direct, **Value** | 0 | sparse trigger spine stays sparse end-to-end |
| F | `clock.gate` (`f32`) → `euclid.clock` (`f32`) | direct, **Value** | 0 | gate is a message; edges read via slicing |
| G | `envelope.cv` (`f32_buffer`) → `envelope.gate` (`f32`) of another node | **plan error** | — | **Signal→Value is hard-errored** (no env→gate yet, by design) |
| H | `oscillator.out` (`f32_buffer`) → `filter.mode` (`enum`) | **plan error** | — | Signal→Value-only type illegal |
| I | `sequencer.degrees` (`note`) → `filter.cutoff` (`f32_buffer`) | **plan error** | — | Event→Signal illegal, needs an explicit op |

**Watch:**
- Fixture **G** is the headline new behavior — the sig→val gap is *deliberate*. Its error message
  must name the missing converter (envelope follower / quantizer), since a user *will* try this wire.
- Fixtures replaced the old "graph resolves forms" cases wholesale — there is nothing to resolve.
- **Out of scope:** feedback cycles. Still a hard `PlanError::Cycle` (Kahn sort), per
  [ADR-0009](0009-graph-lifecycle.md). The checker assumes a DAG.

---

## 3. Operator migration map (drives step 5 fan-out — 1 agent per op)

29 ops. Each numeric port is declared `f32` (Value) or `f32_buffer` (Signal) per the ADR's locked
table. Grouped by migration wave; the gate/CV ports are **settled** (no ⚠ decisions remain).

### Wave 0 — author the new `*_value` math nodes (net-new, not a migration)
`add_value` `mul_value` `power_value` (and `differentiate_value`/`integrate_value` as needed)
- All ports `f32` (Value). Sparse control arithmetic. **These don't exist yet** — author test-first.
- **Rename** the existing `add`/`mul`/`power`/`differentiate`/`integrate` → `*_signal` (all ports
  `f32_buffer`). The bare names retire; instruments referencing them are re-blessed to the suffixed
  name. Naming is settled: `_value` / `_signal` suffix on both variants.

### Wave 1 — Signal generators (`f32_buffer` out)
`oscillator` (freq `f32_buffer`, out `f32_buffer`) · `lfo` · `noise`
- enum inputs (`waveform`) stay Value. oscillator = the ADR's Value→Signal materialize sink for freq.

### Wave 2 — audio processors (`f32_buffer` audio in+out, controls per nature)
`filter` (audio + cutoff `f32_buffer`, mode enum) · `delay` · `djfilter` · `reverb` · `pan` · `output`
- `filter` = flagship: both audio and cutoff are `f32_buffer`; no `match`, just `io.in_signal`.
- `output` has a manual descriptor (`output.rs:28`) — migrate by hand, not via the macro sweep.

### Wave 3 — message-domain producers/consumers (the gate/CV spine)
`clock` · `euclid` · `voicer` · `envelope` · `sample` · `sequencer`
- `clock`: phase `f32_buffer` (Signal), **gate `f32` (Value)** — emit edge crossings inside the
  phasor loop via `set(frame, v)`.
- `euclid`: clock-in `f32` (Value, reads `clock.gate` edges), gate-out `f32` (Value).
- `voicer`: notes `note` (Event), harmony `harmony`, **freq/gate out `f32` (Value)** — the op
  already builds a sparse change-list; stop ZOH-expanding it into a buffer.
- `envelope`: gate-in `f32` (Value, edge via slicing), ADSR `f32`, **cv-out `f32_buffer` (Signal)**.
  Envelope is the msg→sig boundary.
- `sample`: freq/gate/root/gain/start/channel `f32` (Value), audio-out `f32_buffer`.
- `sequencer`: clock-in `f32` (Value), step/length/pitch `f32`, gate_mode enum, degrees-out `note`.

### Wave 4 — pure event/context ops (no numeric form change)
`chord` · `snap` · `strum` · `transpose` · `osc_out` · `harmony` (HarmonyOp)
- `note` → Event; `harmony` → Value; enums → Value; any numeric → `f32` (Value).
- `osc_out` is a **boundary op** — keeps its address (ADR step 7).

### Skip
`map` — ADR defers its Float reframe.

### Boundary ops (step 7, addresses)
`osc_out`, `output` — keep address mapping; everything else goes addressless.

---

## Parallelization shape (what the /tdd plan should encode)

- **Sequential spine:** oracle(0) → wire-checker+fixtures(1) → `f32_buffer` rename(2) → Io API(3) →
  declare forms(4) → [step 5 fan-out] → coercion-error messages(6) → boundary/addresses(7) →
  docs(8). Steps 1-4 are a hard chain.
- **One parallel burst = step 5**, but it now has an ordering wrinkle: **Wave 0 (the `*_value`
  nodes + `*_signal` rename) is net-new authoring and the math story's foundation**, so land it
  before fanning out waves 1-4. Within each wave, 1 agent per operator, worktree-per-op.
- **No remaining design gates.** The gate/CV scrub closed every ⚠ and the math naming is settled
  (`_value` / `_signal`); step 5 can fan out without re-deciding anything.
