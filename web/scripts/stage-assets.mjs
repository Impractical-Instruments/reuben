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
//   3. copies the instrument surface docs (surfaces/*.json, ADR-0043 presentation layer)
//      into public/surfaces/ so main.js's resolution order (surfaces/<id>.web.json ??
//      surfaces/<id>.json ?? auto-derived) finds whatever exists for a Toy.
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
import { access, copyFile, mkdir, readdir, readFile, rm, writeFile } from "node:fs/promises";
import { constants } from "node:fs";
import { createHash } from "node:crypto";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { loadInstrument } from "../../crates/reuben-web/js/loader.mjs";

const HERE = dirname(fileURLToPath(import.meta.url)); // web/scripts
const WEB = resolve(HERE, ".."); // web
const ROOT = resolve(WEB, ".."); // repo root

// Staging roots (issue #416). Each is env-overridable so the private repo — which consumes this
// repo as a git submodule at engine/ (epic #414) — can point them at engine/crates/reuben-web,
// engine/instruments, and engine/surfaces while THIS monorepo build stays byte-for-byte unchanged
// when they're unset. A relative override resolves against the repo root; an absolute one is used
// verbatim.
const stagingRoot = (name, ...defaults) =>
  process.env[name] ? resolve(ROOT, process.env[name]) : join(ROOT, ...defaults);
const CRATE = stagingRoot("CRATE", "crates", "reuben-web");
const WASM = join(CRATE, "target", "wasm32-unknown-unknown", "release", "reuben_web.wasm");
const INSTRUMENTS = stagingRoot("INSTRUMENTS", "instruments");
const SURFACES = stagingRoot("SURFACES", "surfaces");
// The master PWA icon (issue #227 human prerequisite): a ≥512×512 square committed by a human.
// @vite-pwa/assets-generator derives every icon size from it, but it resolves its output dir
// relative to Vite's publicDir — so the source must live UNDER public/ or the generated icons
// escape dist. We copy the committed master (src/assets/icon.png) into public/ here so the
// generator (pwaAssets.image: 'public/icon.png' in vite.config.js) writes the derived sizes
// straight into the built payload. public/ is build-owned + gitignored, so the copy is transient.
const MASTER_ICON = join(WEB, "src", "assets", "icon.png");
const PUBLIC = join(WEB, "public");
const OUT_INSTRUMENTS = join(PUBLIC, "instruments");
const OUT_SURFACES = join(PUBLIC, "surfaces");
// Where the offline-precache list is written (issue #227, scope item 2). vite.config.js reads
// this at build time and hands it to Workbox as `additionalManifestEntries`, so the SW precache
// is generated from THIS script's transitive discovery — it cannot drift from what ships. Lives
// outside public/ (build-owned, gitignored) so the SW never tries to precache its own list.
const PRECACHE_MANIFEST = join(WEB, ".pwa-precache.json");

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

  // Fresh public/instruments + public/surfaces each run so a Toy removed from the manifest (or
  // a deleted surface doc) can't leave a stale asset behind (public/ is gitignored, build-owned).
  await rm(OUT_INSTRUMENTS, { recursive: true, force: true });
  await mkdir(OUT_INSTRUMENTS, { recursive: true });
  await rm(OUT_SURFACES, { recursive: true, force: true });
  await mkdir(OUT_SURFACES, { recursive: true });
  // schema.json is no longer part of the payload (ADR-0043 §4: the pipes carry the contract, so
  // the player never fetches it) — scrub any copy left by a pre-#247 build so it can't ship.
  await rm(join(PUBLIC, "schema.json"), { force: true });

  await copyFile(WASM, join(PUBLIC, "reuben_web.wasm"));

  // --- 3. stage the surface docs (ADR-0043) ------------------------------------------------
  //
  // Every INSTRUMENT surface doc under surfaces/ ships (they're tiny, and staging them all
  // keeps this script free of per-Toy wiring — main.js auto-discovers by id:
  // surfaces/<id>.web.json ?? surfaces/<id>.json ?? auto-derived). surface.schema.json is
  // authoring-time tooling the resolver never reads, so it stays out of the payload.
  const surfaceDocs = (await readdir(SURFACES))
    .filter((f) => f.endsWith(".json") && f !== "surface.schema.json")
    .sort();
  for (const f of surfaceDocs) {
    await copyFile(join(SURFACES, f), join(OUT_SURFACES, f));
  }
  console.log(`[stage] surfaces: ${surfaceDocs.length} doc(s) (${surfaceDocs.join(", ")})`);

  // Stage the master PWA icon into public/ so the vite-plugin-pwa asset step can derive the icon
  // set from it into the built payload (see MASTER_ICON). Fail loudly if the human prerequisite
  // is missing, matching this script's no-silent-drift stance — the manifest would otherwise link
  // icons that never got generated.
  if (!(await exists(MASTER_ICON))) {
    throw new Error(
      `missing master PWA icon: web/src/assets/icon.png (issue #227 prerequisite)\n` +
        "  add a square ≥512×512 PNG there; the icon set is derived from it at build.",
    );
  }
  await copyFile(MASTER_ICON, join(PUBLIC, "icon.png"));

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

  // --- 4. emit the offline-precache list (issue #227) -------------------------------------
  //
  // The SW's precache is generated HERE, off the exact set this script just staged — the wasm,
  // every Toy document, every surface doc, and every transitive resource `copied` while
  // discovering them. vite.config.js reads this file and hands it to Workbox verbatim, so the offline
  // payload is definitionally the online payload: add a Toy and its resources are discovered,
  // staged, AND precached in one pass; nothing can be shipped-but-not-cached or cached-but-not-
  // shipped. Each entry carries a content revision so Workbox re-fetches only what changed.
  //
  // URLs are root-relative to the deploy (matching vite's `base: './'` and how main.js's
  // asset() builds them). The app shell (hashed JS/CSS, index.html, icons, manifest) is NOT
  // listed here — vite-plugin-pwa injects those into the precache from the build output itself;
  // this list is only the public/ payload Vite copies verbatim and would otherwise miss.
  const payloadUrls = [
    "reuben_web.wasm",
    ...toys.map((t) => `instruments/${t.id}.json`),
    ...[...copied].sort().map((key) => `instruments/${key}`),
    ...surfaceDocs.map((f) => `surfaces/${f}`),
  ];
  const entries = [];
  for (const url of payloadUrls) {
    const bytes = await readFile(join(PUBLIC, url));
    entries.push({ url, revision: createHash("sha256").update(bytes).digest("hex").slice(0, 16) });
  }
  await writeFile(PRECACHE_MANIFEST, `${JSON.stringify(entries, null, 2)}\n`);

  console.log(
    `[stage] done — ${toys.length} Toys, ${copied.size} unique resource(s), ` +
      `${surfaceDocs.length} surface doc(s), wasm → web/public/` +
      `; precache manifest: ${entries.length} entr${entries.length === 1 ? "y" : "ies"} → web/.pwa-precache.json`,
  );
}

main().catch((err) => {
  console.error(`[stage] FAILED: ${err.message}`);
  process.exit(1);
});
