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
"freebsd", "openbsd", "psp"))]`. `uefi` and `windows` have their own supported arm below it
(`declaration.rs:179-194`, PE/COFF boundary elements); a target matching *neither* set falls through
to an arm whose message is `distributed_slice is not implemented for this platform`.
`thumbv7em-none-eabihf` is `target_os = "none"` and is covered by the first;
**`wasm32-unknown-unknown` is `target_os = "unknown"` and is in neither.** Base `dev` builds
`reuben-core` for wasm32 in ~2.8s; the `linkme` branch does not build it at all.

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
alphabetical, must-edit list — 53 `pub mod` declarations and 48 `pub use` re-exports, of which 48
modules carry operators. Every new operator edits it today. The linker was buying independence from a list that exists
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

**Edits per new operator go from three to one**, which is the ergonomic claim that makes this a win
rather than a lateral move. Before: a `pub mod` line in `operators/mod.rs`, a `pub use` re-export
line beside it, *and* a `register_operator!` line at the definition site — the scaffold wrote all
three. After: one census line, which is all three. Adding a *variant* to an
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

**The `codegen-units = 1` rule is retired because the mechanism is gone — and it was live and
load-bearing right up to this commit.** It exists because the linker only pulls an rlib's object
files whose symbols are referenced, so a codegen unit holding nothing but operator impls and their
constructors is dropped. A `const` array that `Registry::builtin()` reads has no such unit: the
descriptor and constructor `fn` pointers are reachable from a symbol the caller names.

**Measuring this needs the link shape the rule actually names** — *"anyone building core into a
single statically-linked artifact, which the C-ABI browser boundary invites third parties to do"*. A
Rust crate with a path dependency is the wrong shape and cannot show the hazard at all: rustc drives
that link and hands the linker the whole rlib, so nothing is ever extracted per object file. Built
the right way instead — `--crate-type staticlib`, an `extern "C"` entry calling only
`Registry::builtin()`, consumed by a C `main` through the system linker, on the pinned 1.96.0:

| tree | `cgu = 1` | `cgu = 16` | `cgu = 256` |
| --- | ---: | ---: | ---: |
| pre-change (`inventory`) | 79 | 79 | **0** |
| this branch | 79 | 79 | **79** |

The browser shape agrees. As a `wasm32-unknown-unknown` `cdylib`, the pre-change tree at
`cgu = 256` retains **1** of 79 operator `type_name`s in a 39 KB image — the rest of the engine
stripped with the constructors that were its only reference — while this branch retains all 79. Both
trees are correct at `cgu = 1`, which is precisely what the rule was buying.

So the retirement rests on *both* halves: the harm was real until this commit, and the mechanism that
caused it no longer exists. Anyone tempted to read the old 36-of-53 figure as folklore should note it
was, if anything, understated. `[profile.bench] codegen-units = 1` is **untouched**: it pins codegen
determinism for the Ir perf gate, an unrelated reason.

**The MSRV `SHF_GNU_RETAIN` reasoning the `linkme` branch added is never introduced.** It documented
when `#[used]` sections survive `--gc-sections`, which bound `rust-toolchain.toml`'s bump procedure to
linker behavior. Nothing here depends on `#[used]` or on section retention, so the toolchain pin owes
that constraint nothing.

**What is given up is real, and it had to be paid for.** An operator's registration is no longer
adjacent to its definition, so the two can drift. One direction is free: a census entry naming a type
that does not exist is a compile error. The other direction is **completely silent**, and this was
checked rather than assumed. A hand-written operator added to a module the census splices with `m::*`
builds with zero warnings — it is `pub`-reachable through the glob, so no dead-code lint fires — and
`describe` simply does not list it; its own tests pass, because an operator test drives
`OpDriver::for_type` directly and never consults the registry. Narrowing an `m::*` entry to
`m::{OneType}` is quieter still: it compiles clean and drops the family's other variants, 79 → 77.

So the drift is made loud by a test rather than by hope. `census_accounts_for_every_operator` reads
`src/operators/*.rs` at test time and asserts that every hand-written `impl Operator` is named by a
census entry, and that every module invoking a family macro is censused with the `*` form. Both sides
are **derived from source**: a hand-maintained roster of expected operators would be precisely the
central list this change exists to delete, and would rot the same way. `Pipe` is the one exception,
and it is pinned rather than skipped — the test also asserts `pipe` stays *out* of the census, since
censusing it would put a loader-built node into the document type vocabulary.

**The `*` form widens `reuben_core::operators`' public surface, additively.** A family module was
previously re-exported by hand and named two types; `pub use m::*;` now lifts every variant, so the
`i32` ones (`AbsI32Value` and its siblings) become public where they were reachable only as
`operators::abs::AbsI32Value` before. Additive, so nothing breaks, and it is the price of not making
the census name types a macro generates. It also glob-re-exports each family's `OPERATORS`, which
makes `operators::OPERATORS` ambiguous (`E0659`) — an error only at a use site, and nothing uses it,
because `CENSUS` reaches each array by its explicit `m::OPERATORS` path rather than through the glob.
Narrowing would mean dropping the flat re-exports of generated types altogether, which removes API
for no in-tree gain; the ambiguity is left standing deliberately.

**A third-party operator crate can no longer register into the built-in set at all.** Under `linkme`
that was the headline property: slices are declared by the core, so an out-of-crate submitter needed
no linker-script fragment. It is worth being clear that this decision spends it. It costs nothing
today — nothing outside `reuben-core` registers an operator or an OSC form, and both macros were
already `pub(crate)`, so the capability was never exposed. The supported seam for an embedder remains
`Registry::register`, which is a runtime call, needs no linker cooperation, and works identically on
all three targets. If a plugin story is ever wanted, that is the seam to build it on rather than a
link section.
