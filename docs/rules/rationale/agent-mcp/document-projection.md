# Why: The agent never loads a reuben-owned document into its context: its whole view is a set of partial structural projections — index, node zoom with reverse edges, pipes, resources — single-sourced in reuben-core and lossless only in aggregate.

[Rule](../../agent-mcp.md#document-projection)

Once the agent is no longer obliged to re-emit a document ([document-verbs](document-verbs.md)), its
*read* stops needing to be lossless — and that, not compression, is where the win comes from. A 1:1
structural projection of a 35 KB instrument is ~4.5 KB: only ~3×, pure re-encoding, and not worth an
API on its own. What the dropped obligation buys is a legal **partial** read — the index plus a zoom
of the two nodes a turn touches is ~1.7 KB against 35 KB, ~20× — and the ratio grows with the
document, because the index grows linearly while each zoom stays flat. Compression would have
preserved every byte; this preserves only the bytes the turn needs.

The views are **partial individually, lossless in aggregate**: every field of the format is reachable
through *some* view (index ∪ node zoom ∪ pipes ∪ resources), so the agent is choosing a view rather
than the projection discarding data, and a completeness guard walks the real format types so a new
field no view exposes fails the build. That guard exists because the failure mode this surface must
defend against is not "too big" but **"the agent never noticed what was missing"** — unlike JSON,
which is lossless by construction, a hand-maintained read surface leaks silently. The same instinct
is why anything a view ought to show and cannot gets a note saying why, and why a document that fails
to load still projects, flagged not-loadable: going blind exactly when the agent needs to see is the
worst possible failure.

**Reverse edges are load-bearing.** A node's zoom carries its consumers, not just its sources,
because without them the agent is blind to the blast radius of every destructive verb. The
projection is measured by the authoring harness rather than assumed complete. Interface pipes get
their own view for the plain reason that they are bulky (90 pipes in a
~6 KB `interface` block on a real instrument) and orthogonal to the node graph.

The projection lives in `reuben-core`, so what the CLI and the sidecar show an agent cannot drift —
cross-door divergence is a compile error rather than a runtime surprise
([portable-tool-contracts](portable-tool-contracts.md)). The web in-page layer still serves the older
by-value boundary view; converging it is designed, not built. It judges nothing: validate remains the
single authority on whether a document loads ([loader-single-authority](loader-single-authority.md)).

Distilled from: ADR-0066
