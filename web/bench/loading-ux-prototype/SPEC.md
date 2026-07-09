# Sample-decode loading / progress UX — implementation spec (P7/B, #253)

**Status:** agent-ready. An implementation agent can execute this cold in one session.
**Resolves:** [#253](https://github.com/Impractical-Instruments/reuben/issues/253) ·
**Map:** [#251](https://github.com/Impractical-Instruments/reuben/issues/251) ·
**AC anchor (#229):** *sample rigs load with visible progress, never a frozen UI.*

**Visual reference:** [`prototype.html`](./prototype.html) — a THROWAWAY mock (fake timing,
no engine). This spec realises **option C + D combined** (aggregate determinate bar +
per-sample rows) from that prototype. The prototype ships nothing; delete it once #253 is
implemented.

Human decisions this spec encodes (settled in the HITL session):
1. **Sample shape = multi-sample (drum-kit style).** Sampler/granulator Toys will carry
   several one-shots → per-sample progress and partial-success are load-bearing.
2. **Fetch = eager + prefetch.** Keep structurally-eager staging; warm sample bytes on
   idle/hover. No lazy redesign, no second construct round.
3. **Freeze fix** — remove the redundant main-thread discovery-time decode.
4. **One new boundary event** — per-sample `staged` progress from the worklet.
5. **Progress UI** — aggregate bar + per-sample rows.
6. **Failure = partial-success** — one bad pad never sinks the rig.

---

## 0. Ground truth (from the code, before any change)

- **Decode is Rust/WASM**, `crates/reuben-web/src/decode.rs::decode_wav_bytes` (hound),
  called synchronously from `bridge.rs::stage_resource` (kind `1`) →
  `shell.rs::WebShell::stage_sample_wav` → `resolver.rs::WebResolver::stage_sample`. It is
  **not** in `render()`/`process()`.
- Decode runs on **two threads today**, once each:
  - **Main thread (discovery instance):** `reuben-engine.mjs::load()` runs the fetch-on-miss
    loop (`loader.mjs::loadInstrument`); every sample miss is `stage_resource`d **on the UI
    thread** and the decoded buffer is **discarded** (the bundle ships raw WAV bytes). ← the
    frozen-UI risk.
  - **Worklet (persistent instance):** `worklet.js::onLoad` re-stages the raw bytes →
    decodes for real → renders.
- **Coarse boundary today:** `load()` is one awaited promise; the worklet posts a single
  `ready`. No progress event of any kind crosses the boundary.
- **UI today (`web/src/main.js`):** binary skeleton → ready. No phases, no progress, no
  per-sample granularity; a stalled fetch shimmers forever (no timeout); any hard
  fetch/stage failure is fatal to the load and shows one generic card + Retry.
- **Empty `SampleBuffer` is legal:** `SampleBuffer::new(Vec::new(), rate)` yields
  `channel_count() == 0`, `frame_count() == 0` — exactly ADR-0016's degrade shape.

---

## 1. State machine

### 1.1 Per-sample state (one row per sample)

```
queued ──▶ fetching ──▶ decoding ──▶ ready
   │           │            │
   │           ▼            ▼
   │        failed(fetch) failed(decode)
   ▼
 (never reached if the doc load fails first)
```

| State            | Meaning                                                                 | Driven by                          |
|------------------|-------------------------------------------------------------------------|------------------------------------|
| `queued`         | discovered as a miss, not yet fetched                                    | miss enumerated in discovery loop  |
| `fetching`       | bytes downloading; carries `{received, total}` when `content-length` known | `fetchResource` stream reader (main) |
| `decoding`       | bytes staged into the worklet, hound decode in flight                   | worklet staging loop               |
| `ready`          | decoded, bound into the graph                                           | worklet `staged{status:"ok"}`      |
| `failed(fetch)`  | HTTP non-2xx / network error; empty buffer bound (silent pad)           | `fetchResource` catch (main)        |
| `failed(decode)` | bytes fetched but not a valid WAV; empty buffer bound (silent pad)      | worklet `staged{status:"failed"}`  |

Between `fetching`-complete and `decoding`-start a sample is "fetched, awaiting construct";
fold that into `decoding` (set the moment the bundle ships to the worklet).

### 1.2 Aggregate / rig state

```
idle ──▶ fetching ──▶ decoding ──▶ ready
                          │    └──▶ ready-with-issues
                          └───────▶ failed
```

- `idle` → player screen shown, nothing fetched (or prefetch warming silently).
- `fetching` → discovery loop running; **aggregate bar 0–70 %** = Σ received / Σ total bytes.
- `decoding` → bundle shipped; **bar 70–100 %** = worklet `staged` count `k / total`.
- **Terminal states:**
  - `ready` — every sample `ready`.
  - `ready-with-issues` — `1 ≤ failedSamples < totalSamples`; rig plays, failed pads silent.
  - `failed` — the document (or a **text/voice** resource) failed, **or** `failedSamples ==
    totalSamples` (every pad dead). No playable rig.

Aggregate-bar byte math mirrors prototype option C: fetch is the first 70 %, decode the last
30 %. If `content-length` is missing for a sample, fall back to per-sample-count progress for
that segment (bar advances one notch per sample rather than per byte).

---

## 2. Changes by decision, with anchors

### 2.1 Decision 3 — Freeze fix: stage samples on discovery **without decoding**

**Why:** discovery only needs the sample miss to *clear* so `construct` proceeds and nested
references keep surfacing. Samples carry no further references, and the discovery instance's
decoded buffer is thrown away. So stage a **stub empty buffer** on discovery — zero decode on
the UI thread. Real decode stays in the worklet (off the render path).

**`crates/reuben-web/src/shell.rs`** — add a sibling to `stage_sample_wav`:
```rust
/// Stage a decode-DEFERRED sample: bind an empty buffer under `key` so the discovery
/// construct loop clears the miss without decoding on the main thread (#253). The real
/// decode happens in the worklet via `stage_sample_wav`. Empty buffer == ADR-0016 degrade.
pub fn stage_sample_deferred(&mut self, key: &str) {
    self.error.clear();
    self.resolver.stage_sample(key, SampleBuffer::new(Vec::new(), 1.0));
}
```

**`crates/reuben-web/src/bridge.rs`** — extend `stage_resource` with a new `kind`:
- `kind 0` = text (unchanged), `kind 1` = sample, decode now (unchanged — the worklet path),
- **`kind 2` = sample, stage deferred stub (no decode)** → calls `shell().stage_sample_deferred(key)`, returns `0`.
- Update the ABI doc comment (`bridge.rs:14-27`) and the `stage_resource` doc (`:177-181`).
- **Miss `kind` is unchanged (`1` = Sample):** the *miss* still says "this is a sample"
  so the loader fetches it as bytes. The `kind` passed to `stage_resource` is the caller's
  choice of *how to stage* — discovery passes `2`, the worklet passes `1`.

**`crates/reuben-web/js/loader.mjs`** — `loadInstrument`'s staging call:
- Current: `stageResource(ex, key, kind, bytes)` passes the miss `kind` (so samples decode).
- Target: for a **sample** miss (`kind === 1`), stage with `kind 2` (deferred) on the
  discovery instance; text misses still stage with `kind 0`. Add a small map:
  `stageKind = miss.kind === SAMPLE ? DEFER : miss.kind`. The **bundle still records the
  original kind `1`** so the worklet decodes for real (bundle assembly is in
  `reuben-engine.mjs`, not here).
- Net effect: the discovery instance never decodes a WAV → the main thread never blocks on
  hound. (`stage-assets.mjs`, which imports `loadInstrument`, inherits the no-decode
  discovery for free — a nice side win; its output set is unchanged because keys are
  identical.)

### 2.2 Decision 4 — One new boundary event: per-sample `staged` progress

**Producer — `crates/reuben-web/js/worklet.js::onLoad`** (the staging loop over `msg.bundle`,
currently `:177-193`): after each `stage_resource`, post progress. Precise shape:
```js
// after staging entry i of msg.bundle (k = i+1, total = msg.bundle.length)
this.port.postMessage({ type: "staged", k, total, key: entry.key,
                        kind: entry.kind, status /* "ok" | "failed" */ });
```
- Sample entries (`kind 1`) report `status:"ok"` on decode success, `"failed"` on decode
  reject (see §2.3 for the non-fatal change). Text entries (`kind 0`) report `"ok"` or abort
  the load (text failure is fatal).
- This is the **only** new message type on the wire.

**Consumer — `crates/reuben-web/js/reuben-engine.mjs`**:
- In `node.port.onmessage` (the `switch (msg.type)` at `:94`), add:
  ```js
  case "staged":
    onProgress?.({ phase: "decode", k: msg.k, total: msg.total,
                   key: msg.key, status: msg.status });
    break;
  ```
- **Callback API:** add an optional `onProgress` to `load(name, { onProgress } = {})` and
  `loadBundle({ docText, resources, onProgress })`. `onProgress` is fire-and-forget
  (never awaited, never affects the `ready` promise). It receives:
  ```ts
  type LoadProgress =
    | { phase: "fetch";  key: string; received: number; total: number | null }
    | { phase: "decode"; key: string; k: number; total: number; status: "ok" | "failed" }
    | { phase: "settled"; result: "ready" | "ready-with-issues" | "failed";
        failed: Array<{ key: string; reason: "fetch" | "decode" }> };
  ```
  `shipBundle` closes over the active load's `onProgress` so the `staged` events route to the
  right subscriber (one load at a time is already enforced by the `loading` guard).

**Fetch progress — main side, `reuben-engine.mjs::load()` `fetchResource` callback**
(currently `:239-246`): replace the `await r.arrayBuffer()` with a streamed read so bytes are
observable:
```js
const total = Number(r.headers.get("content-length")) || null;
const reader = r.body.getReader();
let received = 0; const chunks = [];
for (;;) {
  const { done, value } = await reader.read();
  if (done) break;
  chunks.push(value); received += value.length;
  onProgress?.({ phase: "fetch", key, received, total });
}
const bytes = concat(chunks, received);
```
(If `r.body` is absent — e.g. a test double — fall back to `arrayBuffer()` and emit a single
`{received: total, total}`.)

### 2.3 Decision 6 — Partial-success failure policy

Two error classes are already distinguishable at the JS boundary:
- **fetch class** — `fetch <url>: HTTP <status>` (or a network throw) raised in the
  `fetchResource` callback, `reuben-engine.mjs::load()`.
- **decode class** — `stage_resource <key> rejected: <reason>` surfaced from the worklet
  (`worklet.js` `errorBytes()` / `loader.mjs::stageResource`).

**Current (fatal) behavior → target (per-sample non-fatal):**

| Failure                         | Today                                                            | Target                                                                                             |
|---------------------------------|-----------------------------------------------------------------|----------------------------------------------------------------------------------------------------|
| Sample fetch 404 / net error    | `fetchResource` throws → `load()` rejects → whole rig fails      | Catch in `load()`; mark `{key, reason:"fetch"}`; **still stage the key** deferred (empty) so discovery clears the miss; record in bundle as `failed:true`. Continue. |
| Sample decode reject (bad WAV)   | `worklet.js::onLoad` posts `error`, aborts the whole load        | Don't abort: stage an **empty stub** for that key (via `kind 2`), post `staged{status:"failed"}`, continue the loop. |
| Doc (`name.json`) fetch fails    | `load()` rejects                                                 | **Unchanged — fatal** (`failed`).                                                                   |
| Text/voice resource fetch/decode | throws → rig fails                                               | **Unchanged — fatal** (a missing sub-patch is structural, not a silent pad).                        |
| ALL samples failed               | rig fails                                                        | **`failed`** (per decision 6 — a rig with every pad dead is not "ready").                           |
| Some (not all) samples failed    | n/a                                                             | **`ready-with-issues`** — rig plays; failed pads silent.                                            |

**Precise change points:**
- `reuben-engine.mjs::load()` — wrap the per-key `fetchResource` in try/catch. On a **sample**
  fetch error: push to a `failedSamples` list, ensure the discovery instance still clears the
  miss (return empty bytes AND have the loader stage it deferred — or call a dedicated
  `stage_sample_deferred` path), add a bundle entry `{key, kind:1, bytes:empty, failed:true}`.
  On a **text** fetch error: rethrow (fatal). **Livelock note:** because a failed sample is
  still staged (empty), its miss clears — the `loader.mjs` livelock guard (`:117-124`) is not
  tripped by a failed sample.
- `worklet.js::onLoad` — for a bundle entry marked `failed` (from a main-side fetch failure),
  stage it with `kind 2` (empty stub) instead of `kind 1`. For a **live** sample whose real
  decode (`kind 1`) rejects, stage an empty stub and mark `failed` **instead of** the current
  abort at `:189-192`; only text/doc rejects abort.
- After the staging loop, the worklet computes the terminal result
  (`ready` / `ready-with-issues` / `failed`) and includes `failed[]` in the `ready` reply (or a
  new `settled` field). `reuben-engine.mjs` forwards it as `onProgress({phase:"settled", …})`
  and the `load()` promise **resolves** for `ready` and `ready-with-issues` (adding
  `result` + `failed` to the resolved `{channels, inputChannels, blockSize}`), and **rejects**
  for `failed`.

**Copy (owned by `web/src/main.js`, never the engine):**
- Per failed sample row: fetch class → "couldn't download"; decode class → "couldn't decode".
- Rig `ready-with-issues` banner: "Loaded — N sound(s) couldn't load and are silent." + a
  **Retry failed** affordance that re-runs only the failed keys.
- Rig `failed`: "Couldn't load <Toy> — <first error>." + Retry (existing card).

### 2.4 Decision 2 — Eager + prefetch

- **Staging stays eager** (unchanged): `resolver.rs` requires every reference resolved before
  `construct`, and the worklet cannot `fetch` — all bytes staged before the graph is built.
- **Prefetch extension — `web/src/main.js::prefetchToy`** (currently warms only
  `instruments/<id>.json` + `schema.json`, `:110-113`): also warm the Toy's **sample bytes**.
  main.js is schema-blind, so it must not parse instrument JSON. Feed it a static manifest:
  - **`web/scripts/stage-assets.mjs`** already discovers each Toy's transitive keys (it logs
    them and writes `.pwa-precache.json`). Add one output: `public/toy-assets.json` =
    `{ "<toyId>": ["samples/…wav", …], … }` (sample keys only — `kind 1`). This cannot drift
    because it comes from the same discovery pass.
  - `prefetchToy(id)` fetches those sample URLs through `asset()` (best-effort, errors
    swallowed — same contract as today). Call it on Toy-card **hover/focus** (idle warm) in
    `launcherScreen`, in addition to the existing default-Toy prefetch at boot.
- Because prefetch only warms the **HTTP cache**, the real `load()` still runs the same eager
  discovery — it just finds the sample bytes already cached, so `fetching` is instant and the
  bar jumps to the decode phase. No second construct round, no lazy path.

### 2.5 UI — aggregate bar + per-sample rows (`web/src/main.js`)

Realises prototype **C + D**. In `buildPlayerScreen` / `openToy`:
- Replace the 3-row shimmer skeleton with a **loading panel**: an aggregate determinate
  `.bar` (fetch 0–70 % by bytes, decode 70–100 % by staged count) + a **per-sample row list**
  (name · phase dot · mini-bar), built from the miss set as samples are discovered.
- Subscribe by passing `onProgress` into `e.load(id, { onProgress })`:
  - `phase:"fetch"` → advance that sample's row to `fetching`, update its mini-bar +
    the aggregate fetch segment.
  - `phase:"decode"` → move rows to `decoding`/`ready`/`failed` per `k`/`status`; advance the
    aggregate decode segment.
  - `phase:"settled"` → swap to the terminal UI: `ready` (existing "Playing…"),
    `ready-with-issues` (banner + silent-pad rows + Retry failed), `failed` (error card).
- Keep the `loadToken` supersede guard (`:489`) so a stale load's progress can't paint over a
  newer pick.
- **Rows appear as samples are discovered:** the first `content-length`-less environment still
  shows rows (they just animate on phase, not bytes). A rig with zero samples (today's Toys)
  shows no rows and the panel collapses to the current instant-ready path.

---

## 3. Acceptance criteria (mapped to #229 AC)

**AC: "sample rigs load with visible progress, never a frozen UI."**

1. **No main-thread decode.** Loading a sample rig performs **zero** WAV decodes on the main
   thread; the UI thread is never blocked on hound. (Discovery stages deferred stubs; the only
   decode is in the worklet.) *Never a frozen UI.*
2. **Visible progress — multi-sample on a slow link.** A drum-kit rig (≥4 samples) on a
   throttled connection shows: per-sample rows transition `queued → fetching (byte %) →
   decoding → ready`, and the aggregate bar advances monotonically fetch→decode. No indefinite
   blank/spinner. *Visible progress.*
3. **One pad fails, rig still plays.** With one sample 404'd (fetch class) or corrupt (decode
   class): the rig reaches `ready-with-issues`, plays, the failed row shows "couldn't
   download/decode" + a Retry-failed affordance, and every other pad sounds. The `load()`
   promise **resolves**, not rejects.
4. **Total failure still fails cleanly.** Doc fetch failure, a missing voice/text resource, or
   **all** samples failing → `failed` card + Retry; no half-built silent rig pretending to be
   ready.
5. **Eager + prefetch.** Hovering a sample Toy warms its sample bytes; the subsequent open
   spends ~0 time in `fetching` and goes straight to `decoding`. Staging remains eager (all
   bytes present before `construct`); no lazy/second-construct path exists.
6. **Zero-sample rigs unchanged.** Today's five Toys (no samples) load exactly as now — no
   rows, instant ready — proving the change is additive.

---

## 4. Test points (TDD)

**Rust (host-testable, `crates/reuben-web`):**
- `shell.rs`: `stage_sample_deferred(key)` binds a buffer with `channel_count()==0`; a
  subsequent `construct` **clears the miss** for that key (no miss recorded) and succeeds
  (degraded — empty buffer bound, ADR-0016).
- `bridge.rs`: `stage_resource(kind=2)` returns `0` and clears a sample miss without invoking
  `decode_wav_bytes` (assert no decode error path taken on junk bytes: `kind 2` with garbage
  bytes still returns `0`, whereas `kind 1` with the same garbage returns `1`).
- Regression: existing `decode.rs` / `resolver.rs` / `shell.rs` sample tests still pass;
  `stage-assets.mjs` discovery still enumerates the identical key set.

**JS (Node / jsdom or a WASM-in-Node harness):**
- `loader.mjs`: a sample miss is staged with `kind 2` (deferred), a text miss with `kind 0`;
  the fetch-on-miss loop terminates against a real sampler doc with **no main-thread decode**.
- `reuben-engine.mjs::load`:
  - `onProgress` receives `fetch` events with monotonic `received`, then `decode` events with
    `k` 1..total, then one `settled`.
  - A single **sample** fetch 404 → resolves `ready-with-issues` with `failed:[{key,
    reason:"fetch"}]`; other samples present.
  - A **text** fetch 404 → **rejects** (fatal).
  - All samples fail → **rejects** `failed`.
- `worklet.js::onLoad` (worklet harness): a decode-rejecting sample entry posts
  `staged{status:"failed"}` and the loop **continues** to the next entry (no abort); a
  text-reject aborts.
- `main.js` (Playwright smoke, extends the existing suite): opening a stubbed multi-sample rig
  shows N rows reaching `ready`; a stubbed 404 rig lands `ready-with-issues` with a Retry-failed
  button; the aggregate bar reaches 100 % only at a terminal state; a zero-sample Toy shows no
  rows (unchanged path). Assert on `document.body.dataset` / row states via a small test hook.

---

## 5. Message-contract summary (the one new event)

| Message           | Direction        | Producer                | Consumer                       | Shape |
|-------------------|------------------|-------------------------|--------------------------------|-------|
| `staged`          | worklet → main   | `worklet.js::onLoad`    | `reuben-engine.mjs` onmessage  | `{type:"staged", k, total, key, kind, status:"ok"\|"failed"}` |
| `ready` (extended)| worklet → main   | `worklet.js::onLoad`    | `reuben-engine.mjs`            | add `result:"ready"\|"ready-with-issues"\|"failed"`, `failed:[{key,reason}]` |
| `onProgress` (cb) | main-internal    | `load()` fetch + `staged`| `web/src/main.js`             | `fetch` \| `decode` \| `settled` (see §2.2) |

Fetch progress is main-side only (streamed `content-length` reads) — it does **not** cross the
worklet boundary. Decode progress is the single new cross-boundary event.

---

_Prototype `prototype.html` is throwaway — the visual reference only; remove it when this lands._
