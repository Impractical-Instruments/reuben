# ADR-0084 — operator registration is a census in the module declaration, because no linker covers all three targets

**Overturns** [ADR-0081](0081-operator-self-registration-moves-to-linkme.md) — *"`linkme` replaces
`inventory` for both registries, on every target"*. That swap was implemented and abandoned: `linkme`
gates `#[distributed_slice]` behind a `target_os` allowlist that has no wasm32 entry, so it fixed
bare metal by breaking the browser. ADR-0081's *diagnosis* survives intact and is the reason this
decision exists; only its remedy is replaced.

**Overturns** [`composition-operators.md#operator-self-registration`](../rules/composition-operators.md#operator-self-registration)
— not on the clause ADR-0081 marked (which gatherer) but on the clause underneath it: that there is a
gatherer at all, and that registration happens *at the definition site*. It now happens in the module
declaration.

**Overturns** [`composition-operators.md#product-type-unpack-operators`](../rules/composition-operators.md#product-type-unpack-operators)
on its *"self-registers through inventory"* clause — the same restatement ADR-0081 flagged as needing
clearing in the fold, now false rather than merely stale.

**Overturns** [`web-product-process.md#static-link-operator-registration`](../rules/web-product-process.md#static-link-operator-registration)
— the `codegen-units = 1` obligation, which exists only to keep self-registration constructors alive
through linking. It is retirable for a *sound* reason now rather than a conditional one.

## Context

**Link-time distributed registration is a linker feature, and this repo's three targets have three
different linkers.** That is the generalization ADR-0081 did not have, because at the time only two
targets were in evidence. It is what makes "try a different crate" the wrong move.

The measured record, on the pinned toolchain:

| mechanism | `x86_64-unknown-linux-gnu` | `thumbv7em-none-eabihf` | `wasm32-unknown-unknown` |
| --- | --- | --- | --- |
| `inventory` 0.3 | works | **links clean, registry silently empty** — nothing runs `.init_array` | works |
| `linkme` 0.3.37 | works | works | **does not compile** |
| `ctor` | works | fails as `inventory` does — the same constructor mechanism | works |
| hand-rolled `#[link_section]` + `#[used]` | works | works | reimplements `linkme`, inherits its problem |

`linkme`'s failure is not a bug to wait out; it is a written-down boundary.
`linkme-impl-0.3.37/src/declaration.rs:136-148` emits the slice's boundary-symbol `extern` block under
`#[cfg(any(target_os = "none", "linux", "macos", "ios", "tvos", "android", "fuchsia", "illumos",
"freebsd", "openbsd", "psp"))]`, and everything outside that list takes an arm whose message is
`distributed_slice is not implemented for this platform`. `thumbv7em-none-eabihf` is `target_os =
"none"` and is covered; **`wasm32-unknown-unknown` is `target_os = "unknown"` and is not.** Base `dev`
builds `reuben-core` for wasm32 in ~2.8s; the `linkme` branch does not build it at all.

Each of these crates asks a linker to collect scattered definitions into one contiguous run — a named
ELF section swept by `__start_`/`__stop_` symbols, or a run of pre-main constructors. Both are
platform ABI, and a target either has an agreement about them or does not. Bare metal has no startup
that runs constructors; wasm32 has neither the section-boundary symbols nor a stable answer for them.
The set of platforms any one crate covers is the set its author has tested and written a `cfg` arm
for, and no such set covers ours today. **A fourth crate would be a fourth allowlist to be outside
of** — the correct response to that proposal is not to compare its allowlist to ours but to note that
we no longer need one.

The obligation that made the linker attractive was never *"collect at link time"*; it was **"adding an
operator must not edit a central list to merge-conflict on"** ([ADR-0024](../rules/rationale/composition-operators/operator-self-registration.md)).
That premise is what shifted. `crates/reuben-core/src/operators/mod.rs` is **already** a central,
alphabetical, must-edit list — 57 module declarations, one per operator module, plus a re-export
block. Every new operator edits it today. The linker was buying independence from a list that exists
anyway, and charging a per-target compatibility problem for it.

## Decision

**Get rid of the linker. Fold registration into the module declaration.**

`operators/mod.rs` invokes one `operator_census!` block whose entries are the modules, alphabetically.
Each entry emits three things that were three separate edits: the `pub mod`, the flat re-export, and
the operator's `OpReg` entry in a plain `const CENSUS: &[&[OpReg]]` that `Registry::builtin()`
iterates. Two entry forms, because operators arrive two ways:

```rust
crate::operator_census! {
    abs::*,           // a macro-generated family — splice the module's own OPERATORS array
    chord::{Chord},   // hand-written types, named
}
```

**The `::*` form is what makes this compose with the generating macros**, which was the design risk.
`number_operator_contract!` mints three operators from one `variants:` list and `unpack_op!` mints
one; a census that had to name every type would force a second edit whenever a variant was added. So
each family macro emits its module's own `OPERATORS: &[OpReg]`, and the census splices it. The
generated family's census stays its `variants:` declaration, exactly where it is today.

The boundary's second registry moves the same way, but **not into the same file**. Its submitters are
struct vocab types, not operators, so its census is `OSC_FORMS` in `vocab/mod.rs` — beside the types
it names, which is where the opt-out decision (`Harmony` has no wire form) is actually made. The
boundary consumes it and owns nothing in it.

`inventory` is dropped from the workspace. `register_operator!`/`register_osc_form!` — item macros
that *submitted* — become `op_reg!`/`osc_form!`, expression macros that *build a value*; both stay
`pub(crate)`, as do `OpReg`, `OscForm`, and the census arrays.

## Consequences

**Edits per new operator go from two to one**, which is the ergonomic claim that makes this a win
rather than a lateral move. Before: a `pub mod` line in `operators/mod.rs` *plus* a
`register_operator!` line at the definition site (plus, in practice, a `pub use` re-export line — the
scaffold wrote all three). After: one census line, which is all three. Adding a *variant* to an
existing generated family stays one edit, in the `variants:` list, as today. The scaffold now makes a
single sorted insert instead of two, and emits no registration in the operator file at all.

**The three properties ADR-0081 required to survive, survive — and one of them got a real test.**

- *Order-independent determinism.* `Registry::builtin()` delegates to a private
  `from_census(impl IntoIterator<Item = &OpReg>)`, which is what lets
  `permuting_the_census_yields_the_same_iteration_order` feed the identical entries forward and
  reversed and compare `type_names()`. This is not a `BTreeMap` tautology: census order is genuinely
  not name order (a family emits `abs_f32_value`, `abs_f32_signal`, `abs_i32_value` in declaration
  order), and mutating `Registry` to enumerate insertion order instead of the `BTreeMap` re-key makes
  the test fail. It was run that way to confirm it can.
- *The duplicate-name check stays in `builtin()`, not `register()`.* `register()` keeps
  last-writer-wins because it is the embedder override seam. Taking the census as a parameter is what
  let the assertion stay on the built-in path while still being reachable from a test.
- *Non-empty.* Kept, and demoted in the comments from a canary to cheap insurance: an array a
  function indexes cannot be dead-stripped, so the failure mode it guarded no longer exists. The
  comments that explained it in terms of linker dead-stripping are corrected rather than deleted —
  the assertion is still worth its two lines, for a different and smaller reason.

**The `codegen-units = 1` rule is retired on the mechanism, not on a measurement.** It exists because
the linker only pulls an rlib's object files whose symbols are referenced, so a codegen unit holding
nothing but operator impls and their constructors is dropped. A `const` array that
`Registry::builtin()` reads has no such unit: the descriptor and constructor `fn` pointers are
reachable from a symbol the caller names. Confirmed by building at `codegen-units = 256`, at
`lto = "fat"` + `opt-level = "s"`, and at both together — 79 operators in each — including from an
out-of-tree crate that depends on `reuben-core` by path and calls nothing but `Registry::builtin()`,
which is the scenario the rule actually described.

Worth recording honestly: **the historical failure no longer reproduces for `inventory` either.** The
same out-of-tree probe at `codegen-units = 256` against the pre-change tree also yields 79, where the
rule's rationale recorded 36 of 53. So the rule is retired on the strength of the mechanism no longer
existing, not on a fresh reproduction of the harm — and anyone re-reading that rationale should know
its measurement is from a much older toolchain. `[profile.bench] codegen-units = 1` is **untouched**:
it pins codegen determinism for the Ir perf gate, an unrelated reason.

**The MSRV `SHF_GNU_RETAIN` reasoning the `linkme` branch added is never introduced.** It documented
when `#[used]` sections survive `--gc-sections`, which bound `rust-toolchain.toml`'s bump procedure to
linker behavior. Nothing here depends on `#[used]` or on section retention, so the toolchain pin owes
that constraint nothing.

**What is given up is real, and it is smaller than it looks.** An operator's registration is no longer
adjacent to its definition, so the two can drift: a module can exist and not be registered. That
failure is *loud in the direction that matters* — an unregistered operator has no `describe` row and
no schema entry, which its own tests notice — and the reverse, a census entry naming a type that does
not exist, is a compile error. `pipe` is deliberately declared outside the census and is the standing
example that the distinction is intentional.

**A third-party operator crate can no longer register into the built-in set at all.** Under `linkme`
that was the headline property: slices are declared by the core, so an out-of-crate submitter needed
no linker-script fragment. It is worth being clear that this decision spends it. It costs nothing
today — nothing outside `reuben-core` registers an operator or an OSC form, and both macros were
already `pub(crate)`, so the capability was never exposed. The supported seam for an embedder remains
`Registry::register`, which is a runtime call, needs no linker cooperation, and works identically on
all three targets. If a plugin story is ever wanted, that is the seam to build it on rather than a
link section.
