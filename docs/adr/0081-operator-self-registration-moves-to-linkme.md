# ADR-0081 — operator self-registration moves to `linkme`, because `inventory` cannot run on bare metal

**Superseded in the remedy — not the diagnosis — by
[ADR-0084](0084-operator-registration-is-a-census-not-a-linker-feature.md), on 2026-08-12.** The swap
below was implemented and abandoned: `linkme` 0.3.37 gates `#[distributed_slice]` behind a
`target_os` allowlist with no wasm32 entry, so it fixed bare metal by breaking the browser. **The
diagnosis here stands in full** — `inventory` registers through `.init_array` constructors that
nothing on bare metal runs, and the silently-empty registry is the dangerous failure mode — and it is
what forced the replacement. What does not stand is *"`linkme` replaces `inventory` … on every
target"*, and with it the per-registry cost argument that decided it. ADR-0084 draws the general
conclusion this ADR was one target short of: link-time distributed registration is a linker feature,
and three targets means three linkers. Read this for why the old mechanism had to go; read ADR-0084
for what replaced it.

**Overturns** [`composition-operators.md#operator-self-registration`](../rules/composition-operators.md#operator-self-registration)
on one clause: the rule names `inventory` as what *"gathers them at link time"*. What the rule protects
— each built-in registering at its own definition site, with no central list to merge-conflict on — is
untouched, and is what constrained this decision.

Also overturns [ADR-0067](0067-core-splits-and-reuben-api-is-the-one-window.md)'s *"Operator
registration keeps `inventory`"*. Its companion decision in the same sentence — one `ensure_linked()`
called by whoever assembles the Registry — is mechanism-independent and survives.
[ADR-0080](0080-the-render-crate-goes-no-std-and-the-document-half-moves-out.md) is the port-shape half
of the same work.

## Context

The recorded escape hatch was `linkme` as *"the documented fallback if the core ever goes `no_std`"*.
That put the trigger on the wrong property, and the error is instructive.

**`inventory` 0.3.24 is itself `#![no_std]`.** So by the recorded trigger the fallback was
unnecessary. That reading was tested rather than trusted, and it was wrong: **`inventory` registers
through ELF `.init_array` constructors, and nothing on a bare-metal target runs them.** `#![no_std]`
removed a *compilation* obstacle and left the *startup* obstacle untouched. Its registry is a
runtime-built intrusive linked list headed by an `AtomicPtr` in `.bss`, not a link-time slice.

Two failure modes, and the second is the dangerous one:

- **Registration never happens.** With a stock `cortex-m-rt` linker script the build fails outright —
  an `.init_array` orphan overlapping `.text`. Loud.
- **It links clean with a silently empty registry.** Fix the orphan the obvious way and this is what
  you get: an engine that loads no instrument, rather than a link error.

Dead-stripping — the risk the original choice actually hedged against — **was ruled out by inspecting
the linked artifact.** Under `lto = "fat"`, `opt-level = "s"` and `--gc-sections`, all spike entries
survive, flagged `SHF_GNU_RETAIN`. So the reasoning behind preferring `inventory` is *confirmed*, not
stale. Only its premise moved.

Upstream has said so for years. `inventory`'s README names its supported platforms, then: *"Beyond
this, other platforms will simply find that no plugins have been registered."* It points at `linkme`
as *"a different approach … that does not involve life-before-main"*, and its maintainer has twice
closed `no_std` requests by redirecting there. `inventory` has no embedded CI at all.

**And bare metal is not the first time this mechanism has cost us.** The rule requiring every
statically-linked or wasm embedder to build at `codegen-units = 1` exists *only* because `inventory`'s
constructors are otherwise dropped, and its rationale names the casualties — `voicer` and `clock` among
the operators that went missing. The same mechanism has now failed on two of three target classes.

## Decision

**`linkme` replaces `inventory` for both registries, on every target — the host included.**

Not a `cfg`-selected dual mechanism: ADR-0080 removes the `std` feature precisely so host and target
are one build shape, and this is the one mechanism whose failure mode is a silently empty registry.

Not keeping `inventory` with an embedder-side `.init_array` walk, which does link and costs only 36
bytes of `.text`. Rejected because it puts a startup obligation on every embedder, costs **12 bytes of
writable RAM per entry** (each node's `next` is mutated in place, so it cannot live in flash), and runs
against the documented contract of the crate providing it.

What decided it is how the cost scales. **`linkme`'s price is per registry, not per operator:** two
linker-script lines named after the *slice*, so four lines for two registries, fixed for good. Slices
are declared by the core, so a third-party operator crate submits into an existing one and needs **no
linker-script fragment on any target.** That is the property that matters when the point of
self-registration is that nobody owns the list of who registers.

## Consequences

**Three existing properties must survive, and each already has a test.** The registry and schema stay
deterministic regardless of link order. The duplicate-name check stays in `builtin()` rather than
`register()` — `register()` keeps last-writer-wins, which is the embedder override seam. And the
dead-strip canary stays: `linkme` leans harder on `--gc-sections` than `inventory` does, which is why
it was not chosen first, so the canary becomes more load-bearing rather than less.

**Nothing currently proves registration on a bare-metal target, and that gap is closed by mandate.**
Both canaries are `#[cfg(test)]` host tests and `cargo test` does not run on `thumbv7em-none-eabihf`,
so today's loud-red-test guarantee covers every platform except the one that needs it. **The build gate
must assert the registered count on the target in CI**, not merely that `cargo check --target`
succeeds. Host-only proof was rejected as the reasoning that just failed: `inventory` passed every host
test here while being structurally incapable of registering on the target, and a compile-only gate
would not have caught it either. QEMU runs a `thumbv7em` binary, so the cheap version is real.

**Upstream's embedded coverage is adjacent, not identical** — `linkme` CI-tests a `thumbv7m-none-eabi`
QEMU job asserting a slice's length; ours is `thumbv7em-none-eabihf`. A further reason the target-side
assertion is ours to own rather than inherited. Retention was separately confirmed on our own pinned
toolchain during the spike.

**The `codegen-units = 1` rule can be dropped once the swap lands, not merely absorbed.** It exists
solely to keep constructors alive through linking, and `linkme` has no constructors. It already carries
ADR-0067's marker; this is what makes it removable rather than restatable.

**One more restatement needs clearing in the same fold.** The product-type-unpack-operator rule in
[composition-operators](../rules/composition-operators.md) also says its generated operator
*"self-registers through inventory"*. It stays accurate until the swap lands and is not what that rule
is about, so it is not marked here — but it states the old mechanism in the present tense and the guard
cannot see it.

**Two stale comments are corrected alongside this ADR**, because they send a reader down a migration
for the wrong reason: `Cargo.toml`'s dependency note and the self-registration rationale both gave
`no_std` as the trigger. The conclusion survives; the trigger is that nothing runs `.init_array`
constructors on bare metal.
