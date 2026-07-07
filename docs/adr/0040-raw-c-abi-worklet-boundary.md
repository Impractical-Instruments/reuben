# ADR-0040: Raw C-ABI at the WebAudio worklet boundary, no wasm-bindgen

## Status

Accepted (2026-07-07). Flagged for recording by the P1 spike
([#223](https://github.com/Impractical-Instruments/reuben/issues/223), PASS on a real
iPhone; archive branch `claude/issue-223-spike-jn11jr`), which supplies the evidence;
productized by P2 ([#224](https://github.com/Impractical-Instruments/reuben/issues/224),
the `crates/reuben-web` shell). **Rides on**
[ADR-0039](0039-engine-in-core-embed-surface.md) (the embed surface this shell wraps) and
[ADR-0012](0012-boundary-and-threading.md) (removable I/O shells).

## Context

The web player runs `reuben-core` inside an `AudioWorkletProcessor` — WASM on the browser's
real-time audio rendering thread. The default Rust↔JS story is `wasm-bindgen` +
`wasm-pack`: generated glue, typed wrappers, npm packaging. The P1 spike tried the worklet
boundary both ways and the findings were one-sided:

- **bindgen's generated JS glue assumes a Window/Worker global** and fights
  `AudioWorkletGlobalScope` (no `TextDecoder`, no `fetch`, restricted module loading).
- The whole boundary the engine actually needs is **flat**: a handful of exports moving
  bytes and floats through linear memory, plus one `log` import for diagnostics. There is
  no object graph to marshal — bindgen buys nothing and costs compatibility.
- Worklet-specific platform quirks dominate the design anyway, and they live in hand-written
  JS regardless: Chromium silently refuses to deliver a structured-cloned
  `WebAssembly.Module` to a worklet (post raw **bytes**, sync-compile inside — the ~4 KB
  sync-compile limit is main-thread-only), and async `instantiate` can stall in a suspended
  context (the render thread isn't pumping microtasks), so worklet init must be fully
  synchronous.

## Decision

### 1. Raw C-ABI, plain `cargo build`

`crates/reuben-web` is a `cdylib` of `#[no_mangle] extern "C"` exports over the embed
surface, built with `cargo build --target wasm32-unknown-unknown --release`. No
`wasm-bindgen`, no `wasm-pack`, no npm packaging (P4's concern). The ABI is documented in
one place — `src/bridge.rs` — and the co-located ES-module JS (under the crate's `js/`)
codes against it. Data
crosses as `(ptr, len)` byte regions via `alloc`/`dealloc`; strings are UTF-8; audio is
planar `f32` at fixed static offsets (a static's offset never moves, so the host fetches
each pointer once and only re-wraps its `Float32Array` views per quantum — memory *growth*
detaches views, the P1 finding).

**Considered and rejected:** `wasm-bindgen` (the Context findings — glue vs
`AudioWorkletGlobalScope`); `wasm-pack` (packaging for a boundary with no generated glue);
`wasm-opt` (deferred size pass, not a boundary concern).

### 2. Logic host-tested; only the shims are wasm-gated

Everything with behavior — the flat control codec, the fetch-on-miss resolver, WAV decode,
the shell lifecycle — is plain portable Rust, exercised by `cargo test` on the host. The
`#[cfg(target_arch = "wasm32")]` bridge is one-line shims plus the `log` import, so the
untestable surface is minimized by construction.

### 3. Panics ship their message before trapping

On `wasm32-unknown-unknown` a panic is a trap that silently kills the processor. Every
entry point installs (once) a panic hook that ships the message through `log` first. Known
gap, accepted: a panic inside a static ctor predates any hook and surfaces as an opaque
`RuntimeError: unreachable` (P1 finding — LLD synthesizes ctor calls into every export's
prologue).

### 4. `codegen-units = 1` is load-bearing for operator registration

New finding (P2): operator self-registration
([ADR-0024](0024-compile-time-operator-registration.md), `inventory`) plants a ctor in the
object file of whatever codegen unit holds the operator. In a default release build rustc
splits `reuben-core` into many CGUs, and **the linker only pulls an rlib's object files
whose symbols are referenced** — a CGU containing nothing but operator impls and their
ctors is silently dropped, and those operators don't exist at runtime. Observed concretely:
36 of 53 operators registered; `oscillator`, `voicer`, and `clock` were among the missing.
The P1 spike saw all 53 by CGU-partitioning luck, not by construction. The web crate
therefore pins `[profile.release] codegen-units = 1`: one object per crate, always pulled,
every ctor linked. Any future WASM (or other statically-linked) embedder of `reuben-core`
must do the same — recorded here because the failure is silent and looks like a broken
registry, not a broken link.

## Consequences

- The JS side owns the platform quirks explicitly (bytes handoff, sync compile, ctor dance,
  view re-wrapping) — they're documented in the worklet source, not hidden in generated
  glue.
- The ABI is stable and language-neutral; the game shell (#222) reuses the embed surface
  natively and never sees this boundary.
- A future OSC-over-WebSocket door is additive: `queue_osc_bytes(ptr, len)` backed by
  `rosc`-in-wasm, funneling into the same `Plan::osc_in_message` as the flat codec.
- Diagnostics depend on the single `log` import; a host that doesn't wire it gets traps
  without messages.
