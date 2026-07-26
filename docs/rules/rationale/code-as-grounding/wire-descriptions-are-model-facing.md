# Why: A doc comment generated into a wire surface is model-facing prose, not a comment.

[Rule](../../code-as-grounding.md#wire-descriptions-are-model-facing)

The [comment sweep](comments-never-restate-code.md) split comment prose three ways — rationale moves
to a topic, mechanics stays, restatement is deleted — and then hit a case the split could not
classify. A doc comment on a field of a `JsonSchema`-deriving type is not only read by whoever opens
the file. It **is** that field's advertised schema `description`, handed to a model in every turn
that loads the tool. The sweep's own linter had to carve it out: its issue-citation check was put out
of reach of `///` precisely because a field doc and a comment are textually identical and governed
differently.

Dumping the real `tools/list` from the sidecar is what made the size of it visible. Across the whole
advertised roster, eight descriptions carried markup written for a Rust reader and shipped to a model:
`[`projection`](crate::projection)`, `[`Descriptor::inputs`]`, `(#608)`, `(`reuben_core::format`)`.
The model can resolve none of it. It cannot follow `crate::projection`, cannot look up an issue, and
has no access to a module named `reuben_core::format` — it pays tokens for the brackets and skips
them. And the leverage is lopsided: one of the eight, `EditResult`'s `zoom` field, is repeated across
every document verb's `outputSchema`, so a single reworded sentence moved an advertised copy of it
on every one of them.

Nothing is traded away to fix it. Plain backticks render in `cargo doc` exactly as well as a link
does for the sentence's meaning; what is lost is a hyperlink, and what is gained is prose a model can
read. So the rule is not a compromise between two audiences — the Rust reader was never being served
by the part the model chokes on.

The kind is **orthogonal to the three-way split**, which is why this is a rule of its own rather than
a fourth bucket. `zoom`'s description is textbook mechanics and stays under the existing rule; this
one governs how it is *worded* because it also crosses a wire. Collapsing the two would blunt the
split that is the comment rule's whole value.

Unlike comment prose, this **is** mechanically checkable, because the advertised surface can be
dumped. That decides where the guard reads from: not the declarations, which would mean
re-implementing which doc comments reach the wire and getting it wrong — a struct-level doc on a
tool's `…Params` is replaced by the `#[tool(description = …)]` string and never ships. The guard
spawns the real shim and scans what comes back. A surface the door does not advertise cannot fail it,
and a surface it does advertise cannot escape it.

It scans **all three** advertised surfaces — `tools/list`, `resources/list`, and the server
`instructions` — though only `tools/list` had offenders: a rule that says "an advertised description"
and a guard checking one of three places is a guard that does not test its claim. The same reasoning
bounds it: the guard reads advertised *metadata* only, never resource payload.
`reuben://guide/authoring` is Markdown, and its links are the kind a model can follow. Every
advertised description is grounding the model pays for on every turn — metric (a) in
[`eval/`](../../../../eval/README.md) — and each rewrite is strictly shorter than what it replaced.

Guarded by: crates/reuben-mcp/tests/stdio_tools_list.rs::advertised_prose_is_model_facing

Decided in: issue #637 — settled directly, no ADR.
