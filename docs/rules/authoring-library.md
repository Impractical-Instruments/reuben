# Authoring surface & instrument library

> How authoring surfaces and the instrument library sit on top of the graph — decoupled surface docs over interface pipes, Good Buttons, the sample/resource store, library resolution and format versioning, and the launch Toys.

## Now

Everything in this topic sits **on top of** the one recursive graph and its operator model
([composition-operators](composition-operators.md)): the surfaces a human plays, and the library
of instruments they play. The graph renders sound; this layer is how a person authors, reuses,
and reaches into it.

An instrument is a document, and the document is the durable thing — you save the `InstrumentDoc`
you loaded and edited, references intact; `from_graph` is a deliberately one-way flatten for
export, so the render engine carries no authoring state to reverse-engineer. A document names its
nested children and its samples by **source**, resolved sibling-first (a library patch bundles its
private sub-patches next to itself) with a configurable instrument root behind it, and the resolver
canonicalizes identity so two spellings of one path are one thing. The document carries a
`format_version`; absent means 1, save writes the current version, breaking shape changes bump it
and ship a parse-time migration, and only a *newer-than-known* document is refused. That whole
version invariant — refuse the future, migrate the past, strip retired presentation, stamp — is
held by a type rather than by prose.

Instruments depend on more than params. A sample player needs **external bytes** — an audio file
resolved and decoded before render — so decoded audio lives in a `ResourceStore` outside the
operator, off the RT path and behind one pure accessor. Load failures follow a settled discipline:
a missing or malformed resource **degrades the node to silence** with a surfaced warning, while
structural and wiring errors stay fatal — the "dark-degrade" philosophy the surface layer inherits.

A person plays an instrument through a **surface**. Presentation is decoupled from the instrument:
the player-facing boundary is the instrument's `interface` input pipes (the engine-enforced quantity
contract, defined in [composition-operators](composition-operators.md)), and a **surface doc** is a
separate, reference-based document that binds pipe *names* to widgets — change a pipe's range and
every surface follows. With no surface file at all, a default is auto-derived straight from the
wireable pipes, so a new instrument is instantly playable with zero configuration. One surface
format and one resolver semantics project to two targets — the live web renderer and a disposable
TouchOSC `.tosc` — over a superset widget vocabulary each target renders its subset of, skipping the
rest loudly. Five kinds ship in both targets (`fader`, `radial`, `param-toggle`, `note-toggle`,
`chord-button`); `xy-pad`, `grid`, `visualizer`, and `keyboard` are reserved in the format and
**not built in either target**. A curated control that is hard to make sound bad is a **Good Button**, built from
composition (a fan of `map`s to enumerated targets), never from new format machinery.

Reuse rides the same machinery. The one unit of reuse is a validated instrument document consumed
by-reference through a `subpatch` node — there is no separate "recipe" format, directory, or kind;
an instrument's *role* (a voice while hosted by a voicer, a nested instrument while referenced) is
read off its interface, never off a path or filename. An instrument's reuse story — its
**recipe-role** — is the first sentence of its `doc` field, trusted for selection only (the face is
always projected mechanically from the `interface` block and enforced by the loader), and discovery
is a **generated signature-line index** over the available-set, never a hand-kept curated list. The
launch **Toys** — the beginner instruments that prove the thesis "instant music for a non-technical
person" — are exactly this: each is an ordinary instrument assembled from existing operators plus a
generated surface, one per distinct player gesture, never new format machinery.

## Rules

<a id="resource-store"></a>
### Decoded resource bytes live in a central Coordinator-built ResourceStore, are referenced by logical id from a top-level resources table, and are read on the RT path through one pure accessor.

[why](rationale/authoring-library/resource-store.md)

<a id="load-errors-degrade-dark"></a>
### A missing or malformed resource degrades the node to silence with a surfaced warning, while structural and wiring errors stay fatal.

[why](rationale/authoring-library/load-errors-degrade-dark.md)

<a id="surface-docs"></a>
### Player-facing controls are the instrument's interface input pipes, and presentation lives in a separate reference-based surface doc that binds pipe names to widgets.

[why](rationale/authoring-library/surface-docs.md)

<a id="advertised-range-is-a-subset"></a>
### An interface override may narrow what a port advertises but never widen it past what the engine enforces: a range override must be a non-inverted subset of the inner port's range, while `unit` renames freely because it cannot lie about a value the engine will accept.

[why](rationale/authoring-library/advertised-range-is-a-subset.md)

<a id="default-surface"></a>
### With no surface file, a default surface is auto-derived from the wireable input pipes so every instrument is instantly playable with zero configuration.

[why](rationale/authoring-library/default-surface.md)

<a id="surface-format-two-targets"></a>
### One surface format and resolver projects to both the live web renderer and the disposable TouchOSC .tosc over a superset widget vocabulary, each target rendering its subset and skipping the rest loudly.

[why](rationale/authoring-library/surface-format-two-targets.md)

<a id="good-button"></a>
### A curated player-facing control is a Good Button, built from composition rather than new format machinery.

[why](rationale/authoring-library/good-button.md)

<a id="library-resolution"></a>
### A nested reference names a source, never a version, resolved sibling-first with a configurable instrument root, and the resolver canonicalizes identity so two spellings of one source are one identity.

[why](rationale/authoring-library/library-resolution.md)

<a id="document-is-save-source"></a>
### The loaded and edited InstrumentDoc is the save source of truth, while from_graph is the one-way flatten and export path that inlines every nested reference.

[why](rationale/authoring-library/document-is-save-source.md)

<a id="format-versioning"></a>
### The document carries an integer format_version where absent means 1; a breaking shape change bumps it and ships a parse-time migration, additive changes never bump, and only a newer-than-known document is refused.

[why](rationale/authoring-library/format-versioning.md)

<a id="normalized-doc-gate"></a>
### The version gate is a type: a NormalizedDoc mints once at the parse boundary and every build and load path accepts it, so a document past the gate is current-shaped and migrated exactly once.

[why](rationale/authoring-library/normalized-doc-gate.md)

<a id="instrument-is-the-unit-of-reuse"></a>
### The one unit of reuse is a validated instrument document consumed by-reference through subpatch, and an instrument's role is read off its interface, never off a path, filename, or separate kind.

[why](rationale/authoring-library/instrument-is-the-unit-of-reuse.md)

<a id="recipe-role-and-index"></a>
### An instrument's reuse story is the first sentence of its doc field, trusted for selection only, and discovery is a generated signature-line index over the available-set rather than a hand-kept curated list.

[why](rationale/authoring-library/recipe-role-and-index.md)

<a id="toys-are-instruments"></a>
### The launch Toys are beginner instruments assembled from existing operators plus a generated surface, each an ordinary instrument and never new format machinery.

[why](rationale/authoring-library/toys-are-instruments.md)

## Terms

- **ResourceStore** — the central store of decoded resource bytes, built by the Coordinator at load and read immutably by Render through one pure `(id, channel, frame)` accessor, keyed by logical id.
- **Good Button** — a curated player-facing control that is hard to make sound bad, built from composition (a fan of `map`s) rather than from new instrument-format machinery.
- **surface doc** — the presentation-only document that binds an instrument's interface input-pipe names to widgets, decoupled from the instrument itself.
- **NormalizedDoc** — the type minted once at the parse gate that every build and load path accepts, proving a document is current-shaped and migrated exactly once.
- **format_version** — the document's integer shape marker; absent means 1, save writes the current version, and only a breaking shape change bumps it.
- **recipe-role** — an instrument's reuse story: the first sentence of its `doc` field, trusted for selection only, never for wiring.
- **available-set** — the set of instruments a session can reference.
- **library index** — the generated one-signature-line-per-instrument projection of the available-set (name + recipe-role + interface face).
- **Toy** — a launch beginner instrument assembled from existing operators plus a generated surface, one per distinct player gesture.
