// Headless verification for the #223 spike — the checkpoints a CI-less throwaway can
// still machine-check. Runs the exact wasm binary the browser gets, mirroring the
// worklet's init order (instantiate → ctors → init → render):
//
//   1. module instantiates outside a browser (checkpoint 1's artifact really links)
//   2. registry_count() > 0 — the inventory-on-WASM verdict (checkpoint 2)
//   3. init(48000, id) == 0 for vibrato AND sequence (load + Plan::instantiate work)
//   4. rendered audio is non-silent, in-range, and free of NaN/inf
//
// The audible half (checkpoints 3–5: real browsers, real iPhone, headroom + listening
// verdict) is the manual protocol in the README.
//
// Usage: node node-check.mjs [path/to/spike.wasm]

import { readFileSync } from "node:fs";

const wasmPath =
  process.argv[2] ??
  new URL(
    "./target/wasm32-unknown-unknown/release/reuben_wasm_worklet_spike.wasm",
    import.meta.url,
  ).pathname;

const BLOCK = 128;
const CHANNELS = 2;
const decoder = new TextDecoder();
let failures = 0;

function check(ok, label) {
  console.log(`${ok ? "PASS" : "FAIL"}  ${label}`);
  if (!ok) failures++;
}

async function freshInstance() {
  let mem = null;
  const { instance } = await WebAssembly.instantiate(readFileSync(wasmPath), {
    env: {
      log: (ptr, len) =>
        console.log(`  [wasm] ${decoder.decode(new Uint8Array(mem.buffer, ptr, len))}`),
    },
  });
  const ex = instance.exports;
  mem = ex.memory;
  // Same ctor dance as worklet.js — on this toolchain neither export exists (lld
  // synthesizes ctor calls into the exports); registry_count() proves it either way.
  if (typeof ex._initialize === "function") ex._initialize();
  else if (typeof ex.__wasm_call_ctors === "function") ex.__wasm_call_ctors();
  return ex;
}

function renderStats(ex, blocks) {
  const outPtr = ex.output_ptr();
  let peak = 0;
  let sumSq = 0;
  let badSamples = 0;
  for (let b = 0; b < blocks; b++) {
    if (ex.render() !== 0) throw new Error(`render() failed at block ${b}`);
    const out = new Float32Array(ex.memory.buffer, outPtr, CHANNELS * BLOCK);
    for (const s of out) {
      if (!Number.isFinite(s) || Math.abs(s) > 4) badSamples++;
      peak = Math.max(peak, Math.abs(s));
      sumSq += s * s;
    }
  }
  return { peak, rms: Math.sqrt(sumSq / (blocks * CHANNELS * BLOCK)), badSamples };
}

for (const [id, name] of [
  [0, "vibrato (the gate)"],
  [1, "sequence (the stretch)"],
]) {
  console.log(`\n=== instrument ${id}: ${name} ===`);
  const ex = await freshInstance();
  const count = ex.registry_count();
  check(count > 0, `registry_count() = ${count} (inventory ctors ran in WASM)`);
  const status = ex.init(48000, id);
  if (status !== 0) {
    const reason = decoder.decode(
      new Uint8Array(ex.memory.buffer, ex.error_ptr(), ex.error_len()),
    );
    check(false, `init() = ${status}: ${reason}`);
    continue;
  }
  check(true, "init(48000) = 0");
  // ~2 s of audio: enough for sequence's clock to fire several beats.
  const stats = renderStats(ex, 750);
  check(
    stats.badSamples === 0,
    `all samples finite and in range (bad: ${stats.badSamples})`,
  );
  check(
    stats.rms > 1e-4,
    `output is non-silent (rms ${stats.rms.toFixed(5)}, peak ${stats.peak.toFixed(3)})`,
  );
}

// Bad-path coverage: an unknown instrument id must fail loudly, not trap.
console.log("\n=== bad instrument id ===");
{
  const ex = await freshInstance();
  const status = ex.init(48000, 99);
  const reason = decoder.decode(
    new Uint8Array(ex.memory.buffer, ex.error_ptr(), ex.error_len()),
  );
  check(status !== 0 && reason.length > 0, `init(_, 99) fails with reason: "${reason}"`);
  check(ex.render() !== 0, "render() before successful init reports failure");
}

console.log(failures === 0 ? "\nAll checks passed." : `\n${failures} check(s) FAILED.`);
process.exit(failures === 0 ? 0 : 1);
