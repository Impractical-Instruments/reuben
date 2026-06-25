---
name: create-operator
description: Author a new reuben Operator in Rust — the unit of DSP behind every instrument node. Aligns on the operator's contract (ports, params, behavior), scaffolds the skeleton + registration with `reuben scaffold-operator`, implements the `process` DSP test-first, and proves it compiles, registers, and validates. Use when the user says "make a new operator", "add an operator", "write a chorus/compressor/wavefolder", "I need a node that does X", or describes DSP behavior no existing operator provides.
---

# create-operator

An Operator is one unit of DSP — authored **single-Lane** (one mono voice; the engine fans it out
per-Voice, ADR-0010) as a Rust file in [`crates/reuben-core/src/operators/`](../../crates/reuben-core/src/operators):
index consts, a state struct, and `impl Operator` (`descriptor` / `process` / `spawn`), declared in
`operators/mod.rs` and self-registered by its own `register_operator!` line (no central list to
edit, ADR-0024). This skill authors that end-to-end (ADR-0021).

First check it doesn't already exist: `reuben describe --json` lists every operator — a request is
often a *patch* of existing ones (the `patcher` skill), not new Rust. This skill is only for
behavior no operator provides. It does **not** build graphs (`patcher`), write `control` blocks
(`control-surface`, ADR-0018), or edit the living docs (`sync-docs`). Its review mirror is
[`rust-hot-path-review`](../rust-hot-path-review/SKILL.md) — run that over the diff to check the
`process` you wrote stays RT-safe.

## The loop: align → scaffold → implement (TDD) → gate → hand off

Run all `reuben`/`cargo` commands from the repo root.

1. **Align on the contract.** The descriptor is frozen at scaffold time and the rig builder wires
   against its port/param **indices** — getting it wrong is expensive. If the operator is at all
   underspecified, **invoke the `grilling` skill** to pin: each input/output by its **`Arg` type**
   (ADR-0030) — `buffer` (a dense per-sample signal), `float` (a number; add
   `{ min..max, default, unit, lin|exp }` for its materialized default), `enum(VocabType)` (a
   live-switchable choice naming a shared *vocab* enum), or `note`/`harmony` for a `Note`/`Harmony`
   port — plus any **`Constant`** (instantiate-time `config`, e.g. `voices`), the lane rule, and —
   critically — **the DSP behavior and its test oracle: "how will we know it's right?"** Use
   `domain-modeling` for naming. Skip the interview only when the user hands a precise contract.

2. **Scaffold.** Write the contract to a JSON file (format below) and run:
   `cargo run -q -p reuben-native --bin reuben -- scaffold-operator --spec <contract.json>`
   This writes `operators/<name>.rs` (descriptor filled in, a silence-writing `process` stub, its
   own `register_operator!` self-registration line, and an **intentionally-red placeholder test**)
   and the sorted `mod.rs` inserts — `registry.rs` is not touched (ADR-0024). It refuses to clobber
   and rejects a malformed spec.

3. **Implement `process` test-first** — lean on the `tdd` skill. The scaffold starts you **red**;
   turn the contract's oracle into real tests (drive `process` via `Io::new`, assert observable
   output), then write the DSP to pass. **Copy the structure of
   [`lfo.rs`](../../crates/reuben-core/src/operators/lfo.rs)** — a clean stateful operator with a
   `run` harness and continuity/spawn tests. Honour the **realtime authoring contract**:
   - `process` runs on the **hot** path — it must not allocate, lock, block, or panic. The
     canonical hot/cold boundary + RT rules live in
     [authoring.md#rt-safe-render](../../docs/agents/authoring.md#rt-safe-render) (single
     source); the operator-specific contract follows.
   - **Single-Lane**: write one mono stream; ignore `io.lane()` unless you're an *expander*
     (`LaneRule::FromParam`, the Voicer pattern).
   - **Read each input by its `Arg` type** (ADR-0030) — the read view is a *static* choice, never
     conditional on what's wired:
     - **`Buffer` / `F32` control** → `io.signal(IN) -> &[f32]` (per-sample DSP — a `buffer` input
       or the materialized buffer of a `float` control), or `io.last::<f32>(IN) -> Option<f32>` for
       a `float` control's held scalar (block-rate). The buffer is always `io.frames()` long.
       `io.varying(IN)` lets a const-folding op skip recompute on a held block.
     - **enum** → `io.last::<MyVocabEnum>(IN).unwrap_or_default()` — a real Rust enum, not an index.
     - **`Harmony`** → `io.last::<Harmony>(IN)`; **`Note`** → `io.stream::<Note>(IN)` (an iterator of
       `Stamped<Note>` — `.frame`, `.payload`).
   - **Write outputs by `Arg` type** — `Buffer` → `io.signal_mut(OUT)`; emit a `Note` or publish a
     `Harmony` with `io.emit(OUT, addr, payload, frame)` where `payload` is one `Arg` (**Lane 0 only**).
   - **Persistent state carries across blocks** — keep phase/filter state in the struct; use `f64`
     for a phase accumulator so it doesn't drift (lfo/clock).
   - **`spawn`** resets per-Lane state but **carries any resource binding forward** (ADR-0016).
   - The **index consts are the contract** downstream nodes reference — don't renumber casually.

4. **Close the gate** — `validate` can't prove DSP is correct, so the gate is richer than the
   patcher's. In order:
   1. `cargo test -p reuben-core` — your tests (the real oracle), the registry self-registration
      invariants (your op is gathered, names stay unique + snake_case), and `committed_schema_is_in_sync`.
   2. `cargo run -p reuben-core --example gen_schema` — regenerate + commit the schema (**owned
      here**: the staleness test fails otherwise, and `patcher` can't use the op until it's listed).
   3. `cargo clippy -p reuben-core --all-targets` — clean.
   4. `reuben describe <op> --json` — confirm the registered contract matches the freeze.
   5. `reuben validate <throwaway instrument using it>` — prove it wires in a real graph.
   6. **Honest audible caveat.** The above prove it compiles, registers, wires, and meets its
      written oracle — **not that it sounds right.** When behavior is subjective, recommend an
      ear-check: `patcher` a tiny instrument around it, then `reuben play`.

5. **Hand off the prose.** New ROADMAP/authoring/ARCHITECTURE lines and domain terms are the
   `sync-docs` skill's job — don't inline them here.

## The contract spec

```json
{
  "type_name": "tremolo",
  "inputs":  [ { "name": "in",   "ty": "buffer" },
               { "name": "rate", "ty": "float",
                 "float": { "min": 0.1, "max": 20.0, "default": 5.0, "unit": "Hz", "curve": "exponential" } },
               { "name": "wave", "ty": "enum", "vocab": "Waveform" } ],
  "outputs": [ { "name": "out",  "ty": "buffer" } ],
  "resources": [],
  "lanes": "inherit"
}
```

- **`ty`** (ADR-0030) ∈ `buffer` | `float` | `enum` | `note` | `harmony` — the port's `Arg` type. A
  `buffer` port is a dense audio/CV signal (no settable default). A `float` port adds
  `"float": { min, max, default, unit, curve }` for a materialized `F32` control that owns a settable
  default (the old "signal port + same-named param", now one declaration); `curve` ∈ `linear` |
  `exponential` (default linear), `unit` defaults `""`.
- An **`enum` port** names its shared *vocab* enum in `"vocab": "Waveform"` (PascalCase) — the
  descriptor reads its variants and `#[default]` from `Waveform::enum_meta`. The vocab type must
  already exist in `crates/reuben-core/src/vocab/`; if it's new, define it there, `#[derive(ArgValue)]`,
  and add one variant to `Arg` **first** (ADR-0030), then reference it here. `note`/`harmony` ports
  need no extra fields.
- The generated `IN_*`/`OUT_*`/`P_*` index consts follow declaration order — the scaffold renders
  the contract in `operator_contract!` grammar, so a `buffer`/`float { .. }`/`enum(VocabType)` spec
  lands as the real port declaration, no Stage-B retyping.
- A **`Constant`** is declared as a `param` plus an expander lane rule: `"lanes": { "from_param":
  "voices" }` names the param that sizes lanes (the loader routes it to the patch's `config` block).
- `resources: ["wave"]` adds a `ResourceSlot` and a `bind_resources` stub (ADR-0016).

## Scope

| Thing | Action |
|---|---|
| New Operator: Rust impl + descriptor + tests + registration | **author** (TDD `process`, close the gate) |
| `instrument.schema.json` | **regenerate** via `gen_schema` after the op lands (part of the gate) |
| Instrument/Rig graphs | **never** — that is the `patcher` skill |
| `control` blocks | **never** — that is the `control-surface` skill (ADR-0018) |
| ROADMAP / authoring.md / ARCHITECTURE / domain terms | **never inline** — hand to `sync-docs` |

## Report

End with: the operator's type name + one-line behavior, its final descriptor (ports/params), the
gate results (`cargo test` green, schema regenerated, `describe`/`validate` confirmations), and the
honest audible status — whether it was ear-checked or still needs one. Note that `sync-docs` should
sweep the prose, and `patcher` can now use the operator in instruments.
