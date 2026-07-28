# Why: Every input is addressable through one address space — an interface pipe is the node it mints, port `in` — and a verb whose operation is meaningless on an address refuses by naming what that address is: a boundary input cannot be wired because it is fed from outside the graph, and a wired input is never silently severed.

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

**The address space is one; the operations over it are not.** From a consumer's side every input is
the same thing — it can be set to a value, and all but this document's own boundary inputs can be
wired — so every verb that addresses an input resolves every input address. What differs is whether
a given operation *means* anything there, and the rule is that a verb which cannot act says so **in
terms of what the address is**.

The alternative, which shipped first and was wrong, is reporting `no node at address /cutoff`. That
sentence is false: `/cutoff` is a first-class address in the flat namespace, which is the whole
premise of sharing one. A model told an address does not exist looks for a typo, invents a node, or
gives up; a model told *this is the instrument's boundary input `cutoff`, here is what reaches it*
makes the next call correctly. An address space only some verbs resolve is worse than two honest
ones, because nothing tells the model which verbs are in the club — and the fix for that is to make
every verb resolve it, not to advertise a caveat.

**Why a boundary input cannot be wired** is a property of the document, not of how it is being used.
This document's `interface.inputs` pipes are its **boundary**: what feeds them is outside the graph
— a live `send`, a channel binding, or the host's wire onto this face when the document is nested.
A wire from inside would stop them being a boundary. That is a local fact, true whether the document
is played at top level or nested, so the refusal needs no notion of context. It also does not reach
*other* instruments' inputs: a nested child's interface names appear as ordinary ports on the
`subpatch` node's own address, a synthesized boundary face, and wiring those is plain node
addressing that has always worked. Wiring *from* a pipe is likewise ordinary and untouched — it is a
source like any other.

**One asymmetry, recorded rather than left as folklore.** A node input can be returned to *unset*
(the wiring verb's clear does exactly that). **A pipe's value cannot**: no verb clears one, and the
meta verb no longer writes that slot. Adding a clear through the value verb was rejected on the same
grounds this whole ticket rests on — it would be a second verb-path for what the wiring verb already
does to a node input, which is the defect class being corrected, not a fix. So seeding a pipe that
had no value is a one-way edit, and it is the one place this rule's own "cheap to undo" argument does
not hold. The refusal below is what keeps that from being worse: an edit that cannot be undone must
at least never destroy something the caller did not name.

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
