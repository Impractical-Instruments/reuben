# Why: The core and native layer are written in Rust exposing a C ABI.

[Rule](../../execution-runtime.md#rust-core)

The stack choice is driven by the two riskiest subsystems the design commits to — lock-free plan
[hot-swap](engine-swap-unit.md) and [deterministic parallel execution](deterministic-render.md) —
which are exactly what Rust's ownership and `Send`/`Sync` checks catch at compile time rather than
as runtime heisenbugs. The design is already Rust-shaped: stable operator identity via arena +
generational-index IDs rather than pointers, message passing, and an immutable
[Plan](plan-lifecycle.md) swapped wholesale. `cargo`, cross-compilation, wasm, and FFI
(`cbindgen`, `godot-rust`, P/Invoke) are first-class, and the **C ABI** is what lets the core embed
in native, web, and game shells.

C++ was rejected not on its merits — faster day-one velocity, a deeper DSP shelf, VST3/AU interop —
but because those advantages are front-loaded or concentrated in hosting formats reuben does not
target (VST3/AU hosting is a non-goal), while Rust's safety advantages compound over a long-lived
solo project precisely on its hardest subsystems. The accepted cost is front-loaded friction:
allocation-free audio-thread discipline (allocs are easy to hide — `assert_no_alloc` + preallocated
pools), the borrow-checker adjustment for graph structures (resolved by IDs-not-pointers, already
the model), and a thinner DSP shelf (acceptable — writing operators *is* the project). Author
motivation also counts for a project that previously stalled from energy depletion.

Distilled from: ADR-0002
