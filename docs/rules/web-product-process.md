# Web/product boundary & dev process

> How this repo sits under the web/product boundary: the BSD SDK a private product consumes, the raw C-ABI browser contract and sample-trust obligation it owes, and the branch, release, toolchain, and perf-benchmark process that governs it.

## Now

reuben is an engine, never an app — always driven by something else (a script, TouchOSC, a
consuming application), and its product surface is its I/O contract, not pixels. That principle
draws the outer boundary of this repo: **this repo is the reuben SDK**, BSD-3-Clause — the engine
core, the native CLI and its audio/OSC/filesystem host, the stdio MCP sidecar, and the
instrument/surface library those tests load. The actual **product** — the browser player, its app
shell, the WASM C-ABI shell, the share-link codec, and the chat-authoring agent — lives in a
separate **private, AGPL** repo that pins this one as a git submodule and builds against
`reuben-core` through a path dependency. The seam was drawn there not by licence but by *support
surface*: we decline to maintain a public browser SDK we have no second consumer for; the AGPL and
support boundaries simply coincide. The submodule pin is the version boundary — the engine version
is a property of a cross-repo SHA, adopted when the product bumps its pin.

Because the shell left, the browser story this repo tells is a **contract, not a binding**:
`reuben-core` compiles to `wasm32-unknown-unknown` untouched, and the documented raw C-ABI worklet
boundary (one `Engine::fill` per audio quantum, `(ptr, len)` byte regions through linear memory,
fetch-on-miss resource staging, a flat tagged control channel, no `wasm-bindgen`) is the reference
a third party rebuilds their own binding from. Two obligations outlive the extracted product and
stay owed by public core: any statically-linked or wasm embedder must build core at
`codegen-units = 1` or operator self-registration constructors are silently dropped by the linker;
and externally-sourced sample bytes are untrusted, so the WAV decoder must bounds-check its declared
data-chunk length before any sample-bearing share bundle can carry a stranger's bytes.

The dev process that governs the repo is deliberately small and self-verifying. One
`rust-toolchain.toml` pins the exact toolchain so a contributor's local fmt/clippy verdict equals
CI's, with the workspace MSRV held in lockstep and a CI job failing on drift; version-controlled
`.githooks/` (pre-commit fmt, pre-push clippy) catch failures early as a convenience ahead of the
real gate, which is always CI. `dev` is the default long-lived integration branch every PR targets;
production ships only by **fast-forward-only promotion** of `dev` onto `main`, run as a workflow (a
true ff preserves SHAs so the branches never diverge) authored by a GitHub App token, with no direct
commits to `main`. And the render hot path is fenced by a perf gate: an instruction-count
(iai-callgrind) CI check that diffs HEAD against its base ref and fails a PR on a >10% regression,
with noisy wall-clock benchmarking left to local runs. The engine itself ships headless — the SDK
crate is the primary product, the CLI binary a secondary convenience shipped as versioned,
installer-free release archives cut from a `v*` tag.

## Rules

<a id="sdk-product-split"></a>
### This repo is the reuben SDK — engine core, native CLI, MCP sidecar, and the instrument/surface library — while the browser shell, player app, and authoring agent live in a separate private product repo that consumes this one as a submodule.

[why](rationale/web-product-process/sdk-product-split.md)

<a id="license-boundary"></a>
### The licence boundary is the repo boundary: this repo is BSD-3-Clause, the private product repo is AGPL-3.0, and no file is dual-licensed.

[why](rationale/web-product-process/license-boundary.md)

<a id="wasm-c-abi-boundary"></a>
### reuben-core compiles to wasm32 untouched, and its browser story is the documented raw C-ABI worklet boundary — no wasm-bindgen, no maintained binding shipped — from which a third party reconstructs its own binding.

[why](rationale/web-product-process/wasm-c-abi-boundary.md)

<a id="static-link-operator-registration"></a>
### Any statically-linked or wasm embedder of reuben-core builds it at codegen-units = 1 so every operator's self-registration constructor survives linking.

[why](rationale/web-product-process/static-link-operator-registration.md)

<a id="sample-bytes-trust-boundary"></a>
### Externally-sourced sample bytes are untrusted: the WAV decoder must bounds-check its declared data-chunk length before any sample-bearing share bundle can carry them.

[why](rationale/web-product-process/sample-bytes-trust-boundary.md)

<a id="dev-integration-branch"></a>
### `dev` is the default long-lived integration branch that every PR targets, and every push to it runs the full CI suite.

[why](rationale/web-product-process/dev-integration-branch.md)

<a id="ff-promotion-to-main"></a>
### Production ships only by fast-forward-only promotion of `dev` onto `main`, run as a workflow authored by a GitHub App token, with no direct commits to `main`.

[why](rationale/web-product-process/ff-promotion-to-main.md)

<a id="versioned-release-archives"></a>
### The engine is headless — the SDK crate is the primary product and the CLI binary ships as versioned, installer-free CI release archives cut from a `v*` tag.

[why](rationale/web-product-process/versioned-release-archives.md)

<a id="toolchain-pin"></a>
### A single `rust-toolchain.toml` pins the exact toolchain as the one source of truth, with the workspace MSRV held in lockstep with the pinned channel and enforced in CI.

[why](rationale/web-product-process/toolchain-pin.md)

<a id="shared-git-hooks"></a>
### Git hooks are version-controlled under `.githooks/` and shared via `core.hooksPath` — pre-commit fmt, pre-push clippy — as a convenience ahead of the authoritative CI gate.

[why](rationale/web-product-process/shared-git-hooks.md)

<a id="perf-benchmark-gate"></a>
### The render hot path is guarded by an instruction-count perf gate that diffs HEAD against its base ref and fails a PR on a >10% regression, with wall-clock benchmarking left to local runs.

[why](rationale/web-product-process/perf-benchmark-gate.md)

## Terms

- **SDK** — this (BSD-3-Clause) repo: the engine core, native CLI, MCP sidecar, and instrument/surface library that the product consumes.
- **product repo** — the separate private AGPL repo holding the browser shell, player app, share-link codec, and chat-authoring agent, which pins this repo as a submodule.
- **C-ABI worklet boundary** — the documented raw `extern "C"`, `(ptr, len)`-over-linear-memory interface a browser host drives per audio quantum, carrying no `wasm-bindgen` glue and shipped as a contract to rebuild against, not a maintained binding.
- **share link** — an origin-independent encoded bundle that boots an instrument in the browser; a product-repo feature whose residue here is the sample-bytes trust obligation.
- **promotion** — the fast-forward-only advance of `dev` onto `main` that ships production, run as a workflow so commit SHAs are preserved and the branches never diverge.
- **toolchain pin** — the exact-version `rust-toolchain.toml` that local dev and CI share so their fmt/clippy verdicts are identical, kept in lockstep with the workspace MSRV.
- **perf gate** — the CI iai-callgrind instruction-count check that fails a PR on a >10% regression of the render hot path, base-ref-relative so toolchain drift cancels.
