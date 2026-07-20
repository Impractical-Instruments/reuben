# Why: A curated player-facing control is a Good Button, built from composition rather than new format machinery.

[Rule](../../authoring-library.md#good-button)

**Good Button** is the official term for both the design principle — a control that is hard to make
sound bad — and the artifact — a curated, often range-mapped control. "Meta param," "meta-control,"
and "macro" all name the same thing and are avoided.

The load-bearing decision is that a Good Button is **built from composition, not from format
machinery**. A brightness knob is an identity `map` at the public address whose output fans (via the
graph's existing message fan-out) to N ranged `map`s — `map_cutoff [0,1]→[800,10000]`,
`map_res [0,1]→[0.2,0.7]` — each feeding one internal input, the per-target ranges living in the
maps' params. The fan is free; the public address is just that node's address; control reaches it
exactly as any OSC to a node. So the surface metaphor "one good knob to N enumerated, transformed
targets" needs **no new instrument-format section** — it is operators plus existing wiring. A "fan
map" primitive (one input, N differently-ranged outputs) was rejected: it buys nothing over
composition and a variable-output-arity operator has no home in the static-`Vec` descriptor model.

This is why the concept survives the surface-doc decoupling intact. When presentation moved to
interface pipes + surface docs ([surface-docs](surface-docs.md)), the Good Button *pattern* did not
change — it is still a composed fan, now exposed as an interface pipe and bound by a surface widget.
It is durable enough that instrument reuse seeds "the Good Button fan-out" as a first-class library
recipe ([instrument-is-the-unit-of-reuse](instrument-is-the-unit-of-reuse.md)). The reason to keep
naming it is prescriptive: it tells an author the unit of a good playable control is a *curated,
range-shaped* control assembled from the graph, not a raw internal parameter thrown onto a fader.

(The carrier-era scaffolding around the original Good Button — the Message-vs-Signal control domain,
the `m2s` converter, the one-port-one-type sweep — is retired to
[composition-operators](composition-operators.md); what survives here is the curated-control principle
and its composition-not-format-machinery construction.)

Distilled from: ADR-0017
