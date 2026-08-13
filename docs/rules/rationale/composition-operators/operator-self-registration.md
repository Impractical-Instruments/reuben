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
`--gc-sections` behavior — sound reasoning, and ADR-0081 confirmed that risk was worth hedging.

This line used to add that `linkme` was the fallback *"if the core ever goes `no_std`"*, which put the
trigger on the wrong property: `inventory` is itself `#![no_std]`. The real obstacle is startup, not
compilation — it registers through ELF `.init_array` constructors, and nothing on a bare-metal target
runs them, so registration silently yields nothing. ADR-0081 switched to `linkme` on that finding.

**Everything above about a *gatherer* is now history, mechanism and consequences alike.** The `linkme`
swap was implemented and abandoned — it does not compile for `wasm32-unknown-unknown`, which is
outside its `target_os` allowlist — and ADR-0084 draws the general conclusion: link-time distributed
registration is a linker feature, and this repo's three targets have three different linkers, so no
crate of this kind covers them. Registration folded into the module declaration instead: a census
block in `operators/mod.rs` emits each module's `pub mod`, its re-export **and** its entry in a plain
array, so the merge-conflict argument that opened this file is served by *replacing* the central list
rather than avoiding one. The dead-strip failure mode is gone with the linker; the non-empty check
survives as cheap insurance rather than a canary.

What the fold costs is the adjacency this file's argument leaned on: registration is no longer beside
the definition, so the two can drift, and drift is silent — an uncensused operator compiles, warns
nothing, and passes its own tests, because an operator test builds its type directly and never
consults the registry. That is bought back by a forcing function rather than by care:
`census_accounts_for_every_operator` reads `src/operators/*.rs` at test time and requires every
hand-written `impl Operator` to be named by a census entry, and every family module to be censused
with the `*` form. Both sides are derived from source, because a hand-maintained roster of expected
operators would reintroduce exactly the list this whole argument is about. What still holds unchanged: function pointers not
values, the `BTreeMap` re-key for determinism, and the duplicate check in `builtin()` rather than
`register()`.

This is an **operator-only** problem — Instruments are pure JSON discovered from the filesystem and
never collide this way.

Distilled from: ADR-0024
