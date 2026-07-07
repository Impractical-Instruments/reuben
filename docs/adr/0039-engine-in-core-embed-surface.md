# ADR-0039: Engine lives in reuben-core as the shared embed surface

## Status

Accepted (2026-07-07). The structural choice of the web player epic
([#151](https://github.com/Impractical-Instruments/reuben/issues/151), P2:
[#224](https://github.com/Impractical-Instruments/reuben/issues/224)), with its evidence
supplied by the P1 spike ([#223](https://github.com/Impractical-Instruments/reuben/issues/223):
`reuben-core` compiles to `wasm32-unknown-unknown` with zero changes). The future game shell
([#222](https://github.com/Impractical-Instruments/reuben/issues/222)) depends on this record.
**Rides on** [ADR-0012](0012-boundary-and-threading.md) (portable core, removable native layer)
and [ADR-0002](0002-rust-core.md); **amends neither** — it moves one type across
the line those ADRs drew, it does not move the line.

## Context

The `Engine` — the bridge between the fixed block-size core render and a host's
arbitrary-length audio pull (`queue_osc` → `fill`/`fill_duplex` → `drain_outbound`) — was born
in `reuben-native` because cpal's callback was its only caller. Three embedders now want
exactly that bridge: native (cpal + UDP/OSC + fs), web (a WebAudio `AudioWorkletProcessor`
quantum, [#224](https://github.com/Impractical-Instruments/reuben/issues/224)), and a game
engine mix step ([#222](https://github.com/Impractical-Instruments/reuben/issues/222)). The
P1 spike had to re-write the Plan + Renderer + scratch-block glue by hand — throwaway proof
that the bridge is not native-specific. Its implementation was already OS-free (its only
imports were `reuben_core::{message, plan, render}`); the single native coupling was the
`queue_osc(&OscIn)` signature, where `OscIn` is native's UDP-decode target.

Each shell also re-wired the same construction chain (`load_instrument` →
`Plan::instantiate` → `Engine::new`) by hand — glue worth inventing exactly once.

## Decision

### 1. `Engine` descends into `reuben_core::engine`

A curated module, **not** a new crate: the embed surface is
`ResourceResolver` in → construct → `queue_osc` → `fill`/`fill_duplex` → `drain_outbound`,
and it lives where the rest of the portable surface (Plan, Renderer, format, resources)
already lives. Constraint unchanged: **no non-portable dependency enters `reuben-core`** —
the crate keeps compiling to `wasm32-unknown-unknown` untouched (the P1 finding this rides
on).

**Considered and rejected:** a separate `reuben-engine` crate — a crate boundary with
nothing to fence off (every consumer of Engine already depends on all of core), bought at
the cost of another workspace member, version, and docs surface.

### 2. `queue_osc` takes the flat primitive form directly

The core signature is `queue_osc(&mut self, address: &str, args: &[Arg])` — the
already-decoded **flat primitive form** of
[ADR-0030](0030-osc-as-all-data-one-message-type.md) (`F32`/`I32`/`Str`), which is what every
shell's decode layer produces anyway (native's rosc datagrams, web's flat tagged control
buffers). Native keeps its `OscIn` struct as the UDP-decode target and calls the core
signature with `&osc.address, &osc.args`. Protocol decode stays in the shells; the
Plan-aware typing (`Plan::osc_in_message`) stays in the engine, which owns the Plan.

**Considered and rejected:** moving `OscIn` into core — it is native's decode artifact, and
a struct holding exactly the two arguments of a method call adds a type where a signature
suffices.

### 3. `Engine::from_document` is the one construction glue

`Engine::from_document(text, registry, &resolver, config) -> Result<(Engine, Vec<LoadWarning>),
FromDocumentError>` packages `load_instrument` → `Plan::instantiate` → `Engine::new`. Every
shell calls it instead of re-wiring the chain. It returns the load warnings because resource
problems are non-fatal ([ADR-0016](0016-sample-player-and-resource-store.md)) but must be
*surfaced* by every shell, not swallowed by the convenience; `FromDocumentError` keeps the load/instantiate distinction the two underlying
error types already draw.

### 4. What stays native

cpal streams and device negotiation, the UDP/OSC codec (`rosc`), the filesystem resolver,
device profiles, diagnostics — the removable I/O per ADR-0012. `reuben-native` re-exports
`Engine` so its embedders keep one dependency.

## Consequences

- Web ([#224](https://github.com/Impractical-Instruments/reuben/issues/224)) and game
  ([#222](https://github.com/Impractical-Instruments/reuben/issues/222)) shells wrap
  `reuben_core::engine` instead of re-inventing the bridge; the embed API is coordinated
  here, once.
- The Engine's tests (chunk-size independence, duplex alignment, outbound drain, stale-input
  pins) moved to core with it and now run wherever core runs — including any future
  wasm-target test lane.
- The engine's RT-debt note (the `pending` Vec handoff churns the heap when messages flow)
  moves with it unchanged; fixing it now benefits all shells at once.
- ARCHITECTURE.md's portable/native split line gains a name for the portable side's rim:
  the **embed surface**.
