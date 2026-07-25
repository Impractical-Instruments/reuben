# Why: Every search inherits the scope `.ignore` pre-declares, and bypassing it is a smell.

[Rule](../../code-as-grounding.md#search-is-pre-scoped)

`.ignore` is read by the search tools themselves, so the scope applies without anyone remembering to
pass a flag — the same reason the toolchain pin and the git hooks are checked in rather than described.
What it hides is build output, `.git`, caches, and binary fixtures: **generated or derived**, every
one. A hit inside them is at best a duplicate of a hit in the source that produced it, and at worst a
stale copy from an older build that reads as current evidence.

So `--no-ignore` does not widen a search, it corrupts one. The `target/` directory alone can carry
several generations of generated code, and a grep that finds a symbol there answers "this spelling
existed at some point" when the question was "does this spelling exist now." Whatever a bypass finds
either has a source-of-truth counterpart the scoped search already returned, or is not a source of
truth at all.

The honest exception is debugging the build or the ignore file itself — inspecting an artifact
deliberately, knowing it is an artifact. That is a different activity from navigating the source, and
it is why this is a smell rather than a prohibition: reaching for the flag while answering a question
about the code means the question was aimed at the wrong tree.

Decided in: issue #635 — settled directly, no ADR.
