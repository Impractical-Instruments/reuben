# Why: The portable Engine bridge (queue_osc, fill, drain_outbound) lives in reuben-core as the one embed surface every host shell wraps.

[Rule](../../execution-runtime.md#embed-surface)

The `Engine` — the bridge between the fixed block-size core render and a host's arbitrary-length
audio pull (`queue_osc` → `fill`/`fill_duplex` → `drain_outbound`) — was born in `reuben-native`
only because cpal's callback was its first caller. Three embedders now want exactly that bridge:
native (cpal + UDP/OSC + fs), web (a WebAudio `AudioWorkletProcessor` quantum), and a game-engine
mix step. A P1 spike proved it is not native-specific — it had to hand-rewrite the Plan + Renderer +
scratch glue, and its only imports were `reuben_core::{message, plan, render}`. So `Engine`
descends into `reuben_core::engine`: a curated **module**, not a new crate — a crate boundary would
fence off nothing (every consumer of Engine already depends on all of core) at the cost of another
workspace member, version, and docs surface (the same shape rejected for a `reuben-coordinator`
crate). The hard constraint holds: no non-portable dependency enters `reuben-core`, so it keeps
compiling to `wasm32-unknown-unknown` untouched.

The surface is drawn to keep protocol decode in the shells and Plan-aware typing in the engine:
`queue_osc(&mut self, address, &[Arg])` takes the already-decoded flat primitive form every shell's
decode layer produces anyway, so native keeps its `OscIn` UDP-decode struct rather than pushing it
into core. `Engine::from_document` packages `load_instrument` → `Plan::instantiate` →
`Engine::new` — the construction chain each shell otherwise rewires by hand — and returns load
warnings because resource problems are non-fatal but must be *surfaced* by every shell, not
swallowed by the convenience. What stays native is the removable I/O per the boundary rule: cpal
streams, the UDP/OSC codec, the filesystem resolver, device profiles
([single-writer-coordinator](single-writer-coordinator.md)). This names the portable side's rim —
the **embed surface** — and coordinates the embed API once instead of three shells re-inventing the
bridge (and its RT-debt notes) in parallel.

Distilled from: ADR-0039
