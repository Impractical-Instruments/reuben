# Why: A comment never restates the code or answers what LSP, grep, or glob would answer.

[Rule](../../code-as-grounding.md#comments-never-restate-code)

Comments were an **unguarded third copy of the rules corpus**, and the measurement is what forced the
rule: 14,568 comment lines against 62,059 total, 23.5% of the tree, with the `// see rules: <topic>`
pointer convention already present in 29 of 171 `.rs` files — established, and stalled at 17%.
Nothing checked any of it. Nothing ever could, because prose about a design sitting in code has no
build-time relationship to the design.

The exemplar was nine lines atop `reuben-core/src/tools.rs` arguing that the tool schemas and bodies
stay per-door because they need "rmcp/schemars machinery reuben-core must never depend on" — a
position that had been **false** since core gained an optional `schemars` feature and `EditResult`
began deriving `JsonSchema` behind it. It stayed green, read authoritative, and was cited as
justification for keeping duplication that the repo had already decided to remove. That is the whole
failure mode: a comment cannot be wrong loudly.

The **three-way split** is what makes the rule actionable, and the third bucket is the one that was
missing from the original framing. Rationale — a position argued, a tradeoff explained, a constraint
binding beyond the file — is a rule that landed in the wrong file; it moves to a topic and leaves a
pointer, since a pointer cannot drift. Mechanics — a `SAFETY:` justification, an invariant a caller
must uphold, why a constant is *this* number, a non-obvious step, the signature-level `///` on a
public item — stays, because it is exactly what `hover` and `cargo doc` exist to serve and no rule
would ever carry it. **Restatement** is neither, and it is the largest share: prose that re-describes
the code, or reports what `goToDefinition` and `findReferences` already answer. It is deleted, not
moved and not harvested, because there is nothing in it the code does not already say.

Deleting rather than harvesting restatement matters for cost, not just tidiness. Treating the sweep as
"relocate 14.5k lines" prices it as a rewrite of the corpus; recognizing that most of it is
restatement prices it as a deletion with a small harvest. But the ordering is still
**harvest → write the rule → point at it → delete**, per module: the corpus was distilled from ADRs,
never from comments, so an unknown fraction of the rationale bucket is the only copy of its reasoning
and a delete-first sweep destroys it.

Restatement is also the *dangerous* bucket, which is why it earns a rule of its own rather than a
style note. A stale rationale comment contradicts a rule a reader can go check. A restatement that has
drifted from the line beneath it is indistinguishable from one that has not — and agents read comments
in preference to the code they sit on, so a drifted restatement is believed. That is the drift surface
this rule exists to close, and it is why the rule is paired with
[lsp-first-navigation](lsp-first-navigation.md): "do not write what `hover` returns" is only coherent
if `hover` is how the code gets read.

Guarding it is not optional. The pointer convention reached 17% on discipline alone and stopped there,
so the rule ships with a build-time check: a `//!` module doc past a length threshold that names no
topic is failing the build, applied crate-by-crate as each is swept so the gate is never green by
exemption. A length heuristic cannot classify prose — it only catches the shape rationale takes when
it accumulates — but it is enough to keep the ratchet from slipping backwards, which is the failure
the 17% documents.

Decided in: issue #635 — settled directly, no ADR.
