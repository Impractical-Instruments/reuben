# Code as a grounding surface

> How this repo's own source text is governed as grounding an agent reads — comment discipline that points at rules instead of restating them, LSP-first navigation, and pre-scoped search.

## Now

The source tree is **read far more often than it is written, and mostly by agents.** That makes the
comments, the navigation path an agent takes, and the search scope it inherits grounding surfaces in
their own right, governed like every other one here: single-source it, point at it, guard it. The
[agent framework & MCP](agent-mcp.md) topic governs grounding for the agent *authoring instruments*;
this topic governs grounding for the agent *working on this repo*.

Comments carry **three** kinds of prose and only one belongs in the code. **Rationale** — a position
argued, a tradeoff explained, a constraint binding beyond the file — moves to a topic and leaves a
`// see rules: <topic>` pointer, because a pointer cannot drift and a copy always does. **Mechanics**
— the local fact the code cannot state itself — stays, because that is what `hover` and `cargo doc`
serve. **Restatement** is deleted rather than moved. Restatement is the largest bucket and the most
dangerous: one that has drifted from the line beneath it is indistinguishable from one that has not,
and agents read comments in preference to the code they sit on. So the reading posture is the other
half of the discipline — navigation is **LSP-first**, and every search runs inside the scope
[`.ignore`](../../.ignore) pre-declares.

A doc comment a `JsonSchema` derive turns into an advertised `description` is model-facing prose as
well as a comment. Rustdoc link syntax, an issue number, and an internal crate path are tokens the
model pays for and cannot resolve, and plain backticks cost the Rust reader nothing. The kind is
orthogonal to the split: such a description is usually mechanics and stays, but how it is *worded* is
governed here.

The discipline is **guarded, not merely documented**: `scripts/check_rules_refs.py` fails the build
across every crate, and the wire half is checked from the door's own `tools/list`, `resources/list`,
and `instructions` output rather than from what a type declares. Tests are grounding too — a test
whose job is that **two lists match** is evidence that generation was not attempted, and the
requirement is that it says which case it is, where someone reads it before trusting it.

## Rules

<a id="comments-never-restate-code"></a>
### A comment never restates the code or answers what LSP, grep, or glob would answer: rationale moves to a rules topic and the comment points at it, mechanics the code cannot state itself stays, and restatement is deleted rather than moved.

[why](rationale/code-as-grounding/comments-never-restate-code.md)

<a id="wire-descriptions-are-model-facing"></a>
### A doc comment generated into a wire surface is model-facing prose, not a comment: an advertised description carries no rustdoc link syntax, no issue number, and no internal crate path — and the guard reads what the door advertises, rather than what a type declares.

[why](rationale/code-as-grounding/wire-descriptions-are-model-facing.md)

<a id="lsp-first-navigation"></a>
### Code is navigated LSP-first — definitions, references, symbols, and hover — and text search is reserved for non-code text, so the language server is the one authority on what the code says.

[why](rationale/code-as-grounding/lsp-first-navigation.md)

<a id="search-is-pre-scoped"></a>
### Every search inherits the scope `.ignore` pre-declares, and bypassing it to reach build output, caches, or binary fixtures is a smell rather than a technique — nothing it hides is a source of truth.

[why](rationale/code-as-grounding/search-is-pre-scoped.md)

<a id="parity-test-is-a-defect-marker"></a>
### A test whose job is that two lists match is a defect marker rather than a solution: it carries a `Parity:` line recording why one list cannot be generated from the other, and a marker recording no reason fails the build.

[why](rationale/code-as-grounding/parity-test-is-a-defect-marker.md)

## Terms

- **Advertised description** — prose a door hands a model over the wire (a tool or schema `description`, the server `instructions`); generated from a doc comment, governed as model-facing prose.
- **Mechanics** — the one kind of prose a comment may carry: the local, non-obvious fact the code cannot state itself (a `SAFETY:` justification, a caller-upheld invariant, why a constant is this number).
- **Restatement** — comment prose that re-describes the code or answers what LSP or a search would answer; the third comment kind, deleted outright rather than moved to a rule.
