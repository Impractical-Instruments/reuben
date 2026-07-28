# Why: Musical intent language grounds in one curated, registry-keyed word-to-move table — instrument-blind, delivered in-prompt for reading and applied by the engine as one atomic batch of value edits for writing — kept referentially fresh by CI and musically fresh by evals.

[Rule](../../agent-mcp.md#intent-vocabulary)

An agent turning "make it warmer / busier / sadder" into parameter moves needs the word→move mapping
stated somewhere. Prior art is decisive on two points: every published mapping binds to a *fixed*
parameter space, so what transfers to reuben's arbitrary operator graph is the word→parameter-**move**
form, not a word→parameter-vector; and a model with the mapping **in its prompt, zero-shot**, beat
learned optimization — **in-prompt grounding is the validated form.** Word traffic is also heavily
cliffed (a handful of words dominate), so the vocabulary can be small with confidence. Hence one
curated **document** — not a lookup tool (which prices the mapping into the volatile tail and hides
it until asked) and not per-instrument annotations (which mint N drift pairs and cover only annotated
instruments).

The binding is **instrument-blind and registry-keyed**: rows key on operator types + input names
only — the registry-owned vocabulary — and never name an instrument, a file, or a pipe.

The join to a concrete instrument was originally left **in context** — the projection names each
node's operator type, so the model could match rows to nodes itself. Measured against the shipped
library, that join is the expensive part and the mechanical part: thirty word×instrument pairs fan
out past one target (*looser* on `acid-techno` is nine), and each target also costs the model a
range lookup and a piece of arithmetic. So the table became a **machine contract** — `direction` is
a closed set, `magnitude` a closed set of forms, and only `description` stays prose — and the engine
does the join in one atomic verb. Three things that decides, none of which a model reliably gets
right: the step is per **curve class**, not per range, because a wide declared range is a safety
envelope rather than the musical one (a fraction of `clock.tempo`'s `[1..999]` reads *slightly
faster* as 120 → 220 BPM); a **wired** input is followed to the interface pipe feeding it and that
pipe's own value moves, because in the shipped library the vocabulary's targets are mostly wired and
writing a literal there would silently sever the instrument's own control surface; and a move with
no seat in this document is a **skip**, because an instrument-blind table missing most rows on most
documents is the design working. What is left in context is what a table cannot decide — a word
outside it, and whether the result sounds right — so the in-prompt view stays exactly what it was.

A recipe-authoring guideline carries the
transfer to nested instruments — a face pipe uses the same name the move targets (`cutoff`, `decay`,
…) — so type-keyed vocabulary reaches faces by name. Freshness is split by what each check can own:
**referential** truth is mechanical — a staleness test parses every move and asserts its operator
type + input still exist in the registry, so an operator rename breaks the build, not the agent;
**musical** truth (does "warmer" still do the right thing) is judged by evals, since no mechanical
check can own it. The structured source is agent-drafted but **human-locked**; the prompt sees a
compact generated view. The table is engine-canonical (it lives next to the registry that sweeps it)
and serves all three lanes from one artifact — skills point at it, MCP serves it as
`reuben://guide/vocabulary` read at request time, web bundles it — the delivery axis
[cross-lane-grounding](cross-lane-grounding.md) governs.

Distilled from: ADR-0058
