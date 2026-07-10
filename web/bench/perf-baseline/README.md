# Web-player perf/size baseline, budget, and method (P7/C)

Resolves [#254](https://github.com/Impractical-Instruments/reuben/issues/254). Establishes the
before/after perf-and-size numbers [#229](https://github.com/Impractical-Instruments/reuben/issues/229)
asks for, the budget ceilings to hold, and the repeatable method — and surfaces the candidate
optimizations (each a future agent-ready spec ticket per the [#251](https://github.com/Impractical-Instruments/reuben/issues/251)
map's _Not yet specified_).

This is a **measurement task**: it does not optimize anything. It makes the specific
optimization work specifiable, and states plainly what still needs a real device.

## Question

What does the reuben web player cost to boot — WASM size, total payload, and cold-load time
navigation → engine ready → first-audio-ready — and **what dominates** that cold load: network
fetch, WASM compile/instantiate, resource discovery (`loader.mjs` fetch-on-miss), or sample
decode? What budget should P7 hold, by what repeatable recipe?

## Method

Two committed layers, both driving the **real** artifacts (the strum-message-rate harness's
stance — measure the real code, not a model):

- **Size** — the release WASM (`cargo build --release --target wasm32-unknown-unknown` in the
  detached `crates/reuben-web` crate) and the full `web/` build output (`npm run build`), each
  measured raw and at gzip-9 / brotli-11 transfer size (Cloudflare Pages serves brotli).
- **Timing** — `measure.mjs` (Playwright + CDP) serves the built payload from a local static
  server (brotli `Content-Encoding`, so a CDP network throttle sees production-like on-wire
  bytes) and, in headless Chromium:
  1. **Primitive attribution** — each cold-load primitive timed in isolation against the real
     release WASM: `fetch(wasm)`, `WebAssembly.compile`, `WebAssembly.instantiate` (+ ctor
     dance), `audioWorklet.addModule`, and a discovery load split into its **network** (fetch
     docs) vs **CPU** (construct/stage/decode) halves. This is where "what dominates" is read.
  2. **End-to-end** — the real `createReubenEngine()` + `engine.load('groovebox')` +
     `context.resume()`, wall-clocked (median of 3 cold runs; the HTTP cache is disabled via CDP
     `Network.setCacheDisabled`, so every run is a genuine cold load regardless of server headers).
  Both run under four conditions: **baseline** (dev host, local net) and an **emulated
  low-end-phone proxy** at 4× and 6× CPU throttle (`Emulation.setCPUThrottlingRate`) plus a
  slow-4G network profile (`Network.emulateNetworkConditions`, ~1.6 Mbps / 150 ms RTT).

```
cd web && npm run build                     # stage + vite build → dist/ (needs the release wasm)
CHROMIUM_PATH=/path/to/chrome node bench/perf-baseline/measure.mjs
```

(Needs `playwright` resolvable — symlink a `node_modules` next to `measure.mjs` while running;
don't commit it. Numbers below from headless Chromium 1194 on the CI-class dev host.)

> **Emulated ≠ real.** The 4×/6× CPU + slow-4G rows are a **proxy**, clearly labelled. The real
> low-end-device number is a **HITL leg left for a human** — see _Still needs a real device_.

## Results — size

Release WASM (`reuben_web.wasm`, current committed profile — `codegen-units = 1`, default
release opt):

| | raw | gzip-9 | brotli-11 |
|---|---|---|---|
| `reuben_web.wasm` | **745.1 KB** | 251.0 KB | **196.6 KB** |

Full `web/dist` payload (28 files), by category:

| category | files | raw | gzip | brotli |
|---|---|---|---|---|
| **wasm** | 1 | 745.1 | 252.0 | **196.6** |
| app-js (`index` + `worklet` chunks) | 2 | 43.1 | 15.9 | 14.1 |
| app-css | 1 | 5.5 | 1.7 | 1.5 |
| html | 1 | 1.4 | 0.8 | 0.6 |
| `schema.json` | 1 | 197.5 | 6.6 | 4.7 |
| instrument assets (5 Toys, docs + voices + subpatches) | 11 | 56.8 | 13.3 | 11.5 |
| icons (PWA PNGs — not on the boot path) | 7 | 50.4 | 49.6 | 49.2 |
| service worker | 3 | 22.8 | 8.6 | 7.7 |
| manifest | 1 | 0.6 | 0.3 | 0.3 |
| **TOTAL** | **28** | **1123.3** | **348.8** | **286.1** |

**Critical-path first-boot transfer** — what a fresh visitor downloads to reach first audio on
the default Toy (`groovebox`): `index.html` (0.6) + app JS (10.5) + CSS (1.5) + `worklet.js`
chunk (3.5) + **wasm (196.6)** + `schema.json` (4.7) + `groovebox` doc + 3 voices (3.6) ≈
**≈221 KB brotli**, of which the **WASM is ~89 %**. (Icons and the other Toys' assets are
precached by the service worker _after_ first paint — 25 precache entries — not on the
first-audio path.)

## Results — cold-load timing

Median of 3 cold runs, milliseconds. `e2e total` = `createReubenEngine` + `load(groovebox)` +
`resume()`, i.e. **navigation → first-audio-ready** (context reaches `running`), excluding the
human Start tap (that's a gesture-latency, not a load cost).

| condition | fetch wasm | compile | instantiate | addModule | discovery (fetch / cpu) | **e2e total** |
|---|---|---|---|---|---|---|
| baseline (1× CPU, local net) | 15 | 2.3 | 0.2 | 8.2 | 12.8 (10.3 / 2.5) | **59.7** |
| _emulated_ 4× CPU | 48 | 5.3 | 0.8 | 17.6 | 45.9 (30.4 / 13.3) | **134.9** |
| _emulated_ 6× CPU | 43 | 6.6 | 1.5 | 14.4 | 53.1 (33.7 / 16.5) | **183.3** |
| _emulated_ 6× CPU + slow-4G | 1153 | 7.9 | 1.7 | 12.4 | 525 (508 / 17) | **1886.2** |

## Startup profile — what dominates

- **On a fast/local network, nothing dominates on CPU.** The engine boots in **< 200 ms even at
  6× CPU throttle**. WASM compile is trivially cheap (**2–8 ms** — Chromium's baseline compiler);
  instantiate < 2 ms; `addModule` (worklet) 8–18 ms; discovery CPU (construct/stage) 2–17 ms.
  Shrinking the WASM will **not** meaningfully speed compile — the win is transfer, not CPU.
- **The dominant cold-load cost is network transfer of the WASM.** Under slow-4G the WASM fetch
  alone is **≈1.15 s (~61 % of the 1.9 s cold load)**; app-shell + WASM transfer + discovery
  fetches are essentially the whole number. WASM is ~89 % of the boot-transfer bytes, so it is
  the highest-leverage byte on the page.
- **Discovery is latency-bound on slow links.** `loader.mjs` fetches each round's misses
  **sequentially** (`for (const miss of misses) await fetchResource(...)`), so on a 150 ms-RTT
  link `groovebox`'s 3 tiny voice JSONs cost **≈508 ms of serialized RTTs** vs **17 ms** of CPU.
- **Sample decode is not on the bundled critical path.** None of the five `toys.json` Toys carry
  a `.wav` (they synthesize their audio), so first-audio decode cost is **zero** today.
  Characterized separately on `granulator-demo` (a frozen harness payload under `web/bench/fixtures/`, `testvoice.wav`
  ≈614 KB): decode **CPU is cheap** (8 ms baseline → 62–69 ms at 6× throttle), but the sample
  **transfer dominates** — ≈3.2 s to fetch the wav on slow-4G, and audio is ~incompressible
  (brotli barely helps). A future sample-carrying Toy pays transfer, not decode.

## Budget — proposed ceilings

Held by the size table (`npm run build` prints gzip; brotli via this harness) and the e2e timing.

| Metric | Current | Ceiling | Rationale |
|---|---|---|---|
| **WASM transfer (brotli)** | 196.6 KB | **≤ 200 KB** | Dominant asset (89 % of boot transfer). Any growth is the single highest-leverage regression — guard it hardest. |
| WASM raw | 745 KB | ≤ 760 KB | Governs the worklet's synchronous render-thread compile; tripwire on growth. |
| Critical-path boot transfer (brotli) | ≈221 KB | ≤ 240 KB | Small headroom; catches non-WASM creep on the first-audio path. |
| Full precache payload (brotli) | 286 KB | ≤ 320 KB | Offline-install size (all Toys + icons + SW). |
| App-shell JS (`index` chunk, brotli) | 10.5 KB | ≤ 16 KB | Catch bundle bloat before it competes with the WASM. |
| Cold load nav→audio-ready, _emulated_ 6× CPU + slow-4G | ≈1.9 s | ≤ 2.5 s | **Proxy** ceiling; the real-device figure is HITL (below). |
| Engine boot compute, 6× CPU, local net | 183 ms | ≤ 300 ms | CPU headroom for low-end; compile/instantiate must stay cheap. |
| Sample-carrying Toy | n/a (none bundled) | decode ≤ 150 ms @ 6× CPU; **decode off the first-audio path**, sample fetch **eager + prefetch-warmed, with visible progress** | Decode is cheap; transfer is the cost — warm samples via prefetch and show progress rather than freezing on a download (per [#253](https://github.com/Impractical-Instruments/reuben/issues/253) / SPEC.md §2.4; lazy fetch is out of scope per map [#251](https://github.com/Impractical-Instruments/reuben/issues/251)). |

## Candidate optimizations (evidence-grounded — each graduates to its own ticket)

Ordered by leverage. Each cites the measurement that justifies it.

1. **WASM size trim (opt-for-size profile, then wasm-opt).** *Highest leverage — WASM is 89 % of
   boot transfer and the slow-link bottleneck.* The committed release profile sets only
   `codegen-units = 1` (default opt = speed). A probe build with
   `opt-level="z" + lto=true + panic="abort" + strip=true` (no wasm-opt) already cuts the WASM to
   **493 KB raw (−34 %) / 151.6 KB brotli (−23 %)**; `wasm-opt -Oz` on top typically trims
   further. **Validation the ticket owns:** `panic="abort"` changes a Rust panic to a wasm trap —
   confirm `worklet.js`'s render-trap `try/catch` (and the discovery error paths) still surface
   diagnostics; re-run the `crates/reuben-web` test suite + this harness. **Evidence:** size table
   (196.6 KB brotli, 89 % of ≈221 KB path); trim probe in this README.
2. **Serve WASM as brotli on the deploy — verify Cloudflare Pages actually does.** brotli vs gzip
   on the dominant asset is **196.6 vs 251.0 KB — a 55 KB / 22 % swing for free** if the host
   negotiates `br`. Confirm the production `Content-Encoding` (and precache/SW interaction).
   **Evidence:** size table (gzip vs brotli columns).
3. **Parallelize discovery resource fetch.** `loader.mjs` awaits each round's misses
   sequentially; on a 150 ms-RTT link `groovebox`'s 3 voices cost ≈508 ms of serial RTTs vs 17 ms
   CPU. `Promise.all` over one round's misses collapses them to ~one RTT. **Evidence:** timing
   table, discovery (fetch / cpu) column at slow-4G (508 / 17).
4. **Sample loading UX — prefetch + visible progress, decode off the render path.** For a
   sample-carrying Toy the sample *transfer* dominates (≈3.2 s for a 614 KB wav on slow-4G) while
   decode CPU is cheap (~60 ms). The fix is the **eager + prefetch** loading design already spec'd
   in [#253](https://github.com/Impractical-Instruments/reuben/issues/253) (SPEC.md §2.4): warm
   sample bytes ahead of play and show determinate progress, so first audio is never blocked by —
   nor the UI frozen on — a sample download; decode already stays off the render critical path.
   (Lazy fetch is out of scope per map #251; no bundled Toy pays this yet.) **Evidence:**
   `sample_decode` rows (fetch 3184 ms vs cpu 62 ms at slow-4G).
5. **Code-split the non-first-paint app JS.** The `index` chunk (31.6 KB raw / 10.5 KB brotli)
   bundles engine + surface + share (`share.mjs`) + PWA registration into the shell; `share.mjs`
   and `workbox-window` aren't needed for first paint or first audio. Modest (~a few KB brotli) —
   lower priority than the WASM levers. **Evidence:** size table (app-js 43.1 KB raw).

## Still needs a real device (HITL)

The emulated 4×/6× CPU + slow-4G rows are a **proxy**. A human must record the **real low-end
phone** figure, which emulation does **not** capture:

- Real ARM-mobile V8 WASM compile/instantiate speed (a throttled desktop V8 is not a mobile V8).
- Real thermal throttling, background-app scheduling, and storage/cache-eviction behavior.
- Real mobile-network variance (a fixed slow-4G profile is not a live cellular link).
- The **actual audio-start latency after the Start tap** on iOS/Android — the `context.resume()`
  → first audible sample gap that the desktop harness reports as ~0.

**Recipe for the human:** run this harness's e2e path (or the deployed app) on a representative
budget Android over throttled real 4G, and record **navigation → first-audio-ready** plus the
**resume-tap → first-sound** latency. Fill the "6× CPU + slow-4G" proxy row's real-device
counterpart into the budget table; if it exceeds ~2.5 s, pull optimization #1 (WASM trim) forward.
