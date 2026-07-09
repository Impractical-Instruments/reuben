// Web-player cold-load + startup-profile harness (P7/C, issue #254).
//
// Answers #254 (and the "documented perf numbers, before/after" AC of #229): how long does
// the reuben web player take to go from navigation → engine ready → first-audio-ready, and
// WHAT dominates that cold load — network fetch, WASM compile, WASM instantiate, worklet
// module load, resource discovery (loader.mjs fetch-on-miss + construct), or sample decode?
//
// Faithful by construction (the strum-message-rate harness's stance — measure the REAL code):
// it imports the REAL crates/reuben-web/js/reuben-engine.mjs + loader.mjs and drives the REAL
// release wasm + the REAL staged payload (web/dist, produced by `npm run build`). Two layers:
//
//   1. PRIMITIVE ATTRIBUTION — each cold-load primitive timed in isolation against the real
//      artifact: fetch(wasm), WebAssembly.compile, WebAssembly.instantiate (+ctor dance),
//      audioWorklet.addModule, and a discovery load split into its network (fetch docs) vs
//      CPU (construct/stage/decode) halves. This is where the "what dominates" answer lives.
//   2. END-TO-END — the real createReubenEngine() + engine.load(defaultToy) + context.resume(),
//      wall-clocked, for the top-line "navigation → first-audio-ready" number a user feels.
//
// Both layers run under three CPU/network conditions via CDP: baseline (dev host), and an
// EMULATED low-end-phone proxy at 4x and 6x CPU throttle + a slow-network profile. The
// emulated rows are a PROXY, clearly labelled — the REAL low-end-device number is a HITL leg
// left for a human (see README "Still needs a real device").
//
// Sample decode: NO bundled Toy carries a .wav (the five in web/toys.json synthesize their
// audio), so decode is off the current critical path. To characterize it anyway (a future
// sample-carrying Toy would pay it), a separate pass loads `granulator-demo` (testvoice.wav,
// ~614 KB) straight from the repo's instruments/ and times its construct.
//
// Run:  cd web && npm run build            # produce dist/ (needs the release wasm staged)
//       CHROMIUM_PATH=/path/to/chrome node bench/perf-baseline/measure.mjs
//       (needs playwright resolvable — symlink one next to this file while running)

import { createServer } from "node:http";
import { readFile, stat } from "node:fs/promises";
import { createReadStream } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join, resolve, extname } from "node:path";
import { gzipSync, brotliCompressSync, constants as zc } from "node:zlib";
import { chromium } from "playwright";

const HERE = dirname(fileURLToPath(import.meta.url)); // web/bench/perf-baseline
const WEB = resolve(HERE, "../..");                   // web
const ROOT = resolve(WEB, "..");                      // repo root
const DIST = join(WEB, "dist");
const ENGINE_JS = join(ROOT, "crates", "reuben-web", "js");
const RAW_INSTRUMENTS = join(ROOT, "instruments");

const DEFAULT_TOY = "groovebox"; // web/toys.json `default`
const SAMPLE_INSTRUMENT = "granulator-demo"; // decode characterization (not a bundled Toy)

// --- static server: mirrors the deploy (brotli Content-Encoding on compressible types, so a
//     CDP network throttle sees production-like on-wire bytes; application/wasm; long routes). --
const MIME = {
  ".html": "text/html", ".mjs": "text/javascript", ".js": "text/javascript",
  ".json": "application/json", ".wasm": "application/wasm", ".css": "text/css",
  ".wav": "audio/wav", ".png": "image/png",
};
const COMPRESSIBLE = new Set([".html", ".mjs", ".js", ".json", ".wasm", ".css"]);

// Brotli-q11 on the 745 KB wasm costs ~2 s — compressing per request would masquerade as
// fetch latency. Compress ONCE per (path, encoding) and cache, so the response is served from
// memory and the CDP network throttle is the only thing pacing the wire.
const encCache = new Map();
function encoded(path, raw, enc) {
  const k = `${enc}:${path}`;
  let hit = encCache.get(k);
  if (!hit) {
    hit = enc === "br"
      ? brotliCompressSync(raw, { params: { [zc.BROTLI_PARAM_QUALITY]: 11 } })
      : gzipSync(raw, { level: 9 });
    encCache.set(k, hit);
  }
  return hit;
}

// Route table: URL prefix -> on-disk root. First match wins.
const ROUTES = [
  ["/engine/", ENGINE_JS],           // the real ES modules (source of truth, ADR-0041)
  ["/instruments/", join(DIST, "instruments")], // staged bundled-Toy payload
  ["/raw-instruments/", RAW_INSTRUMENTS],        // repo instruments (decode characterization)
  ["/", DIST],                        // wasm, schema.json, index.html, app assets
];

function resolvePath(urlPath) {
  for (const [prefix, root] of ROUTES) {
    if (urlPath.startsWith(prefix)) {
      const rel = urlPath.slice(prefix.length);
      return join(root, rel);
    }
  }
  return null;
}

const HARNESS_HTML = `<!doctype html><meta charset=utf8><title>perf-baseline</title>
<body><script type="module">
import { createReubenEngine } from "/engine/reuben-engine.mjs";
import { loadInstrument } from "/engine/loader.mjs";
const T = () => performance.now();
const ctorDance = (ex) => { if (typeof ex._initialize==="function") ex._initialize();
  else if (typeof ex.__wasm_call_ctors==="function") ex.__wasm_call_ctors(); };

// One clean instantiate for the primitive attribution (isolated from the real API's own).
async function primitives(toy, assetBase) {
  const m = {};
  let t = T();
  const bytes = await (await fetch("/reuben_web.wasm")).arrayBuffer();
  m.fetch_wasm_ms = T() - t; m.wasm_bytes = bytes.byteLength;
  t = T(); const mod = await WebAssembly.compile(bytes); m.compile_ms = T() - t;
  t = T(); const inst = await WebAssembly.instantiate(mod, { env: { log: () => {} } });
  ctorDance(inst.exports); m.instantiate_ms = T() - t;
  m.registry = inst.exports.registry_count ? inst.exports.registry_count() : null;
  const ctx = new (globalThis.AudioContext || globalThis.webkitAudioContext)();
  t = T(); await ctx.audioWorklet.addModule("/engine/worklet.js"); m.addmodule_ms = T() - t;

  // Discovery load on a FRESH instance: split network (fetch docs) vs CPU (construct/stage/decode).
  const disc = (await WebAssembly.instantiate(mod, { env: { log: () => {} } })).exports;
  ctorDance(disc);
  const docText = await (await fetch(assetBase + "/" + toy + ".json")).text();
  let fetchMs = 0, nFetched = 0;
  t = T();
  await loadInstrument(disc, docText, ctx.sampleRate, async (key) => {
    const ft = T();
    const b = new Uint8Array(await (await fetch(assetBase + "/" + key)).arrayBuffer());
    fetchMs += T() - ft; nFetched++;
    return b;
  });
  m.discovery_total_ms = T() - t;
  m.discovery_fetch_ms = fetchMs;
  m.discovery_cpu_ms = m.discovery_total_ms - fetchMs; // construct + stage + decode
  m.discovery_resources = nFetched;
  disc.destroy();
  await ctx.close();
  return m;
}

// End-to-end via the REAL public API — the number a user feels.
async function endToEnd(toy) {
  const m = {};
  let t = T();
  const engine = await createReubenEngine({
    assetBase: "/instruments", wasmUrl: "/reuben_web.wasm", workletUrl: "/engine/worklet.js",
  });
  m.engine_create_ms = T() - t;
  t = T(); const info = await engine.load(toy); m.load_ms = T() - t;
  t = T(); await engine.context.resume(); m.resume_ms = T() - t;
  m.context_state = engine.context.state;
  m.total_ms = m.engine_create_ms + m.load_ms + m.resume_ms;
  m.channels = info.channels; m.block = info.blockSize;
  engine.destroy();
  return m;
}

// Sample-decode characterization: load a wav-carrying instrument straight from repo instruments/.
async function sampleDecode(name) {
  const bytes = await (await fetch("/reuben_web.wasm")).arrayBuffer();
  const mod = await WebAssembly.compile(bytes);
  const ex = (await WebAssembly.instantiate(mod, { env: { log: () => {} } })).exports;
  ctorDance(ex);
  const ctx = new (globalThis.AudioContext || globalThis.webkitAudioContext)();
  const docText = await (await fetch("/raw-instruments/" + name + ".json")).text();
  let fetchMs = 0, sampleBytes = 0;
  const t = T();
  await loadInstrument(ex, docText, ctx.sampleRate, async (key) => {
    const ft = T();
    const b = new Uint8Array(await (await fetch("/raw-instruments/" + key).catch(() => null).then(r => r)).arrayBuffer());
    fetchMs += T() - ft; if (key.endsWith(".wav")) sampleBytes += b.length;
    return b;
  });
  const total = T() - t;
  ex.destroy(); await ctx.close();
  return { total_ms: total, fetch_ms: fetchMs, cpu_ms: total - fetchMs, sample_bytes: sampleBytes };
}

window.__measure = async () => ({
  primitives: await primitives("groovebox", "/instruments"),
  e2e: await endToEnd("groovebox"),
  sample_decode: await sampleDecode("granulator-demo"),
});
</script></body>`;

const server = createServer(async (req, res) => {
  const urlPath = decodeURIComponent(req.url.split("?")[0]);
  if (urlPath === "/harness.html") {
    res.writeHead(200, { "content-type": "text/html" });
    res.end(HARNESS_HTML);
    return;
  }
  const path = resolvePath(urlPath);
  if (!path) { res.writeHead(404).end(); return; }
  try {
    const info = await stat(path);
    if (!info.isFile()) { res.writeHead(404).end(); return; }
    const ext = extname(path);
    const headers = { "content-type": MIME[ext] || "application/octet-stream" };
    const accept = req.headers["accept-encoding"] || "";
    if (COMPRESSIBLE.has(ext)) {
      const raw = await readFile(path);
      let body = raw, enc = null;
      if (/\bbr\b/.test(accept)) { body = encoded(path, raw, "br"); enc = "br"; }
      else if (/\bgzip\b/.test(accept)) { body = encoded(path, raw, "gzip"); enc = "gzip"; }
      if (enc) headers["content-encoding"] = enc;
      headers["content-length"] = body.length;
      res.writeHead(200, headers);
      res.end(body);
    } else {
      headers["content-length"] = info.size;
      res.writeHead(200, headers);
      createReadStream(path).pipe(res);
    }
  } catch {
    res.writeHead(404).end();
  }
});

// Warm the brotli cache for the heavy assets up front so no measured request pays the
// one-time compression cost as phantom fetch latency.
for (const f of ["reuben_web.wasm", "schema.json"]) {
  encoded(join(DIST, f), await readFile(join(DIST, f)), "br");
}

await new Promise((r) => server.listen(0, r));
const PORT = server.address().port;
const BASE = `http://127.0.0.1:${PORT}`;

const launch = {
  args: ["--autoplay-policy=no-user-gesture-required"],
  ...(process.env.CHROMIUM_PATH ? { executablePath: process.env.CHROMIUM_PATH } : {}),
};
const browser = await chromium.launch(launch);

// CDP network profiles (bytes-on-wire are the brotli-encoded responses this server sends).
const NET = {
  none: { offline: false, latency: 0, downloadThroughput: -1, uploadThroughput: -1 },
  // "Slow 4G"-ish low-end mobile: ~1.6 Mbps down, 150 ms RTT. A proxy, not a measured link.
  slow4g: { offline: false, latency: 150, downloadThroughput: (1.6 * 1024 * 1024) / 8, uploadThroughput: (0.75 * 1024 * 1024) / 8 },
};

async function runScenario(label, cpuRate, netKey) {
  const context = await browser.newContext({ bypassCSP: true });
  const page = await context.newPage();
  const cdp = await context.newCDPSession(page);
  await cdp.send("Network.enable");
  await cdp.send("Network.emulateNetworkConditions", NET[netKey]);
  if (cpuRate > 1) await cdp.send("Emulation.setCPUThrottlingRate", { rate: cpuRate });
  const runs = [];
  for (let i = 0; i < 3; i++) {
    await page.goto(`${BASE}/harness.html`, { waitUntil: "load" });
    const r = await page.evaluate(() => window.__measure());
    runs.push(r);
  }
  await context.close();
  // Median across 3 cold runs (each a fresh context = cold cache).
  const med = (arr) => arr.slice().sort((a, b) => a - b)[Math.floor(arr.length / 2)];
  const pick = (path) => med(runs.map((r) => path.split(".").reduce((o, k) => o?.[k], r)));
  return {
    label, cpu_throttle: cpuRate === 1 ? "1x (none)" : `${cpuRate}x (emulated)`, network: netKey,
    primitives: {
      fetch_wasm_ms: +pick("primitives.fetch_wasm_ms").toFixed(1),
      compile_ms: +pick("primitives.compile_ms").toFixed(1),
      instantiate_ms: +pick("primitives.instantiate_ms").toFixed(1),
      addmodule_ms: +pick("primitives.addmodule_ms").toFixed(1),
      discovery_total_ms: +pick("primitives.discovery_total_ms").toFixed(1),
      discovery_fetch_ms: +pick("primitives.discovery_fetch_ms").toFixed(1),
      discovery_cpu_ms: +pick("primitives.discovery_cpu_ms").toFixed(1),
      wasm_bytes: runs[0].primitives.wasm_bytes,
      registry: runs[0].primitives.registry,
      discovery_resources: runs[0].primitives.discovery_resources,
    },
    e2e: {
      engine_create_ms: +pick("e2e.engine_create_ms").toFixed(1),
      load_ms: +pick("e2e.load_ms").toFixed(1),
      resume_ms: +pick("e2e.resume_ms").toFixed(1),
      total_ms: +pick("e2e.total_ms").toFixed(1),
      context_state: runs[0].e2e.context_state,
    },
    sample_decode: {
      total_ms: +pick("sample_decode.total_ms").toFixed(1),
      cpu_ms: +pick("sample_decode.cpu_ms").toFixed(1),
      fetch_ms: +pick("sample_decode.fetch_ms").toFixed(1),
      sample_bytes: runs[0].sample_decode.sample_bytes,
    },
  };
}

const results = [];
results.push(await runScenario("baseline (dev host, local network)", 1, "none"));
results.push(await runScenario("emulated low-end phone — 4x CPU", 4, "none"));
results.push(await runScenario("emulated low-end phone — 6x CPU", 6, "none"));
results.push(await runScenario("emulated low-end phone — 6x CPU + slow-4G", 6, "slow4g"));

console.log(JSON.stringify({ chromium: "pw-1194", results }, null, 2));

await browser.close();
server.close();
