# Why: Setting a value reaches a node input and an interface pipe's value through one address space — the pipe is addressed as the node it mints, port `in` — and refuses a wired input rather than severing it; the wiring verbs stay node-only.

[Rule](../../agent-mcp.md#value-verbs-one-address-space)

**One address space, because an `interface.inputs` entry already *is* a node.** It mints an address
in the flat node namespace, the registry reserves its type name so nothing can collide, and internal
consumers wire from it with ordinary wire-refs. A node input literal and a pipe seed are the same
concept at every level that matters: both are written in the document, both are latched at load,
both are overridable by a live `send` that targets the address by name, both are ephemeral until the
next Swap re-reads the document. Two verbs for one concept cost the model a decision it has no
grounding to make, and the decision was the wrong way round for the commonest edit: the player-facing
controls of every shipped instrument are pipes.

The agent-facing word is **value** in both places; the field on disk stays `default`. A format
migration across ~20 fixtures for a cosmetic win is not worth it, and the precedent is already set —
`doc` on disk, `description` in the view. See the near-collision note under **Value** in the
[index](../../README.md#avoid-these-synonyms): the word is not the Arg type.

The pipe's meta verb keeps what its name always claimed and nothing more: the *quantity* contract —
channel binding, range, curve, unit — around a value it does not own.

**How far the shared address space actually reaches, and why the rule says so.** Setting a value
reaches a pipe; **wiring does not**. Whether a pipe should be wire-addressable is a live design
question with consequences this rule does not get to decide — a pipe's `in` is fed by whatever is
outside the boundary, so a wire into it means something different from a wire into a node input.
Until that is settled the wiring verbs address nodes only, and the rule names the limit rather than
implying a symmetry the code does not have. An address space that only some verbs honour is a worse
grounding surface than two honest ones, because a model cannot tell which verbs are in the club — so
the advertised sentence scopes the claim to the verb that honours it, and does not describe a
namespace.

**Two known asymmetries, recorded rather than left as folklore:**

- A node input can be returned to *unset* (the wiring verb's clear does exactly that). **A pipe's
  value cannot**: nothing reaches a pipe address to clear it, and the meta verb no longer writes
  that slot. So seeding a pipe that had no value is a one-way edit today. That is a consequence of
  the deferred question above, not an independent choice, and it is the one place where this rule's
  own "cheap to undo" argument does not hold.
- The refusal below is what keeps that from being worse: a value edit that cannot be undone must at
  least never destroy something the caller did not name.

**Refusing a wired input, rather than severing it with a note.** The severance is the destructive
half of a verb whose purpose is not destruction, and in the shipped library it is the common case
rather than the exotic one: the vocabulary's target inputs are wired, and every one of them is fed by
an interface input pipe. Three things decide it:

- A note is a warning nobody reads. `remove_instrument_node` may cascade loudly because destruction
  is what it is *for* — the author asked for it and reads the receipt. A value edit's author asked
  for a number.
- The composite that fans a value out across an instrument is built on this surface, and it follows
  a wire to the pipe feeding it rather than severing it. A base verb that silently severed would put
  the fan-out one bug away from dismantling an instrument.
- Refusing stays cheap. Undo is not built, so the agent holds no pre-image and a severed wire is not
  recoverable from the result; a refusal costs one bounce and hands back the verb name that severs
  on purpose.

Decided in: issue #622 — settled directly, no ADR.
