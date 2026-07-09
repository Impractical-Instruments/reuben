// strum-harp control-message rate + per-message cost harness.
//
// Answers issue #252 (P7/A1): under a hard continuous drag on strum-harp's strum bar,
// how many control messages/sec does the postMessage channel carry, and what does each
// cost? The number gates the SharedArrayBuffer control ring (#257 / P7/A2).
//
// Why this is faithful: the auto-UI fader (crates/reuben-web/js/surface/render.mjs,
// buildFader) sends exactly ONE control message per DOM `input` event, with no throttle.
// So control-message rate === the `input`-event rate of the real widget
// (<input type=range min=0 max=1 step=0.001>). The strum op's per-string note plucks
// happen INSIDE the engine (signal->event), not as extra control messages, so they don't
// change the channel rate. This harness therefore measures the input-event rate directly,
// without booting the WASM/AudioContext stack, and microbenches the real encodeControl
// (crates/reuben-web/js/codec.mjs) plus a transferable postMessage round-trip.
//
// Run:  CHROMIUM_PATH=/path/to/chrome node web/bench/strum-message-rate/measure.mjs
//       (needs `npm i playwright`; CHROMIUM_PATH optional — falls back to Playwright's own)
import { chromium } from "playwright";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, "../../..");
const codecSrc = readFileSync(`${REPO}/crates/reuben-web/js/codec.mjs`, "utf8")
  .replace(/^export /gm, ""); // inject the REAL encoder into page scope

const launch = process.env.CHROMIUM_PATH ? { executablePath: process.env.CHROMIUM_PATH } : {};
const browser = await chromium.launch(launch);
const page = await browser.newPage({ viewport: { width: 500, height: 400 } });

await page.setContent(`<!doctype html><meta charset=utf8>
  <input id="f" type="range" min="0" max="1" step="0.001" style="width:400px;height:40px">`);
await page.evaluate(codecSrc);
await page.evaluate(() => {
  window.__ts = [];
  window.__frames = 0;
  const tick = () => { window.__frames++; requestAnimationFrame(tick); };
  requestAnimationFrame(tick);
  // Mirror buildFader's wiring: one send per `input` event (we record its timestamp).
  document.getElementById("f").addEventListener("input", () => window.__ts.push(performance.now()));
});

const box = await page.locator("#f").boundingBox();
const y = box.y + box.height / 2, x0 = box.x + 2, x1 = box.x + box.width - 2;
const cdp = await page.context().newCDPSession(page);
let dir = 1, x = x0, inflight = 0;
function fireOne() {
  x += dir * 8;
  if (x >= x1) { x = x1; dir = -1; } else if (x <= x0) { x = x0; dir = 1; }
  inflight++;
  cdp.send("Input.dispatchMouseEvent", { type: "mouseMoved", x, y, button: "left", buttons: 1 })
    .catch(() => {}).finally(() => { inflight--; });
}

// Drive a hard continuous back-and-forth drag for WINDOW_MS at a target average move
// cadence (un-awaited/pipelined so CDP round-trip latency doesn't cap us). targetHz=0 =>
// fire as fast as the pipe allows (absolute synthetic ceiling).
async function driveAt(targetHz, label) {
  await page.evaluate(() => { window.__ts = []; window.__frames = 0; });
  await cdp.send("Input.dispatchMouseEvent", { type: "mousePressed", x: x0, y, button: "left", buttons: 1, clickCount: 1 });
  const WINDOW_MS = 2000, t0 = Date.now();
  let moves = 0;
  while (Date.now() - t0 < WINDOW_MS) {
    const due = targetHz ? Math.floor((targetHz * (Date.now() - t0)) / 1000) : Infinity;
    if ((targetHz ? moves < due : true) && inflight < 128) { fireOne(); moves++; }
    else await new Promise((r) => setTimeout(r, 0));
  }
  await cdp.send("Input.dispatchMouseEvent", { type: "mouseReleased", x, y, button: "left", buttons: 0, clickCount: 1 });
  const elapsed = (Date.now() - t0) / 1000;
  const { ts, frames } = await page.evaluate(() => ({ ts: window.__ts, frames: window.__frames }));
  let peak = 0;
  for (let i = 0; i < ts.length; i++) {
    let j = i; while (j < ts.length && ts[j] - ts[i] <= 100) j++;
    peak = Math.max(peak, (j - i) * 10); // busiest 100ms window -> /s
  }
  return {
    label, target_move_hz: targetHz || "unbounded",
    actual_move_hz: +(moves / elapsed).toFixed(0),
    rAF_hz: +(frames / elapsed).toFixed(0),
    sustained_msg_per_s: +(ts.length / elapsed).toFixed(0),
    peak_msg_per_s: peak,
  };
}

const sweep = [];
for (const [hz, label] of [
  [125, "typical mouse ~125Hz"],
  [120, "hi-refresh touch ~120Hz"],
  [500, "fast gaming mouse ~500Hz"],
  [1000, "1000Hz gaming mouse"],
  [0, "unbounded (synthetic ceiling)"],
]) sweep.push(await driveAt(hz, label));

// Per-message JS cost: real encodeControl + a transferable postMessage round-trip.
const cost = await page.evaluate(async () => {
  const N = 200000;
  let t = performance.now(), bytes = 0;
  for (let i = 0; i < N; i++) bytes += encodeControl("/strum/position", [i / N]).length;
  const encodeNs = ((performance.now() - t) * 1e6) / N;
  const url = URL.createObjectURL(new Blob(
    [`onmessage=e=>{const b=new Uint8Array(e.data);let s=0;for(let k=0;k<b.length;k++)s+=b[k];postMessage(s)}`],
    { type: "text/javascript" }));
  const w = new Worker(url), M = 20000;
  await new Promise((res) => {
    let got = 0; w.onmessage = () => { if (++got === M) res(); };
    const t2 = performance.now();
    for (let i = 0; i < M; i++) { const buf = encodeControl("/strum/position", [i / M]); w.postMessage(buf.buffer, [buf.buffer]); }
    window.__postMs = performance.now() - t2;
  });
  w.terminate();
  return { encodeNs, encodedBytes: bytes / N, postSendUs: (window.__postMs * 1000) / M };
});

console.log(JSON.stringify({ drag_rate_sweep: sweep, per_message_cost: {
  encode_ns: +cost.encodeNs.toFixed(1), encoded_bytes: cost.encodedBytes, post_send_us: +cost.postSendUs.toFixed(2),
} }, null, 2));
await browser.close();
