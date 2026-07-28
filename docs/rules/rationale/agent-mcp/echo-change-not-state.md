# Why: A verb echoes the state when it wrote fields the caller did not specify, and echoes the change — from and to — when the caller specified exactly what changed.

[Rule](../../agent-mcp.md#echo-change-not-state)

Every document verb answers with a rendered echo, and the question the echo has to settle is *what
did that do?* The two halves of the vocabulary need different answers, and the split is mechanical
rather than a matter of taste:

- **The verb wrote things the caller did not specify** — a one-shot add lands a node whose unnamed
  inputs sit at descriptor defaults; a removal cascades into consumers the caller never mentioned;
  a rename rewrites references the caller never listed. What the caller does not already know is
  the *state*, so the echo is the state: the node zoom, the pipe view, the index after a removal.
- **The caller specified exactly what changed** — a value edit names an address, an input and a
  value, and nothing else moves. The caller already holds every byte of the resulting state, so a
  zoom re-tells it what it just said. What the caller does *not* hold is the prior value, and a
  projection structurally cannot supply it: the prior document is gone by the time one could be cut.

So the value verbs echo `address`, `input`, `from` and `to`. It is both smaller than a zoom and
strictly more informative, and that matters more than usual while undo is unbuilt: a from→to line is
the only pre-image an agent gets.

**It is a pre-image, not a guarantee of reversibility**, and the difference is worth stating because
it is easy to overclaim. The echo restores a *previous value* — set it back and the document is
where it was. It does not restore *absence*: `(unset) → 880` records truthfully that the slot held
nothing, and putting nothing back is a different move that the value verb cannot make. For a node
input the wiring verb's clear makes it anyway; for an interface pipe's value nothing does, by
decision rather than by omission (see
[value-verbs-one-address-space](value-verbs-one-address-space.md)). The echo's job is to carry
the fact; whether a verb exists to act on it is a separate question, and one this rule must not be
read as answering.

The rule is stated as a test on the *verb*, not as a list of verbs, because the list is what drifts:
the next verb added answers "did I write anything the caller did not name?" and its echo follows.

Decided in: issue #622 — settled directly, no ADR.
