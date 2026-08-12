# ADR-0080 — the render crate goes `no_std` + `alloc`, and the document half moves out

Closes the `## Open` section of [ADR-0067](0067-core-splits-and-reuben-api-is-the-one-window.md) —
*"crate names and how many; whether `reuben-core` survives as a name or dissolves into the pieces;
sequencing and what lands first; and feature-gating"*. ADR-0067's decision is not disturbed.

Overturns no rule. Cites [execution-runtime](../rules/execution-runtime.md) and
[composition-operators](../rules/composition-operators.md) for context. The registration mechanism is
a separate decision: [ADR-0081](0081-operator-self-registration-moves-to-linkme.md).

## Context

An embedder wants the render path on a bare-metal Cortex-M7: `thumbv7em-none-eabihf`, no OS, no
filesystem, 128 KB of internal flash (so the image lives in external QSPI behind a bootloader), and
external SDRAM for the heap.

rustup ships no `std` for any `*-none-*` target — std's per-platform layer needs an OS. So this is
not "std is awkward here", it is "std does not exist here". `alloc` *is* available, so the destination
is `no_std` + `alloc`, never `no_std` alone.

ADR-0067 already established that `reuben-core` had *"become the place things land rather than a
module with a boundary"*, with its layering *"a convention a reviewer enforces rather than a
dependency edge a compiler does."* This target makes that a compilation problem: the authoring half
cannot build for it, and the render half very nearly already can.

The surface was measured rather than estimated, and it is small. ~55 transcendental `f32` calls across
20 files are the real work. The ~200 other float methods are already `core`. Nine of eleven
`std::collections` sites are already `BTreeMap`/`BTreeSet`. All eleven `std::error::Error` impls are
free — `core::error::Error` has been stable since 1.81. What remains is two `HashSet` sites, one
`OnceLock`, and the coordinator's clock.

**One belief that shaped earlier planning was false.** `resources.rs` was thought to hold five
`std::fs::read_to_string` sites, making the filesystem seam the central question. It holds **zero** —
the five are in `introspect.rs` inside `#[cfg(test)] mod tests`, and test code does not cross-compile.
There is no production filesystem code in the crate at all, and the resource seam was already drawn
correctly: `FsResolver` lives in `reuben-api` behind a default-off feature.

## Decision

### Two crates now; four remains the destination

**`reuben-core` keeps its name and takes the render half** — its manifest already claims exactly that
role, so this makes an existing description true rather than relocating a name. **`reuben-document`
takes the authoring half**; `reuben-authoring` was rejected because `reuben-api` already has an
`authoring` feature.

ADR-0067's plugin-mechanism and first-party-operators crates are **deferred, not dropped.**
`operators/` is 12,489 lines and extracting it buys no capability today, while taking on the
cross-crate registration risk ADR-0067 calls *"the risk that sinks the Operators crate if it is
wrong"* — during a port whose linker behaviour is itself the open question.

### The partition

**`reuben-core` (render):** `signal message graph plan render operator operators descriptor registry
dsp config vocab tuning wavetable boundary resources coordinator::{mailbox,slot}`

**`reuben-document` (authoring):** `format edit projection introspect describe engine contract
vocabulary coordinator::{swap,manifest}`

**Leaves the library:** `guide.rs`, 222 lines of docs tooling with no in-crate consumers.

Three placements are not the obvious guess, and each was measured:

- **`coordinator/` splits by file.** `mailbox` and `slot` are the RT side; `swap` and `manifest` are
  off-thread and authoring.
- **`vocabulary.rs` is authoring; `vocab/` is render.** The former carries nine `Deserialize` derives
  and is consumed only by `edit`; the latter is the musical types, serde-free.
- **`resources.rs` moves whole into the render crate.** The `ResourceResolver` trait sits below and
  `reuben-document` imports it upward, which is the permitted direction. Accepted cost: the render
  crate owns a trait its own render path never calls.

### `reuben-contract` is untouched, and must not be absorbed

Every `NUMBER_MIN`/`NUMBER_MAX` site in `reuben-core` is authoring-side, so the split alone removes
the dependency. More generally: **`reuben-macros` is a proc-macro crate, so it and its dependencies
compile for the host, never the target.** `reuben-contract` reaches bare metal by no path at all.

It also cannot be folded into `reuben-document` — `reuben-macros` imports it and `reuben-core` depends
on `reuben-macros`, so that creates a Cargo-rejected cycle. It stays a leaf.

### Unconditionally `no_std` + `alloc`, with no `std` feature

**No feature at all**, so Cargo's additivity requirement stops being a question rather than being
answered. With the authoring half gone the residue is `Arc`, `BTreeMap`/`BTreeSet`, `RefCell`, `fmt`
and `core::error::Error` — all `core` or `alloc`. A default-on `std` feature would exist solely to be
switched off, and a build that forgets `--no-default-features` compiles on the host and fails at link
time on the target. One build shape means the existing test run *is* the portability gate.

`bench_support` and `op_driver` stay `#[cfg]`-gated and may use `std`; a `no_std` crate's own test
harness may link `std`, so the dev-dependencies are unaffected.

### `alloc` at instantiate, forbidden per block

**`Plan::instantiate` may allocate freely. `render_block` allocates nothing** — all per-block scratch
is preallocated into `RenderScratch` at construction and reused.

This **records an already-tested property** rather than imposing a new one: `tests/rt_safe.rs`
installs a counting `#[global_allocator]` and asserts zero allocations across 1000 blocks, with
`coordinator_rt_safe.rs` and `install_slot_rt_safe.rs` also asserting zero frees, all in CI.

Two qualifications the target makes load-bearing:

- **The guarantee is conditional by design.** Per ADR-0067, render is allocation-free *given operators
  that are*. On a fixed heap whose allocator aborts, an operator that allocates does not degrade the
  audio — it stops the program. ADR-0067's `testing`-feature harness is the mechanism, and it matters
  more here, not less.
- **It covers `instantiate` and `render_block` only.** A live swap is a third phase, named when it
  arrives.

### The embedder goes through the window

Per ADR-0067's *"every host shell wraps `reuben-api`"*, the embedder depends on `reuben-api` with
`default-features = false, features = ["render"]`. That looks close to free: the render half needs
`Vec` and `String` beyond `core`, both `alloc`.

**`scripts/check_core_privacy.py` is scaffolding, not an invariant** — it exists because it was a
convenient way to hold the API refactor for the web target. If the render window turns out not to be
portable, un-enforcing it is legitimate rather than a violation. Recorded because a green check
otherwise reads as doctrine.

## Consequences

**The split reduces to breaking one import.** Exactly three files carry a render→authoring edge, all
under `coordinator/`; two move authoring-side anyway, and the third —
`coordinator/slot.rs`'s `use crate::engine::Engine` — is one line in a 375-line file. Everything else
that pattern-matches is a rustdoc link or `#[cfg(test)]` code.

**`check_core_privacy.py` has to learn about `reuben-document`**, since it currently allows
`reuben-api` alone to name `reuben-core`.

**`reuben-api`'s features and the new boundary must not drift** — `reuben-api` already draws this seam
as `authoring`/`render`, so after the split there are two expressions of one boundary.

**Sequencing is record, then execute.** This ADR and ADR-0081 first, the crate move second, the
math/mechanical/build-gate work last. A build gate written against a layout that is about to change
gates the wrong thing. The crate move is also the safest kind of large diff: no behaviour changes, so
the existing suite is the proof.

**`wavetable.rs`'s `OnceLock` should become a `no_std` once-cell, not a `const` table.** The `const`
table is the obvious `no_std` answer and the wrong one here: the image executes from external QSPI,
and a per-sample fetch across that bus is exactly the latency a realtime block budget cannot absorb.
Building into RAM at boot keeps the read local.

## Open

**When the deferred crates un-defer.** The natural trigger is a third-party operator crate that
actually exists, but that is not stated as a commitment.

**Whether `reuben-document` is the right long-run boundary.** Its members are defined today by
"cannot compile for a bare-metal target" — a sound reason to exist, a weak one to be permanent.
