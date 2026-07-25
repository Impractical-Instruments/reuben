# Why: A test whose job is that two lists match is a defect marker rather than a solution: it carries a `Parity:` line recording why one list cannot be generated from the other, and a marker recording no reason fails the build.

[Rule](../../code-as-grounding.md#parity-test-is-a-defect-marker)

Single-sourcing did not erode here through bad discipline. `operator_contract!` emits the index
consts **and** the `Descriptor` from one declaration, and operator contracts do not drift — not
because a rule forbids hand-writing a descriptor, but because the macro is cheaper than hand-writing
one. The same discipline, applied where generation got expensive (per-door schema derives, each
flavoured by its own host's machinery), produced a second list and a test to hold it level. What
varied was the mechanism, not the care.

A test asserting two lists match is therefore **evidence about the design, not a property of it**.
It says: there are two lists, and nothing but this assertion keeps them equal. That is worth knowing.
The problem is that it does not read that way — a green parity test reads as reassurance, and the
list it guards reads as safe. `stdio_tools_list.rs` asserting the advertised roster against
`CONTRACTS` was the standing evidence that the argument surface behind those names was *not*
single-sourced, and it was read for years as proof the tool surface was fine.

So the marker is not a warning label on a bad test. Some parity tests are correct and permanent: two
lists genuinely cannot be generated from each other when one lives behind a boundary the other
cannot cross — a wire response the door produces at runtime, a foreign crate's derive, another
repo's file. Those deserve a parity test and always will. The requirement is only that the test
**says which case it is**, in the place someone reads before trusting it. A recorded reason turns a
green check back into the piece of evidence it always was.

The guard checks the half a machine can decide: a `Parity:` marker exists, and it records something.
It cannot decide the other half — whether an unmarked test is parity-shaped — and deliberately does
not try. The shapes that look decidable are not: `assert_eq!(a.len(), b.len())` appears seven times
in this workspace and every one is a round-trip comparing a value with itself through a transform,
while the parity test that motivated the rule matches no name or shape pattern at all. A detector
with that ratio trains people to ignore it, which is the same failure the
[claim ledger](../../README.md#conventions) refuses when it routes counts to a reviewer instead of
guessing at them. The forcing function is that the reason is demanded at **writing** time, while the
author still knows whether generation was tried.

Guarded by: scripts/test_check_rules_refs.py::test_parity_marker_without_a_reason_fails

Decided in: issue #634 — settled directly, no ADR.
