# ADR-0076 — a Swap may install one hosted sub-Plan, so the whole Engine is no longer the only unit

**Overturns** [`execution-runtime.md#engine-swap-unit`](../rules/execution-runtime.md#engine-swap-unit),
whose opening clause — *"the unit of a Swap is the whole Engine"* — is what this widens. The rule is
marked pending absorption in the same change, along with the `## Now` prose and the `Swap` `## Terms`
entry that restate it.

Builds on [ADR-0075](0075-hosting-splits-on-activity-so-the-voicer-is-not-the-only-host.md), which
makes hosted sub-`Plan`s general rather than the Voicer's private arrangement. Without that there is
nothing at sub-`Plan` granularity worth installing.

## Context

Every change to the graph, including the first build, is a Swap: the Coordinator instantiates a new
Plan off-thread, the whole **Engine** vessel crosses the RT boundary through a pair of single-slot
atomic mailboxes, survivors keep their state by pointer transplant, the retiree is reclaimed
off-thread, and because the install is audibly abrupt it is wrapped in a fixed master-gain ramp.

That is the right unit for what it was designed for. A person reshapes an instrument; the reshape can
touch anything; rebuilding the vessel is honest, and the ramp covers a discontinuity the rebuild
genuinely produces.

It is the wrong unit for editing one member of a hosted set. Under ADR-0075 a host holds a fixed set
of resident sub-`Plan`s and renders a chosen subset. Editing a **parked** member changes nothing that
is sounding — and yet the only way to install that edit is to rebuild the entire Engine, transplant
every survivor in the graph, and duck the master gain to cover a discontinuity that does not exist.
The cost is paid, the ramp is audible, and nothing about the sounding output needed to change.

The asymmetry gets worse the more useful hosting becomes. A consumer editing one part of one section
of a playing arrangement pays for the whole arrangement, every time, on a path where the *point* is
that the rest is untouched.

## Decision

**The Coordinator may install a single hosted sub-`Plan` into a playing Engine, without rebuilding
the vessel.**

**The host's boundary face is fixed across an install.** A sub-`Plan` install may not change the
host's interface — same pipes, same `Arg` types, same combine arity. This is the constraint that
makes everything else hold: the outer graph did not change, so the outer topological schedule is not
recomputed and the static-schedule guarantee is untouched. An edit that *does* change the host's face
is a structural change to the parent and remains a whole-Engine Swap.

**The install crosses by the same discipline, addressed to a host slot rather than the Engine root.**
Instantiate happens off-thread where all allocation already lives; the render side adopts at a block
boundary; the retired sub-`Plan` and its arena are reclaimed off-thread. Survivor matching is
unchanged — address, operator type, instantiate-time fingerprint — scoped to the slot being replaced.

**Neither install path carries a gain ramp, because neither produces a discontinuity.**

- Installing into a **parked** slot changes no sounding output at all. There is nothing to cover.
- Installing into the **live** slot is staged and adopted at the host's next boundary, so the
  replacement begins where the member it replaced would have begun a cycle.

The master-gain ramp remains correct for a whole-Engine Swap and does not extend here. This is the
substantive difference from the existing unit: the ramp exists because a whole-vessel install is
abrupt, and a sub-`Plan` install is not abrupt, so importing the ramp would be inventing a
discontinuity in order to hide it.

**A host declares where its boundaries are.** "The next boundary" is meaningless to the engine in the
abstract; the host knows when its live member completes a cycle and is the only thing that can. A
host that cannot name a boundary adopts at the next block, which is the degenerate case rather than a
separate mechanism.

## Consequences

- **Editing what is not sounding becomes free of audible cost**, which is the property the whole
  arrangement case rests on.
- **Two install units now exist**, and a caller has to know which it is performing. The distinction
  is not stylistic: a change to a host's face is a Swap and a change inside a member is an install,
  and getting it wrong is a structural mismatch rather than a degraded result. The Coordinator
  enforces it rather than trusting the caller.
- **Partial-install survivorship is narrower than it looks.** Only the replaced slot's members are
  candidates. A node that moved from one member to another is not a survivor and does not transplant,
  which is correct and will surprise someone.
- **Adoption is no longer synchronous with the install call.** A live-slot install completes at a
  boundary the host chooses, so "installed" and "audible" are two moments. A caller that needs to
  know which has happened needs to be told, and today's swap report is shaped for a vessel-level
  transition.
- **The reclaim story gains a case.** A retired sub-`Plan` is freed off-thread like a retired Engine,
  but its lifetime is bounded by a boundary the host picks rather than by the mailbox handshake.
- **The whole-Engine Swap is not deprecated.** It remains the unit for anything that touches
  structure above a host, and the first build is still a Swap from the empty Plan.

## Open

- Whether more than one sub-`Plan` install may be in flight, and whether they are ordered per host or
  globally. One-in-flight is the conservative start and matches the existing mailbox discipline.
- Whether the split from [ADR-0074](0074-a-swap-splits-at-the-build-so-a-door-can-decline.md) applies
  — a *prepared* sub-`Plan` install a door can measure and decline before committing. The shape fits;
  no caller has asked.
- Whether the whole-Engine Swap eventually becomes the degenerate case of a general install at the
  root, or stays a distinct verb. Collapsing them is tempting and would put the ramp on a code path
  that mostly must not have one.
- How a swap report describes a transition whose audible moment has not happened yet.
