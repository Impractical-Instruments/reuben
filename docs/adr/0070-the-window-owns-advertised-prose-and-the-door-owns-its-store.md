# ADR-0070 — The window owns the advertised sentence; the door owns its shape and its store

## Context

[ADR-0067](0067-core-splits-and-reuben-api-is-the-one-window.md) makes `reuben-api` the one window,
[ADR-0068](0068-reuben-api-declares-its-own-types.md) decides what flows through it, and
[ADR-0069](0069-reuben-api-is-one-crate-with-two-feature-halves.md) decides what the crate is. All
three were argued with one door built. The native CLI is the second, and a second door is the first
time "one surface, two doors" can be wrong rather than merely unproven.

Three questions only a second door asks: which prose is shared, how far sameness goes when the two
doors are not shaped alike, and what a door is still allowed to decide for itself.

## Decision

**The roster's tool-level sentences belong to the window.** They are the last piece of advertised
prose a door still owned; the field descriptions and the `$defs` descriptions already rode the
window's types. A sentence is what a model picks a verb by, so a per-door copy is a per-door
opinion about what a verb does, drifting silently.

The MCP door cannot name a const where it needs one — its tool macro takes a string literal — so
the door leaves the attribute off and **stamps** the window's sentence onto the router it built.
That is a mechanism, not a decision, with one trap worth recording: with the attribute absent the
macro falls back to the method's *rustdoc*, which is prose for a Rust reader. The fallback is
non-empty, so a roster test, a schema test and a markup scan all pass while a model reads the wrong
thing. The guard therefore asserts the stamped value, and the door refuses to start if a roster
contract has no sentence at all.

**A door whose surface is shaped differently writes its own help.** The CLI is five subcommands,
not twenty-two roster entries: `describe` alone serves two verbs, `play` and `scaffold-operator`
serve none. Single-sourcing its `--help` would mean authoring a merged sentence *twice* — once as
the merge, once as the parts — which is more copies of the prose, not fewer. The rule is about a
door that advertises the roster verb-for-verb, and the browser is the one that will.

So "one surface, two doors" is a claim about the verbs, the guards, the glosses, the result shapes
and the advertised sentences. It was never a claim that two doors are the same door.

**The rendered projection is the deliverable, so a door stops carrying a second serialization of
it.** The CLI's `--json` used to emit a structured shape per projection view. Nothing consumed it,
and the compact line grammar is what the projection exists to produce — a parallel structured
encoding that only one door had is free to drift from the one every other consumer reads. The
boundary is the exception and stays structured, because it has a real program behind it: the
control-surface generator parses those pipes. Same question, two consumers, one verb underneath.

**The resource store is the door's, and two doors may legitimately differ there.** The window
takes a store; it does not decide what one is. `validate` on the CLI **decodes** referenced
samples, because it is the dry run of the load `play` will do on the same machine moments later —
a sample that is present but undecodable has to surface as the warning `play` would hit. The MCP
sidecar stats instead: it authors documents and drives no audio, so decoding every referenced WAV
per call is waste. The introspection reads stat in both doors, because describing a port's metadata
never touches audio.

This is the boundary of the sameness claim, and it is easy to cross by accident in the direction
that loses information: routing every command through one convenient store makes the doors agree
and makes `validate` — the verb the window's own prose calls the single authority on whether a
document is legal — quietly stop noticing a broken sample.

## Consequences

The window grows an advertised-prose module beside the argument and result types, under the same
constraint they carry: it ships to a model, so no rustdoc markup, no issue numbers, no crate paths.

A door adding a roster contract now has two obligations rather than one — the contract and its
sentence — enforced at construction rather than by review.

`reuben describe <path> --view index|nodes|pipes|resources --json` changed shape. It is a
narrowing of a surface with no consumers, taken deliberately; the boundary view's shape is
unchanged and is the one under test.

Nothing here decides where the *view-argument coherence* question lives. The window refuses
`select` against the boundary view; the CLI routes the boundary away before that check runs, so the
flag is silently ignored there. That gap predates this work and is left open on purpose — it needs
an answer about which layer owns argument coherence when a door merges verbs, and merging verbs is
the CLI's shape, not the window's.

## Related

ADR-0067, ADR-0068, ADR-0069. Context topic: `agent-mcp`.
