# Why: A missing or malformed resource degrades the node to silence with a surfaced warning, while structural and wiring errors stay fatal.

[Rule](../../authoring-library.md#load-errors-degrade-dark)

A missing sample or a bad decode **must not crash** a live instrument mid-performance — but it is a
real authoring error the user has to see. So load outcomes split into two tiers. Structural/wiring
errors stay **fatal** (`LoadError`: unknown type, duplicate address, port-kind mismatch) — the graph
genuinely cannot be built. Resource errors become **non-fatal warnings**: a node naming a missing
id, a resolve failure, or a decode failure binds an **empty (zero-length) `SampleBuffer`** — so
`process` outputs silence — and a structured `LoadWarning` is collected. Hard-failing the whole load
on any resource error was rejected: it is consistent with the other `LoadError`s, but a single
missing file would take down an entire rig.

The division of labour matters: **core returns structured warnings; the shell surfaces them**
(stderr, app log, the web player's diagnostics) — presentation stays at the boundary, and the
convenience never *swallows* a problem every shell must show. The silent node is a *reachable,
tested* state, not a defensive dead branch, precisely because it is a normal degraded mode of a
correct load.

This "degrade dark, warn loudly, never fatal, never silent" discipline is the philosophy the rest of
the authoring surface inherits rather than reinventing. A surface control whose `bind` names no pipe
is skipped with a warning; a widget kind a target cannot render is skipped loudly; the v2→v3
migration *ignores* leftover presentation with a warning rather than failing — all the same shape.
The invariant it protects is that an authoring mistake costs a quiet control or a silent voice and a
visible diagnostic, never a crashed instrument.

Distilled from: ADR-0016
