# ADR-0047: Normalization is a type — `NormalizedDoc` mints once at the gate

## Status

Accepted (2026-07-10). Resolved in a grilling session on
[#215](https://github.com/Impractical-Instruments/reuben/issues/215) (architecture-deepening
review batch #214–#218). **Amends [ADR-0036](0036-instrument-library-and-format-versioning.md)
§4**: the version-gate invariant moves from prose-and-re-checks into a type. Carries the
enforcement slice of the `format.rs` split
([#218](https://github.com/Impractical-Instruments/reuben/issues/218) stays open for the rest).

## Context

ADR-0036 §4 put the version gate at the parse boundary (`InstrumentDoc::from_json`) and noted
that the public `Deserialize` can build an `InstrumentDoc` *around* the gate, "so the load path
re-checks the version before trusting the shape." That re-check grew:

- **Two entry points migrated differently.** A v1 `interface` entry re-exporting a nested
  child's boundary port needs the child document to type its pipe; resolver-less `from_json`
  typed it `"f32"` as a fallback while `from_json_with` typed it for real — same document, two
  migrations, guarded only by a doc-comment telling resolver-holding callers which one to use.
- **The load path defensively re-ran the pipeline.** `load_doc_guarded` re-checked the version,
  re-migrated on a clone, and (after ADR-0043 added the v2→v3 presentation strip) grew
  `carries_retired_presentation` to detect leftovers hiding under a current stamp. Two
  migrations funneled through the same untyped seam, each bypass hazard patched by another
  re-check.

The invariant — "a document past the gate is current-shaped, migrated exactly once" — was held
by prose. In Rust it can be held by a type, but only across a module boundary: a newtype
declared inside the 5,199-line `format.rs` is prose-guarded against its own neighbors.

## Decision

### 1. A minimal module extraction carries the invariant

`format.rs` becomes `format/mod.rs` (pure rename) plus **`format/normalize.rs`**, holding the
whole normalize pipeline — version gate, v1→v2 migration engine (including `child_input_pipe`),
v2→v3 presentation strip, stamp — and **`NormalizedDoc`**, a newtype whose field is private to
that module. Only the pipeline can mint one; the compiler enforces it everywhere else. This is
deliberately *not* the full #218 split — only the slice that carries the invariant; the module
is named for the seam it exports (normalize = gate + migrate + strip + stamp), not for one
stage of it.

### 2. One mint entry, resolver optional

`NormalizedDoc::from_json(json, &Registry, Option<&dyn ResourceResolver>)` replaces both
`InstrumentDoc::from_json` variants (deleted). `None` keeps the documented degrade-dark `"f32"`
fallback for a re-exported child pipe — behaviorally `None` ≡ an always-failing resolver, so
there is one migration to reason about, parameterized by resolver, never two entry points to
diverge through. Sugar keeps its signatures: `load`, `load_instrument`,
`Engine::from_document`.

### 3. Raw documents get a visible door

`InstrumentDoc` keeps public `Deserialize` — it is also the save path, and structural
introspection is legitimate. A host holding a raw doc enters via consuming
**`NormalizedDoc::from_doc(doc, &Registry, Option<&dyn ResourceResolver>)`**, which replaces
`load_doc_guarded`'s defensive clone-and-re-migrate outright (`carries_retired_presentation`
deleted with it). Nothing in-repo hand-deserializes an `InstrumentDoc` today; the defensive
path guarded a hypothetical, and the hypothetical now has a visible door instead of a silent
re-run.

### 4. Read by `Deref`, mutate through the gate

`NormalizedDoc: Deref<Target = InstrumentDoc>` plus `into_inner()`; **no `DerefMut`**. The
data model can still represent v1-only shapes (that is why `check_pipe_shape` exists), so
mutation exits via `into_inner` and re-enters through `from_doc` — edits visibly re-pass the
gate. Read sites (`describe_boundary`, the resource passes) work unchanged via deref coercion.

### 5. `build` moves to `NormalizedDoc`; `from_graph` mints one

`InstrumentDoc`'s public `build` is gone — necessary, since with `Deref` a public
`InstrumentDoc::build` would still let raw docs build and gut the type gate.
`NormalizedDoc::from_graph` routes the flatten's (current-shaped-by-construction) document
through the one real gate and stays infallible via `expect` — a gate failure there is a bug by
definition (it gains a `&Registry` parameter; it had no production callers).
`load_instrument_doc` retypes to `&NormalizedDoc`; internally `LoadCtx::docs` caches
`NormalizedDoc`, so recursive child parses are typed too.

### 6. Tests port to the gate; module privacy is the enforcement

The smuggled-doc test became a compile error — which is the win — so its coverage moved to
`from_doc` (v1-under-a-current-stamp fails closed, stale stamps migrate, future versions
refuse, leftovers strip). The resolver-less vs resolver-fed divergence is asserted directly
via the mint with `None`/`Some` (previously a documented property with no direct test). No
`trybuild` compile-fail harness: privacy is checked by the compiler on every build already.

## Documented non-goal

The **resolver is not captured in the type**. Minting with resolver A and building with
resolver B remains the caller's contract, exactly as before (`describe_patch` already passes
the same resolver to both). What becomes unrepresentable is per-document double migration —
the actual footgun named in-code.

## Consequences

- The two-migrations footgun is a compile error, not a doc-comment.
- `load_doc_guarded` shrinks to build + resource passes; the defensive re-normalize and
  `carries_retired_presentation` are deleted.
- The public parse surface shrinks: one mint (`from_json`) instead of `from_json` ×2, plus the
  explicit `from_doc` door.
- Locality: the version-gate invariant, its machinery, and its tests live in one module.
- Hosts that deserialized an `InstrumentDoc` and called `load_instrument_doc` directly must now
  pass `NormalizedDoc::from_doc` first — a one-line, compiler-guided change.

## Alternatives rejected

- **The full #218 `format.rs` split first**: the invariant needs only one module boundary;
  landing it with the whole split couples a behavior-critical change to a large mechanical one.
- **Newtype inside `format.rs` without the module boundary**: privacy would not bind against
  the file's own 5,000 lines — prose-guarded again, just with extra steps.
- **Capturing the resolver in the type** (e.g. `NormalizedDoc<'r>` holding `&dyn
  ResourceResolver`): infects every signature downstream to prevent a mismatch no code path
  has ever hit, and `describe_patch` already models the contract correctly.
- **A `trybuild` compile-fail harness for the smuggle case**: module privacy *is* the
  enforcement, checked by the compiler on every build; a compile-fail test would re-prove
  what `rustc` already proves, at CI cost.
- **Keeping the defensive re-check alongside the type**: dead code that re-blurs the one
  invariant the type exists to hold — the gate either binds or it doesn't.
