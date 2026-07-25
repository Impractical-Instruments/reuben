# Why: "Descriptor → wired `Io`" has exactly two implementations, and a test or bench harness wraps them rather than becoming a third.

[Rule](../../composition-operators.md#single-io-wiring-path)

Turning a descriptor into a wired `Io` — allocating the buffers, seeding the latches, resolving
every handle to the right slot — is one of the more intricate things the engine does, and it exists
in exactly two places on purpose: the Plan's instantiate does the real seeding, and Render's
per-node step does the real per-block wiring.

An operator's unit tests need to feed it inputs and read its outputs, and the shortest path there is
a hand-rolled `run()` that builds an `Io` directly. It is genuinely tempting: a dozen lines, no
graph, no Plan, and the operator under test is the only thing in scope. The problem is that those
dozen lines are a **third implementation** of the same wiring, and three implementations drift. The
failure mode is the expensive one — the test harness diverges from production seeding, and the tests
keep passing while describing an `Io` the engine never actually builds. A test suite that is green
against a fiction is worse than no suite, because it is trusted.

So the harness builds a one-node graph, instantiates a real Plan, and steps it with a real Renderer.
It is purely **injection and observation** over the production substrate: it decides what goes in and
reads what comes out, and every part in between is the same code the engine runs. Drift is
impossible by construction rather than by discipline — there is no second copy to fall out of sync,
so no one has to remember to update it.

The cost is that a unit test now depends on Plan and Renderer, which is a wider blast radius than a
hand-rolled `Io` would have. That is the right trade: a change to seeding *should* break operator
tests, because it changes what operators receive. A harness that insulated them from that break
would be hiding exactly the signal the tests exist to produce.

The same substrate carries the [per-operator micro-benches](../web-product-process/micro-bench-drives-the-real-path.md),
for the stronger version of the same reason — a bench measuring a hand-wired `Io` would be measuring
a code path that never runs.

Decided in: issue #639 — settled directly, no ADR.
