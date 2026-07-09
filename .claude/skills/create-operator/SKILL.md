---
name: create-operator
description: Author a new reuben Operator in Rust — the unit of DSP behind every instrument node. Aligns on the operator's contract (ports, params, behavior), scaffolds the skeleton + registration with `reuben scaffold-operator`, implements the `process` DSP test-first, and proves it compiles, registers, and validates. Use when the user says "make a new operator", "add an operator", "write a chorus/compressor/wavefolder", "I need a node that does X", or describes DSP behavior no existing operator provides.
---

# create-operator

An Operator is one unit of DSP — authored **single-Voice** (one mono stream; polyphony comes from the
Voicer hosting voice sub-patches, ADR-0010/0032) as a Rust file in [`crates/reuben-core/src/operators/`](../../crates/reuben-core/src/operators):
index consts, a state struct, and `impl Operator` (`descriptor` / `process` / `spawn`), declared in
`operators/mod.rs` and self-registered by its own `register_operator!` line (no central list to
edit, ADR-0024). This skill authors that end-to-end (ADR-0021).

First check it doesn't already exist: `reuben describe --json` lists every operator — a request is
often a *patch* of existing ones (the `patcher` skill), not new Rust. This skill is only for
behavior no operator provides. It does **not** build graphs (`patcher`), author surface docs
(`control-surface`, ADR-0043), or edit the living docs (`sync-docs`). Its review mirror is
[`rust-hot-path-review`](../rust-hot-path-review/SKILL.md) — run that over the diff to check the
`process` you wrote stays RT-safe.

## The loop: align → scaffold → implement (TDD) → gate → hand off

Run all `reuben`/`cargo` commands from the repo root.

1. **Align on the contract.** The descriptor is frozen at scaffold time and the rig builder wires
   against its port/param **indices** — getting it wrong is expensive. If the operator is at all
   underspecified, **invoke the `grilling` skill** to pin: each input/output by its **`Arg` type**
   (ADR-0030) — `f32_buffer` (a dense per-sample Signal), `f32` (a held Value number; add
   `{ min..max, default, unit, lin|exp }` for its materialized default), `enum(VocabType)` (a
   live-switchable choice naming a shared *vocab* enum), or `note`/`harmony` for a `Note`/`Harmony`
   port — plus any **`Constant`** (instantiate-time `config`, e.g. `voices`), and — critically —
   **the DSP behavior and its test oracle: "how will we know it's right?"** Use
   `domain-modeling` for naming. Skip the interview only when the user hands a precise contract.

2. **Scaffold.** Write the contract to a JSON file (format below) and run:
   `cargo run -q -p reuben-native --bin reuben -- scaffold-operator --spec <contract.json>`
   This writes `operators/<name>.rs` (descriptor filled in, a silence-writing `process` stub, its
   own `register_operator!` self-registration line, and an **intentionally-red placeholder test**)
   and the sorted `mod.rs` inserts — `registry.rs` is not touched (ADR-0024). It refuses to clobber
   and rejects a malformed spec.

3. **Implement `process` test-first** — lean on the `tdd` skill. The scaffold starts you **red**;
   turn the contract's oracle into real tests (drive the operator through the real engine with
   [`OpDriver`](../../crates/reuben-core/src/op_driver.rs) — `for_type` / `set` / `push` / `drive` /
   `bind` / `render` / `output` / `emits`, addressing ports by the generated `IN_*` / `OUT_*`
   consts — and assert observable output), then write the DSP to pass. **Copy the structure of
   [`lfo.rs`](../../crates/reuben-core/src/operators/lfo.rs)** — a clean stateful operator with
   `OpDriver` continuity/spawn tests. Honour the **realtime authoring contract**:
   - `process` runs on the **hot** path — it must not allocate, lock, block, or panic. The
     canonical hot/cold boundary + RT rules live in
     [authoring.md#rt-safe-render](../../docs/agents/authoring.md#rt-safe-render) (single
     source); the operator-specific contract follows.
   - **Single-Voice**: write one mono stream. Polyphony is not your concern — the Voicer hosts N
     voice sub-patches and sums them (ADR-0032).
   - **Read each input through its typed handle** (ADR-0037) — `io.read(IN_X)`; the handle's
     form (fixed by the contract declaration) selects the read shape, so a wrong-form read does
     not compile:
     - **Signal (`f32_buffer`)** → `io.read(IN) -> &[f32]` — per-sample DSP, also the
       materialized buffer of a Value wired into a Signal input. **Always exactly `io.frames()`
       long** (the buffer-presence invariant) — index `io.read(IN)[i]` directly, no
       `.get(i).unwrap_or(..)` guard; `io.varying(IN)` lets a const-folding op skip recompute on
       a held block.
     - **held Value (`f32`)** → `io.read(IN) -> f32` — a block-rate scalar, defaulted to the
       contract's declared default (never restate the default in `process`).
     - **enum** → `io.read(IN) -> MyVocabEnum` — a real Rust enum, not an index; defaults to the
       type's `#[default]` variant.
     - **`Harmony`** → `io.read(IN) -> Harmony`; **`Note`** → `io.read(IN)` (an iterator of
       `Stamped<Note>` — `.frame`, `.payload`).
   - **Write outputs through their handles** — Signal → `io.write(OUT) -> &mut [f32]`; a `Note`
     via `io.write(OUT)` (`.emit(frame, note)`, append-only); a held `f32`/`Harmony` via
     `io.write(OUT)` (`.set(frame, v)`, dedup + last-write-wins).
   - **Persistent state carries across blocks** — keep phase/filter state in the struct; use `f64`
     for a phase accumulator so it doesn't drift (lfo/clock).
   - **`spawn`** resets per-voice state but **carries any resource binding forward** (ADR-0016).
   - The **typed `IN_*`/`OUT_*` handles are the contract** downstream nodes reference — don't
     renumber casually.

4. **Close the gate** — `validate` can't prove DSP is correct, so the gate is richer than the
   patcher's. In order:
   1. `cargo test -p reuben-core` — your tests (the real oracle), the registry self-registration
      invariants (your op is gathered, names stay unique + snake_case), and `committed_schema_is_in_sync`.
   2. **Register a micro-bench workload** (#30, ADR-0019) — a forcing function in
      [`bench_support.rs`](../../crates/reuben-core/src/bench_support.rs) (`#[cfg(feature = "bench")]`,
      so plain `cargo test` **won't** catch it — only CI's `check` job does) requires every registered
      operator to have one. Add, **alphabetically**, in lockstep: a `w("<op>", Recipe::<R>)` entry in
      `WORKLOADS`, the matching `"<op>"` line in `MICRO_IAI_KINDS`, and the matching
      `#[bench::<op>(args = ("<op>",), setup = OpHarness::for_kind)]` attribute in
      [`benches/micro_iai.rs`](../../crates/reuben-core/benches/micro_iai.rs). Pick the `Recipe` that
      exercises your real per-sample path, not an idle early-out: `Default` (held defaults — most
      oscillators/filters/math), `Gate` (a `gate` input), `Clocked` (a `clock`-driven stepper, e.g.
      `sequencer`/`euclid`), `Notes`/`ChordSet` (note-event sinks), `Value` (a driven `in`),
      `Sample`/`Position` (the sampler/strummer). If none fit, add a new `Recipe` variant + its
      `apply_recipe` arm. Then prove it: `cargo test -p reuben-core --features bench`
      (`every_operator_has_a_micro_bench_workload`, `iai_list_covers_every_workload`, and
      `every_workload_renders` must pass).
   3. `cargo run -p reuben-core --example gen_schema` — regenerate + commit the schema (**owned
      here**: the staleness test fails otherwise, and `patcher` can't use the op until it's listed).
   4. `cargo clippy -p reuben-core --all-targets` — clean.
   5. `cargo fmt` — the CI `fmt` gate fails on any unformatted line; run it before you commit.
   6. `reuben describe <op> --json` — confirm the registered contract matches the freeze.
   7. `reuben validate <throwaway instrument using it>` — prove it wires in a real graph.
   8. **Honest audible caveat.** The above prove it compiles, registers, wires, and meets its
      written oracle — **not that it sounds right.** When behavior is subjective, recommend an
      ear-check: `patcher` a tiny instrument around it, then `reuben play`.

5. **Hand off the prose.** New ROADMAP/authoring/ARCHITECTURE lines and domain terms are the
   `sync-docs` skill's job — don't inline them here.

## The contract spec

```json
{
  "type_name": "tremolo",
  "inputs":  [ { "name": "in",   "ty": "f32_buffer" },
               { "name": "rate", "ty": "f32",
                 "f32": { "min": 0.1, "max": 20.0, "default": 5.0, "unit": "Hz", "curve": "exponential" } },
               { "name": "wave", "ty": "enum", "vocab": "Waveform" } ],
  "outputs": [ { "name": "out",  "ty": "f32_buffer" } ],
  "resources": []
}
```

- **`ty`** (ADR-0030) ∈ `f32_buffer` | `f32` | `i32` | `enum` | `note` | `harmony` | `arg` — the port's `Arg` type. A
  `f32_buffer` port is a dense audio/CV Signal (no settable default). A `f32` port is a held Value and
  adds `"f32": { min, max, default, unit, curve }` for its materialized default; `curve` ∈ `linear` |
  `exponential` (default linear), `unit` defaults `""`.
- An **`enum` port** names its shared *vocab* enum in `"vocab": "Waveform"` (PascalCase) — the
  descriptor reads its variants and `#[default]` from `Waveform::enum_meta`. The vocab type must
  already exist in `crates/reuben-core/src/vocab/`; if it's new, define it there, `#[derive(ArgValue)]`,
  and add one variant to `Arg` **first** (ADR-0030), then reference it here. `note`/`harmony` ports
  need no extra fields.
- The generated `IN_*`/`OUT_*`/`P_*` index consts follow declaration order — the scaffold renders
  the contract in `operator_contract!` grammar, so a `f32_buffer`/`f32 { .. }`/`enum(VocabType)` spec
  lands as the real port declaration, no Stage-B retyping.
- A **`Constant`** (ADR-0035) is a `PortSpec` in the top-level `constants` array — e.g.
  `"constants": [{ "name": "voices", "ty": "i32", "i32": { "min": 1, "max": 32, "default": 8 } }]` —
  the instantiate-time value that sizes the voice pool (the loader routes it to the patch's
  `config` block, ADR-0032).
- `resources: ["wave"]` adds a `ResourceSlot` and a `bind_resources` stub (ADR-0016).

## Scope

| Thing | Action |
|---|---|
| New Operator: Rust impl + descriptor + tests + registration | **author** (TDD `process`, close the gate) |
| Micro-bench workload (`bench_support.rs` + `micro_iai.rs`) | **register** the new op's `WORKLOADS`/`MICRO_IAI_KINDS`/`#[bench]` entries (part of the gate; CI's `check` job reds without it) |
| `instrument.schema.json` | **regenerate** via `gen_schema` after the op lands (part of the gate) |
| Instrument/Rig graphs | **never** — that is the `patcher` skill |
| Surface docs (`surfaces/*.json`) | **never** — that is the `control-surface` skill (ADR-0043) |
| ROADMAP / authoring.md / ARCHITECTURE / domain terms | **never inline** — hand to `sync-docs` |

## Report

End with: the operator's type name + one-line behavior, its final descriptor (ports/params), the
gate results (`cargo test` green, schema regenerated, `describe`/`validate` confirmations), and the
honest audible status — whether it was ear-checked or still needs one. Note that `sync-docs` should
sweep the prose, and `patcher` can now use the operator in instruments.
