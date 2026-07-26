# Why: Code is navigated LSP-first, and text search is reserved for non-code text.

[Rule](../../code-as-grounding.md#lsp-first-navigation)

A language server and a text search answer different questions, and only one of them is authoritative
about code. `goToDefinition` resolves the definition that a given use *actually binds to*;
`findReferences` enumerates the call sites that actually exist; `hover` reports the type as the
compiler sees it. A grep for the same name returns every lexical match — the definition, the
shadowing local, the doc comment mentioning it, the string literal, the unrelated field with the same
spelling — and silently omits what it cannot see: a re-export, a macro-generated item, a trait method
reached through a blanket impl. For a rename or a signature change the difference is not stylistic:
the reference list has to be complete, and only one of the two tools can promise that.

`reuben-core` is tens of thousands of lines, and several files punish a whole-file read outright.
`documentSymbol` plus a ranged read costs a fraction of what grepping into an unbounded read does,
and lands on the definition rather than near it. The file-by-file inventory lives in `AGENTS.md`,
where it is read per change.

The rule pairs with [comments-never-restate-code](comments-never-restate-code.md), and neither stands
alone. Deleting a comment because `hover` would answer it is only safe if `hover` is what gets
consulted; conversely, navigating by search is what makes duplicated prose feel necessary, because
search results are ambiguous in exactly the way a comment seems to resolve. Removing the second copy
and adopting the tool that makes it unnecessary are one change, not two.

Decided in: issue #635 — settled directly, no ADR.
