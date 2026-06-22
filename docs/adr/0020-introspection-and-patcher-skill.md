# Introspection API and the Patcher skill: `describe` + `validate`, build-on-the-loader

## Context

[ADR-0004](0004-ai-authorability-first-class.md) makes AI-agent authorability first-class:
Operators are self-describing, the instrument format is JSON with a generated schema, and an
**introspection/query API** is "likely needed" so an agent can explore the system. That API was
the last ungrilled item in [OPEN-QUESTIONS](../OPEN-QUESTIONS.md) before the **Patcher skill**
([ROADMAP](../ROADMAP.md) V1.6) — "build/modify Instruments and Rigs via the JSON schema +
introspection API" — could be built. The roadmap flags the skill explicitly: *forces the
introspection/query API shape — grill first.*

The pieces an agent needs to author an instrument already exist as data: the
[`Registry`](../../crates/reuben-core/src/registry.rs) iterates every operator's
[`Descriptor`](../../crates/reuben-core/src/descriptor.rs) (ports, rich `ParamMeta`, resource
slots) in deterministic order — the same source the schema generator reads. The full authoring
load path, [`load_instrument`](../../crates/reuben-core/src/format.rs), already type-checks an
instrument: unknown type/node/port/param, duplicate address, and **Signal↔Message kind
mismatch** are all fatal `LoadError`s; missing resources are non-fatal `LoadWarning`s (ADR-0016);
cycles surface one layer down at `Plan::instantiate`.

So the gap was not *capability* but *a closed feedback loop the agent can drive*: a way to
inspect one operator without grepping Rust, and a way to **check a drafted instrument without
launching the audio player** (which binds a device and makes sound).

This ADR settles the introspection API's shape and the skill that rides it.

## Decision

### A thin introspection CLI, not a live-graph query service

The introspection surface is two `reuben` subcommands over static data and the real load path —
**not** a query API into a running engine:

- **`reuben describe [op] [--json]`** — with no argument, lists every registered operator; with
  one, dumps that operator's ports (`name` + `kind` ∈ signal/message/context), params
  (`name`, `default`, `min`, `max`, `unit`, `curve`), and resource slots. Reads the live
  `Registry::builtin()` so it can never drift from the operators actually compiled in. Redundant
  with the JSON schema by construction, but a focused per-operator view beats reading
  JSON-Schema or `registry.rs`.
- **`reuben validate <path> [--json]`** — the closed loop. Runs the **real** `load_instrument`
  **plus `Plan::instantiate`** against a synthetic [`AudioConfig::default()`], so it catches
  everything the engine would (structural, wiring, kind-mismatch, **and cycles**) with **no
  audio device opened and nothing rendered**.

**Live-graph query is deferred** — inspect/traverse a *running* rig's state. It has no consumer
yet (reuben is OSC-in only; there is no running-engine handle to interrogate) and would add a
runtime/OSC surface. The static + load-path pair is everything the Patcher skill needs.

### Validate on the loader, not a second schema check

`validate` is defined as "does the engine's own load + plan path accept this?" — the loader is
the **single source of truth**. We deliberately do **not** also run a JSON-Schema validation
pass: it would be a second, drifting authority for the same rules the loader already enforces.
The schema stays an *authoring aid the agent reads*, not a validation gate. Concretely:

- **`ok`** ⟺ `load_instrument` succeeds *and* `Plan::instantiate` finds no cycle.
- **Errors** carry the loader's human message verbatim, plus the **node/port** the loader
  already localized, lifted into structured fields so an agent jumps straight to the offending
  node. Cycle has no single offending node, so it reports node-less.
- **Warnings are advisory and never flip `ok`** (ADR-0016): an unresolved sample plays silence;
  the instrument is still valid. `validate` exits **1** only on a hard error; warnings alone
  stay exit **0**.

What `validate` deliberately **cannot** catch is *semantic* emptiness: an instrument with a
disconnected oscillator or no declared output is structurally legal and validates clean, but
makes no sound (unconnected inputs read as silence; out-of-range params clamp — both by design).
The skill, not the validator, owns that gap.

### `describe`/`validate` are pure library functions; the binary is a thin shell

The logic lives in [`reuben_native::cli`](../../crates/reuben-native/src/cli.rs) as pure
functions over `Registry` + JSON returning serde-serializable reports
(`OperatorInfo`, `ValidateReport`); the `reuben` binary only parses args (clap), calls them,
prints human text or `--json`, and maps `ok` to an exit code. Tests exercise the real load/plan
code paths through the library, not a spawned process.

### The Patcher skill: introspect → draft → validate-loop → report

The skill ([`.claude/skills/patcher/`](../../.claude/skills/patcher/SKILL.md)) authors and
modifies instruments (one recursive JSON graph — Operator/Instrument/Rig are scales, not file
types; ADR-0003). Its loop is: **`describe`** the operators it needs → **draft** the graph from
a canonical recipe, an example instrument, and the schema → **`validate --json`** until `ok` →
report. Because `validate` is silent on audible-but-empty patches, the skill carries *moderate
semantic guidance* — canonical sub-graph recipes (a voice chain), a Signal-vs-Message note, and
the gotcha that **validate-pass ≠ audible** — while staying thin on syntax the loader enforces.

**Scope boundaries:** the skill does graph topology + params only. It does **not** author new
Operators (that is the Rust "Developer skill"), write `control` blocks (that is the
`control-surface` skill, ADR-0018), or touch the schema/core.

## Consequences

- **Engine/CLI changes:**
  - New [`reuben_native::cli`](../../crates/reuben-native/src/cli.rs) module: `describe`,
    `validate`, and their serde report types. Adds `serde`/`serde_json` to `reuben-native`.
  - The `reuben` binary moves to **clap subcommands** (`play` / `describe` / `validate`); the
    bare `reuben foo.json` play form is **removed** in favour of `reuben play foo.json`.
    Pre-1.0, no external users; clap buys `--help`/`--json` and a clean place for future
    subcommands. Adds `clap` (already transitively in the lock).
- **Closes the introspection/query API thread** in OPEN-QUESTIONS — the static-data + load-path
  shape, with live-graph query explicitly deferred until a running-engine consumer exists.
- **Unblocks the Patcher skill** (V1.6) and gives every future authoring skill the same
  build → `validate` → fix loop. The `control-surface` skill (ADR-0018) can lean on `validate`
  after its write-back.
- **Not built:** live-graph/running-rig query; two-way state read; a JSON-Schema validation gate
  (loader is sole authority); detection of audible-but-empty patches (a skill concern, not a
  validator one).
