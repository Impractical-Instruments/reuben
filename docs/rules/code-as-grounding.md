# Code as a grounding surface

> How this repo's own source text is governed as grounding an agent reads — comment discipline that points at rules instead of restating them, LSP-first navigation, and pre-scoped search.

## Now

The source tree is **read far more often than it is written, and mostly by agents.** That makes the
comments, the navigation path an agent takes through the code, and the search scope it inherits into a
grounding surface in their own right — one governed by the same anti-drift mechanism as every other
grounding surface here: single-source it, point at it, guard it. The [agent framework &
MCP](agent-mcp.md) topic governs grounding for the agent *authoring instruments*; this topic governs
grounding for the agent *working on this repo*.

Comments carry **three** kinds of prose, and only one of them belongs in the code.
**Rationale** — why the system is this way: a position argued, a tradeoff explained, a constraint
that binds beyond this file — is a rule in the wrong place. It moves to a topic under `docs/rules/`
and the comment becomes a `// see rules: <topic>` pointer, because a pointer cannot drift and a copy
always does. **Mechanics** — the local, non-obvious fact the code itself cannot state: a `SAFETY:`
justification, an invariant a caller must uphold, why a constant is *this* number, a
non-obvious algorithm step, and the signature-level `///` on a public item — stays, because that is
what `hover` and `cargo doc` serve. **Restatement** — prose that re-describes what the code says, or
answers what `goToDefinition`, `findReferences`, or a `grep` would answer — is deleted outright. It
is not moved and it is not harvested: there is nothing there that the code does not already say, and
every copy of it is a line that can go stale while the build stays green.

The three-way split classifies what prose *says*; a second axis classifies where it *goes*. A doc
comment on a field of a `JsonSchema`-deriving type **is** that field's advertised `description`, and
the same sentence that serves a Rust reader through `cargo doc` is shipped verbatim to a model in
every turn that loads the schema. That makes it model-facing prose, and it must read as prose:
rustdoc link syntax the model cannot resolve, an issue number pointing at a closed argument, and an
internal crate path naming a module it has no access to are all noise it pays tokens to skip. Nothing
is lost for the Rust reader — plain backticks render fine in `cargo doc`. The kind is orthogonal to
the split: a wire description is usually **mechanics** and stays, but how it is *worded* is governed
here.

Restatement is the largest of the three and the most expensive, because it is invisible. A wrong
rationale comment at least contradicts a rule someone might check; a restatement that has drifted from
the code beside it looks exactly like a restatement that has not, and agents read comments in
preference to the code they sit on. So the reading posture is the other half of the discipline:
navigation is **LSP-first** — definitions, references, symbols, and types come from the language
server, and text search is reserved for the things it cannot see — and every search runs inside the
scope [`.ignore`](../../.ignore) pre-declares, so build output, caches, and binary fixtures cannot
answer a question about the source. A comment that duplicates what those two tools return is not a
convenience; it is a second copy competing with them.

The discipline is **guarded, not merely documented**: an unenforced convention decays back to the
level it started at. `scripts/check_rules_refs.py` fails the build on a module doc long enough to be
carrying rationale that names no topic, and on a pointer that reaches past its topic to a rule — in
every crate in the workspace. The wire half is guarded from the other side: an integration test
spawns the real sidecar and scans every description it advertises across `tools/list`,
`resources/list`, and the server `instructions`, so the check reads the door's own output and cannot
drift from what a client actually receives.

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

## Terms

- **Advertised description** — prose a door hands a model over the wire: a tool or schema `description`, a resource description, the server `instructions`. Generated from a doc comment, but governed as model-facing prose rather than as a comment.
- **Mechanics** — the one kind of prose a comment may carry: the local, non-obvious fact the code cannot state itself (a `SAFETY:` justification, a caller-upheld invariant, why a constant is this number).
- **Restatement** — comment prose that re-describes the code or answers what LSP or a search would answer; the third comment kind, deleted outright rather than moved to a rule.
