# Why: The version gate is a type: a NormalizedDoc mints once at the parse boundary and every build and load path accepts it, so a document past the gate is current-shaped and migrated exactly once.

[Rule](../../authoring-library.md#normalized-doc-gate)

The version invariant is simple to state — "a document past the gate is current-shaped, migrated
exactly once" — but it was held by **prose**, and the prose sprang leaks: two entry points migrated
the same document differently (a resolver-less parse typed a re-exported child pipe `"f32"` as a
fallback while the resolver-fed one typed it for real), and every further bypass hazard was patched
by another defensive re-check funneled through the same untyped seam.

In Rust the invariant can be held by a **type** instead — but only across a module boundary: a
newtype declared inside the 5,000-line `format.rs` is prose-guarded against its own neighbours. So a
minimal `format/normalize.rs` extraction carries the whole pipeline (gate + v1→v2 migrate + v2→v3
presentation strip + stamp) and **`NormalizedDoc`**, whose field is private to that module. Only the
pipeline can mint one; the compiler enforces it everywhere else. There is **one normalization path**
every mint funnels through, resolver optional (`None` is behaviourally an always-failing resolver
keeping the documented degrade-dark `"f32"` fallback), so there is one migration to reason about —
parameterized by resolver — never two entry points to diverge through. A raw hand-deserialized document gets a
**visible door** (`from_doc`) that replaces the old defensive clone-and-re-migrate, and `from_graph`
routes the flatten's current-shaped output back through the one real gate.

Access is by `Deref` with **no `DerefMut`**: the data model can still *represent* v1-only shapes
(that is why a shape check exists), so mutation exits via `into_inner` and re-enters through the
gate — edits visibly re-pass it. The wins are concrete: the two-migrations footgun becomes a compile
error rather than a doc-comment, the defensive re-normalize and its leftover-probe are deleted, and
the version-gate invariant, its machinery, and its tests live in one module. The resolver is
deliberately *not* captured in the type — that would infect every downstream signature to prevent a
mismatch no code path hits, and the double-migration footgun is the only one worth making
unrepresentable.

Distilled from: ADR-0047
