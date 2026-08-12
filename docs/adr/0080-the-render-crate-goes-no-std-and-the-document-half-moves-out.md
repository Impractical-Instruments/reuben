# ADR-0080 — the render crate goes `no_std` + `alloc`, and the document half moves out

Closes the `## Open` section of [ADR-0067](0067-core-splits-and-reuben-api-is-the-one-window.md),
which accepted the split and deferred *"crate names and how many; whether `reuben-core` survives as
a name or dissolves into the pieces; sequencing and what lands first; and feature-gating so a host
that only wants Render does not compile the authoring surface."* ADR-0067's decision is not
disturbed; this settles what it left open and adds a target it did not contemplate.

Overturns no rule. Cites [execution-runtime](../rules/execution-runtime.md) and
[composition-operators](../rules/composition-operators.md) for context — the render-is-allocation-free
and embed-surface rules both already carry ADR-0067's marker, and nothing here changes what they say.
The registration mechanism is a separate decision with its own blast radius: [ADR-0081](0081-operator-self-registration-moves-to-linkme.md).

## Context

An embedder wants the render path on a **bare-metal Cortex-M7**: `thumbv7em-none-eabihf`, no OS, no
filesystem, 128 KB of internal flash (so the image lives in external QSPI behind a bootloader), and
external SDRAM for the heap.

Note what the target *is* rather than how it is awkward: rustup ships no `std` for any `*-none-*`
target, because std's per-platform layer needs an OS underneath it. This is not "std is
inconvenient here", it is "std does not exist here". `alloc` **is** available — the embedder installs
a global allocator over SDRAM — so the destination is `no_std` + `alloc`, never `no_std` alone.

ADR-0067 established that `reuben-core` had *"become the place things land rather than a module with
a boundary"*, and that its layering was *"a convention a reviewer enforces rather than a dependency
edge a compiler does."* This target turns that from a hygiene argument into a compilation one: the
authoring half cannot compile for it, and the render half very nearly already can.

The surface was measured over `crates/reuben-core/src` rather than estimated, and it is smaller than
expected. ~55 transcendental `f32` calls across 20 files are the real work. The ~200 other float
methods (`abs`/`clamp`/`floor`/`round`/`rem_euclid`/`fract`/`trunc`/`signum`) are already `core`.
Nine of eleven `std::collections` sites are already `BTreeMap`/`BTreeSet` and only re-import. All
eleven `std::error::Error` impls are free — `core::error::Error` has been stable since 1.81 and the
MSRV is 1.96. What genuinely remains is two `HashSet` sites, one `OnceLock`, and the coordinator's
clock.

**One belief that shaped earlier planning was false and is worth recording as such.** `resources.rs`
was thought to hold five `std::fs::read_to_string` sites, making the filesystem seam the central
question. It holds **zero**: its std usage is `RefCell` (core), `BTreeMap` (alloc), `fmt` (core),
`Arc` (alloc), and one `std::error::Error` impl. The five `read_to_string` sites are in
`introspect.rs`, all inside `#[cfg(test)] mod tests`, behind a test-only `DirResolver`. Test code
does not cross-compile. There is no production filesystem code in the crate at all, and the resource
seam was already drawn correctly — `FsResolver` lives in `reuben-api` behind a default-off
`fs-resolver` feature.

## Decision

### The split is two crates now, and four remains the destination

**`reuben-core` keeps its name and takes the render half.** Its manifest already describes itself as
*"Portable, OS-free realtime core for reuben: graph, plan, render"* — this makes an existing
description true rather than relocating a name. **`reuben-document` takes the authoring half**, named
for the document and the verbs over it; `reuben-authoring` was rejected because `reuben-api` already
has an `authoring` feature and the two would be one word for two scopes.

ADR-0067's other two crates — the plugin mechanism, and first-party operators — are **deferred, not
dropped.** `operators/` is 12,489 lines, over half the render side, and extracting it buys no
capability today while taking on the cross-crate registration risk ADR-0067 itself calls *"the risk
that sinks the Operators crate if it is wrong"*. Doing that during a port whose linker behaviour is
itself in question is two unknowns at once.

### The partition

**`reuben-core` (render):** `signal message graph plan render operator operators descriptor registry
dsp config vocab tuning wavetable boundary resources coordinator::{mailbox,slot}`

**`reuben-document` (authoring):** `format edit projection introspect describe engine contract
vocabulary coordinator::{swap,manifest}`

**Leaves the library:** `guide.rs` — 222 lines of docs tooling that slices
`docs/agents/authoring.md`, with no in-crate consumers.

Three placements are not the obvious guess, and each was measured rather than assumed:

- **`coordinator/` straddles the seam and splits by file.** `mailbox` (the atomic swap primitive) and
  `slot` (the RT-side `RenderSlot`) are render; `swap` and `manifest` are off-thread and authoring.
- **`vocabulary.rs` is authoring, `vocab/` is render.** The former carries nine `Deserialize`
  derives and a hand-written `impl<'de> Deserialize<'de> for Magnitude`, and its only in-crate
  consumer is `edit`. The latter is the musical types, serde-free.
- **`resources.rs` moves whole into the render crate**, neither split nor promoted. The
  `ResourceResolver` trait sits below and `reuben-document` imports it upward, which is the permitted
  direction.

### `reuben-contract` is untouched, and must not be absorbed

Every `NUMBER_MIN`/`NUMBER_MAX` site in `reuben-core` is authoring-side, so the split alone removes
the render half's dependency. More generally: **`reuben-macros` is a proc-macro crate, so it and its
dependencies compile for the host, never the target.** `reuben-contract` therefore reaches bare metal
by no path at all and needs no port, ever.

It also cannot be folded into `reuben-document`: `reuben-macros` imports it in six files and
`reuben-core` depends on `reuben-macros`, so absorbing it would create
`reuben-document → reuben-core → reuben-macros → reuben-document`, which Cargo rejects. It stays a
leaf below everything.

### `reuben-core` is unconditionally `no_std` + `alloc`, with no `std` feature

Not `#![cfg_attr(not(feature = "std"), no_std)]` with a default-on `std` feature. **No feature at
all**, so Cargo's additivity requirement stops being a question rather than being answered.

With the authoring half gone, nothing in the render crate wants `std`: the residue is `Arc`,
`BTreeMap`/`BTreeSet`, `RefCell`, `fmt`, and `core::error::Error`, all `core` or `alloc`. A default-on
`std` feature would exist solely to be switched off, and a build that forgets `--no-default-features`
would compile on the host and fail at link time on the target. Unconditional `no_std` makes host and
target one build shape, which means the existing test run *is* the portability gate.

Two carve-outs: `bench_support` and `op_driver` stay `#[cfg]`-gated as they are and may use `std`
freely, and a `no_std` crate's own test harness may link `std`, so the `approx`/`hound`/`criterion`/
`iai-callgrind` dev-dependencies are unaffected.

### `alloc` is assumed at instantiate and forbidden per block

**`Plan::instantiate` may allocate freely. `render_block` allocates nothing.** All per-block scratch
is preallocated at construction into `RenderScratch` and reused.

This **records an existing, already-tested property** rather than imposing a new one. `tests/rt_safe.rs`
installs a counting `#[global_allocator]` and asserts zero allocations across 1000 blocks plus 100
message-bearing blocks; `coordinator_rt_safe.rs` and `install_slot_rt_safe.rs` additionally assert
zero *frees*; all run in CI's ordinary test step.

Two qualifications the target makes load-bearing:

- **The guarantee is conditional by design.** Per ADR-0067, render is allocation-free *given
  operators that are*, and RT-safety is an internal quality bar rather than a term of the API
  contract. On a fixed SDRAM heap whose allocator aborts on exhaustion, an operator that allocates
  does not degrade the audio — it stops the program. ADR-0067's `testing`-feature allocation-counting
  harness is the mechanism, and it matters more here than on a host, not less.
- **The contract covers `instantiate` and `render_block` only.** A live graph swap is a third phase
  and is named when it arrives rather than being silently covered.

### The embedder goes through the window

The bare-metal embedder depends on `reuben-api` with `default-features = false, features =
["render"]`, per ADR-0067's *"every host shell wraps `reuben-api`"*. This looks close to free: the
render half of `reuben-api` needs `Vec` and `String` beyond `core`, both `alloc`, and CI already
checks that feature combination.

**`scripts/check_core_privacy.py` is scaffolding, not an invariant.** It exists because it was a
convenient way to make the API refactor hold for the web target. If the render window turns out not
to be portable, un-enforcing that script is a legitimate move rather than a violation. Recorded
because a green check reads as doctrine to the next person otherwise.

## Consequences

**The split reduces to breaking one import.** Exactly three files carry a render→authoring edge, all
under `coordinator/`, and two of them (`swap.rs`, `manifest.rs`) move authoring-side anyway. The
third — `coordinator/slot.rs`'s `use crate::engine::Engine` — is one line in a 375-line file and is
the only genuine blocker. Everything else that pattern-matches is a rustdoc intra-doc link or
`#[cfg(test)]` code. The one-way layering this preserves is already asserted in `plan.rs`'s own
documentation.

**`check_core_privacy.py` has to learn about `reuben-document`.** It currently asserts that
`reuben-core` may be named by `reuben-api` alone, dev-dependencies included, so a second workspace
crate naming `reuben-core` trips it.

**`reuben-api`'s features and the new crate boundary must not drift.** `reuben-api` already draws
this same seam one layer up as `authoring` / `render`, and after the split there are two expressions
of one boundary.

**Sequencing is record, then execute.** This ADR and ADR-0081 land first; the crate move second; the
transcendental-math, mechanical-remainder and build-gate work last. The build gate exists to keep the
port from rotting, and a gate written against a crate layout that is about to change gates the wrong
thing. The crate move is also the safest kind of large diff — it changes no behaviour, so the
existing suite is the proof.

**`wavetable.rs`'s `OnceLock` should become a `no_std` once-cell, not a `const` table.** It caches a
4,097-`f32` single-cycle sine table. A build-time `const` table is the obvious `no_std` answer and is
the wrong one *for this target*: the image executes from external QSPI, and a per-sample fetch across
that bus is exactly the latency a realtime block budget cannot absorb. Building it into RAM at boot
keeps the read local. The mechanical-remainder work owns this; the reasoning is recorded here because
it is target-specific and would otherwise be rediscovered.

## Open

**When the deferred crates un-defer.** ADR-0067's plugin-mechanism and first-party-operators crates
have no trigger attached. The natural one is a third-party operator crate that actually exists, but
that is not stated as a commitment here.

**Whether `reuben-document` is the right long-run boundary or an interim one.** It holds the document,
the verbs over it, the projections, introspection, and the off-thread coordinator — a set defined
today by "cannot compile for a bare-metal target" rather than by a single responsibility. That is a
sound reason for the boundary to exist and a weak one for it to be permanent.
