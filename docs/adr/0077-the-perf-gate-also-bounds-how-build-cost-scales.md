# ADR-0077 — the perf gate also bounds how build cost *scales*, on a surface that is not the hot path

**Overturns** [`web-product-process.md#perf-benchmark-gate`](../rules/web-product-process.md#perf-benchmark-gate)
on two clauses: the guarded surface is no longer only *"the render hot path"*, and one of the gate's
checks is no longer *"diffs HEAD against its base ref"*. The rule is marked pending absorption in the
same change, along with the `## Now` prose and the `perf gate` `## Terms` entry that restate it.

## Context

The perf gate measures instruction counts for rendering one second of audio and compares them to the
PR's base ref. Everything about that shape follows from what it guards: render cost is paid per
block, forever, so a few percent matters; and because it is paid forever, yesterday's number is the
right thing to compare against.

Graph **construction** — parse, build, instantiate — is not that. It is paid once per Swap, and it is
paid on the caller's thread, which in the browser is the main thread. It was unmeasured and
unbudgeted, and two consumers had made large graphs ordinary rather than hypothetical: every section
of a song is instantiated and parked in the graph, and an agent writing an instrument has no
human-scale limit on how many nodes it asks for.

Measured in-engine at three node counts per shape, the build turned out to cost ~2.9–3.5× per
doubling of the document, against 2.0× for a linear build — roughly n^1.6 to n^1.8, corroborating a
browser harness that had seen ~4 s at 8 000 nodes. Four separate per-node scans of the flat wire list
were responsible, three in Instantiate and one in pipe dissolution.

The worst of the three shapes was the **nested** one, which is also the one that resembles a real
song: repetition is what puts hundreds of interface pipes in front of the dissolver. A flat document
is large but not shaped like anything anyone writes, so it under-reports the problem it is being
used to find — which is the argument for benching more than one shape, not for picking the right
one.

Two things about that are worth more than the fix:

**A base-ref comparison structurally cannot catch it.** Superlinearity is not a regression against
yesterday; it is a property of the code as it stands. The scans had been there for as long as the
code had, so every PR since compared clean against its parent, and would have gone on doing so
forever. A gate that only ever asks "worse than last time?" will hold a quadratic build steady
indefinitely and report green the whole way.

**An absolute number at one size cannot catch it either.** A per-node scan is an unremarkable figure
at any single node count. What distinguishes a build that doubles with its input from one that
quadruples is the *ratio between two sizes*, so the measurement is a size pair, and a benchmark that
picks one representative document — which is what every bench in this repo did — is blind to it by
construction.

## Decision

**The perf gate gates construction as well as render, and gates it on scaling rather than on level.**

A third bench layer (`construct_iai`) builds synthetic documents at three node counts that double,
in three shapes: `wide` (a fan-in tree — maximum edges, minimum depth), `deep` (a serial chain — the
dual), and `nest` (subpatch reuse, the repetitive shape a real song has, and the one that reaches
pipe dissolution). Each case is gated against the baseline like every other case.

**Additionally, `perf-gate.sh` fails the run when the Ir ratio between consecutive sizes of a shape
exceeds 2.4×** — linear is 2.0, warn at 2.2. That check reads no baseline at all. A PR that merely
inherits a superlinear build is as red as the one that introduced it, which is the only arrangement
under which the property gets fixed rather than grandfathered.

The band between 2.0 and 2.4 is room for the *fixed* per-build cost — registry construction, JSON
parse — that makes the small end of a sweep cheaper than proportional. It is not room for a per-node
scan, which lands near 3 and climbs. The runner reads a little higher than a dev machine (2.005–2.112
against 1.999–2.034 locally, on a different target-cpu), so the usable headroom is nearer 0.3 than
0.4 — worth knowing before anyone tightens the limit toward 2.2.

**The workload asserts its own shape.** A resource that fails to resolve is deliberately non-fatal
in this engine, so a `nest` document whose child stopped resolving would still build, still scale
linearly — a degenerate graph is linear — and still read as a large *improvement* against the
baseline. Both gates would be green while the only case exercising pipe dissolution had silently
stopped exercising it. The measured region therefore asserts zero load warnings and an exact node
count, turning that into a hard failure. This is the same hazard `perf-gate.sh` already handles for
`instruments/` by swapping fixtures with `src/`; `benches/` is deliberately never swapped, so the
generator has to state it itself.

## Consequences

The build is linear in node count and the shapes cost 3.9–6.5× fewer instructions at 8 192 nodes.
That is a consequence of the measurement, not the point of it: the point is that the exponent is now
a gated, recorded number, so the next scan to land is a red PR rather than a bug report from a
frozen tab.

Construction is still unbudgeted in *absolute* terms — nothing here says how many milliseconds a
Swap may take, only that the cost may not grow faster than the patch. An absolute budget wants a
target machine and a target song size, neither of which this repo owns.

The trend branch records each construct case's absolute Ir alongside the render layers, and the
dashboard charts them with the growth factor derived, so the exponent is readable across commits
rather than only at the moment a gate trips.

Two known scans are deliberately left: `from_graph_doc` rescans the wire list per node on **save**,
and `Graph::find` is linear per resource-bearing node at **load**. Neither is on the construct path
this gate measures, and neither has been shown to matter; they are named here so a later measurement
knows they were seen and not missed.
