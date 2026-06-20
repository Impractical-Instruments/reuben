# Rust for the core and native layer

## Context

The core must run realtime with no GC pauses, exploit multiple cores deterministically, hot-swap a graph plan lock-free, stay OS-free so the native layer is removable, and embed in game engines and other realtime hosts via a C ABI. Linux is the lead platform, Windows next, with mobile/wasm later. The author is a long-time C++ programmer choosing the stack for a long-lived solo project.

## Decision

Write the core and native layer in **Rust**, exposing a **C ABI** for embedding.

The two riskiest subsystems we committed to — lock-free plan hot-swap and deterministic parallel execution — are exactly what Rust's ownership and `Send`/`Sync` checks catch at compile time rather than as runtime heisenbugs. The design is already Rust-shaped: stable operator identity via IDs (arena + generational indices) rather than pointers, message-passing, and an immutable plan that is swapped wholesale. `cargo`, cross-compilation, wasm, and FFI (`cbindgen`, `godot-rust`, Unity P/Invoke) are first-class. Plugin hosting does not force C++: CLAP hosting is viable in pure Rust (`clack`), and LV2 via `livi`.

## Considered and rejected

- **C++:** faster day-one velocity for a fluent author, deeper DSP shelf (JUCE), more mature SIMD, and native VST3/AU + Unreal interop. Rejected because its advantages are front-loaded or concentrated in hosting formats we are not targeting (VST3/AU hosting is a non-goal), while Rust's safety advantages compound over the life of the project precisely on our hardest subsystems. Author motivation to use Rust is also a real factor for a project that previously stalled from energy depletion.

## Consequences

- Front-loaded friction: allocation-free audio-thread discipline (no GC, but allocs are easy to hide — use `assert_no_alloc` and preallocated pools), borrow-checker adjustment for graph structures (resolved by IDs-not-pointers, already our model), and RT-safe concurrency primitives (`rtrb`, `triple-buffer`, `crossbeam`).
- Thinner DSP library shelf; more operators ported from C++ references by hand — acceptable since writing operators is the project itself.
- VST3/AU plugin hosting effectively off the table (already a non-goal).
