# ADR-0081 — operator self-registration moves to `linkme`, because `inventory` cannot run on bare metal

**Overturns** [`composition-operators.md#operator-self-registration`](../rules/composition-operators.md#operator-self-registration)
on one clause: the rule names `inventory` as what *"gathers them at link time"*. What the rule
protects — that each built-in registers at its own definition site, with no central list to
merge-conflict on — is untouched and is the reason this decision was constrained the way it was.

Also overturns [ADR-0067](0067-core-splits-and-reuben-api-is-the-one-window.md)'s *"Operator
registration keeps `inventory`"*. Its companion decision in the same sentence — one `ensure_linked()`
in the operators crate, called by whoever assembles the Registry — is mechanism-independent and
survives unchanged. [ADR-0080](0080-the-render-crate-goes-no-std-and-the-document-half-moves-out.md)
is the port-shape half of the same work.

## Context

ADR-0067 kept `inventory` and the rationale behind the original choice recorded an escape hatch:
`linkme` as *"the documented fallback if the core ever goes `no_std`"*. That framing put the trigger
on the wrong property, and the error is instructive rather than incidental.

**`inventory` 0.3.24 is itself `#![no_std]`.** So by the recorded trigger, the fallback was
unnecessary and the port had one fewer problem. That reading was tested rather than trusted, and it
was wrong: **`inventory` registers through ELF `.init_array` constructors, and nothing on a
bare-metal target runs them.** `#![no_std]` removed a *compilation* obstacle and left the *startup*
obstacle exactly where it was. Its registry is a runtime-built intrusive linked list whose head is an
`AtomicPtr` in `.bss`, not a link-time slice.

The failure separates into two modes, and the second is the dangerous one:

- **Registration never happens.** With a stock `cortex-m-rt` linker script the build fails outright —
  an `.init_array` orphan section overlapping `.text`, in debug and release alike. Loud.
- **It links clean with a silently empty registry.** Fix the orphan the obvious way and this is what
  you get: an engine that loads no instrument, rather than a link error.

Dead-stripping — the risk the original choice was actually hedging against — **was ruled out by
inspection of the linked artifact.** Under `lto = "fat"`, `opt-level = "s"`, `codegen-units = 1` and
`--gc-sections`, all five spike entries survive, flagged `SHF_GNU_RETAIN`, and identically so with no
`KEEP` directive in the script. So the reasoning behind preferring `inventory` originally is
*confirmed*, not stale. Only its premise moved.

Upstream has said as much for years. `inventory`'s own README names its supported platforms and then:
*"Beyond this, other platforms will simply find that no plugins have been registered."* It points at
`linkme` as *"a different approach … that does not involve life-before-main"*, and its maintainer has
twice closed `no_std` requests by redirecting to `linkme`. `inventory` has no embedded CI at all — no
`--target`, no cross-compile, no `thumb*`.

**And bare metal is not the first time this mechanism has cost us.** The rule requiring every
statically-linked or wasm embedder to build at `codegen-units = 1` exists *only* because
`inventory`'s constructors are otherwise dropped by the linker, and its rationale names the
casualties — `voicer` and `clock` among the operators that went missing. That rule is already marked
superseded pending absorption. The same mechanism has now failed on two of three target classes.

## Decision

**`linkme` replaces `inventory` for both registries, on every target — the host included.**

Not a `cfg`-selected dual mechanism. ADR-0080 removes the `std` feature from `reuben-core`
specifically so that host and target are one build shape, and a `cfg`-selected registry would
reintroduce divergence at the one place whose failure mode is a silently empty registry — the defect
that is invisible until something asks the registry a question.

Not keeping `inventory` with an embedder-side `.init_array` walk, which does link and costs only 36
bytes of `.text`. It was rejected on three counts: it puts a startup obligation on every embedder; it
costs **12 bytes of writable RAM per registered entry**, because each node's `next` is mutated in
place and so cannot live in flash, which scales with exactly the thing an extensible operator set
grows; and it runs against the documented contract of the crate providing it.

The property that decided it is how the cost scales. **`linkme`'s price is per registry, not per
operator or per contributor:** two linker-script lines named after the *slice*, so four lines for the
two registries, fixed for good. Slices are declared by the core, so a third-party operator crate
submits into an existing one and needs **no linker-script fragment on any target**. That is the
property that matters when the whole point of self-registration is that nobody owns the list of who
registers.

## Consequences

**Three existing properties must survive the swap, and each already has a test.** The registry and
schema stay deterministic regardless of link order. The duplicate-name check stays in `builtin()`
rather than `register()` — `register()` keeps last-writer-wins, because that is the embedder override
seam, and moving the assertion breaks it. And the dead-strip canary stays: `linkme` leans harder on
`--gc-sections` behaviour than `inventory` does, which is precisely why it was not chosen first, so
the canary becomes more load-bearing rather than less.

**Nothing currently proves registration on a bare-metal target, and that gap is now closed by
mandate.** Both canaries live in `#[cfg(test)]` host tests, and `cargo test` does not run on
`thumbv7em-none-eabihf` — so today's loud-red-test guarantee covers every platform except the one
that needs it. **The build gate must assert the registered count on the target in CI**, not merely
that `cargo check --target` succeeds. Accepting host-only proof was rejected as the exact reasoning
that just failed: `inventory` passed every host test in this repo while being structurally incapable
of registering on the target, and a compile-only gate would not have caught it either. QEMU runs a
`thumbv7em` binary, so the cheap version is real.

**The toolchain pin becomes load-bearing in a way it was not before.** `linkme` depends on
`#[used]`/`SHF_GNU_RETAIN`, and plain `#[used]` only became `#[used(linker)]` on ELF in rustc **1.89**.
The MSRV is 1.96 and retention was *observed* working under release codegen, so this is fine today —
but a pin rollback is now more dangerous than it was under `inventory`, and that belongs in the MSRV
comment beside the pin rather than only here.

**Upstream's embedded coverage is adjacent, not identical.** `linkme` CI-tests a
`thumbv7m-none-eabi` QEMU job asserting a slice's length. Ours is `thumbv7em-none-eabihf` — same
family, different target — which is a further reason the mandated target-side assertion is ours to
own rather than inherited.

**The `codegen-units = 1` rule can be dropped once the swap lands, not merely absorbed.** It exists
solely to keep constructors alive through linking, and `linkme` has no constructors. It already
carries ADR-0067's marker; this decision is what makes it removable rather than restatable, and the
fold should delete rather than reword it.

**One more restatement of the old mechanism needs clearing in the same fold.** The
product-type-unpack-operator rule in [composition-operators](../rules/composition-operators.md) also
says its generated operator *"self-registers through inventory"*. It is accurate until the swap
lands and it is not what that rule is about, so it is not marked here — but it states the old
mechanism in the present tense and the guard cannot see it, so absorbing this ADR has to fix it too.

**Two stale comments are corrected in the same change as this ADR**, because they send a reader down
a migration for the wrong reason: `Cargo.toml`'s dependency note and the self-registration rationale
both give `no_std` as the trigger for the `linkme` fallback. The conclusion survives; the trigger is
that nothing runs `.init_array` constructors on bare metal.
