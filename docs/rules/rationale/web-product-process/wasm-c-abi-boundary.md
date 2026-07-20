# Why: reuben-core compiles to wasm32 untouched, and its browser story is the documented raw C-ABI worklet boundary — no wasm-bindgen, no maintained binding shipped — from which a third party reconstructs its own binding.

[Rule](../../web-product-process.md#wasm-c-abi-boundary)

The default Rust↔JS story — `wasm-bindgen` + `wasm-pack` — was tried both ways at the worklet
boundary and lost one-sidedly. Its generated glue assumes a Window/Worker global and fights
`AudioWorkletGlobalScope` (no `TextDecoder`, no `fetch`, restricted module loading), and the boundary
the engine actually needs is **flat**: a handful of exports moving bytes and floats through linear
memory plus one `log` import — no object graph to marshal, so bindgen buys nothing and costs
compatibility. The worklet-specific quirks dominate the design and live in hand-written JS regardless
(Chromium refuses a structured-cloned `WebAssembly.Module` to a worklet, so post raw bytes and
sync-compile inside; async `instantiate` can stall in a suspended render context, so init must be
fully synchronous). Hence a raw C-ABI: `#[no_mangle] extern "C"` exports over the embed surface,
plain `cargo build --target wasm32-unknown-unknown`, data crossing as `(ptr, len)` byte regions, UTF-8
strings, and planar `f32` audio at fixed static offsets (a static's offset never moves, so a host
fetches each pointer once and only re-wraps its `Float32Array` views per quantum — memory *growth*
detaches views). A small `format_version()` export lets JS tell a document-from-the-future apart from
an envelope-from-the-future without guessing.

What changed is *where the shell lives*, not the contract. The concrete `cdylib` shell left with the
product repo, so this repo no longer exports those symbols — but `reuben-core` still compiles to
`wasm32-unknown-unknown` untouched, and the C-ABI is now **the** public browser story: a third party
who wants reuben in a browser does not get a maintained binding from us, they get core-to-wasm plus
the documented boundary (one `Engine::fill` per quantum, fetch-on-miss resource staging, a flat
tagged control channel) and reconstruct the binding themselves. That the shell was thin enough to be
worth rebuilding is exactly why we felt free to stop publishing it; the trade-off — no ready-made BSD
browser binding, so someone who wants one has work to do — is accepted on the judgement that the
C-ABI is stable and documented enough to carry the weight. See
[static-link-operator-registration](static-link-operator-registration.md) for the one build flag any
such wasm embedder must not miss.

Distilled from: ADR-0040, ADR-0056, ADR-0042
