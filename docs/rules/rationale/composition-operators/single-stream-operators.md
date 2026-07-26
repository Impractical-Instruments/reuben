# Why: An operator is authored as one single-voice, single-channel stream — one input block plus its state to one output block — and cross-cutting fan-out lives in structural constructs above the operator layer, never inside an operator.

[Rule](../../composition-operators.md#single-stream-operators)

A synth has many voices, each spanning channels — so the voice×channel fan-out has to be handled
*somewhere*. The choice is: every operator author handles it, or the engine does. Authoring ergonomics
and AI-authorability are first-class, and operators must stay small and composable, so the engine
owns it. An author writes **one stream** — a single voice, a single channel, a block at a time:
"given one input block and my state, produce one output block." The author (or agent) reasons about
one stream, never the matrix, and *cannot* botch fan-out because they never touch it. ("Single-voice"
is about stream multiplicity, not sample granularity — an operator always processes a whole block.)

Cross-cutting concerns are therefore expressed **above** the operator layer, as structural constructs
rather than inside ordinary operators: voicing / note→voice assignment is the Voicer
([nesting-inline-or-host](nesting-inline-or-host.md)); mixing / summing is deterministic fan-in in the
connection layer, not an operator; panning / channel spread is a boundary construct. The reasoning is
centralization: the engine carries the fan-out machinery *once* instead of smearing it across every
operator's `process`.

Polyphony is the Voicer hosting standalone voice patches, so an operator is a plain single-stream
node with no voice awareness at all
([operator.rs](../../../../crates/reuben-core/src/operator.rs)).

Distilled from: ADR-0010, ADR-0032
