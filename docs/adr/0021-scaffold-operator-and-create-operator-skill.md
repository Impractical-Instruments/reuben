# Scaffolding a new Operator: the `scaffold-operator` subcommand and the `create-operator` skill

> **Superseded in part by [ADR-0024](0024-compile-time-operator-registration.md).** This ADR
> describes registration as edits to **three sites** (`mod.rs`, `registry.rs` `builtin()`, and a
> name-list test). ADR-0024 replaced the latter two with compile-time self-registration: an
> operator carries its own `register_operator!` line, `builtin()` gathers them, and the name-list
> test is gone. The scaffold now edits only `mod.rs` (and emits the self-registration line in the
> generated file). Everything else here — the subcommand, the red placeholder test, the skill loop
> — stands.

## Context

[ADR-0004](0004-ai-authorability-first-class.md) frames a suite of agent skills serving three
audiences: **developers** (scaffold a new Operator: Rust + descriptor + tests),
**patchers** (build/modify Instruments and Rigs), and **end users** (natural language → Toy).
The patcher audience shipped as the `patcher` skill ([ADR-0020](0020-introspection-and-patcher-skill.md));
the control-surface slice shipped as `control-surface` ([ADR-0018](0018-control-surface-generation.md)).
This ADR settles the **developer** audience — the [ROADMAP](../ROADMAP.md) V1.6 item that doc
prose has been calling the "Developer skill."

That label names the *audience*, not the artifact, which makes it the odd one out next to its
siblings (`patcher`, `control-surface`, `sync-docs` — all named for what they produce). The skill
is named **`create-operator`**: it authors one new Operator end-to-end.

Creating an Operator in this codebase is a fixed, multi-site mechanical act on top of a genuinely
creative one:

- **Mechanical (deterministic).** A new operator is a file
  [`operators/<name>.rs`](../../crates/reuben-core/src/operators) — index consts, a state struct,
  `new()`, `impl Operator` (`descriptor`/`process`/`spawn`), and a `#[cfg(test)]` module — plus
  registration at **three sites**: [`operators/mod.rs`](../../crates/reuben-core/src/operators/mod.rs)
  (`pub mod` + `pub use`, sorted), [`registry.rs`](../../crates/reuben-core/src/registry.rs)
  `builtin()`, and that file's name-list test (sorted insert). The
  [`Descriptor`](../../crates/reuben-core/src/descriptor.rs) is fully determined by a contract
  spec (ports, params, lanes); only `process` carries real logic.
- **Creative (not deterministic).** The DSP in `process`, and the behavioral tests that pin it
  down. No tool proves this correct — `validate` checks topology, not whether a filter filters
  ([ADR-0020](0020-introspection-and-patcher-skill.md): "validate-pass ≠ audible").

The gap, as with the patcher, is a closed feedback loop — but here the deterministic half is
*code generation across three hand-maintained Rust files*, which an LLM gets subtly wrong
(unsorted inserts, a missed name-list entry, a malformed descriptor) far more often than it gets
a single JSON edit wrong.

## Decision

### A `reuben scaffold-operator` subcommand — deterministic codegen, not a skill script

The mechanical half is a fourth `reuben` subcommand, mirroring how [ADR-0020](0020-introspection-and-patcher-skill.md)
put `describe`/`validate` in [`reuben_native::cli`](../../crates/reuben-native/src/cli.rs) as pure,
in-crate-tested functions rather than brittle external shell scripts. The choice over a
skill-local script is deliberate:

- The error-prone part is **parsing and editing Rust source** (sorted insertion into `mod.rs`,
  `registry.rs`, and the name-list test). That is far more robust as tested Rust than as a
  regex script outside the crate.
- It is a **first-class author tool**, usable by a human writing an operator by hand — not just
  agent plumbing. Scaffolding boilerplate is tedious whoever does it.

`reuben scaffold-operator --spec contract.json [--json]`:

- **`contract.json`** mirrors the `Descriptor`: `type_name`, `inputs:[{name,kind}]`,
  `outputs:[{name,kind}]`, `params:[{name,min,max,default,unit,curve}]`, `resources`, `lanes`.
  Deterministic input — an agent writes it from the design interview; a human hand-writes it.
- **Writes** `operators/<name>.rs`: index consts, state struct, `new()`, a full `impl Operator`
  with the **descriptor filled in from the spec**, a `process` stub (writes silence / `todo!`),
  `spawn`, and a test module with an `Io::new` `run` harness plus **one intentionally-failing
  placeholder test** — so the author starts Stage B red (see below).
- **Edits** the three registration sites in sorted position.
- **Does not** regenerate the schema (the skill's gate owns `gen_schema`), write `process` DSP,
  or touch docs.
- **Refuses to clobber**: errors (non-zero exit) if `<name>.rs` exists or the type is already
  registered, and **rejects a malformed spec** (unknown port kind, unknown curve, missing field)
  before writing anything.

The logic lives in `reuben_native::cli` as pure functions — render the skeleton from a spec,
insert into `mod.rs`/`registry.rs` content — tested directly on strings and on a temp fixture,
not through a spawned process. The binary is a thin shell, as for `describe`/`validate`.

### The intentionally-red placeholder test

The generated test module compiles and **fails**. This is not an oversight: it seats the operator
in the engine's two-stage authoring rhythm already documented in `operators/mod.rs` — **Stage A**
freezes ports/params (now the `contract.json` + the generated descriptor), **Stage B** fills in
behavior **test-first**. A green-on-arrival stub would invite shipping a silent operator; a red
one makes "make this pass" the first and obvious step.

### The `create-operator` skill: grill → scaffold → TDD → gate → hand off

The skill ([`.claude/skills/create-operator/`](../../.claude/skills/create-operator/SKILL.md)) is
the full author for the developer audience. Its loop:

1. **Align on the contract.** If the operator is underspecified, delegate to the `grilling` skill
   for a focused Stage-A interview — ports + kinds, params + metadata, lane rule, and the DSP
   behavior **plus its test oracle** ("how do we know it's right?"). Skip when the user hands a
   precise contract. Lean on `domain-modeling` for naming.
2. **Scaffold.** Write `contract.json`, run `scaffold-operator`.
3. **Implement Stage B test-first** (lean on the `tdd` skill): turn the oracle into real tests,
   drive `process` red → green. The skill carries the **realtime authoring contract** as moderate
   semantic guidance — `process` must not allocate; single-Lane authoring (ADR-0010); params are
   constant for the call (ADR-0011); persistent state carries across blocks (f64 phase to avoid
   drift); `spawn` resets per-Lane state but carries resource bindings; `emit`/`publish_context`
   are Lane-0 only; **index consts are the wiring contract** — with `lfo.rs` as the named exemplar
   to copy structure from.
4. **Close the gate** (the developer analog of patcher's `validate → ok`, richer because no single
   command proves DSP correctness): `cargo test -p reuben-core` (the op's tests, the registry
   name-list test, the `schema_is_in_sync` test) → regenerate the schema
   (`cargo run -p reuben-core --example gen_schema`, **owned here** — the staleness test fails
   otherwise and the patcher can't use the op until the schema lists it) → `cargo clippy` →
   `reuben describe <op>` (confirm the registered contract matches the freeze) →
   `reuben validate <throwaway instrument>` (prove it wires in a real graph) → an **honest
   audible caveat**: these prove it compiles, registers, wires, and meets its written oracle, not
   that it *sounds* right; recommend an ear-check via `patcher`/`run` when behavior is subjective.
5. **Hand off prose** — ROADMAP V1.6, `docs/agents/authoring.md`, ARCHITECTURE, new domain terms —
   to the `sync-docs` skill rather than inlining.

**Scope boundaries:** the skill owns the Rust operator + registration + schema + tests. It does
**not** author graphs (`patcher`), write `control` blocks (`control-surface`, ADR-0018), or edit
the living docs (`sync-docs`).

## Consequences

- **Engine/CLI changes:** a `scaffold` function (and spec types + the pure source-edit
  transforms) added to `reuben_native::cli`; a fourth `reuben` subcommand `scaffold-operator`.
  No change to the core runtime or the engine; scaffolding is author-time only.
- **Closes the "Developer skill" ROADMAP V1.6 item** and completes the developer-audience leg of
  ADR-0004. Every authoring audience now has a skill that closes its own build → check loop.
- **Naming settled:** the skill is `create-operator`; the "Developer skill" label is retired from
  prose (patcher's SKILL.md and the ROADMAP updated).
- **Deliberately not built:** scaffolding does not generate DSP or real tests (creative, not
  deterministic — owned by the TDD step); it does not regenerate the schema or edit docs (owned
  by the gate and `sync-docs`); there is no detection of audible-but-empty operators (an ear
  concern, as audible-but-empty patches are for the patcher).

## Update (ADR-0025)

The scaffold no longer emits a hand-written const block plus a `Descriptor` literal: per
[ADR-0025](0025-single-source-operator-contract.md) it emits a single `operator_contract!` call,
and its spec types + validator are shared with that macro via the `reuben-contract` crate.
