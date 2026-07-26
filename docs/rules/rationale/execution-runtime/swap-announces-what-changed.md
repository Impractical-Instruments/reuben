# Why: A Swap announces what happened to the sounding graph rather than leaving it to be discovered by ear.

[Rule](../../execution-runtime.md#swap-announces-what-changed)

A [Swap](plan-lifecycle.md) is the moment an edit becomes sound, and its only feedback channel is
the music — which cannot distinguish "the swap did something structural you did not intend" from
"the edit sounded worse than you hoped."

So the swap reports its own structural outcome, keyed by the survivor fingerprint. **`survived`** is
how many nodes kept their state. **`state_reset`** lists addresses present in *both* documents whose
node did not survive — a type change, or an instantiate-time fingerprint change. That list is the
one most worth having: it is the case where the document looks unchanged at the address in question
and the sound changed anyway, because the node was rebuilt cold. Without it, the only symptom is a
filter that suddenly rang out or an envelope that restarted, and nothing to attribute it to.

**`added`** and **`removed`** catch a different accident: whole-document re-emission. An agent or a
tool that meant to tweak one parameter but re-emitted the document with a typo'd address produces a
swap that removes `/voice1` and adds `/voicel`, and the report says so — while it is still cheap to
fix, rather than after a session of wondering where the voice went. This is the failure that a
document-level diff would not catch either, because the document *is* internally consistent; only
the comparison against what is currently playing reveals it.

The shape extends past the gapless case on purpose: a door that rebuilds every node cold reports
`survived: 0` rather than omitting the field or inventing a number, so a client reads structural
outcome the same way everywhere and weaker swap semantics are *visible* as weaker instead of
indistinguishable from nothing having survived.

Decided in: issue #639 — settled directly, no ADR.
