# Why: Each built-in operator registers itself at its own definition site through `register_operator!`/inventory, gathered into the built-in set at link time, so there is no central operator list to edit.

[Rule](../../composition-operators.md#operator-self-registration)

Adding a built-in operator used to touch **five sites across two shared files** — module declarations,
imports, a `register()` line in a central `builtin()` list, and a hand-enumerated name test — every
one a sorted insert into the *same lines of the same files*. Scaffolding automated the edits, but
automation doesn't help the real cost: two operators authored on parallel branches **conflict on
merge**, worst at `builtin()` and the name test, which every new operator changes. The boilerplate is
busywork; the merge conflict is the tax. Removing the central list is what removes the conflict — so
each operator **self-registers where it is defined**.

The mechanism is the `inventory` crate: each operator submits an `OpReg { make, descriptor }` at its
own definition site, and `Registry::builtin()` iterates the link-time slice
([registry.rs](../../../../crates/reuben-core/src/registry.rs)). The entry holds **function pointers,
not values** — a `Descriptor` owns `Vec`s of ports, so it is non-`const` and cannot live in a
`static`; `make`/`descriptor` are zero-capture `fn` items, which can. A thin `register_operator!`
wrapper keeps the per-operator line one readable token and, deliberately, keeps the macro **name** as
the one greppable census: `grep -rn 'register_operator!' src/operators/` enumerates every built-in —
the discoverability the old hand-list used to provide, now distributed but still greppable.

Two subtleties the code pins. The `BTreeMap` re-keys by `type_name`, so iteration and the generated
schema stay **deterministic regardless of link order**, and the duplicate-name check lives in
`builtin()`, **not** in `register()` — `register()` keeps its last-writer-wins insert, which is the
**embedder override seam** (an embedder may register a type that shadows a built-in), while the
assertion only governs the built-in gathering. The one new failure mode is a linker dead-stripping
the submissions; a non-empty + a canary test (`oscillator`/`output`/`voicer` present) turn that from
a silent gap into a loud red test. `inventory` was chosen over `linkme` because it leans less on
`--gc-sections` behavior; `linkme` is the documented fallback if the core ever goes `no_std`. This is
an **operator-only** problem — Instruments are pure JSON discovered from the filesystem and never
collide this way.

Distilled from: ADR-0024
