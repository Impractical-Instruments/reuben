# ADR-0036: Instrument library — resolution identity, search path, save source of truth, and format versioning

> **Amended by [ADR-0044](0044-normalization-is-a-type.md).** §4's parse-boundary gate is now
> held by a type: `NormalizedDoc` mints once in `format/normalize.rs` (gate + migrate + strip +
> stamp), `build` and the load paths accept it by type, and the "load path re-checks the
> version" defensive re-run this section describes is deleted — a raw `Deserialize`d document
> enters via `NormalizedDoc::from_doc` instead.

## Status

Accepted (2026-07-02). Resolved in a grilling session — the design half of nesting **P7**
([#122](https://github.com/Impractical-Instruments/reuben/issues/122), the trailing pass of the
nesting epic [#123](https://github.com/Impractical-Instruments/reuben/issues/123); overlaps
[#65](https://github.com/Impractical-Instruments/reuben/issues/65)). Implements the
canonicalization note [ADR-0034](0034-instrument-nesting.md) §1 left to the resolver seam, and
closes the save-round-trip question [ADR-0016](0016-sample-player-and-resource-store.md)'s
resource bindings and ADR-0034's inline both raised.

## Context

Once an instrument can reference another (ADR-0034), *how* that reference is named, found, and
versioned becomes real:

- The cycle guard and the per-load dedup caches keyed on the **raw source string**, so
  `a.json` and `./a.json` were two identities (ADR-0034 §1 flagged this and assigned the fix
  to the resolver seam).
- Every reference resolved against **one base directory** — the top-level instrument's — so a
  nested patch could not bundle a private sub-patch or sample next to itself, and shared
  patches had no home.
- P4's inline **dissolves** the `subpatch` node, so `from_graph` of a built graph emits the
  flattened equivalent and the reference is gone — fine for export, wrong if `from_graph` is
  read as "the save path".
- The format had **no version marker**, so a breaking shape change had no way to announce
  itself to an older engine (or vice versa).

## Decision

### 1. The document is the save source of truth; `from_graph` is the flatten/export path

Saving an instrument means serializing the `InstrumentDoc` you loaded/edited — nested
references survive via serde, untouched. `from_graph(built_graph)` is deliberately the
**flatten/export** path: every spliced subpatch appears as its inlined nodes. No splice
provenance is recorded on the `Graph`; the render engine carries zero authoring state, and no
future build transform has to stay invertible. Editing flows mutate the document, never
reverse-engineer a built graph.

### 2. Identity is canonical, and canonicalization belongs to the resolver

`ResourceResolver::canonical(source, referrer) -> String` (default: identity — right for
in-memory and test resolvers whose sources are exact keys). The loader canonicalizes every
source **before** `resolve`/`resolve_text` and keys the cycle guard and the per-load
fetch/parse/decode caches on canonical ids: two spellings of one source are one identity, so a
cycle spelled two ways is caught and a diamond spelled two ways fetches once. The filesystem
resolver canonicalizes by **lexical normalization** of the winning absolute path — `.`/`..`
folded, symlinks deliberately not chased, no IO beyond the sibling-vs-root existence probe —
so identity is deterministic, works for missing files, and holds under `stat_only`
introspection.

### 3. Sibling-first resolution with a configurable instrument root

A reference resolves **relative to the document that names it** (`referrer` — the canonical
id of the referencing document — threads through the recursive load; the top level uses the
resolver's base). If nothing exists there and a library root is configured, a hit under the
root wins instead. A miss in both canonicalizes to the sibling candidate, so the eventual
`NotFound` warning names the path the author most likely meant. Consequences: a library patch
bundles private sub-patches and samples next to itself; a local file shadows the library copy;
shared patches come from one configured place (`reuben --instrument-root <DIR>`, env fallback
`REUBEN_INSTRUMENT_ROOT`). The non-file side of the seam is concrete: core's public
`MemoryResolver` serves patches and samples by exact key for embedded/WASM hosts and tests.

### 4. `format_version`: absent means 1, save writes it, only the future refuses

An optional integer `format_version` on the document. Absent means **1** — every document
written before versioning is a valid v1. Saving always writes the current version. The gate
lives at the parse boundary (`InstrumentDoc::from_json`), so every load path — top-level,
voice, subpatch — refuses a **newer** document (`LoadError::UnsupportedVersion`, naming both
versions and the remedy) before touching its shape; a too-new *child* is fatal to the host
load, like any structural error (ADR-0034 §1). `from_json` also **normalizes** the accepted
document to the current version (older versions migrate here first), so "save writes the
current version" is a mechanism, not a coincidence — a migrated document never saves back
under its old number. The public `Deserialize` can build an `InstrumentDoc` around the gate,
so the load path re-checks the version before trusting the shape. Bump rules:

- **Additive** changes (a new optional field) never bump the version.
- **Breaking** shape changes bump it and ship a parse-time migration, so older documents keep
  loading — only the future is unreadable.

The format is **fail-closed** on unknown fields (`deny_unknown_fields` throughout), so a typo
in a hand-authored document fails loudly at parse rather than being silently dropped. The
consequence is deliberate: an **older** engine rejects a newer-but-still-v1 document that
carries an additive field it doesn't know. Old engines are not expected to read newer
documents — the engine and its instruments version together; upgrade the engine. "Additive
changes never bump" means a document stays loadable by the **same-or-newer** engine without a
version dance, and keeps the `format_version` gate reserved for *breaking* shape changes,
where it gives a clear, actionable error instead of a shape misparse. Forward-reading old→new
is a non-goal; strict typo detection is worth more than it to a hand-authored format.

### 5. No reference pinning

A nested reference names a source, not a version (`pad.json`, never `pad.json@2`). Pinning
semantics belong to a registry with immutable published versions; filesystem and in-memory
sources have neither, and inventing the syntax without a consumer would freeze guesses into
the format. Revisit when a real library/registry exists.

## Consequences

- Warnings and errors name **canonical** ids (full paths for the fs resolver) — more useful,
  slightly noisier.
- Nested documents' references are now doc-relative: the one cross-dir reference in-repo
  (`voices/sampler-voice.json`) moved to the doc-relative spelling (`../samples/blip.wav`).
  Resolvers that don't override `canonical` keep exact-key behavior unchanged.
- `from_graph` output of a nested instrument is the flattened equivalent **by design**;
  document-level round-trips preserve the reference.
- The instrument JSON schema gains `format_version`; documents saved from now on carry it.
- Cross-version bench/CI comparisons that straddle a resolver-contract change skip gracefully
  (the perf gate's designed degrade path) and heal when the baseline advances.

## Alternatives rejected

- **Splice provenance on the `Graph`** (re-fold the subpatch on save): adds authoring state
  the engine never renders with, and makes every future build transform (metadata overrides,
  namespacing, boundary rewires) carry an inverse forever. The document already holds the truth.
- **`fs::canonicalize` for identity**: chases symlinks but requires the file to exist, does IO
  per identity check, breaks `stat_only` introspection, and is platform-noisy (UNC). Lexical
  normalization covers the spelling problem ADR-0034 §1 actually flagged.
- **Canonicalization in the core loader**: the loader can't know source-string semantics (path?
  URL? bundle key?) — ADR-0034 §1 already assigned the judgment to the resolver seam.
- **Root-only resolution** (no sibling search): simpler, but breaks local project references
  the moment a root is configured and forbids a library patch bundling private sub-patches.
- **Policy doc without a version field**: near-zero cost now, but tools couldn't distinguish
  "old file" from "file that predates versioning", and the field costs one optional integer.
