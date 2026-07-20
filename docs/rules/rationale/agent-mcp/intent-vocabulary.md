# Why: Musical intent language grounds in one curated, registry-keyed word-to-move table delivered in-prompt and instrument-blind, joined to the concrete document in the agent's context, and kept referentially fresh by CI and musically fresh by evals.

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
only — the registry-owned vocabulary — and never name an instrument, a file, or a pipe. The join to
the concrete instrument happens **in context**: under the whole-document edit contract the agent
already holds the document, whose nodes name their operator types, so the model matches rows to nodes
itself ([whole-document-edit](whole-document-edit.md)). A recipe-authoring guideline carries the
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
