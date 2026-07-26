# ADR-0072 — The render half re-exports, the resource seam is neither half's, and core privacy is a guard

## Context

[ADR-0067](0067-core-splits-and-reuben-api-is-the-one-window.md) makes `reuben-api` the one window,
[ADR-0068](0068-reuben-api-declares-its-own-types.md) decides what flows through it,
[ADR-0069](0069-reuben-api-is-one-crate-with-two-feature-halves.md) decides what the crate is,
[ADR-0070](0070-the-window-owns-advertised-prose-and-the-door-owns-its-store.md) draws the line a
second door found, and [ADR-0071](0071-the-window-owns-both-ends-of-the-structure-channel.md) gives
the window both ends of the wire. Every one of them was argued off the audio thread.

The render half is the last one, and it is the half where the earlier decision has to stop.
ADR-0069 said so in the abstract — "the render half re-exports, with an `#[inline]` passthrough as
the most it may cost" — but it said it about a crate with two empty modules. Building it asks what
that limit actually forbids, because a host does not only *call* the render path: it has to
**construct** one, and the constructor needs a registry and a resolver.

It also asks the question the whole window rests on. "`reuben-core` is private" has been the claim
since ADR-0067, and Rust has no keyword for it.

## Decision

**Everything a block touches is a re-export; the pair's constructor is not, and that is not a
contradiction.** `Coordinator`, `RenderSide`, `RenderSlot`, `AudioConfig`, `Message`, `Arg`, the
load warnings, and the single-slot mailbox primitive are re-exported by identity — the same items,
so there is nothing between the callback and the engine to cost anything. `render::install_initial`
is a real function: it hides the registry, and it takes the window's own resource seam rather than
the engine's.

The limit ADR-0069 drew is about **cost per block**, not about the absence of functions. An install
happens once per session; a swap happens when an author swaps. What made hiding the registry worth
the one call is that it is the only argument a host would otherwise have to name the engine to
supply — there is one operator set, a host does not choose it, and "every consumer goes through the
window" would have been false for every host that ever builds an engine.

**The resource seam is neither half's.** ADR-0069 left it in `authoring` and deferred the shape:
"what the window's own resolver trait looks like belongs with the authoring surface it serves."
Building the render half answers it — the load behind `install_initial` calls the same seam a
document verb calls, so it sits at `reuben_api::resources`, above both halves, and `authoring`
re-exports it for the doors that only drive that one. `FsResolver` implements it **once**; the
second impl ADR-0069 tolerated ("the engine-side impl goes when it comes through the window") is
gone, along with the class of bug where two impls of the same four methods answer differently.

**`reuben-core` is private, and the guard is what makes that a fact.** `scripts/check_core_privacy.py`
reads every manifest in the workspace and fails if any dependency table outside
`crates/reuben-api/Cargo.toml` names `reuben-core`. It runs unconditionally in CI, like the
reference-linter and the sample-alias guard, because a reintroduced edge is one manifest line in
any change.

Dev-dependencies count, and that is the load-bearing part. Every crate reached the engine through
its tests long after its `src/` had stopped: a test that reaches around the window is a report that
the window is missing something, and letting tests keep their own path is how that report stops
being filed. Four of `reuben-native`'s integration tests were rewritten to install through
`render::install_initial` and drive a `RenderSlot` — which is what a device does, so they now prove
what a device would play rather than what the load path returns.

## Consequences

**`reuben-native` names the window and nothing behind it**, and its `pub use reuben_core` — the
re-export whose comment read "so embedders only depend on this crate" — is deleted. An embedder that
took the engine through the native crate now takes `reuben-api`, which is the sentence that was
supposed to be true already.

**The perf gate cannot see this change, and saying so is the honest form of "no regression."** The
gate swaps `reuben-core`/`reuben-macros`/`reuben-contract` source to the baseline and benches
through core directly; this branch touches none of those three, so both sides compile identical
engine source and the comparison is a no-op. What protects the block is not the gate here — it is
that no wrapper exists on the per-block path to cost anything. A future `#[inline]` passthrough
would be equally invisible to the gate, which is worth knowing before one is written.

**The advertised surface did not move**: 27 tools, 72,976 bytes, eval gate green at 17,389 fixed
grounding tokens. The window grew one non-roster verb, `authoring::library_index_line`, so the
library-index sweep can project a document without naming a registry; a Rust-level verb no door
serves changes no advertised byte.

**Two dead things went with the migration.** `rigs::default_rig()` and its `EmbeddedVoices` resolver
had no caller — `play` reads the embedded JSON and resolves its voice through the session store —
and they existed only to hold a `load_instrument` call. Deleting them beat routing the engine's
whole load path through the window to serve nothing.

**One test assertion changed shape.** `nested_resolution` proved a nested patch had spliced by
looking up `/m/inner/osc` in the graph; the window serves no graph, and the projection shows the
document's nodes rather than the spliced ones. It now renders a block and asserts the leaf
**sounds** — a stronger claim about the same fact, and the dissolved case asserts silence, so the
two cannot both pass vacuously. Node-splice addressing stays covered where it belongs, in the
engine's own `nesting.rs`.
