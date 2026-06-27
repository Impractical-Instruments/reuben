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
| 3 — new `Io` API | ✅ done | `fadd3ed` |
| 5 Phase A — accessor migration | ✅ done | `e411a7a` |
| 5 Phase A — math `*_f32_signal` rename | ✅ done | `3821aa2` |
| 5 Phase A — osc.freq/filter.cutoff → f32_buffer | ✅ done | `f1e8fdc` |
| 5 Phase A — output migration (`emit`→`EventWriter`/`MsgWriter`) + delete old verbs | ✅ done | `a43c9c1`·`6775aa1`·`b4e558b` |
| 5 Phase B — forks resolved (grill session 2) | 🔍 scoped | — |
| 5 Phase B pre-commit — forced f32→f32_buffer (math operands + swept controls) | ✅ done | `cb437c0` |
| 5 Phase B — forks re-resolved (grill session 3): `is_materialized` + per-Voice | 🔍 re-scoped | issues `#99`·`#100`·`#101` |
| 5 Phase B — per-Voice re-ruled (grill session 4): **flip them too, stub Voicer silent** | 🔍 **re-scoped** | issue `#99` (expanded) |
| 5 Phase B — atomic barrier (flip + spine rewrites + value-math) | ⬜ pending | — |
| 6–8 | ⬜ pending | — |

**Suite is green workspace-wide at `b4e558b`** (`cargo test --workspace`, clippy clean).
**Phase A is fully done** — the only `Io` read/write verbs are now `input::<T>` / `output::<T>`
(plus `varying`); `EventWriter`/`MsgWriter` are the two output writers. One commit per step
(Phase A's last step took 3 green sub-commits: add arms → migrate call sites → delete).

### ▶ Pickup for Phase B (next session)

The atomic green barrier (Decision B). In one sequence, on this branch:

1. **Flip `port_kind`** (`plan.rs:56`): `F32 ⇒ Value` (currently `F32 | F32Buffer ⇒ Signal`).
   `F32Buffer` stays Signal. After this, every still-`f32` port is a held Value.
2. ~~**Fix `is_materialized`**~~ — **SUPERSEDED by grill session 3: do NOT change it; keep
   `meta.is_some()`.** `is_materialized` does *not* drive buffer allocation (that is `port_kind` at
   `plan.rs:351`); its only role is backing `materialized_input` (the settable-numeric-input lookup),
   where `meta.is_some()` stays correct for both `f32` Value and `f32_buffer` Signal numeric controls.
   The planned `matches!(F32Buffer) && meta.is_some()` would silently drop JSON/OSC numeric overrides
   on every bare-`f32` Value control. `contract_shapes.rs` passes unchanged. See session 3 below.
3. **Gate/CV-spine reads/writes → held-Value** — **WIDENED by grill session 4 (reverses session 3's
   Fork 2): flip the FULL gate/CV spine, including the per-Voice ports.**
   - **Inputs → `f32` held** (read held + block-sliced edge-detect; tests switch `drive(buffer)` →
     `push(port, frame, v)`): `euclid.clock`, `sequencer.clock`, **`sample.gate`/`freq`**, **`envelope.gate`**.
   - **Outputs → `f32` / `MsgWriter`**: `clock.gate`, **`voicer.freq`/`gate`** (`euclid.gate` already done).
   - **Stay `f32_buffer`**: `envelope.cv` (a true continuous Signal) and `oscillator.freq` (the V→S
     materialize sink — `voicer.freq` Value → `osc.freq` Signal is a legal materialize edge).
   - `sample`/`envelope` are **fully migrated** and stay correct in **mono**; only **polyphony** breaks,
     because Voicer is the fan-out and per-Lane Value routing doesn't exist yet (emission is Lane-0-only;
     the Value latch is node-global/broadcast). So **Voicer is stubbed silent** (no-op `process`, emits
     nothing → unset `f32` latches read `0.0` → downstream silent) and its tests are deleted, along with
     the voicer-driven integration tests (see session 4). Voicer rewrite + polyphony restore → issue `#99`.
4. **Author the net-new `*_f32_value` math family** beside the `*_f32_signal` structs in the same
   family file (`add.rs`, …) — value shell calls the shared scalar `fn` once; signal shell loops it.
5. Re-bless any op descriptor snapshots that change; keep `cargo test --workspace` + clippy green
   across the sequence (it is one barrier, so expect a transient-red working tree until the flip
   sequence is complete — do not commit mid-flip).

`Emit.address` field still exists (writers set `""`); its removal + boundary rework is **step 7**.
Note `cargo doc -D warnings` is **not** a CI gate (reuben-contract + some reuben-core links were
already broken pre-Phase-A); don't be alarmed by it.

### ✅ Resolved (grilling session 4, 2026-06-27) — reverse Fork 2: flip the per-Voice ports, break Voicer

Session 3's Fork 2 kept `voicer.freq`/`gate`, `sample.freq`/`gate`, `envelope.gate` as `f32_buffer` to
preserve polyphony. **User ruling (this session): reverse it.** Carry a uniform "all gate/trigger ports
are `f32` Value" model now; accept that this breaks Voicer (the engine has no per-Lane Value routing yet);
rewrite Voicer later under `#99`. The engine constraints from Fork 2 are unchanged and confirmed in code —
emission is Lane-0-only (`render.rs:661`), the Value latch is node-global (`render.rs:608`) — so a Value
`freq`/`gate` genuinely collapses polyphony. We choose to eat that.

**Rulings (each confirmed in the grill):**

1. **Flip set** (`f32_buffer → f32`): `voicer.freq`/`gate` (outputs), `sample.freq`/`gate`,
   `envelope.gate` (inputs). **Stay `f32_buffer`:** `envelope.cv` (continuous CV Signal) and
   `oscillator.freq` (V→S materialize sink). After the flip: `voicer.freq`(Value)→`osc.freq`(Signal) is a
   legal V→S materialize edge; `voicer.{freq,gate}`→`sample`/`envelope` are V→V direct.
2. **Broken-Voicer shape = silent stub (not mono-audible).** `voicer::process` becomes a no-op that emits
   nothing. Silence is *guaranteed* by the engine, no extra work: an unset `f32` Value latch reads `0.0`
   (`plan.rs:104`), so `envelope`/`sample` gates sit at `0.0` (never trigger) and `osc.freq` materializes
   to `0.0` → `sin(0)` → silent. **Keep `lanes: from_param(voices)` untouched** (don't spend churn on a
   lane model `#99` rewrites; the stub is silent regardless of Lane count).
3. **`sample` + `envelope` are fully migrated, not broken.** Rewrite their per-sample buffer edge-detect
   (`sample.rs:121,126`, `envelope.rs:107`) into held-Value block-sliced edge-detect; migrate their unit
   tests `drive(buffer)` → `push(port, frame, v)`. They stay correct in **mono** (a node-global latch IS
   correct for a single Lane — e.g. `euclid.gate`/`clock.gate` → `envelope.gate`). Only polyphony (which
   only exists via Voicer) is broken.
4. **Test removal — broader than Voicer's unit tests** (all logged in `#99` for restore):
   - **Delete** (subject *is* Voicer's musical brain, now stubbed away): `voicer.rs` unit tests;
     **all** of `chord_player.rs`; the Voicer-resolution tests in `tonal_context.rs`
     (`degree_note_resolves…`, `context_change_mid_block…`, `snap_quantizes…`, `demo_instruments_load_and_play`);
     `first_sound.rs`'s two audio asserts (`rig_makes_a_non_silent_tone_at_440hz`, `envelope_attack_is_audible`).
   - **Neuter** (subject is schema/format/load; Voicer only supplied sound) to load + instantiate +
     render-without-panic, dropping the audio-content assertion: `instrument_format.rs`'s `…440hz`
     (loads `default.json`), `groovebox_snare_gate.rs`, and the `*_load_and_play`/`*_makes_sound`
     "honest sound check" tests.
   - **Rewire, don't delete, `first_sound.rs`** to keep ONE green end-to-end sound canary through the
     barrier: drive `osc.freq` from a constant Value source and `env.gate` from `euclid.gate`/`clock.gate`
     (both becoming Value this barrier) instead of from Voicer. The spine sound path
     (osc→filter→env→power→mul→output) is exactly what this barrier churns, so the canary earns its keep.
5. **`#99` scope expands** from "Voicer per-Lane message routing" to **"per-Lane Value/message routing so
   Voicer + downstream polyphony works again, AND restore all the deleted/neutered tests above."**

**Net barrier scope after session 4** (replaces session 3's net): flip `port_kind` `F32 ⇒ Value`; **don't**
touch `is_materialized` (Fork 1 stands); rewrite held edge-detect for `euclid.clock`, `sequencer.clock`,
`sample.gate`/`freq`, `envelope.gate`; rewrite `MsgWriter` outputs for `clock.gate`, `voicer.freq`/`gate`
(Voicer's is the silent no-op); rewrite `m2s.in` (held read + smooth, stays `f32`); author
`add_f32_value`/`mul_f32_value`/`power_f32_value`; stub Voicer silent + remove/neuter/rewire the tests in (4);
re-bless descriptor + schema goldens (`descriptors.txt` voicer/sample/envelope rows; `instrument.schema.json`).

### ✅ Resolved (grilling session 3, 2026-06-27) — two Phase-B forks found mid-execution

Two contradictions surfaced while scoping the barrier against the live engine; both confirmed in a
grill and ruled by the user. They **supersede** the matching session-2 bullets.

**Fork 1 — `is_materialized` must NOT change (keep `meta.is_some()`).** Session 2 said flip it to
`matches!(F32Buffer) && meta.is_some()` because a post-flip bare-`f32` no longer materializes a buffer.
But `is_materialized` is **never consulted for buffer allocation** — that decision is purely
`port_kind == Signal` (`plan.rs:351`). Its only callers are `materialized_input` (`graph.rs:98/115`,
`format.rs:402`, `schema.rs:220`), the lookup that resolves an author-set **numeric input override** by
name. The planned change would make `materialized_input("attack")` (and every other bare-`f32` Value
control: `clock.tempo`, `euclid.steps`, `sample.root`, `m2s.rate`, …) return `None`, so `set_param`
falls through to `set_enum` (no-op) and the override is **silently dropped**. Resolution: leave the
predicate `meta.is_some()` — correct for both Value and Signal numeric controls. `contract_shapes.rs:55-56`
passes unchanged. The session-2 "⚠ obligation" (under the osc.freq/cutoff resolution) is **void**.

**Fork 2 — per-Voice ports cannot become Value; flip is spine-only.** **⚠ SUPERSEDED by grill session 4
(above): the per-Voice ports DO flip; Voicer is deliberately broken (silent stub) and rewritten under `#99`.
The engine facts below still hold — they are exactly *why* flipping breaks polyphony — we just choose to
eat that breakage now.** Session 2 listed
`voicer.freq`/`gate`, `sample.freq`/`gate`, `envelope.gate` among the Value conversions. Two engine
facts block this for **post-fan-out** (per-Voice) data:
- **Emission is Lane-0 only** (`render.rs:~661`: `if lane == 0 { io.with_emit(...) } else { io }`) — a
  `MsgWriter` write from Voice>0 has no sink (silent loss).
- **Value inputs read a node-global latch** (`render.rs:~606`, one `node.latch[port]`, not per-Lane) —
  a Value `freq`/`gate` broadcasts one Voice's value to all Voices, collapsing polyphony.

Voicer is the fan-out (`lanes: from_param(voices)`; downstream `Inherit`s N Lanes), so its `freq`/`gate`
are per-Voice **buffers**; `sample.*` and per-Voice `envelope.gate` consume them. Flipping any of these
would also make `voicer(buffer) → sample(Value)` an S→V hard-error. Resolution (user ruling): **leave
them `f32_buffer` — they already are, so no rewrite.** The Value flip applies to the single-Lane,
pre-fan-out trigger spine only: `clock.gate` (output→`MsgWriter`), `euclid.clock` + `sequencer.clock`
(inputs→held edge-detect); `euclid.gate` already done. Voicer full rewrite (per-Lane message routing)
deferred → **issue `#99`**. Block-rate knobs still flip fine (broadcast is correct for shared settings).

**Net barrier scope after session 3** *(⚠ superseded — see session 4's net above, which adds the per-Voice
flips + Voicer stub):* flip `port_kind` `F32 ⇒ Value`; **don't** touch `is_materialized`;
redeclare + rewrite `euclid.clock`/`sequencer.clock` (held edge-detect) and `clock.gate` (`MsgWriter`);
rewrite `m2s.in` (held read + smooth, stays `f32`); author `add_f32_value`/`mul_f32_value`/`power_f32_value`;
re-bless snapshots. **Pre-commit (`cb437c0`) already shipped** the forced f32→f32_buffer set. Deferred
issues filed: `#99` (Voicer), `#100` (strum.position retrofit), `#101` (map `_value`/`_signal`).

### ✅ Resolved (grilling session 2, 2026-06-27) — Phase B fork rulings + execution shape

A full audit of every `io.input::<&[f32]>` / `io.input::<f32>` site against its port declaration
surfaced ports the plan above underspecified. Rulings (all confirmed in a grill):

**Forced f32→f32_buffer (read per-sample as a slice today; the flip would break that read).**
- **Signal-math operands** — `add`/`mul` (`a`,`b`), `power` (`x` only; `exponent` stays `f32`
  Value, read held), `differentiate`/`integrate` (`in`): declared `f32_buffer` **with meta** so the
  identity/default still materializes (`add` default 0, `mul` default 1 — decision (a) path). The
  Phase-A "rename to `*_f32_signal`" was struct-only and left the ports `f32`; this is where they
  become buffers.
- **Swept controls** — `filter.resonance`, `pan.pan`, `djfilter.position`, `strum.position`,
  `map.in`: all `f32_buffer` (behaviour-preserving — they're read per-sample, a constant
  materializes, modulation preserved, no read-logic rewrite). *Issues to file:* (1) retrofit
  `strum.position` back to `f32` Value; (2) give `map` `_value`/`_signal` variants like the math
  nodes (its Float reframe stays deferred).

**Gate/CV spine — full flip to `f32` Value (the chosen, ADR-faithful path; rewrite per-sample
buffer edge-detection into held-value reads driven by block-slicing).**
- **Inputs** `f32_buffer → f32`: `euclid.clock`, **`sequencer.clock`** (plan's step-3 list omitted
  it — ruled an oversight; flipped for consistency so audio→clock hard-errors everywhere),
  `envelope.gate`, `sample.gate`, `sample.freq`. Each reads `io.input::<f32>` once per block-slice
  and compares to held state for the edge; tests switch from `drive(buffer)` to `push(port, frame,
  v)` message injection (`OpDriver::push` already supports it).
- **Outputs** `f32_buffer → f32` (buffer write → `MsgWriter`): `clock.gate` (continuous square wave
  → sparse rising/falling `set()` emits inside the phasor loop; `clock.phase` stays `f32_buffer`),
  `voicer.freq`/`voicer.gate` (the op already builds a sparse change-list). `euclid.gate` is already
  `f32`+`MsgWriter` (Phase A) — no change. `envelope.cv` stays `f32_buffer`.
- **m2s.in** stays `f32` Value (it is THE V→S converter — its input is conceptually a Value); rewrite
  its loop to read the held target once per block-slice and smooth toward it within each constant
  segment (state threads across). *Not* redeclared `f32_buffer`.

**Net-new `*_f32_value` math family — `add`/`mul`/`power` only.** All-`f32` ports, Value form; the
value shell reads its held operands via `io.input::<f32>`, calls the **same** shared scalar `fn`
once, and emits the result via `io.output::<f32>(OUT).set(0, v)` (`MsgWriter`, deduped). Block-slicing
re-runs `process` at every operand change, so the output is sample-accurate. `differentiate_f32_value`
/`integrate_f32_value` are **skipped** (inherently temporal; dubious as Value) — *issue to file* if
wanted later.

**`is_materialized` fix:** key on `matches!(ty, F32Buffer) && meta.is_some()` (an `f32_buffer`-with-
meta materializes; a bare `f32` Value does not). Update `contract_shapes.rs` (the `filter_demo`
fixture's `f32` cutoff stops being "materialized"; redeclare it `f32_buffer` or move the assertion).

**Execution shape — carve a green pre-commit, then the irreducible barrier:**
1. **Pre-commit (stays green under `F32 ⇒ Signal`):** all the *forced f32→f32_buffer* edits above
   (signal-math operands + swept controls). `f32_buffer`-with-meta is Signal under the current
   classification too and materializes from its default, so the slice reads keep working. Re-bless
   the descriptor golden (+ schema/instrument goldens if they move). Commit.
2. **Atomic barrier (one commit/sequence — transient-red until done, do not commit mid-flip):**
   flip `port_kind` `F32 ⇒ Value`; fix `is_materialized`; the gate/CV input rewrites; the gate/CV
   output rewrites; `m2s` loop rewrite; author the three `_value` math ops; re-bless all snapshots;
   file the issues.

Rationale for the split: the gate/CV held-read rewrites are only *correct* after the flip (a
materialized Signal port's `io.input::<f32>` reads the end-of-block latch, not a block-sliced held
value), so they cannot be green pre-flip — but the f32→f32_buffer edits can, shrinking the red window.

### ✅ Resolved (grilling session) — "delete old Io verbs" is really *finish output migration, then delete*

The progress table's earlier "accessor migration ✅ done" covered **inputs only**; `emit` (the
output/event side) was never migrated. So this step = migrate every `emit` call site to the step-3
`output::<T>` verb, *then* delete the five value-access verbs. Resolved scope:

- **Delete set = the 5 value-access verbs:** `signal` / `last` / `stream` / `signal_mut` / `emit`.
  (Decision B's Phase-A bullet listing `emit` for plain "deletion" was misleading — `emit` must be
  *migrated*, not merely dropped, since 14 live call sites carry events/held-values.)
- **`varying` is OUT of scope — kept.** It is an engine-fed optimization *hint* (computed in
  `render.rs` post-block from latch deltas, fed via `with_varying`), not a value carrier. Filter's
  flagship const-fold path and `harmony`'s change-scan depend on it; no replacement is designed.
- **Event-write API:** add a new **`EventWriter`** returned by `output::<Note>(port)` —
  `.emit(frame, note)`, **append-only, no dedup, no last-write-wins** (chord tones land many-per-frame),
  addressless, mirrors old `emit`'s `frame_offset` add. `output::<Harmony>(port)` **reuses `MsgWriter`**
  (held Value, dedup+LWW is correct). euclid's gate (`f32` 0/1) uses the existing `output::<f32>`
  `MsgWriter`. (`io.input::<&[f32]>` returns an **arena-lifetime** slice, not a `&io` borrow, so euclid
  can hold the gate writer across its per-sample loop — no borrow conflict.)
- **`Emit.address` stays for now (writers set `""`).** The OSC boundary already routes by
  `plan.outbound_taps[].address` (the node address), **not** `Emit.address` (`render.rs:238`), so the
  field is already dead for routing. Tests asserting `e.address == "notes"/"gate"` get their address
  assertion dropped. Removing the field itself remains **step 7**.
- **Stays Phase-A green** (`F32 ⇒ Signal` untouched): Note=Event / Harmony=Value port kinds are
  unaffected by the future flip, and the euclid `f32` gate still materializes downstream exactly as the
  old `emit` did.
- **Commits: 3 green sub-commits** — (1) additive `EventWriter` + `Note`/`Harmony` output arms + unit
  tests (old verbs still present); (2) migrate `emit` call sites op-by-op (chord, snap, transpose,
  strum, sequencer, euclid, harmony, osc_out) + update address-asserting tests; (3) delete the 5 verbs
  + fix the `scaffold.rs` `signal_mut` template & its test.

### ✅ Resolved (grilling session) — osc.freq/filter.cutoff → f32_buffer

The fork below was resolved **(a)**: an `f32_buffer` input may carry an **optional `meta` block**
(`f32_buffer { 20..=20k, default 440, "Hz", exp }`). It classifies Signal (so an LFO/envelope
wires straight in — no S→V converter), yet unwired/knob-set it still materializes a buffer ZOH from
`meta.default`, exactly like today's `f32`. The rename is then a behaviour-preserving tag-swap whose
only purpose is to opt these two ports **out** of the Phase-B `F32⇒Value` flip. `seed_latch` seeds
an f32_buffer-with-meta from override-or-default; a bare `f32_buffer` (audio) stays a placeholder.

Keyword stays **`f32_buffer`** (not `f32_signal`): it names the *representation* (a buffer) — a
distinct axis from the *kind* (`port_kind` → Signal) and from the math op *form* suffix
(`add_f32_signal`). With (a), an f32_buffer-with-meta is **not a pure signal** (it holds a default),
so `f32_buffer` is the honest label. Done @ `f1e8fdc`.

**⚠ Phase-B obligation this creates:** ~~`is_materialized` must key on type/kind post-flip.~~
**VOID — see grill session 3, Fork 1.** `is_materialized` doesn't drive buffer allocation (`port_kind`
does); keeping it `meta.is_some()` is correct for its sole role (the settable-numeric-input lookup).

<details><summary>Original fork (for the record)</summary>

Today `filter.cutoff` / `oscillator.freq` are `f32` scalar controls: their unwired/knob-set
**default** lives in the port's `meta` and rides the latch, which the engine materializes into a
buffer. Once re-declared `f32_buffer` (Signal), an `f32_buffer` input carries **no `meta` and no
latch** — so an *unwired* port (or one set by a bare param/knob, not a wire) has no source and would
get an empty buffer. Options: (a) let `f32_buffer` inputs carry optional `meta`+latch and materialize
from it when unwired (mirrors today's path); (b) require a constant to be wired as an explicit Value
source (the fixture-A V→S materialize path) and drop the bare-knob affordance; (c) something else.
</details>

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

### Decision B (resolves *how* the per-op flip stays green — Decision A left this implicit)

Grilling surfaced that Decision A's "the `f32`→Value flip rides along per-op" is **not directly
implementable**: `port_kind` keys on the *type* (`plan.rs:56`, `F32|F32Buffer ⇒ Signal`), the
contract macro has **no per-port form override**, and 20 ops read `f32` inputs per-sample via the
buffer. So there is no edit that makes *one* op's `f32` ports Value without making *every* op's
`f32` ports Value in the same stroke. The flip is atomic in effect.

**Resolution (chosen): order the sweep so the atomic flip is a late *green* barrier — no red
window, sequential on one branch (the parallel worktree model can't host a global flip: a worktree
that flips `port_kind` breaks its other 19 ops → never green → never merges).**

- **Phase A (green, per-op).** Pure **accessor migration**: replace the old verbs (`signal`/`last`/
  `stream`/`signal_mut`/`emit`) with the step-3 verbs (`io.input::<T>` / `io.output::<T>`), **port
  types unchanged**. Green because under `F32 ⇒ Signal` the new verbs are behaviourally identical
  to the old ones for every current declaration (`io.input::<&[f32]>` and `io.signal` read the same
  buffer; `io.input::<f32>` and `io.last` read the same latch). Also: rename dense math
  `add`/`mul`/… → `*_f32_signal` (+ re-bless instruments/golden/schema), and re-declare the two
  Signal-intended *control* inputs `oscillator.freq` / `filter.cutoff` `f32 → f32_buffer` (so the
  flip never touches them — a constant feeds them via the V→S materialize path; unwired default
  handled at re-declaration). Old verbs deleted at the end of Phase A.

  **Math naming + file rule (decided in grilling, was getting lost):** math variants are
  per-**type** *and* per-**form** — `add_f32_signal`, `add_f32_value`, room later for
  `add_i64_value`, … — and the rename is **in-place (struct only), no file moves**. One file per
  math *family* (`add.rs`) holds the shared scalar `fn add` (issue-#83 pure-fn seam) plus every
  form/type struct; the signal shell loops the fn per-sample, the value shell calls it once.
  `AddF32Value` etc. land beside `AddF32Signal` in the *same* file in Phase B. (Rename done @ `3821aa2`.)
- **Phase B (one green barrier commit/sequence).** Now the only remaining `f32` ports are the
  genuinely-Value ones. Flip `port_kind: F32 ⇒ Value`; the gate/CV-spine ops whose **edge/trigger**
  ports actually take runtime Value messages and must block-slice (`euclid.clock`, `envelope.gate`,
  `sample.gate`/`freq`, and the Value *outputs* `clock.gate`/`euclid.gate`/`voicer.freq`/`gate`)
  swap their reads/writes to held-Value (`io.input::<f32>` / `MsgWriter`); `envelope.cv` declared
  `f32_buffer`; author the net-new `*_value` math family. Green because every `f32` port left is now
  correctly Value. (Block-rate knobs read via `io.last`/`io.input::<f32>` already work under both
  classifications — the latch is seeded regardless — so they need no flip-day change beyond the
  accessor swap done in Phase A.)

So Wave 0's "author `_value` ops" moves into **Phase B**; Wave 0 keeps only the `*_signal` rename.

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
