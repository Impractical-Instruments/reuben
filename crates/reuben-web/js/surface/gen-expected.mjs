#!/usr/bin/env node
// Regenerates testdata/expected-widgets.json — the cross-implementation surface fixture
// (ADR-0043 §9). For every committed instrument + surface-doc pair the JS resolver
// (widget-model.mjs) and the Python twin (.claude/skills/control-surface) must resolve the
// same widget list and layout; this file is the shared oracle both test suites deep-equal.
//
// Shape: `{ "<name>": { "widgets": [...], "rows": [[address, ...], ...] } }` — widgets
// verbatim from buildSurface, rows collapsed to their widgets' addresses. Output is
// deterministic (2-space indent, trailing newline), so a regeneration with no semantic
// change is byte-identical.
//
// Run: node js/surface/gen-expected.mjs   (from any cwd — paths resolve via import.meta.url)

import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { buildSurface } from "./widget-model.mjs";

// The committed pairs: surfaces/<name>.json presents instruments/<path>. Space's instrument
// document lives under patches/. Fixture order is fixed here so the output is stable.
const PAIRS = [
  ["groovebox", "instruments/groovebox.json"],
  ["chord-player", "instruments/chord-player.json"],
  ["strum-harp", "instruments/strum-harp.json"],
  ["euclidean-drums", "instruments/euclidean-drums.json"],
  ["space", "instruments/patches/space.json"],
];

const ROOT = new URL("../../../../", import.meta.url); // repo root
const ORACLE = new URL("testdata/expected-widgets.json", import.meta.url);

const readJson = (url) => JSON.parse(readFileSync(url, "utf8"));

const oracle = {};
let dirty = false;
for (const [name, instrumentPath] of PAIRS) {
  const instrument = readJson(new URL(instrumentPath, ROOT));
  const surfaceDoc = readJson(new URL(`surfaces/${name}.json`, ROOT));
  const { widgets, rows, warnings } = buildSurface(instrument, surfaceDoc);
  // A committed pair must resolve clean — a warning means the instrument and its surface
  // doc drifted, and an oracle baked from a drifted pair would pin the drift as truth.
  for (const w of warnings) {
    console.error(`WARN  ${name}: ${w}`);
    dirty = true;
  }
  oracle[name] = { widgets, rows: rows.map((row) => row.map((w) => w.address)) };
}
if (dirty) {
  console.error("refusing to write an oracle from pairs that resolve with warnings");
  process.exit(1);
}

writeFileSync(ORACLE, `${JSON.stringify(oracle, null, 2)}\n`);
console.log(
  `wrote ${fileURLToPath(ORACLE)}: ${PAIRS.map(([n]) => `${n}(${oracle[n].widgets.length})`).join(", ")}`,
);
