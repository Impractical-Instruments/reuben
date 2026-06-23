# Compile-time operator self-registration via `inventory`

## Context

Adding a built-in Operator used to touch **five sites across two shared files**, every one a
sorted insert into a central, hand-maintained list:

1. `operators/mod.rs` — `pub mod <name>;`
2. `operators/mod.rs` — `pub use <name>::<Type>;`
3. `registry.rs` — `use crate::operators::{…}` import
4. `registry.rs` — a `r.register(…)` line in `Registry::builtin()`
5. `registry.rs` — the operator's name in a hardcoded `names` vec test

[`scaffold-operator`](0021-scaffold-operator-and-create-operator-skill.md) *automated* these
edits, but automation doesn't help the real cost: they are inserts into the **same lines of the
same files**, so two operators authored on parallel branches conflict on merge — worst at
`builtin()` and the name-list test, which every new operator changes. The V1.3 Toys (groovebox,
chord player) hit exactly this. The boilerplate is busywork; the merge conflict is the tax.

This is an **operator-only** problem. Instruments are pure JSON, discovered from the filesystem,
and carry zero registration boilerplate — they never collide this way and are out of scope here.

The fix is to **defer registration to build/link time**: let an operator register itself *where it
is defined*, and let `builtin()` gather the submissions, so the central list — the conflict magnet
— disappears entirely.

## Decision

### Self-register via the `inventory` crate

Each operator submits a registration at its own definition site; `inventory` collects them into a
link-time slice that `Registry::builtin()` iterates. `registry.rs` declares the collected type and
the gathering loop:

```rust
pub struct OpReg {
    pub make: fn() -> Box<dyn Operator>,
    pub descriptor: fn() -> Descriptor,
}
inventory::collect!(OpReg);
```

The entry holds **function pointers**, not values: `Descriptor` is non-`const` (it owns `Vec`s of
ports/params), so it can't be stored in a `static`. `make` and `descriptor` are zero-capture `fn`
items, which can.

### A `register_operator!` convenience macro — the greppable census

A thin wrapper over `inventory::submit!` keeps the per-operator line a single readable token and,
deliberately, keeps the macro **name** as the one greppable anchor for "what built-ins exist":

```rust
macro_rules! register_operator {
    ($t:ty) => {
        inventory::submit! {
            $crate::registry::OpReg {
                make: || Box::new(<$t>::new()),
                descriptor: <$t>::descriptor,
            }
        }
    };
}
```

`grep -rn 'register_operator!' src/operators/` enumerates every built-in operator — the census the
old `builtin()` list used to provide, now distributed but still discoverable. The macro is invoked
**by path** at each definition site, after the `impl Operator` block:

```rust
crate::register_operator!(Oscillator);
```

Path invocation needs the macro reachable at the crate root regardless of module declaration order
(`operators` is declared before `registry`), so it is re-exported there: `pub(crate) use
registry::register_operator;` in `lib.rs`. The `signal_pointwise!` macro (the math family's
`add`/`mul` generator) emits the line itself, so macro-generated operators self-register too.

### `builtin()` gathers and asserts uniqueness

```rust
pub fn builtin() -> Self {
    let mut r = Self::new();
    for reg in inventory::iter::<OpReg> {
        let descriptor = (reg.descriptor)();
        assert!(
            r.get(descriptor.type_name).is_none(),
            "duplicate operator type_name {:?} …", descriptor.type_name
        );
        r.register(reg.make, descriptor);
    }
    r
}
```

The slice is in link order; the registry's `BTreeMap` re-keys by `type_name`, so iteration and the
generated schema stay **deterministic** regardless of link order. The duplicate-name assertion is a
build-time guard that two operators don't silently claim the same name.

Crucially, the dup check lives in `builtin()`, **not** in `register()`. `register()` keeps its
last-writer-wins insert, which is the embedder override seam from
[ADR-0004](0004-ai-authorability-first-class.md): an embedder may `register()` a type that shadows
a built-in. The assertion only governs the built-in set gathering itself.

### Name-list test → churn-free invariants + a dead-strip canary

The enumerated `names` vec (site 5) is deleted. Listing every operator by hand is exactly the churn
self-registration removes, so it's replaced with invariants over *whatever* the slice gathered:
the set is non-empty, every entry's map key matches its descriptor and constructs without panic,
no duplicate names, all names snake_case. One small **canary** asserts a few load-bearing operators
(`oscillator`, `output`, `voicer`) are present — a guard against the one real failure mode of
life-before-main registration: a linker dead-stripping the submissions (see Consequences).

### The scaffold stops editing `registry.rs`

`scaffold-operator` now edits only `operators/mod.rs` (both sorted inserts) and emits the
`register_operator!` line in the generated operator file. All `registry.rs`-editing logic — the
import insert, the `builtin()` register-call insert, the name-list insert, and their tests — is
deleted. The `mod.rs` re-exports (`pub use`) are **kept**: they are public API imported by examples
and integration tests, and a single sorted line merges cleanly.

## Alternatives considered

- **A `build.rs` that scans `operators/` and generates the list.** Directory structure ≠
  registration: `math.rs` holds five operator structs (`add`/`mul` macro-generated, plus
  `map`/`differentiate`/`integrate`), and other files carry free helpers. A file-per-operator scan
  would mis-count; parsing Rust to find `impl Operator` blocks in `build.rs` reinvents a compiler.
  Rejected — the submission lives correctly at the `impl`, not the file.
- **`linkme` distributed slices.** Same self-registration shape with no runtime ctor, but its
  correctness leans harder on linker `--gc-sections` behavior; a dead-strip drops operators
  silently. `inventory` is the better-trodden path for this exact use case. `linkme` is kept as the
  **documented fallback** should the core ever go `no_std` (`inventory` needs `std` today).
- **`ctor` directly.** `inventory` *is* the curated wrapper over this pattern; using `ctor` raw
  would re-implement its slice bookkeeping for no gain.
- **Keep the hand-list, just dedupe via tooling.** Treats the symptom. The list itself is the
  conflict surface; only removing it removes the conflict.

## Consequences

- **Dependency:** `inventory = "0.3"` added to `[workspace.dependencies]` and `reuben-core`. It is
  a small, widely-used crate; the core remains `std` (it already was, despite the "OS-free
  portable core" framing — `no_std` is a separate future step, at which point `linkme` is the
  swap-in).
- **Adding an operator is now one local act:** write the file, declare it in `mod.rs`, add its
  `register_operator!` line. No central list, so **parallel branches no longer conflict** in
  `registry.rs` — the original goal.
- **Dead-strip is the new failure mode**, mitigated: because submissions are referenced only
  through the linker slice, an aggressive GC *could* drop them. The non-empty + canary tests fail
  loudly and immediately if that ever happens on a target/toolchain, turning a silent gap into a
  red test. Not observed on the pinned toolchain ([ADR-0023](0023-toolchain-pin-and-git-hooks.md)).
- **Determinism preserved:** the `BTreeMap` keeps schema output stable, so
  `committed_schema_is_in_sync` is unchanged by this refactor — the registry's *contents* and
  order are identical, only the *assembly* moved from a hand-list to link-time gathering.
- **The embedder seam is intact:** `register()` is untouched; `builtin()` simply feeds it from the
  slice instead of from inlined calls.
- **[ADR-0021](0021-scaffold-operator-and-create-operator-skill.md) is superseded in part:** its
  "three registration sites" is now one (`mod.rs`) plus a self-registration line; the subcommand,
  the red placeholder test, and the skill loop stand.

## Update (ADR-0025)

`register_operator!` self-*registers* an operator; its sibling `operator_contract!`
([ADR-0025](0025-single-source-operator-contract.md)) self-*describes* one — single-sourcing the
port/param index consts and the `Descriptor`. Both turn a stated-twice fact into one declaration.
