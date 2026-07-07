#!/usr/bin/env node
// CI gate for the reuben web player (#224): drives the real release wasm through the
// documented C-ABI lifecycle (crates/reuben-web/src/bridge.rs) exactly the way the worklet
// will — one persistent instance, destroy() between instruments — and machine-checks:
//
//   1. the module instantiates outside a browser (imports = {env: {log}} only)
//   2. registry_count() > 0 AND equals the operator count pinned by the committed schema
//      (crates/reuben-core/schema/instrument.schema.json) — the silent codegen-unit
//      ctor-drop failure class (ADR-0040 §4)
//   3. the full instrument matrix loads via the fetch-on-miss loop and renders ~2 s
//      (750 quanta) of finite audio; self-playing instruments must be non-silent;
//      mic-space exercises the duplex input path with deterministic synthetic input
//   4. the control channel changes the audio: groovebox at default tempo vs 180 BPM
//   5. bad paths fail loudly (render before construct, construct with no document,
//      malformed control buffer, garbage WAV bytes, malformed document JSON)
//
// Usage: node check.mjs   (from any cwd — paths resolve via import.meta.url)
// Prerequisite: cargo build --release --target wasm32-unknown-unknown  (in crates/reuben-web)

import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

import { encodeControl } from "./codec.mjs";
import { writeBytes, readError, loadInstrument } from "./loader.mjs";

const WASM_URL = new URL(
  "../target/wasm32-unknown-unknown/release/reuben_web.wasm",
  import.meta.url,
);
const SCHEMA_URL = new URL(
  "../../reuben-core/schema/instrument.schema.json",
  import.meta.url,
);
const INSTRUMENTS_URL = new URL("../../../instruments/", import.meta.url);

const SAMPLE_RATE = 48000;
const BLOCK = 128; // asserted against block_size() below — JS never trusts its own copy
const QUANTA = 750; // ~2 s at 128 frames / 48 kHz — enough for every clock to fire
const SILENCE_RMS = 1e-4;

// The instrument matrix (#224). SELF_PLAYING must render non-silent audio unprompted;
// NOTE_DRIVEN only has to load and render finitely (nothing plays a note in CI);
// DUPLEX gets deterministic synthetic input and must pass it through audibly.
const SELF_PLAYING = [
  "metronome",
  "vibrato",
  "sequence",
  "scale-demo",
  "sampler-arp",
  "djfilter-demo",
  "groovebox",
  "euclidean-drums",
  "granulator-demo", // bonus: exercises the second sample path (samples/testvoice.wav)
  "resonator-demo", // bonus: clock-pinged resonator, not in the issue's 20 but it plays
];
const NOTE_DRIVEN = [
  "default",
  "echo",
  "reverb",
  "autotune",
  "sampler",
  "good-button",
  "auto-filter",
  "chord-player",
  "strum-harp",
  "stereo-autopan",
  "nested-space",
];
const DUPLEX = ["mic-space"];

const decoder = new TextDecoder();
const encoder = new TextEncoder();
const failures = [];
let passes = 0;

function check(ok, label) {
  console.log(`${ok ? "PASS" : "FAIL"}  ${label}`);
  if (ok) passes += 1;
  else failures.push(label);
}

// --- instantiate the one persistent instance (the worklet's real lifecycle) -------------

let wasmBytes;
try {
  wasmBytes = await readFile(WASM_URL);
} catch {
  console.error(
    `FAIL  wasm artifact missing: ${fileURLToPath(WASM_URL)}\n` +
      "      run `cargo build --release --target wasm32-unknown-unknown` in crates/reuben-web first",
  );
  process.exit(1);
}

let mem = null;
const { instance } = await WebAssembly.instantiate(wasmBytes, {
  env: {
    log: (ptr, len) =>
      console.log(
        `  [wasm] ${mem ? decoder.decode(new Uint8Array(mem.buffer, ptr, len)) : `<log before memory export: ptr=${ptr} len=${len}>`}`,
      ),
  },
});
const ex = instance.exports;
mem = ex.memory;
// Same ctor dance as the worklet: on this toolchain lld usually synthesizes ctor calls
// into the exports, but run whichever init export exists; registry_count() proves the
// outcome either way.
if (typeof ex._initialize === "function") ex._initialize();
else if (typeof ex.__wasm_call_ctors === "function") ex.__wasm_call_ctors();
check(true, "module instantiates with imports {env: {log}}");

// --- helpers (memory-view discipline: re-wrap every view, every call) -------------------

async function fetchResource(key) {
  // Disk stand-in for `fetch(assetBase + key)`: canonical keys are root-relative
  // (e.g. "voices/kick-voice.json", "patches/space.json", "samples/testvoice.wav").
  const buf = await readFile(new URL(key, INSTRUMENTS_URL));
  return new Uint8Array(buf.buffer, buf.byteOffset, buf.byteLength);
}

function queueControlBytes(bytes) {
  const ptr = writeBytes(ex, bytes);
  const rc = ex.queue_control(ptr, bytes.length);
  ex.dealloc(ptr, bytes.length);
  return rc;
}

/// Render `quanta` quanta on the live engine; returns stats and (optionally) the full
/// planar sample stream. `writeInput(input, q, inCh)` stages synthetic input -> render(1).
function renderQuanta(quanta, { writeInput = null, capture = false } = {}) {
  const ch = ex.channels();
  const stream = capture ? new Float32Array(quanta * ch * BLOCK) : null;
  let peak = 0;
  let sumSq = 0;
  let bad = 0;
  for (let q = 0; q < quanta; q++) {
    let hasInput = 0;
    if (writeInput) {
      const inCh = ex.input_channels();
      // Re-wrap the input view every quantum — growth detaches old views.
      const input = new Float32Array(mem.buffer, ex.input_ptr(), inCh * BLOCK);
      writeInput(input, q, inCh);
      hasInput = 1;
    }
    if (ex.render(hasInput) !== 0) throw new Error(`render(${hasInput}) failed at quantum ${q}`);
    // Re-wrap the output view every quantum too.
    const out = new Float32Array(mem.buffer, ex.output_ptr(), ch * BLOCK);
    for (const s of out) {
      if (!Number.isFinite(s)) bad += 1;
      const a = Math.abs(s);
      if (a > peak) peak = a;
      sumSq += s * s;
    }
    if (stream) stream.set(out, q * ch * BLOCK);
  }
  return { peak, rms: Math.sqrt(sumSq / (quanta * ch * BLOCK)), bad, stream };
}

/// Deterministic synthetic mic: 330 Hz sine at 0.25 amplitude, phase-continuous across
/// quanta, written planar (input[ch*BLOCK + f]) like the worklet does.
function syntheticInput(input, q, inCh) {
  for (let c = 0; c < inCh; c++) {
    for (let f = 0; f < BLOCK; f++) {
      const t = q * BLOCK + f;
      input[c * BLOCK + f] = 0.25 * Math.sin((2 * Math.PI * 330 * t) / SAMPLE_RATE);
    }
  }
}

async function loadByName(name) {
  const doc = await readFile(new URL(`${name}.json`, INSTRUMENTS_URL), "utf8");
  return loadInstrument(ex, doc, SAMPLE_RATE, fetchResource);
}

// --- bad paths that must come before any construct ---------------------------------------

console.log("\n=== bad paths (pre-construct) ===");
check(ex.render(0) !== 0, "render(0) before any construct returns nonzero");
{
  const rc = ex.construct(SAMPLE_RATE);
  const err = readError(ex);
  check(
    rc === 1 && err.length > 0,
    `construct with no document returns 1 with error: "${err}"`,
  );
}

// --- registry pin (ADR-0040 §4: the silent ctor-drop failure class) ----------------------

console.log("\n=== registry pin ===");
{
  // The committed schema is generated from Registry::builtin(): $defs.node has one
  // `type` enum variant (and one allOf if/then arm) per registered operator. Counting
  // the enum pins registry_count() to the committed operator set.
  let expected = null;
  let source = "schema $defs.node.properties.type.enum";
  try {
    const schema = JSON.parse(await readFile(SCHEMA_URL, "utf8"));
    const variants = schema?.$defs?.node?.properties?.type?.enum;
    if (Array.isArray(variants) && variants.length > 0) expected = variants.length;
  } catch {
    // fall through to the hardcoded pin below
  }
  if (expected === null) {
    // LOUD FALLBACK: the schema's shape changed and the enum count above came up empty.
    // 53 tracks Registry::builtin() as of 2026-07 — if this fires, fix the schema walk
    // (and bump this number only when operators are genuinely added/removed).
    expected = 53;
    source = "hardcoded 53 (schema walk failed — tracks Registry::builtin())";
  }
  const count = ex.registry_count();
  check(
    count > 0 && count === expected,
    `registry_count() = ${count}, expected ${expected} from ${source}`,
  );
}

// --- the instrument matrix on one persistent instance -------------------------------------

console.log("\n=== instrument matrix ===");
const matrix = [
  ...SELF_PLAYING.map((n) => [n, "SELF_PLAYING"]),
  ...NOTE_DRIVEN.map((n) => [n, "NOTE_DRIVEN"]),
  ...DUPLEX.map((n) => [n, "DUPLEX"]),
];
for (const [name, cls] of matrix) {
  try {
    const info = await loadByName(name);
    if (info.blockSize !== BLOCK) {
      throw new Error(`block_size() = ${info.blockSize}, expected ${BLOCK}`);
    }
    if (info.channels < 1) throw new Error("channels() = 0 after a ready construct");
    if (cls === "DUPLEX" && info.inputChannels !== 1) {
      throw new Error(`input_channels() = ${info.inputChannels}, expected 1`);
    }
    const stats = renderQuanta(QUANTA, {
      writeInput: cls === "DUPLEX" ? syntheticInput : null,
    });
    if (stats.bad > 0) throw new Error(`${stats.bad} non-finite sample(s)`);
    if (cls !== "NOTE_DRIVEN" && stats.rms <= SILENCE_RMS) {
      throw new Error(`silent output (rms ${stats.rms.toExponential(2)})`);
    }
    check(
      true,
      `${name} [${cls}]: ${QUANTA} quanta finite, rms ${stats.rms.toFixed(5)}, peak ${stats.peak.toFixed(3)}`,
    );
  } catch (e) {
    check(false, `${name} [${cls}]: ${e.message}`);
  } finally {
    // Toy switch on the SAME instance — the worklet's real lifecycle.
    ex.destroy();
  }
}

// --- control channel: /clock/tempo must change groovebox's audio --------------------------

console.log("\n=== control channel (groovebox /clock/tempo) ===");
try {
  await loadByName("groovebox");
  const base = renderQuanta(QUANTA, { capture: true });
  ex.destroy();

  await loadByName("groovebox");
  const rc = queueControlBytes(encodeControl("/clock/tempo", [180]));
  check(rc === 0, "queue_control(/clock/tempo [180]) returns 0");
  const fast = renderQuanta(QUANTA, { capture: true });

  check(
    base.bad === 0 && fast.bad === 0,
    `both streams finite (bad: ${base.bad} + ${fast.bad})`,
  );
  check(
    base.rms > SILENCE_RMS && fast.rms > SILENCE_RMS,
    `both streams non-silent (rms ${base.rms.toFixed(5)} vs ${fast.rms.toFixed(5)})`,
  );
  let identical = base.stream.length === fast.stream.length;
  if (identical) {
    for (let i = 0; i < base.stream.length; i++) {
      if (base.stream[i] !== fast.stream[i]) {
        identical = false;
        break;
      }
    }
  }
  check(!identical, "tempo 180 stream differs from default-tempo stream");
} catch (e) {
  check(false, `control check: ${e.message}`);
} finally {
  ex.destroy();
}

// --- remaining bad paths -------------------------------------------------------------------

console.log("\n=== bad paths (post-construct) ===");
try {
  await loadInstrument(ex, "{ not json", SAMPLE_RATE, fetchResource);
  check(false, "malformed document text was accepted");
} catch (e) {
  check(
    e.message.length > 0,
    `malformed document fails construct with error: "${e.message}"`,
  );
}
ex.destroy();

{
  // Garbage bytes staged as kind 1 (WAV) must be rejected at stage time.
  const key = encoder.encode("samples/garbage.wav");
  const data = new Uint8Array([0xde, 0xad, 0xbe, 0xef, 1, 2, 3]);
  const keyPtr = writeBytes(ex, key);
  const dataPtr = writeBytes(ex, data);
  const rc = ex.stage_resource(keyPtr, key.length, 1, dataPtr, data.length);
  ex.dealloc(keyPtr, key.length);
  ex.dealloc(dataPtr, data.length);
  check(rc === 1, `stage_resource(kind 1, garbage bytes) returns 1: "${readError(ex)}"`);
}
ex.destroy();

try {
  await loadByName("default");
  check(
    queueControlBytes(new Uint8Array([1, 2, 3])) === 1,
    "malformed control buffer [1,2,3] returns 1 after a successful construct",
  );
} catch (e) {
  check(false, `malformed-control setup: ${e.message}`);
} finally {
  ex.destroy();
}

// --- summary --------------------------------------------------------------------------------

console.log("");
if (failures.length === 0) {
  console.log(`${passes} checks passed`);
  process.exit(0);
}
console.log(`${failures.length} of ${passes + failures.length} check(s) FAILED:`);
for (const f of failures) console.log(`  - ${f}`);
process.exit(1);
