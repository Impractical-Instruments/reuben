# ADR-0041: The web player app lives in-repo at `/web`

> **Superseded by [ADR-0056](0056-web-product-extracted-to-private-repo.md).** The app does not
> live at `/web`, or anywhere in this repo — it was extracted to a private AGPL repo that consumes
> this one as a submodule. Of the three arguments below for keeping it in-repo — the engine already
> reached into the browser from here; this was the only place it could be built and deployed;
> "own repo" bought isolation the app didn't want, since it must track the engine lockstep — the
> first two were artifacts of not yet having a submodule, and the third is answered by the pin. This
> ADR's supersession of [ADR-0026](0026-v1-finish-line-osc-out-and-stereo.md)'s "own repo" clause
> is itself reversed: that clause is restored. Kept as the record of why the app was built the way
> it was — which is still true, it just isn't built here.

> **Amended by [ADR-0043](0043-surface-docs-decouple-presentation-from-instruments.md).**
> The auto-UI renders from interface pipes + surface docs, not from ADR-0018's `control`
> blocks (the "Rides on ADR-0018" line below is retired with them).

## Status

Accepted (2026-07-08). The repo-shape decision of the web player epic
([#151](https://github.com/Impractical-Instruments/reuben/issues/151), P4:
[#226](https://github.com/Impractical-Instruments/reuben/issues/226)) — settled at P4
kickoff, which #151's repo-shape line explicitly deferred ("*Confirm at P4 kickoff —
recorded there.*"). **Supersedes** [ADR-0026](0026-v1-finish-line-osc-out-and-stereo.md)'s
the "a dedicated UI… belongs to a consuming application (its own repo)" stance for *this* app.
**Rides on** [ADR-0039](0039-engine-in-core-embed-surface.md) (the `reuben-core` embed
surface), [ADR-0040](0040-raw-c-abi-worklet-boundary.md) (the `reuben-web` C-ABI shell,
wrapped by the ES-modules the app imports), and
[ADR-0018](0018-control-surface-generation.md) (the `control`-block contract the auto-UI renders).

## Context

ADR-0026 drew a clean line — *reuben is an engine, always driven by something else; a
dedicated UI is out of scope for the **project**, it belongs to a consuming application in
its own repo.* That line was right for the argument it settled (what ships in v1, where the
product surface is), and #151 honored it: the epic's reuse map put `reuben-web`
(engine-adjacent) here and pencilled the *frontend app* into "its own repo."

Three things have changed since, all pulling toward one repo:

- **The engine already reaches into the browser from here.** P2 ([#234](https://github.com/Impractical-Instruments/reuben/issues/234)/[#235](https://github.com/Impractical-Instruments/reuben/issues/235))
  shipped `crates/reuben-web/` — the WASM engine plus the ES-module JS API — and P3
  ([#238](https://github.com/Impractical-Instruments/reuben/issues/238)) shipped the auto-UI
  renderer (`crates/reuben-web/js/surface/`). The app is not a greenfield consumer of a
  published artifact; it is a thin shell (launcher + first-run flow + build config) over
  modules that already live in this tree and are already tested by this repo's CI.
- **This repo is the only place it can be built and deployed.** The app's payload is
  produced by a build script that runs the engine's own fetch-on-miss discovery
  (`crates/reuben-web/js/loader.mjs`) to enumerate each Toy's transitive resources, and by
  a `cargo build --target wasm32-unknown-unknown` of `reuben-web`. Both need the workspace.
  A separate repo would have to vendor or submodule the engine, the surface, the
  instruments, and the schema — re-introducing exactly the version-skew the monorepo avoids.
- **"Its own repo" bought isolation the app doesn't want.** The value of a separate repo is
  an independent release cadence and a clean dependency boundary. But the app's whole job is
  to track the engine and the instrument set *lockstep* — a Toy added to `instruments/`
  should appear in the launcher build from the same commit; a `reuben-web` ABI change should
  break the app's CI in the same PR. Co-location makes that automatic; a split makes it a
  cross-repo coordination cost with no offsetting gain.

## Decision

### 1. The app lives permanently in-repo at top-level `/web`

Not `crates/reuben-web/web/` (that path holds P2's throwaway harness, which this app
replaces): a top-level sibling to `crates/`, `instruments/`, and `docs/`, because it is a
peer product artifact, not a sub-directory of the engine crate. `/web` owns only the
launcher, the app shell, the asset-staging build script, and the build/deploy config. It
**imports** the engine and surface from `crates/reuben-web/js/` as the source of truth — it
does not fork or vendor them, so P2/P3's tests keep covering that code unchanged.

### 2. ADR-0026's "own repo" clause is superseded for this app, and only for this app

ADR-0026's deeper claim — *the reuben **core/engine** stays headless; its product surface is
its I/O contract, not pixels* — is untouched and still governs `reuben-core`/`reuben-native`.
What is retired is the narrower packaging inference that the *player UI* therefore belongs in
a foreign repo. The UI is still a **consuming application** in the ADR-0026 sense (it embeds
the engine over a stable boundary; the engine never learns it exists) — it simply consumes
from the next directory over instead of across a repo boundary. Nothing about the core's
scope, threading, or I/O-only product surface changes; a monorepo home for a consumer is an
orthogonal choice ADR-0026 didn't actually need to make.

### 3. Consequence for later passes

P5 (PWA/offline, [#227](https://github.com/Impractical-Instruments/reuben/issues/227)) and
P6 (share links, [#228](https://github.com/Impractical-Instruments/reuben/issues/228)) build
on `/web`; they inherit this location without re-deciding it. A future *second* consuming
app (e.g. the game shell, [#222](https://github.com/Impractical-Instruments/reuben/issues/222))
is not bound by this ADR — it makes its own repo-shape call on its own merits; this record
is about the web player specifically.

## Alternatives considered

- **Honor ADR-0026 literally — a separate frontend repo.** Rejected: it optimizes for an
  isolation the app actively doesn't want (it must track the engine + instruments lockstep),
  and forces the build/deploy pipeline to reach a workspace it no longer contains. The
  original rationale ("reuben is an engine") survives without the packaging clause.
- **`crates/reuben-web/web/` (extend the P2 harness in place).** Rejected: the app is not a
  sub-artifact of the engine crate, and burying a deployable product three levels down under
  a `cdylib` crate misrepresents the dependency direction (the app depends on the crate, not
  the reverse). The harness there is explicitly throwaway (#224) and gets replaced, not grown.
- **A new crate (`crates/reuben-player/`).** Rejected: it is not a Rust crate — no
  `Cargo.toml`, no workspace membership, no Rust source. It's a JS/Vite app that consumes one
  crate's WASM output. `crates/` is for crates.

## Consequences

- **[ADR-0026](0026-v1-finish-line-osc-out-and-stereo.md) is superseded in part:** its
  "dedicated UI → own repo" packaging clause no longer holds for the web player; its
  engine-scope and I/O-contract decisions are unaffected. A superseded-in-part note is added
  to the top of ADR-0026 pointing here.
- **#151's reuse-map line is resolved:** the "frontend app in its own repo *(Confirm at P4
  kickoff)*" entry is confirmed the other way — in-repo at `/web`.
- The root tree gains a top-level `/web` directory; CI gains an app build/test lane and (P4's
  PR3) a Cloudflare Pages deploy job. `crates/reuben-web/web/` (the P2 harness) is retired by
  the app that replaces it.
