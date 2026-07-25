# ADR-0067 — reuben-core splits, and reuben-api is the one window

## Context

`reuben-core` is ~35k lines and has become the place things land rather than a module with a
boundary. It owns the data model, the graph, the document format and its loader, the Plan, Render,
the Coordinator, the operator registry, every built-in Operator, the projections, the document
verbs, and the tool roster. Nothing states which of those are public, so in practice all of them
are: a door reaches for whatever it needs, and the reach is the design.

Two symptoms make the cost concrete. `edit::` is nineteen flat positional functions — one carries
nine positional parameters and an `#[allow(clippy::too_many_arguments)]` — and it is flat because
the MCP door wants nineteen flat tools. That is the consumer's shape pushed into the engine. And
the browser worklet, a host shell living in another repository, consumes an embed surface that the
corpus describes as core's own, so the boundary that matters most is the one least declared.

The engine's own architecture is already layered — data model, graph, Plan, Render, Operators — but
the crate is not, so the layering is a convention a reviewer enforces rather than a dependency edge
a compiler does.

## Decision

**`reuben-core` splits into crates along the boundaries it already has:** one owning the graph and
the document format, one owning graph Render, and one defining the plugin mechanism by which the
Operator set is extended. The first-party Operators move out into their own crate and ship with
reuben as one.

**`reuben-api` becomes the single window every external consumer goes through**, and it is where we
decide what is exposed rather than a mirror of what exists. It does not need to match the surface
of the crates behind it, and there is no completeness obligation between them. All hosts are
consumers of it, including the native shell and the browser worklet.

Its surface is:

- **Authoring CRUD**, shaped flat because flat is what MCP and generated wrappers want, whatever
  shape the internals prefer.
- **A way to call Render.** The host drives it however it likes — a tight loop for offline render,
  a device callback, a Web Audio render callback. When distributing graph Render across threads
  becomes worth doing, that is an API setting (thread count, or a supplied pool), not a new
  entry point.
- **A way to pass control changes into the engine.**
- **Introspection** (what Operators exist, their ports and constants), **Swap**, and
  **diagnostics** — none of which fit the first three and all of which a host needs.
- **The resource resolver, which the host implements.** This is the one call *in*: samples and
  nested documents are never handed to the engine as bytes, so the engine calls back through a
  seam the host provides — the filesystem natively, the staging seam in the browser. A wrapper
  generator has to treat "what you call" and "what you must provide" differently, so the crate
  names them as different kinds of thing.

**RT-safety is an internal quality bar, not a term of the API contract.** A caller cannot affect
whether Render allocates and needs no obligation about it; we hold the bar because the primary use
case is realtime. What the host *does* still owe the engine is inputs on time and one clock — that
survives untouched, and it is the substance of the host-shell obligations.

**Operator registration keeps `inventory`** and adds one `ensure_linked()` in the Operators crate
that whoever assembles the Registry calls. `register_operator!` goes from `pub(crate)` to `pub`.
The alternative — an explicit `install()` naming every Operator — is robust and needs no linker
argument, but it resurrects the hand-maintained list that self-registration exists to delete. One
hand-maintained call site is a better trade than one hand-maintained list.

**The RT-safety allocation-counting harness ships as part of the plugin contract**, behind a
`testing` feature, so a third-party Operator crate proves its own `process` is allocation-free with
the same counter the first-party Operators are held to.

## Consequences

**The plugin mechanism changes what "non-negotiable invariant" means, and the honest statement of
the guarantee changes with it.** `Registry::register` is already public and already documented as
the seam for authoring new Operators in Rust, so third-party Operators are not introduced here —
only made official. But allocation-freedom is verified by tests covering the Operators that ship in
tree, and no test can cover an Operator we have not seen. So the engine's guarantee is that Render
is allocation-free **given Operators that are**, with the harness as the means of being one. This
is the DAW bargain: a host cannot promise a plugin will not wreck the audio callback, and can give
plugin authors the instrument to measure it. A conditional guarantee with a mechanism attached is a
different thing from a weakened one.

This qualifies `execution-runtime.md#render-is-allocation-free`, which states the invariant
unconditionally.

**The embed surface moves.** `execution-runtime.md#embed-surface` puts the portable Engine bridge in
`reuben-core` as the one surface every host shell wraps; under this decision every host shell wraps
`reuben-api`. Related: the C-ABI surface the repo map already describes has never actually been
built — there is no `extern "C"` in `crates/reuben-core/src` today — so `reuben-api` is filling a
hole that was described and left empty rather than relocating working code.

**Cross-crate `inventory` is the risk that sinks the Operators crate if it is wrong.**
`web-product-process.md#static-link-operator-registration` already exists because static and wasm
linking drop self-registration constructors, and it works around that with `codegen-units = 1` for
embedders of core. A crate whose only purpose is side-effectful registration is a harder case:
nothing in the graph or Render crates references the Operators crate, so the linker is free to drop
the objects carrying the `inventory` statics. That rule now binds the Operators crate rather than
embedders of core, and `ensure_linked()` is the referenced symbol that forces the link.

**Verify this before the split lands, not after.** The failure mode is a `cargo test` on Linux that
passes while a wasm release build silently registers zero Operators — an engine that loads no
Instrument rather than a link error.

Self-registration itself survives intact (see [composition & operators](../rules/composition-operators.md)):
Operators still register at their own definition site, and there is still no central list to edit.

## Open

Crate names and how many; whether `reuben-core` survives as a name or dissolves into the pieces;
sequencing and what lands first; and feature-gating so a host that only wants Render does not
compile the authoring surface.
