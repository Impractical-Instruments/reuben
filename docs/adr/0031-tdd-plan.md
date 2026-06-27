# ADR-0031 Parallel /tdd Plan

Execution plan for [0031](0031-float-resolves-to-value-or-signal-by-wiring.md) +
[0031-impl-prep.md](0031-impl-prep.md). Decided in a grilling session:

- **Fixtures** = thin `Graph` test-helper (wire nodes via `Graph::add`/`connect` →
  `Plan::instantiate` → `Result<Plan, PlanError>`). Not OpDriver, not JSON. Surfaces plan errors
  directly (G/H/I need that).
- **Step 5** = wave-gated worktree fan-out, **1 agent per op**. Wave 0 is a barrier.
- **Spine (0–4)** = **vertical** tracer bullets, one fixture/behavior at a time. No horizontal
  "all tests then all code".
- This turn = **written plan only**. No code until approved.

---

## Progress / pickup (resume here)

| Step | State | Commit |
|---|---|---|
| 0 — oracle infra | ✅ done | `0ed6ba6` |
| 1 — `PortKind` + wire checker | ✅ done | `b9b451c` |
| 2 — `f32_buffer` rename | ✅ done | `64498fe` |
| 3 — new `Io` API | ✅ done | (this commit) |
| 4–8 | ⬜ pending | — |

**Suite is green workspace-wide at step 3** (`cargo test --workspace`, clippy clean).
One commit per step.

Step 3 notes (API-shape decision — the ADR was stale): the read/write surface is **two
return-type-dispatched verbs**, not five named ones. `io.input::<T>(port)` (`&[f32]`⇒Signal slice ·
scalar/enum/`Harmony`⇒held `Option<T>` · `Note`⇒`EventStream` iterator) and `io.output::<T>(port)`
(`f32`⇒`MsgWriter` · `&mut [f32]`⇒`&mut [f32]`). `in`/`out` are reserved → `input`/`output`. Trait
machinery: `IoInput`/`IoOutput` (the latter a GAT for the per-call borrow), a no-alloc named
`EventStream`, and `MsgWriter` (writer-local dedup, last-write-wins per frame, addressless `Emit`).
The five-verb spelling was stale ADR text from before the grilling resolved it; ADR-0031 §Read/write
API + Consequences + impl-step-3 were corrected to match. Old verbs (`signal`/`last`/`stream`/
`signal_mut`/`emit`/`varying`) kept intact — additive, nothing migrated yet.

Step 2 notes (full-sweep + align-display decisions): retired the `buffer`/`float`
keywords *and* their internal plumbing (`FloatMeta`→`F32Meta`, `PortSpec.float`→`f32`,
`PortTypeAst`, codegen string tags, scaffold emission). Golden `kind()` display now
prints `f32_buffer`/`f32`; `descriptors.txt` re-blessed. The JSON schema is derived
from param ranges, **not** the keyword, so it needed no re-bless (the plan's
"re-bless schema" was a no-op).

### Decision A (resolves a green-at-each-step conflict the original plan underspecified)

The original spine separated **declare forms (step 4)** from **operator sweep (step 5)**. That can't
stay green: today `PortType::F32` is classified `Dense` and *always materialized into a buffer*, and
~15 operators read `f32` inputs **per-sample via `io.signal`** (incl. `euclid.clock`, `sample.gate`/
`freq`, `envelope.gate` — ports the ADR re-declares as Value). The instant a port flips `f32`→Value
(no buffer) those `io.signal` reads break; and several real wires (`voicer.OUT_FREQ`/`clock.OUT_GATE`
buffers → `f32` inputs) become `Signal→Value` and hard-error the moment forms are declared.

**Resolution (chosen):**
- **Steps 1–3 stay pure substrate.** `port_kind` keeps `F32 ⇒ Signal` (status-quo always-materialize),
  so old `io.signal` keeps working and the suite stays green. The new checker is exercised against
  **synthetic** Signal/Value/Event probe ports (`tests/wire_forms.rs`), not real ones.
- **Steps 4 + 5 fuse per operator.** Each operator's **form declaration and accessor migration land
  together** in one green commit during the wave fan-out. The `f32`→Value mapping flip rides along
  per-op (re-declare the port's `PortType`, migrate the op's reads, re-wire its now-Value outputs, in
  the same change). The locked gate/CV table (ADR §"Locked port-form decisions") still governs *which*
  form each port gets — it's just applied op-by-op, not in one global pass.

So below, treat **"step 4"** as the first half of each op's **step-5** migration, not a separate phase.

---

## Shape

```
SEQUENTIAL SPINE (one driver, vertical TDD, hard chain) — F32⇒Signal throughout
  0 oracle infra ─ 1 wire-checker ─ 2 rename ─ 3 Io API
                                                  │
PARALLEL BURST (step 5, declare-forms fused in per-op — Decision A) ──┘
  Wave 0 (barrier) ─→ Waves 1·2·3·4  [1 agent/op, worktree-per-op]
                                                  │
SEQUENTIAL TAIL ────────────────────────────────┘
  6 coercion msgs ─ 7 boundary/addresses ─ 8 docs+schema sweep
```

Each spine step: green + full test suite passing before next. Each op agent: green in its
worktree before merge.

---

## Step 0 — Oracle infra (precedes everything; build test-first)

Behaviors → tests (vertical, one at a time):

1. `graph_helper` wires 2 nodes, instantiates → `Ok(Plan)`. *(tracer bullet — proves substrate)*
2. `port_form(plan, node, port) -> PortKind` reads a declared input form.
3. `signal_buffer_count(plan) -> usize` = declared-Signal ports + materialized V→S edges.
4. helper returns `Result`, not panic (so error fixtures can assert `Err`).

Deliverable: test-only `graph_helper` + two probe fns over `PlanNode.input_kinds` /
`Plan.num_buffers` / `materialize`. No production behavior change yet.

## Step 1 — `PortKind{Signal,Value,Event}` + per-wire checker (vertical, fixture by fixture)

Add `PlanError::FormMismatch { src, dst, reason }`. Build the checker one fixture at a time —
each red test drives the next slice, NOT all 9 red up front:

| Order | Fixture | Red asserts | Drives |
|---|---|---|---|
| 1 | A const→`filter.cutoff` | materialize, 1 buf | V→S materialize path |
| 2 | B lfo→`filter.cutoff` | direct, 1 buf | S→S plain wire |
| 3 | C tempo→`clock.tempo` | direct, **0 buf** | held knob never materializes |
| 4 | D `voicer.freq`→`osc.freq` | materialize, 1 buf | canonical sparse→dense bridge |
| 5 | E `euclid.gate`→`env.gate` | direct, 0 buf | sparse spine stays sparse |
| 6 | F `clock.gate`→`euclid.clock` | direct, 0 buf | gate-as-message via slicing |
| 7 | G `env.cv`→`env.gate` | **`Err(FormMismatch)`** | **S→V hard error** (headline) |
| 8 | H `osc.out`→`filter.mode` | `Err` | S→Value-only-type illegal |
| 9 | I `seq.degrees`→`filter.cutoff` | `Err` | Event→Signal illegal |

Checker rules: V→S materialize · S→V error · Event mismatch error · like→like direct · alloc
`f32_buffer` only for declared-Signal or materialized edge · Value gets latch slot · block-slice
at Value-input change frames. **No** topological solver / denseness tags / feedback back-edge.
Keep old `Io` verbs working over new allocation. Bless descriptor snapshot.

**G's error message must name the missing converter** (envelope follower / quantizer) — user will
try this wire. Assert the message text in the fixture.

## Step 2 — `Buffer → f32_buffer` rename (mechanical, repo-wide)

`Arg::Buffer→Arg::F32Buffer`, `PortType::Buffer→F32Buffer`, contract-macro keyword
`buffer→f32_buffer`, retire `float`→`f32`. Re-bless schema. Tests: snapshot + existing suite green.

## Step 3 — New `Io` API (additive; old verbs stay)

Two return-type-dispatched verbs only: **`input::<T>(port)`** (`&[f32]`⇒Signal · scalar/enum/
`Harmony`⇒Value held `Option<T>` · `Note`⇒Event iterator) and **`output::<T>(port)`** (`f32`⇒
`MsgWriter` · `&mut [f32]`⇒`&mut [f32]`). (`in`/`out` are reserved-word-adjacent → `input`/`output`.)
Test-first per `T`-arm. `MsgWriter::set(frame,v)` = **deduped** (no-op change emits nothing) +
**last-write-wins per frame** + addressless. Step-3 dedup is **writer-local** (running value seeded
empty each call; cross-block held-latch baseline rides in with the first Value-emitting op in step 5).
No `F32In`/`F32Out`, no `match`, no `varying`. Event-**write** stays the old `emit` verb for now.
Keep old verbs temporarily.

## Step 4 — Declare port forms in the contract (**fused into step 5 per-op — Decision A**)

Apply the locked gate/CV table: each numeric port → `f32` or `f32_buffer`. Engine does no
resolution. **Not a separate global phase:** declaring a port's form flips `F32⇒Value` and so must
land *with* its operator's accessor migration (step 5) to keep the suite green. So per migrating op:
re-declare its ports, migrate its reads, re-wire its now-Value outputs, re-bless that op's descriptor
snapshot. Fixtures C/E/F gain their real-port versions as the relevant ops (`clock`, `euclid`,
`envelope`) migrate.

**End of substrate spine (0–3) — checkpoint for review before fan-out.**

---

## Step 5 — Operator sweep (PARALLEL, wave-gated, 1 agent/op, worktree-per-op)

Each agent: migrate one op to direct accessors + its declared forms, test-first against `OpDriver`,
green in its own worktree → merge → next. Worktree names by op (e.g. `op-filter`), not auto-hash.

**Wave 0 — barrier (land before any other wave).** Math foundation:
- author net-new `add_value` `mul_value` `power_value` (+ `differentiate_value`/`integrate_value`
  as needed), all `f32`, test-first.
- rename existing `add`/`mul`/`power`/`differentiate`/`integrate` → `*_signal` (all `f32_buffer`).
  Re-bless instruments referencing bare names.

Then fan out (waves independent of each other; ops within a wave fully parallel):
- **Wave 1** signal gens: `oscillator` `lfo` `noise` *(osc.freq = V→S materialize sink)*
- **Wave 2** audio procs: `filter`(flagship) `delay` `djfilter` `reverb` `pan` `output`*(manual descriptor, hand-migrate)*
- **Wave 3** gate/CV spine: `clock` `euclid` `voicer` `envelope`*(msg→sig boundary)* `sample` `sequencer`
- **Wave 4** event/context: `chord` `snap` `strum` `transpose` `osc_out` `harmony`

Skip `map` (Float reframe deferred). Per-op acceptance: own tests green + no old-verb refs.
**Delete old `Io` verbs once sweep complete** (final step-5 agent / spine driver).

---

## Step 6 — Coercion enforcement messages (sequential)

Harden step-1 errors: legal V→S materialize; clear S→V message naming the converter op; Event
mismatch message. Re-assert fixtures G/H/I message text.

## Step 7 — Boundary + addresses (sequential)

Drop `address` from internal `Emit`/hot path; keep it only in boundary ops (`osc_out`, `output`).
Tests: internal wires route by connection; OSC boundary round-trips address↔port.

## Step 8 — Docs + schema sweep (sequential)

`/sync-docs`: ARCHITECTURE, README, `docs/agents/authoring.md`, `CONTEXT.md`, create-operator
skill. Teach: declare `f32`/`f32_buffer` by what the port is; direct accessors; value-math vs
signal-math; the one legal coercion (V→S) + hard error on reverse. Re-bless golden snapshots.

---

## Merge order / gates

- Spine 0→1→2→3→4 strictly serial, suite green at each.
- **Gate before step 5:** spine merged to branch.
- **Barrier inside step 5:** Wave 0 merged before waves 1-4 launch.
- Waves 1-4 parallel; per-op merge as each agent goes green.
- **Gate before step 6:** all ops migrated, old verbs deleted.
- 6→7→8 serial.

## Out of scope

Feedback cycles (`PlanError::Cycle` stays, Kahn sort). `map` Float reframe. sig→val converter ops
(the deliberate gap G documents).
