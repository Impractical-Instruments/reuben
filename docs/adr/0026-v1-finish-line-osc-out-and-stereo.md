# The v1 finish line: OSC-out, stereo, and a release workflow (V1.5 rescoped)

> **Superseded in part by [ADR-0041](0041-web-player-app-in-repo.md).** The packaging
> inference here — "a dedicated UI is out of scope for the project entirely… it belongs to a
> consuming application (its own repo)" — is retired *for the web player*: that app lives
> in-repo at `/web` (a monorepo consumer that still embeds the engine over a stable
> boundary). The deeper claim this ADR rests on — reuben's **core/engine** stays headless,
> its product surface is its I/O contract, not pixels — is unchanged and still governs
> `reuben-core`/`reuben-native`. Only the "own repo" clause moves.

## Context

[ROADMAP.md](../ROADMAP.md) listed V1.5 ("Reach & robustness") as the sole remaining v1 phase,
with seven items: live hot-swap, OSC-out, MIDI, clock sync, n-channel I/O, the parallel executor,
and Linux+Windows builds. That list conflated "things the engine could grow" with "things v1 must
ship." A grilling session pulled the finish line back to what the stated v1 definition actually
requires.

The roadmap defines v1 as *"the actual product people use. Linux + Windows, Toys, good-button UX."*
The Toys (V1.3), the playable surface (V1.2), the control-surface skill (V1.4), and the agent
skills (V1.6) all shipped. By the literal definition, only the **platform reach** is unmet.

Two framing decisions shaped the cut:

- **reuben is an engine, not an app.** It is *always driven by something else* (TouchOSC, a script,
  a consuming application), never a binary a non-technical person double-clicks. Its product surface
  is its I/O contract — OSC/MIDI/audio + JSON instruments — not pixels. This is already the
  codebase's shape: the `core`/`native` split (ADR-0012), OSC-in as a boundary adapter with core
  staying OSC-only (ADR-0007), headless from the MVP. Therefore a **dedicated UI is out of scope
  for the project entirely**, not merely deferred — it belongs to a consuming application (its own
  repo). V1.4's TouchOSC generator already gives players a real surface.

- **Two integration models, only one in v1.** *Out-of-process* (a consumer launches the reuben
  binary and drives it over OSC/UDP) is the entire current thesis and needs no new packaging beyond
  a distributable binary. *In-process* splits by language: Rust consumers already work via the
  cargo workspace (a non-issue — settled when the first consuming app is built); non-Rust consumers
  need a C ABI (cbindgen), which has no named consumer and stays in Later.

## Decision

### v1 ship gate = three items; the other five move to Later

In scope for v1:

1. **External OSC I/O (out)** — the boundary's outward half.
2. **Stereo output** — plus a `pan` operator. Scope-crept in deliberately: mono-only output is
   indefensible for a modern audio engine; everything is at least stereo.
3. **Windows build + a release workflow** — the stated reach promise, packaged.

Moved to [Later (post-v1)](../ROADMAP.md#later-post-v1), each by explicit decision (not drift):
live hot-swap, MIDI I/O, clock sync (Link/MIDI/OSC), **full** n-channel I/O, the parallel executor
(still perf-gated, ADR-0019), in-process non-Rust / C ABI, and the dedicated UI (out of project).

### OSC-out is a boundary sink operator carrying Message-domain values only

Core stays OSC-agnostic; UDP encode/send is native's boundary job, mirroring `osc::decode`
(ADR-0007). So:

- A new **`osc-out` sink operator** in core. Its input Messages are collected on an **outbound
  route** (a fourth lane, modelled on the context lane's `io.publish_context` mechanics).
  `render_block` gains an out-parameter for the drained outbound Messages. The op carries the
  outbound OSC `address`.
- **native** gains `osc::encode` (the trivial inverse of the existing `decode`) plus a UDP sender;
  it drains the outbound route each block and sends to a **static configured target** —
  `--osc-out host:port` on `play`, mirroring OSC-in's fixed bind.
- **Message-domain only.** A Good Button's `map` output is already a Message, so value feedback for
  a two-way surface works without new machinery. Sending a live **Signal** value out (a meter, a
  current cutoff) would need the Signal→Message sampler that V1.2 deferred — that sampler stays
  deferred; v1 OSC-out does not send Signal values.

Rejected: reply-to-last-sender (ambiguous with multiple senders, undefined before first packet) and
per-message destinations (pushes network addressing into the graph, leaking the boundary into core).
Both can return later as a `--osc-out reply` mode without a format change.

### Stereo: the Signal stays mono; channels are separate edges on an N-wide logical master

The Signal carries **one channel per edge** (today's model, unchanged). "Stereo" is *two edges*, not
a wider Signal. This leaves all 17 existing operators' `process` untouched — making a Signal carry N
channels (the full n-channel item) would touch every operator and every buffer, and stays deferred.

- The **master bus becomes N logical channels.** A tap (`outputs` entry / `PortRef`) gains an
  optional **`channel: <int>`** — an **index**, deliberately not a `"left"`/`"right"` name, so a
  mono or 5.1 device needs no special-casing later. **Omitted → broadcast to all channels** (the
  current mono-fan; existing instruments are bit-identical and unchanged). `channel: N` → master
  channel N only.
- Master width is **logical, derived from the instrument** (max referenced index + 1, floor 2),
  not from the device. `render_block`'s output contract goes from one buffer to N; `AudioConfig`
  gains the logical channel count.
- **audio.rs owns the logical→device mapping** — the single home for non-stereo-device policy
  (interleave to matching device channels; downmix for a mono device; fan/zero for extra channels).
  Core never learns the device's channel count.
- A new **`pan` operator**: one Signal in → out 0 / out 1 (equal-power, −3 dB center), pan amount as
  a **Signal input** with a param as unwired default (so an LFO auto-pans), following the V1.2
  one-port-one-type rule. The `output` op stays a mono passthrough and is vestigial for stereo —
  pan's two outputs are tapped directly as `channel:0` / `channel:1`.

### Packaging: a distributable binary, no installer; the crate is the primary product

- The **crate** is the primary product (it's what an in-process Rust consumer uses). The standalone
  **binary** is secondary: useful for out-of-process consumers and standalone play.
- Ship the binary as **versioned CI release archives** — a bare binary in an archive (zip/Windows,
  tar.gz/Linux), the Rust-CLI convention (ripgrep, fd, bat). **No installer**: reuben is a headless
  CLI run from a terminal; an MSI/Start-menu/PATH/uninstall flow buys nothing.
- **Build-from-source is the documented primary path** (`cargo build --release`, trivial for the
  Rust audience); the prebuilt archive is the convenience fallback for non-Rust Windows players,
  whose real friction is the MSVC toolchain, not OSC.
- A **release workflow** triggers on a `v*` tag (GitHub's Releases GUI and `gh release create` both
  create the tag and fire it), builds `--release` on both platforms, and attaches the archives.

### Windows is verified on real hardware, not by CI alone

CI (`windows-latest` in the matrix) compiles and runs the non-audio tests — it proves the build and
path/logic, but a headless runner **cannot** prove audio out (no device). So #7 is not "done" until
a **manual smoke pass on real Windows hardware**: metronome clicks on the beat, a Toy makes sound,
OSC-in plays, OSC-out reaches a TouchOSC surface. CI-green-only would be false confidence about
exactly the thing (WASAPI device init) CI can't see.

## Alternatives considered

- **Pull the dedicated UI into v1.** Rejected: it's a phase (build target, framework, two-way sync,
  distribution), not a task, and reuben is an engine — the UI is a consuming app. OSC-out (in v1)
  leaves it fully unblocked as the first Later phase.
- **Full n-channel now.** Rejected: the deferred #5 wearing a stereo costume; touches every
  operator and buffer. Stereo-as-two-edges is how modular/patching engines model it anyway — not a
  compromise.
- **Named L/R channels.** Rejected by the indexed-channel decision above — names force per-device
  special-casing later.
- **Engine-level address tap for OSC-out** (config lists internal addresses to forward). Rejected:
  a format change that leaks routing into the runtime and is less composable than a sink operator.

## Consequences

- **Touch-points, stereo:** `AudioConfig` + `render_block` output contract (1→N buffers),
  `format.rs` (`channel` on a tap), `plan.rs` (`output_taps` carry channel; master width),
  `render.rs` (per-channel master sum), `engine.rs` (N-channel scratch/`fill`), `audio.rs`
  (logical→device map), a new `pan` op, schema regen. All existing instruments must load and render
  bit-identically (asserted).
- **Touch-points, OSC-out:** a core outbound route + `osc-out` op, `render_block` out-parameter,
  native `osc::encode` + UDP sender, the `--osc-out` CLI flag.
- **Build order:** stereo first (the riskier core-contract edit), then OSC-out (independent), then
  Windows + release (packages a stable binary), then docs.
- **Deferred, explicitly:** Signal→Message sampler (so no live Signal-value OSC feedback in v1);
  full n-channel; reply-to-sender and per-message OSC targets; C ABI; the dedicated UI (out of
  project).
