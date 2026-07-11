---
name: create-operator
description: Author a new reuben Operator in Rust — the unit of DSP behind every instrument node. Aligns on the operator's contract (ports, params, behavior), scaffolds the skeleton + registration with `reuben scaffold-operator`, implements the `process` DSP test-first, and proves it compiles, registers, and validates. Use when the user says "make a new operator", "add an operator", "write a chorus/compressor/wavefolder", "I need a node that does X", or describes DSP behavior no existing operator provides.
---

# create-operator

An Operator is one unit of DSP — authored **single-Voice** (one mono stream; polyphony comes from the
Voicer hosting voice sub-patches, ADR-0010/0032) as a Rust file in [`crates/reuben-core/src/operators/`](../../crates/reuben-core/src/operators):
index consts, a state struct, and `impl Operator` (`descriptor` / `process` / `spawn`), declared in
`operators/mod.rs` and self-registered by its own `register_operator!` line (no central list to
edit, ADR-0024). This skill authors that end-to-end (ADR-0021). The **canonical
operator-development contract** — the trait, the `operator_contract!` macro, registration,
`OpDriver`, the RT-safety rules — lives in
[docs/agents/operator-dev.md](../../docs/agents/operator-dev.md); this skill is the workflow
that drives it, not a second copy of it.

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
   and form — `f32_buffer`, `f32` (with `{ min..max, default, unit, lin|exp }`),
   `enum(VocabType)`, `note`/`harmony`; the type system is canonical in the
   [guide's type-system section](../../docs/agents/authoring.md#type-system) — plus any
   **`Constant`** (instantiate-time `config`, e.g. `voices`), and — critically —
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
   `OpDriver` continuity/spawn tests. The **contract you are writing against** is canonical in
   [docs/agents/operator-dev.md](../../docs/agents/operator-dev.md) — read it before writing
   DSP rather than recalling it from here: single-Voice authoring, the typed-handle read/write
   shapes (ADR-0037), state-across-blocks and `spawn` semantics, and the RT rules — `process`
   runs on the **hot** path and must not allocate, lock, block, or panic (the hot/cold boundary
   and hot-path totality live at
   [operator-dev.md#rt-safe-render](../../docs/agents/operator-dev.md#rt-safe-render)).

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
  `f32_buffer` port is a dense audio/CV Signal (no settable default *in the spec* — see the
  Signal-with-default bullet below). A `f32` port is a held Value and
  adds `"f32": { min, max, default, unit, curve }` for its materialized default; `curve` ∈ `linear` |
  `exponential` (default linear), `unit` defaults `""`.
- A **Signal input with a scalar default** (ADR-0031 — knob-set it materializes as a held buffer,
  an LFO/envelope wires straight in; `filter.cutoff`, `saturator.drive`) can't carry its `{ .. }`
  meta in the spec JSON: declare it as a bare `f32_buffer`, scaffold, then hand-add the meta to the
  generated `operator_contract!` line — e.g. `drive: f32_buffer { 1.0..=30.0, default 2.5, "x", exp }`.
  The macro grammar supports it; this is the one intended post-scaffold contract edit.
- An **`enum` port** names its shared *vocab* enum in `"vocab": "Waveform"` (PascalCase) — the
  descriptor reads its variants and `#[default]` from `Waveform::enum_meta`. The vocab type must
  already exist in `crates/reuben-core/src/vocab/`; if it's new, define it there, `#[derive(ArgValue)]`,
  and add one variant to `Arg` **first** (ADR-0030), then reference it here. `note`/`harmony` ports
  need no extra fields.
- The generated `IN_*`/`OUT_*`/`P_*` index consts follow declaration order — the scaffold renders
  the contract in `operator_contract!` grammar, so a `f32_buffer`/`f32 { .. }`/`enum(VocabType)` spec
  lands as the real port declaration, no Stage-B retyping (sole exception: adding `{ .. }` meta to a
  Signal-with-default input, above).
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
| authoring.md / operator-dev.md / ARCHITECTURE / README / domain terms | **never inline** — hand to `sync-docs` |

## Report

End with: the operator's type name + one-line behavior, its final descriptor (ports/params), the
gate results (`cargo test` green, schema regenerated, `describe`/`validate` confirmations), and the
honest audible status — whether it was ear-checked or still needs one. Note that `sync-docs` should
sweep the prose, and `patcher` can now use the operator in instruments.
