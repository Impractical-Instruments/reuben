# WASM-in-AudioWorklet spike (issue #223)

**Throwaway.** This crate proves one thing: `reuben-core` compiled to
`wasm32-unknown-unknown` renders inside a browser's audio thread and makes sound. It is
deliberately **not a workspace member** (own `[workspace]` table in `Cargo.toml`), is
**never merged to main** — the branch is the archive — and the real web crate is P2's
job. Design discussion: the settled-approach comment on
[#223](https://github.com/Impractical-Instruments/reuben/issues/223).

## Shape

- `src/lib.rs` — raw C-ABI `cdylib` (no `wasm-bindgen`): `init(sample_rate, instrument)`,
  `render()`, `output_ptr()`, `error_ptr()/error_len()`, `registry_count()`, plus one
  imported `log(ptr, len)` for diagnostics. `block_size = 128` = one engine block per
  worklet quantum, so no drain adapter.
- `web/worklet.js` — the `AudioWorkletProcessor`: sync-compiles + instantiates the WASM
  from raw bytes posted in by the main thread (a worklet can't fetch; see findings below
  for why bytes and why sync), runs static ctors, `init`s, renders one block per
  `process()`.
- `web/main.js` + `web/index.html` — pre-stages everything at page load; the Start
  button's handler does **only** `ctx.resume()` (the iOS unlock gesture). Runs a
  batch-timed headroom measurement on a second instance after init.
- `node-check.mjs` — headless machine checks (see below).
- Instruments: `vibrato` (the pass/fail gate, zero resources) and `sequence` (the
  stretch — resolver + voicer-host path), both embedded via `include_str!` from
  `instruments/`.

## Build & run

```sh
./build.sh                       # cargo build --release for wasm32 + stage into web/
node node-check.mjs              # headless checkpoints (see below)
python3 -m http.server -d web    # http://localhost:8000 — desktop browsers
```

`?instrument=sequence` on the URL switches to the stretch instrument.

### Real iPhone

AudioWorklet needs a **secure context**; a LAN IP over plain HTTP isn't one. Serve
through an ephemeral HTTPS tunnel:

```sh
cloudflared tunnel --url http://localhost:8000   # or: ngrok http 8000
```

`postMessage`-only (no SharedArrayBuffer) ⇒ **no COOP/COEP headers needed** (P7's
problem).

**iOS false-negative guards** — before declaring failure, confirm: hardware mute/ring
switch **off** (iOS routes WebAudio to the ringer channel), volume up, **not** in
low-power mode, screen unlocked.

## Checkpoints (what the finding on #223 must report)

| # | Checkpoint | How it's checked |
|---|---|---|
| 1 | `reuben-core` compiles to `wasm32-unknown-unknown` | `./build.sh` |
| 2 | `Registry::builtin()` non-empty inside WASM (`inventory` ctors ran) | `node-check.mjs` asserts `registry_count() > 0`; the page logs it too |
| 3 | Sound within one gesture: desktop Chrome / Firefox / Safari | manual, per browser |
| 4 | Real iPhone | manual, over the tunnel |
| 5 | Headroom + 60 s no-glitch listen | the page's `[bench]` log line + your ears |

## Findings so far (machine-checked in this environment)

1. **`reuben-core` compiles to `wasm32-unknown-unknown` with zero changes** — the
   OS-free-by-design claim (ADR-0012) held; nothing incidental surfaced.
2. **`inventory` works in WASM: 53 operators registered.** Toolchain note (Rust 1.96 /
   LLD): the module exports **neither** `_initialize` nor `__wasm_call_ctors` — LLD
   synthesizes the ctor calls into every export, so registration happens on the first
   call into the module. `worklet.js` still tries both named hooks first
   (toolchain-portable), and `registry_count()` makes the verdict loud either way.
   Corollary: a panic **inside a ctor** runs before `init` can install the panic hook,
   so it reaches the page only as an opaque `RuntimeError: unreachable` (caught and
   shown, but without the panic message).
3. **Chromium silently refuses to deliver a structured-cloned `WebAssembly.Module` to a
   worklet** — it surfaces as `messageerror` (which nothing listens to by default), not
   as a throw at `postMessage`. So the main thread posts the raw **bytes** and the
   worklet sync-compiles them (`new WebAssembly.Module`; the ~4 KB sync-compile limit is
   main-thread-only). This deviates from the design comment's "module postMessage'd in"
   — same spirit, hardened form.
4. **Async `WebAssembly.instantiate` inside a *suspended* worklet can stall** until
   `ctx.resume()` (the render thread isn't pumping microtasks), which is why worklet
   init is fully synchronous and the Start button is enabled by pre-staging, not by the
   worklet's `ready` message.
5. **Real-browser render verified headlessly**: an `OfflineAudioContext` in Chromium
   141 drives the identical `worklet.js` + WASM path; both instruments render
   non-silent, finite stereo (vibrato rms 0.707, sequence rms 0.190 over 2 s).
   Realtime-device browsers (checkpoint 3) and the iPhone (checkpoint 4) remain the
   manual protocol above.
