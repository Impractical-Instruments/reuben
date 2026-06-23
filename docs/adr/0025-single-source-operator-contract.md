# Single-source the operator port/param contract via `operator_contract!`

## Context

Every operator declared its ports and params **twice**:

1. By **name**, in `descriptor()` — a runtime `Vec<Port>` / `Vec<ParamMeta>`.
2. By **integer slot**, in a hand-written `pub const IN_/OUT_/P_` block.

`process()` reads raw slots (`io.param(P_FREQ)`, `io.output(OUT_AUDIO)`), and nothing checked the
two declarations against each other. The slot space is **per-kind** (ADR-0010): signal, message,
and context inputs are separate index spaces, so `voicer.rs` legitimately has `IN_NOTES = 0` *and*
`IN_CTX = 0`. That makes the hand-written const block a live footgun — a wrong ordinal, or a const
that drifts from the descriptor after an edit, compiles fine and fails silently at run time.

[`scaffold-operator`](0021-scaffold-operator-and-create-operator-skill.md) generated both halves
for a *new* operator, but that only postpones the drift: once the file exists, the two halves are
edited by hand and can diverge. This is the same disease ADR-0024 cured for *registration* — a
fact stated in two places that must agree — one layer down.

The fix is the same shape: **declare once, generate both halves**, so disagreement becomes a
compile error rather than a runtime surprise.

## Decision

### One declaration, a proc-macro emits both halves

A new `operator_contract!` macro takes a single contract declaration and plants, at module scope,
the index consts **and** an inherent `fn contract() -> Descriptor`:

```rust
crate::operator_contract!(Oscillator {
    inputs:  { freq: signal },
    outputs: { audio: signal },
    params:  { freq:     { 20.0..=20_000.0, default 440.0, "Hz", exp },
               waveform: { 0.0..=1.0,        default 0.0,   "",  lin } },
    lanes: inherit,
});

impl Operator for Oscillator {
    fn descriptor() -> Descriptor { Self::contract() }   // delegate — see "Shape A"
    fn process(&mut self, io: &mut Io) { /* io.param(P_FREQ) — unchanged */ }
    fn spawn(&self) -> Box<dyn Operator> { Box::new(Self::new()) }
}
```

The const ordinals and the descriptor's `vec`s are computed from the **same tokens** by the same
pass, so name↔slot drift is impossible by construction. `process()` is untouched: it still reads
`io.param(P_FREQ)` against the macro-planted `P_FREQ`.

### Shape A (delegate) — the Rust constraint that forced it

`descriptor()` and `process()` are both required methods of `trait Operator`, and a trait `impl`
must be one block. A macro **cannot** inject `descriptor()` into the author's hand-written `impl
Operator` (two `impl Operator for T` blocks is a duplicate-impl error). So the macro emits an
*inherent* `impl T { pub fn contract() -> Descriptor }` at module scope, and the trait impl
delegates with a one-liner: `fn descriptor() -> Descriptor { Self::contract() }`. The macro is
**contract-only** — it does not try to own the whole operator or generate `process`.

### Three crates, no cycle

- **`reuben-contract`** (new, pure leaf): the spec types (`OperatorSpec` / `PortSpec` /
  `ParamSpec` / `LaneSpec`), the naming rules (`screaming`, `struct_name`,
  `type_name_from_struct`), and the **one** `validate()`. Depends only on `serde`.
- **`reuben-macros`** (new, `proc-macro = true`): parses the `operator_contract!` grammar with
  `syn`, builds the contract model (per-kind ordinals, param indices — the old
  `scaffold::port_consts` arithmetic, now computed **once** here), validates via `reuben-contract`,
  and emits tokens with `quote`. Its pure core (`model::build`) and its expansion (`expand`) are
  unit-tested directly.
- **`reuben-core`** depends on `reuben-macros`. `reuben-native`'s scaffold depends on
  `reuben-contract`.

A proc-macro crate can only export macros, so the shared validator/spec types **cannot** live in
`reuben-macros` (the scaffold could not call them). And they cannot live in `reuben-core` (the
macro can't depend on core without a cycle). Hence the dedicated `reuben-contract` leaf — the
single home both the macro and the scaffold import. One validator, not a macro copy and a scaffold
copy that could themselves drift (the disease, recursively).

### `extern crate self as reuben_core`

The macro emits fully-qualified `::reuben_core::descriptor::…` paths so it works for any embedder.
For those paths to resolve **inside** `reuben-core` itself (which uses the macro), the crate
aliases itself: `extern crate self as reuben_core;` in `lib.rs`. Proc-macros have no `$crate`.

### The scaffold emits the macro call, and shares the validator

`scaffold-operator` now emits a single `operator_contract!(…)` invocation in place of the const
block + the `Descriptor` literal, and a `descriptor()` that delegates to `Self::contract()`. It
validates through the same `reuben-contract::validate`. All the descriptor-literal rendering
(`render_ports` / `render_params` / `render_lanes`) and the `port_consts` ordinal arithmetic are
deleted from the scaffold — that logic now lives once, in the macro.

### Golden descriptor snapshots are the migration oracle

`tests/golden/descriptors.txt` pins every built-in operator's `descriptor()` output in a canonical
form, snapshotted **before** any migration. Moving an operator to the macro must leave its
descriptor byte-identical, so `descriptors_match_golden` is what proves the macro reproduces —
exactly — what was hand-written (per-kind ordinals, param order, curves, units, Lane rule). The
committed JSON schema (`committed_schema_is_in_sync`, ADR-0024) is a second, independent witness:
it is generated from the descriptors and is unchanged by this refactor.

### Scope of the migration

Sixteen operators with a **static, hand-enumerated** contract were migrated:
`oscillator, output, noise, lfo, m2s, delay, reverb, filter, djfilter, envelope, clock, chord,
strum, snap, sample, voicer` — covering every shape: per-kind ordinals + `LaneRule::FromParam`
(voicer), resources (sample), keyword-named ports (m2s's `in`), and six-param param banks
(djfilter). Three files are **documented exceptions**, left hand-written:

- **`math.rs`** packs five operators (`add`/`mul` via `signal_pointwise!`, plus `map`,
  `differentiate`, `integrate`) in one module. The macro plants **module-scope** consts, so two
  operators in one module that share a port name (`in`/`out`) would emit duplicate consts. Its
  signal ops are already drift-controlled by `signal_pointwise!` (descriptor generated from the
  same literals); a per-operator-module rule does not fit a deliberately-shared module.
- **`context.rs`** and **`sequencer.rs`** build their param list **programmatically** (a loop over
  `NUM_STEPS` scale/pattern steps) and index it with const arithmetic (`P_STEP0 + i`). The macro's
  grammar is a static enumeration; expressing a generated param bank is out of its designed scope.

The golden snapshot still guards these three — proving they did **not** change — so they are
covered even though they are not migrated.

### Typed accessors are explicitly out of scope (a separable "1b")

The macro does **not** generate typed accessors; `process` keeps `io.param(P_FREQ)`. Generating
`self.freq(io)` to make wrong-slot reads impossible is a separable follow-on, taken only if such
bugs actually appear.

## Alternatives considered

- **Detect drift with a runtime/test check** instead of preventing it. Treats the symptom: the two
  declarations still exist and can disagree between checks. Prevention by codegen removes the
  second declaration entirely.
- **A whole-operator macro** that also generates `process`. Rejected as decision #2: it would
  swallow the DSP body — the interesting, test-first part — and fight Shape A's trait constraint.
  `signal_pointwise!` already shows the fused form is right *only* for a family of near-identical
  ops, not the general case.
- **`build.rs` that scans `operators/`.** Same objection as ADR-0024: directory structure ≠
  contract, and parsing Rust in a build script reinvents the compiler. The declaration belongs at
  the `impl`, and proc-macro expansion is the language's seam for it.
- **Per-operator submodules** so even `math.rs`'s ops get module-scope consts. A larger structural
  change that loses `math.rs`'s "arithmetic written once" cohesion for little gain; the exception
  is cheaper and honestly documented.

## Consequences

- **Dependencies:** `syn` / `quote` / `proc-macro2` added to `[workspace.dependencies]`; two new
  crates (`reuben-contract`, `reuben-macros`). Build-time only — the RT path is unchanged.
- **Drift is now a compile error,** not a silent runtime mismatch: the consts and the descriptor
  cannot disagree because they share one source.
- **Greppability shifts.** `grep IN_FREQ` lands on the `operator_contract!` call, not a literal
  `const` — `operator_contract!` becomes the greppable census of an operator's ports, the same
  trade ADR-0024 made with `register_operator!`. The consts still exist (macro-expanded) and are
  still `oscillator::IN_FREQ` to downstream code.
- **Validation is single-sourced** across the macro and the scaffold; a bad contract is rejected
  with a **span** at macro-expansion time (the offending port/param token is underlined), and the
  scaffold rejects the same spec before writing a file.
- **Supersedes in part** the hand-written-const half of
  [ADR-0010](0010-single-lane-operators.md) (per-kind ordinals are now computed by the macro) and
  [ADR-0021](0021-scaffold-operator-and-create-operator-skill.md) (the scaffold emits a macro call,
  not a const block + descriptor literal). It sits beside
  [ADR-0024](0024-compile-time-operator-registration.md): `register_operator!` self-registers an
  operator; `operator_contract!` self-describes it. Both turn a stated-twice fact into one.
