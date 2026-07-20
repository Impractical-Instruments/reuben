# Why: This repo is the reuben SDK — engine core, native CLI, MCP sidecar, and the instrument/surface library — while the browser shell, player app, and authoring agent live in a separate private product repo that consumes this one as a submodule.

[Rule](../../web-product-process.md#sdk-product-split)

The line was drawn long before the product existed: reuben is *always driven by something else* and
its product surface is its I/O contract (OSC/MIDI/audio + JSON instruments), not pixels — so a
dedicated UI is out of the project, it belongs to a consuming application. That argument briefly
lost when the web player was pulled in-repo at `/web`, on three claims — the engine already reached
into the browser from here, this was the only place it could be built and deployed, and "own repo"
bought an isolation the app didn't want since it must track the engine lockstep. A submodule
answers all three: the first two were artifacts of not yet having one, and the third is answered by
the pin. So the whole browser crate and player app moved, wholesale, to a separate private repo that
pins this one at `engine/` and builds against `reuben-core` through a Cargo path dependency.

The deciding factor is **support surface, not licensing**. The browser crate was a
workspace-detached leaf with exactly one consumer and no intent to support a second; keeping it
public would have committed us to maintaining a public browser binding as an API — with the
compatibility obligations that implies — for a user we do not have. What stays here is the SDK the
product consumes: `reuben-core` (engine + embed surface + the C-ABI contract), `reuben-native` (the
`reuben` CLI and its audio/OSC/fs host), `reuben-mcp` (the stdio sidecar), and `instruments/` +
`surfaces/` (load-bearing SDK fixtures in core's tests). The submodule pin becomes the version
boundary between the repos — the engine version is a property of a cross-repo SHA, adopted when the
product bumps its pin — so the two evolve independently without a shared build. Engine changes land
here on a normal branch and PR; the product adopts them by moving its pin. See also
[license-boundary](license-boundary.md) (the AGPL/BSD split that happens to fall on the same seam)
and [wasm-c-abi-boundary](wasm-c-abi-boundary.md) (the contract the departed shell rebuilds against).

Distilled from: ADR-0056, ADR-0041, ADR-0026
