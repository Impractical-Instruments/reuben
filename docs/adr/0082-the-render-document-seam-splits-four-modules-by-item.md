# ADR-0082 — the render/document seam splits four modules by item, and the render window is not portable yet

**Amends** [ADR-0080](0080-the-render-crate-goes-no-std-and-the-document-half-moves-out.md) on its
`### The partition` section, and corrects one claim in its `### The embedder goes through the window`
section. ADR-0080's decision is otherwise untouched: two crates now, `reuben-core` keeps the name and
the render half, unconditional `#![no_std]` + `alloc` with no `std` feature, `alloc` at instantiate and
never per block, `reuben-contract` left alone.

Overturns no rule. Cites [execution-runtime](../rules/execution-runtime.md) for context.

## Context

ADR-0080 enumerated the partition by module. Executing it measured that enumeration against the
compiler for the first time, and four modules straddle the seam rather than sitting on one side of it.

The governing rule is sharper than a module list, and it is what this ADR partitions by:

> **`reuben-core` holds the mechanics of rendering audio. `reuben-document` holds authoring instrument
> documents and turning them into an engine Plan. Nothing in `reuben-core` may name `reuben-document`.**

ADR-0080's list was an attempt to express exactly that rule, so this is a correction of measurement,
not a change of intent. Three of the four corrections move code *down* into the render crate, which is
the direction that makes the bare-metal target more reachable rather than less.

## Decision

### `engine` splits: `Engine` is render

`Engine` is the render vessel — `new(plan)`, `fill`, `fill_duplex`, `queue`, `queue_osc`,
`drain_outbound`, `transplant_survivors`, and the four geometry accessors. The RT-side `RenderSlot`
**owns one by value** and drives it per callback, so ADR-0080's list is internally inconsistent: it
placed `coordinator::slot` in the render crate and `engine` in the document crate.

`Engine` stays in `reuben-core`. Only `Engine::from_document` — four lines composing `load_instrument`
with `Plan::instantiate` — and its `FromDocumentError` move to `reuben-document`, as the free function
they always were.

### `resources` splits: the store is render, the resolver is authoring

`SampleId`, `SampleBuffer`, `ResourceStore` and `ResolvedRefs` are read on the render path —
`Operator::bind_resources`, and the `sample` and `granulator` operators. They stay in `reuben-core`.

`ResourceResolver`, `ResolveError` and the reference `MemoryResolver` move to `reuben-document`. Every
production consumer of the trait is authoring-side: `from_document`, `format`, `edit`, `introspect`,
`projection`, `describe`, the `Coordinator`, `build_manifest`. **The render crate has none.**

This **retires ADR-0080's one accepted cost** — *"the render crate owns a trait its own render path
never calls"*. It does not, once the trait goes where its callers are. The direction ADR-0080 was
protecting still holds: the document crate builds a `ResolvedRefs` and the render crate reads it, which
is the upward import, not a cycle.

### `coordinator` splits by item, not by file

ADR-0080 had `coordinator/` splitting cleanly along file lines. `mailbox` does — it is generic in its
payload (`RenderMailbox<T>`, `swap_pair<T>`) and names nothing document-shaped. The other three do not:

- **`InstallBundle` and `RenderSide` are render**, though they are defined in `swap.rs`. `RenderSlot`
  holds a `RenderMailbox<InstallBundle>` and a `stranded_retiree: Option<Box<InstallBundle>>`;
  `RenderSide` is the initial `Engine` plus that mailbox. They are the RT boundary's payload types.
- **`Coordinator` and `PreparedSwap` are authoring.** The `Coordinator` holds a `NormalizedDoc`, a
  `Registry`, a boxed `ResourceResolver` and a `Manifest`, and its verbs are `swap_document`,
  `prepare_document`, `document`, `installed_hash`.
- **`MigrationTable` is render**, though it is defined in `manifest.rs`. It is `Vec<(usize, usize)>`
  behind an accessor, it is a field of `InstallBundle`, and the RT slot transplants by it.
  `Manifest`, `NodeIdentity` and `build_manifest` stay authoring — `build_manifest` takes a
  `&NormalizedDoc` and a `&dyn ResourceResolver`.

`Manifest::diff` therefore produces a render-crate type from the document crate. That is the upward
import again, and it is fine.

### The render window is not portable today, and this is why

ADR-0080 said the embedder depending on `reuben-api` with `default-features = false,
features = ["render"]` *"looks close to free."* It is not, and the reason is in `reuben-api` rather
than in the split: its `render` feature re-exports `Coordinator`, `format::LoadWarning` and
`FromDocumentError`, and its only install path is `install_initial(doc_json: &str)`. The render half's
entry point parses JSON.

**Those four move behind the `authoring` feature**, where their dependencies already live. `render`
keeps `RenderSlot`, `RenderSide`, `InstallBundle`, the mailbox pair, `Arg`/`Message`, `AudioConfig` and
`osc_out_args`. This costs no consumer anything: `reuben-native` takes default features and
`reuben-mcp` takes `authoring`, so neither builds `render` alone.

This is a re-filing, not a new capability. **`reuben-api`'s `render` feature still offers no
document-free way to reach a `RenderSlot`** — `Plan`, `Graph` and `Engine` are not on the render
surface, because `install_initial` used to be the only door. Building that door is separate work and
is named in `## Open`.

## Consequences

**The split is four surgical item-level moves, not a file shuffle.** reuben#740's *"the single import
that has to go — one line in a 375-line file"* was measured before `RenderSlot`'s ownership of `Engine`
was noticed, and it understates the work. It does not understate the risk: still no behaviour change,
so the existing suite remains the proof.

**CI's `cargo check -p reuben-api --no-default-features --features render` starts meaning what it
says.** Today it passes while the render half is document-shaped throughout, because everything lives
in one crate. After this it fails if anything document-shaped reaches the render surface — which is the
gate ADR-0080 wanted and could not yet have.

**`scripts/check_core_privacy.py` learns about `reuben-document`,** as ADR-0080 already anticipated.
ADR-0080's note that the script is scaffolding rather than an invariant stands, and is not invoked
here — the render window's portability problem turned out to be a feature-gating mistake with a cheap
fix, not evidence that the window cannot be portable.

## Open

**The document-free install path.** A bare-metal embedder builds its graph in Rust and needs
`Graph` → `Plan::instantiate` → `Engine::new` → `RenderSlot` without touching a document. Every piece
is in `reuben-core` after this ADR; what is missing is the window's render-side surface for them.
Whether that is a plain re-export set or a narrower constructor is undecided.

**Whether `Coordinator` should be reachable render-side at all.** It is filed under `authoring` here
because it is document-native, but it is also the swap driver a *host* runs, and a host is not
necessarily an author. If a future embedder wants live swap without documents, that is the decision to
reopen.
