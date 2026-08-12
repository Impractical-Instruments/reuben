# Why: Each built-in operator registers itself at its own definition site through `register_operator!`/inventory, gathered into the built-in set at link time, so there is no central operator list to edit.

[Rule](../../composition-operators.md#operator-self-registration)

A central `builtin()` list plus a hand-enumerated name test meant two operators authored on parallel
branches **conflict on merge**, on the same lines of the same files, every time. Removing the central
list is what removes the conflict — so each operator **self-registers where it is defined**.

The mechanism is the `inventory` crate: each operator submits an `OpReg { make, descriptor }` at its
own definition site, and `Registry::builtin()` iterates the link-time slice
([registry.rs](../../../../crates/reuben-core/src/registry.rs)). The entry holds **function pointers,
not values** — a `Descriptor` owns `Vec`s of ports, so it is non-`const` and cannot live in a
`static`; `make`/`descriptor` are zero-capture `fn` items, which can. A thin `register_operator!`
wrapper keeps the per-operator line one readable token and, deliberately, keeps the macro **name**
greppable, so `grep -rn 'register_operator!' src/operators/` finds every hand-written operator; the
generated families (`number_operator_contract!`, `unpack_op!`) submit from inside the macro, so their
census is the declaration list, not the registration call.

Two subtleties the code pins. The `BTreeMap` re-keys by `type_name`, so iteration and the generated
schema stay **deterministic regardless of link order**, and the duplicate-name check lives in
`builtin()`, **not** in `register()` — `register()` keeps its last-writer-wins insert, which is the
**embedder override seam** (an embedder may register a type that shadows a built-in), while the
assertion only governs the built-in gathering. The one new failure mode is a linker dead-stripping
the submissions; a non-empty + a canary test (`oscillator`/`output`/`voicer` present) turn that from
a silent gap into a loud red test. `inventory` was chosen over `linkme` because it leans less on
`--gc-sections` behavior — sound reasoning, and ADR-0081 confirmed the dead-strip risk it was hedging
by observing all entries retained under `lto = "fat"` / `opt-level = "s"`. What moved was the
**trigger** this line used to give for switching. It said `linkme` was the fallback *"if the core ever
goes `no_std`"*, which put the condition on the wrong property: `inventory` 0.3.24 is itself
`#![no_std]`, so by that test the fallback was never needed. The real obstacle is startup, not
compilation — `inventory` registers through ELF `.init_array` constructors, and nothing on a
bare-metal target runs them, so registration silently yields nothing. ADR-0081 therefore switches to
`linkme` on every target, and the canary above becomes more load-bearing, not less. This is an
**operator-only** problem — Instruments are pure JSON discovered from the filesystem and never
collide this way.

Distilled from: ADR-0024
