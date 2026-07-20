# Why: The one unit of reuse is a validated instrument document consumed by-reference through subpatch, and an instrument's role is read off its interface, never off a path, filename, or separate kind.

[Rule](../../authoring-library.md#instrument-is-the-unit-of-reuse)

Terminology had begun to fork: "recipe" was being minted as a new kind of thing, "subpatch" as a
kind of document, `voices/` treated as a category — while the format knows only **instruments**. The
correction is one noun. The unit of reuse is a **validated instrument document with a defaulted
interface-pipe face**, consumed by-reference through the existing `subpatch` node
([composition-operators](composition-operators.md)) — no new format kind, no new directory
semantics, no library mechanism beyond `resources` + resolution. The nesting machinery for reuse
already exists and was built for exactly this; the standing preference is to use the engine plumbing,
not invent beside it.

Roles are **contextual, read off the interface or the use**, never off a path, filename, or kind. An
instrument hosted by a voicer is a *voice instrument while hosted* — what makes it usable as one is
its face (`freq`/`gate` in, `audio`/`active` out), nothing else; an instrument referenced from
another is a *nested instrument while referenced*. `subpatch` is demoted to a format keyword (the
node type that references a nested instrument, not a noun for the document), and "recipe" is demoted
to role shorthand — there is no recipe document, directory, or format.

The forces are concrete and evidence-backed. A recipe format/manifest would be a second kind of
document to validate, version, and keep true, when the instrument format already carries everything.
Role-by-directory or naming convention was rejected because storage location is arbitrary (and
heading server-side), so a convention would be a second, silently-drifting encoding of what the
interface already states — and the corpus drift that motivated the whole effort (the `voices/` drum
bodies re-authored inline in euclidean-drums, already diverged) happened *precisely because* `voices/`
was treated as a kind rather than a role. One noun, role from the interface, reuse through the node
that already exists: nothing to keep in sync that the loader does not already enforce.

Distilled from: ADR-0057
