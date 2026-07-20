# Why: A nested reference names a source, never a version, resolved sibling-first with a configurable instrument root, and the resolver canonicalizes identity so two spellings of one source are one identity.

[Rule](../../authoring-library.md#library-resolution)

Once one instrument can reference another, *how* the reference is named, found, and versioned
becomes real. Three problems drove the resolution model. The cycle guard and per-load caches keyed
on the **raw source string**, so `a.json` and `./a.json` were two identities — a diamond fetched
twice, a cycle spelled two ways slipped the guard. Every reference resolved against **one base
directory** (the top-level instrument's), so a nested patch could not bundle a private sub-patch or
sample beside itself and shared patches had no home.

The fix is **canonical identity owned by the resolver**: the loader canonicalizes every source
*before* resolve and keys the cycle guard and fetch/parse/decode caches on canonical ids, so two
spellings of one source are one identity. Canonicalization belongs to the resolver, not the core
loader, because only the resolver knows the source-string's semantics (path? URL? bundle key?). The
filesystem resolver canonicalizes by **lexical normalization** of the winning absolute path —
`.`/`..` folded, symlinks deliberately *not* chased, no IO beyond a sibling-vs-root existence probe
— so identity is deterministic, works for missing files, and holds under `stat_only` introspection.
`fs::canonicalize` was rejected: it chases symlinks but demands the file exist, does IO per check,
breaks `stat_only`, and is platform-noisy.

Resolution is **sibling-first with a configurable root**: a reference resolves relative to the
document that names it (the referrer threads through the recursive load), and only if nothing sits
there does a configured library root win. So a library patch bundles its private sub-patches and
samples next to itself, a local file shadows the library copy, and shared patches come from one
configured place (`--instrument-root`, env fallback). Root-only resolution was rejected — it breaks
local project references the moment a root is set and forbids self-bundling. Finally, a reference
names a **source, never a version** (`pad.json`, not `pad.json@2`): pinning semantics belong to a
registry with immutable published versions, which filesystem and in-memory sources do not have, and
inventing the syntax without a consumer would freeze guesses into the format — revisit when a real
registry exists.

Distilled from: ADR-0036
