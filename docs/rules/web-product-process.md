# Web/product boundary & dev process

> How this repo sits under the web/product boundary: the BSD SDK a private product consumes, the raw C-ABI browser contract and sample-trust obligation it owes, and the release, toolchain, and perf-benchmark process that governs it.

## Now

reuben is an engine, never an app — always driven by something else (a script, TouchOSC, a
consuming application), and its product surface is its I/O contract, not pixels. That principle
draws the outer boundary of this repo: **this repo is the reuben SDK**, BSD-3-Clause — the engine
core, `reuben-api` (the one window every consumer goes through, including the filesystem
resolver), the native CLI and its audio/OSC host, the stdio MCP sidecar, and the
instrument/surface library those tests load. The actual **product** — the browser player, its app
shell, the WASM C-ABI shell, the share-link codec, and the chat-authoring agent — lives in a
separate **private, AGPL** repo that pins this one as a git submodule and builds against the
**window** through a path dependency. The seam was drawn by support surface, not licence; the
two boundaries simply coincide. The submodule pin is the version boundary — the engine version is
a property of a cross-repo SHA, adopted when the product bumps its pin.

The window is where the engine ends and a consumer begins, and that is a checked fact rather than a
habit: no manifest in this workspace but `reuben-api`'s names `reuben-core`, dev-dependencies
included, because a test that reaches around the window is a report that the window is missing
something. The crate is one dependency with two feature halves — an off-thread `authoring` half that
declares its own types and a `render` half that re-exports what a block touches — both on by default,
plus an off-by-default filesystem resolver, with CI building the render-only configuration the
browser worklet takes so that fence is exercised in tree.

Because the shell left, the browser story this repo tells is a **contract, not a binding**:
`reuben-core` compiles to `wasm32-unknown-unknown` untouched, and the documented raw C-ABI worklet
boundary (one `Engine::fill` per audio quantum, `(ptr, len)` byte regions through linear memory,
fetch-on-miss resource staging, a flat tagged control channel, no `wasm-bindgen`) is the reference
a third party rebuilds their own binding from. Two obligations outlive the extracted product: any
statically-linked or wasm embedder must build core at `codegen-units = 1` or operator
self-registration constructors are silently dropped by the linker; and externally-sourced sample
bytes are untrusted, so the one WAV decoder this repo ships must bounds-check its declared
data-chunk length before any sample-bearing share bundle can carry a stranger's bytes.

The dev process that governs the repo is deliberately small and self-verifying, and the rules below
state it: one pinned toolchain so a local verdict equals CI's, shared hooks (which ADR-0079 makes
one installed *set* of per-check fragments rather than a directory a second directory can
displace) as a convenience ahead of the authoritative CI gate, an instruction-count perf gate over
the render hot path (which ADR-0077 widens to cover graph construction, and to gate how that cost
scales rather than only what it is), and versioned release archives for a headless CLI whose
primary product is the crate.

The **branch model is not stated here**: it is `.ii/repo.toml`'s `[branches]` table, read rather
than restated.

## Rules

**Where this repo ends** — the seven rules that answer what is in the SDK and what it owes across the
boundary.

<a id="sdk-product-split"></a>
### This repo is the reuben SDK — engine core, the `reuben-api` window, native CLI, MCP sidecar, and the instrument/surface library — while the browser shell, player app, and authoring agent live in a separate private product repo that consumes this one as a submodule.

[why](rationale/web-product-process/sdk-product-split.md)

<a id="core-is-private-to-the-window"></a>
### `reuben-core` is named by `reuben-api` alone: no other manifest in the workspace declares a dependency on it, dev-dependencies included, and a guard that reads the renamed `package` field as well as the key keeps it that way.

[why](rationale/web-product-process/core-is-private-to-the-window.md)

<a id="window-is-two-feature-halves"></a>
### The window is one crate split by feature rather than into two crates — an off-thread authoring half that declares its own types and a render half that re-exports — both on by default, with CI building the render-only configuration so the fence is a fact rather than an intention.

[why](rationale/web-product-process/window-is-two-feature-halves.md)

<a id="license-boundary"></a>
### The licence boundary is the repo boundary: this repo is BSD-3-Clause, the private product repo is AGPL-3.0, and no file is dual-licensed.

[why](rationale/web-product-process/license-boundary.md)

<a id="wasm-c-abi-boundary"></a>
### reuben-core compiles to wasm32 untouched, and its browser story is the documented raw C-ABI worklet boundary — no wasm-bindgen, no maintained binding shipped — from which a third party reconstructs its own binding.

[why](rationale/web-product-process/wasm-c-abi-boundary.md)

<a id="static-link-operator-registration"></a>
### Any statically-linked or wasm embedder of reuben-core builds it at codegen-units = 1 so every operator's self-registration constructor survives linking.

[why](rationale/web-product-process/static-link-operator-registration.md)

Superseded by: ADR-0067 (pending absorption)

<a id="sample-bytes-trust-boundary"></a>
### Externally-sourced sample bytes are untrusted: the WAV decoder must bounds-check its declared data-chunk length before any sample-bearing share bundle can carry them.

[why](rationale/web-product-process/sample-bytes-trust-boundary.md)

**How a change lands** — how work is gated and shipped.

<a id="versioned-release-archives"></a>
### The engine is headless — the SDK crate is the primary product and the CLI binary ships as versioned, installer-free CI release archives cut from a `v*` tag.

[why](rationale/web-product-process/versioned-release-archives.md)

<a id="toolchain-pin"></a>
### A single `rust-toolchain.toml` pins the exact toolchain as the one source of truth, with the workspace MSRV held in lockstep with the pinned channel and enforced in CI.

[why](rationale/web-product-process/toolchain-pin.md)

<a id="shared-git-hooks"></a>
### Git hooks are version-controlled under `scripts/hooks/` — the one directory the company bootstrap configures — and shared via `core.hooksPath`, as a convenience ahead of the authoritative CI gate.

Superseded by: ADR-0079 (pending absorption)

[why](rationale/web-product-process/shared-git-hooks.md)

<a id="perf-benchmark-gate"></a>
### The render hot path is guarded by an instruction-count perf gate that diffs HEAD against its base ref and fails a PR on a >10% regression, with wall-clock benchmarking left to local runs.

Superseded by: ADR-0077 (pending absorption)

[why](rationale/web-product-process/perf-benchmark-gate.md)

<a id="micro-bench-drives-the-real-path"></a>
### A per-operator micro-bench drives that operator's real per-sample path rather than an early-out idle path, and the census of what is benched is one source the tests hold in sync with the registry.

[why](rationale/web-product-process/micro-bench-drives-the-real-path.md)

## Terms

- **SDK** — this (BSD-3-Clause) repo: the engine core, native CLI, MCP sidecar, and instrument/surface library that the product consumes.
- **product repo** — the separate private AGPL repo holding the browser shell, player app, share-link codec, and chat-authoring agent, which pins this repo as a submodule.
- **C-ABI worklet boundary** — the documented raw `extern "C"`, `(ptr, len)`-over-linear-memory interface a browser host drives per audio quantum, carrying no `wasm-bindgen` glue and shipped as a contract to rebuild against, not a maintained binding.
- **share link** — an origin-independent encoded bundle that boots an instrument in the browser; a product-repo feature whose residue here is the sample-bytes trust obligation.
- **toolchain pin** — the exact-version `rust-toolchain.toml` that local dev and CI share so their fmt/clippy verdicts are identical, kept in lockstep with the workspace MSRV.
- **perf gate** — the CI iai-callgrind instruction-count check over the render hot path, measured base-ref-relative so toolchain drift cancels. ADR-0077 adds a construct layer and, on it, a growth-factor check that reads no baseline.
