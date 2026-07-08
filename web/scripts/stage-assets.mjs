#!/usr/bin/env node
// Asset staging for the reuben web player (issue #226, scope item 3). The build step that
// turns the workspace into the app's payload — run by `npm run stage` (a prerequisite of
// `dev` and `build`). It:
//
//   1. builds the release wasm (`cargo build --release --target wasm32-unknown-unknown` in
//      the detached crates/reuben-web crate) and copies reuben_web.wasm into public/;
//   2. drives the engine's OWN fetch-on-miss discovery (crates/reuben-web/js/loader.mjs,
//      the exact loop the browser runs) over each manifest Toy to enumerate its TRANSITIVE
//      resource keys, then copies exactly that set — no more — into public/instruments/;
//   3. copies the instrument schema (fader ranges/units for the surface) into public/.
//
// The discovery in step 2 is why this is a script and not a static file list: it asks the
// real engine what each Toy references, so the payload is exactly the Toy docs + the voices/
// subpatches + samples/blip.wav they actually pull, and nothing else (smallest payload,
// the AC). A MISSING asset FAILS the build loudly (fetchResource throws ENOENT → non-zero
// exit) — no silent drift between the manifest and what ships.
//
// Set STAGE_SKIP_WASM=1 to reuse an already-built reuben_web.wasm (fast local iteration /
// Playwright reruns); the build fails if that env is set but no artifact exists.

import { spawnSync } from "node:child_process";
import { access, copyFile, mkdir, readFile, rm } from "node:fs/promises";
import { constants } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { loadInstrument } from "../../crates/reuben-web/js/loader.mjs";

const HERE = dirname(fileURLToPath(import.meta.url)); // web/scripts
const WEB = resolve(HERE, ".."); // web
const ROOT = resolve(WEB, ".."); // repo root
const CRATE = join(ROOT, "crates", "reuben-web");
const WASM = join(CRATE, "target", "wasm32-unknown-unknown", "release", "reuben_web.wasm");
const INSTRUMENTS = join(ROOT, "instruments");
const SCHEMA = join(ROOT, "crates", "reuben-core", "schema", "instrument.schema.json");
const PUBLIC = join(WEB, "public");
const OUT_INSTRUMENTS = join(PUBLIC, "instruments");

const SAMPLE_RATE = 48000; // any rate enumerates the same resource set; construct() only needs one

async function exists(p) {
  try {
    await access(p, constants.R_OK);
    return true;
  } catch {
    return false;
  }
}

// --- 1. build the release wasm (or reuse it under STAGE_SKIP_WASM) ------------------------

function buildWasm() {
  if (process.env.STAGE_SKIP_WASM === "1") {
    console.log("[stage] STAGE_SKIP_WASM=1 — reusing the existing wasm artifact");
    return;
  }
  console.log("[stage] building release wasm (cargo, wasm32-unknown-unknown)…");
  const r = spawnSync(
    "cargo",
    ["build", "--release", "--target", "wasm32-unknown-unknown"],
    { cwd: CRATE, stdio: "inherit" },
  );
  if (r.status !== 0) {
    throw new Error(`cargo wasm build failed (exit ${r.status ?? r.signal})`);
  }
}

// --- 2. discovery: enumerate + copy each Toy's transitive resource set --------------------

// Instantiate the freshly built wasm the same way check.mjs / the worklet do: imports are
// {env: {log}} only, then the ctor dance. This instance runs the discovery loop below.
async function instantiate() {
  const bytes = await readFile(WASM);
  let mem = null;
  const { instance } = await WebAssembly.instantiate(bytes, {
    env: {
      log: (ptr, len) => {
        if (!mem) return;
        console.log(`  [wasm] ${new TextDecoder().decode(new Uint8Array(mem.buffer, ptr, len))}`);
      },
    },
  });
  const ex = instance.exports;
  mem = ex.memory;
  if (typeof ex._initialize === "function") ex._initialize();
  else if (typeof ex.__wasm_call_ctors === "function") ex.__wasm_call_ctors();
  return ex;
}

// Copy one root-relative key (e.g. "voices/kick-voice.json", "samples/blip.wav") from the
// repo's instruments/ into public/instruments/, creating sub-dirs. Missing source throws.
const copied = new Set();
async function stageKey(key) {
  if (copied.has(key)) return;
  const src = join(INSTRUMENTS, key);
  if (!(await exists(src))) {
    throw new Error(`missing asset: instruments/${key} (referenced by a Toy but not on disk)`);
  }
  const dest = join(OUT_INSTRUMENTS, key);
  await mkdir(dirname(dest), { recursive: true });
  await copyFile(src, dest);
  copied.add(key);
}

async function main() {
  const manifest = JSON.parse(await readFile(join(WEB, "toys.json"), "utf8"));
  const toys = manifest.toys ?? [];
  if (toys.length === 0) throw new Error("toys.json lists no Toys");

  buildWasm();
  if (!(await exists(WASM))) {
    throw new Error(
      `wasm artifact missing: ${WASM}\n` +
        "  run without STAGE_SKIP_WASM, or build it first in crates/reuben-web",
    );
  }

  // Fresh public/instruments each run so a Toy removed from the manifest can't leave a stale
  // asset behind (public/ is gitignored and fully build-owned).
  await rm(OUT_INSTRUMENTS, { recursive: true, force: true });
  await mkdir(OUT_INSTRUMENTS, { recursive: true });

  await copyFile(WASM, join(PUBLIC, "reuben_web.wasm"));
  await copyFile(SCHEMA, join(PUBLIC, "schema.json"));

  const ex = await instantiate();
  for (const toy of toys) {
    const docPath = join(INSTRUMENTS, `${toy.id}.json`);
    if (!(await exists(docPath))) {
      throw new Error(`missing Toy document: instruments/${toy.id}.json (id "${toy.id}" in toys.json)`);
    }
    const docText = await readFile(docPath, "utf8");
    // Discovery: loadInstrument replays construct()→miss→stage until ready; every miss it
    // reports is fetched (here: read from disk) and simultaneously staged into public/. What
    // reaches public/ is therefore exactly what the engine needed to construct the Toy.
    const transitive = [];
    await loadInstrument(ex, docText, SAMPLE_RATE, async (key) => {
      transitive.push(key);
      await stageKey(key);
      return new Uint8Array(await readFile(join(INSTRUMENTS, key)));
    });
    ex.destroy(); // toy-switch lifecycle: drop this Toy's engine, keep the instance
    // Copy the top-level document itself (the loader reads it from memory, so it isn't a miss).
    await copyFile(docPath, join(OUT_INSTRUMENTS, `${toy.id}.json`));
    console.log(
      `[stage] ${toy.id}: doc + ${transitive.length} resource(s)` +
        (transitive.length ? ` (${transitive.join(", ")})` : ""),
    );
  }

  console.log(
    `[stage] done — ${toys.length} Toys, ${copied.size} unique resource(s), wasm + schema → web/public/`,
  );
}

main().catch((err) => {
  console.error(`[stage] FAILED: ${err.message}`);
  process.exit(1);
});
