// Unit tests for the DOM-free auto-UI inference core (issue #225).
//
// Pure JS, `node --test`, NO wasm. Two kinds of proof:
//   (1) the FIXTURE DIFF — buildSurface over each of the 6 committed instruments must
//       deep-equal the committed oracle (`testdata/expected-widgets.json`): widgets exactly,
//       and each row mapped to its widget addresses exactly. The oracle was generated from the
//       reference algorithm (gen_surface.py) WITH both required corrections baked in, so an
//       exact deep-equal is the strongest possible port-fidelity check.
//   (2) DIRECT, human-legible assertions for both corrections + the tricky cases — the
//       issue demands these be spelled out, not merely implied by a fixture diff.
//
// Run: `cd crates/reuben-web && node --test js/surface/infer.test.mjs`

import test from "node:test";
import assert from "node:assert";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

import {
  loadParamMeta,
  buildSurface,
  emit,
  initial,
  nodeParam,
  isGateStep,
  resolveControl,
  layoutRows,
} from "./infer.mjs";

const here = dirname(fileURLToPath(import.meta.url));

// Schema + instruments live outside the surface dir; testdata is a sibling.
const SCHEMA_PATH = join(here, "../../../reuben-core/schema/instrument.schema.json");
const INSTRUMENT_DIR = join(here, "../../../../instruments");
const ORACLE_PATH = join(here, "testdata/expected-widgets.json");

const readJson = (p) => JSON.parse(readFileSync(p, "utf8"));

const schema = readJson(SCHEMA_PATH);
const paramMeta = loadParamMeta(schema);
const oracle = readJson(ORACLE_PATH);

const INSTRUMENTS = [
  "groovebox",
  "chord-player",
  "strum-harp",
  "euclidean-drums",
  "djfilter-demo",
  "good-button",
];

const loadInstrument = (name) => readJson(join(INSTRUMENT_DIR, `${name}.json`));
const surfaceOf = (name) => buildSurface(loadInstrument(name), paramMeta);

// Rows collapse to their widgets' addresses so they can be compared against the oracle,
// which stores rows as address lists.
const rowsToAddresses = (rows) => rows.map((row) => row.map((w) => w.address));

// ---------------------------------------------------------------------------------------
// (1) The fixture diff — the whole surface, exactly, for every instrument.
// ---------------------------------------------------------------------------------------

for (const name of INSTRUMENTS) {
  test(`buildSurface(${name}) deep-equals the oracle`, () => {
    const { widgets, rows } = surfaceOf(name);
    const expected = oracle[name];
    assert.ok(expected, `oracle is missing an entry for ${name}`);
    assert.deepStrictEqual(widgets, expected.widgets);
    assert.deepStrictEqual(rowsToAddresses(rows), expected.rows);
  });
}

// Guard the fixture's own shape so a silently-truncated oracle can't make the diff pass.
test("oracle covers exactly the six instruments with the documented counts", () => {
  assert.deepStrictEqual(Object.keys(oracle).sort(), [...INSTRUMENTS].sort());
  assert.strictEqual(oracle.groovebox.widgets.length, 53);
  assert.strictEqual(oracle["chord-player"].widgets.length, 9);
  assert.strictEqual(oracle["strum-harp"].widgets.length, 4);
  assert.strictEqual(oracle["euclidean-drums"].widgets.length, 25);
  assert.strictEqual(oracle["djfilter-demo"].widgets.length, 3);
  assert.strictEqual(oracle["good-button"].widgets.length, 2);
});

// ---------------------------------------------------------------------------------------
// (2a) CORRECTION #1 — gate-step detection accepts the `"Gate"` enum symbol.
// ---------------------------------------------------------------------------------------

test("groovebox: all 48 sequencer steps are param-toggles (Correction #1)", () => {
  const { widgets } = surfaceOf("groovebox");
  const steps = widgets.filter((w) => /\/(kick|snare|hat)\/step\d+$/.test(w.address));
  assert.strictEqual(steps.length, 48);
  assert.ok(
    steps.every((w) => w.kind === "param-toggle" && w.widget === "param-toggle"),
    "every gate step must resolve to a param-toggle, not a degree-range fader",
  );

  // Spot-check a couple by address, including the node back-reference the layout uses.
  const byAddr = Object.fromEntries(widgets.map((w) => [w.address, w]));
  assert.deepStrictEqual(byAddr["/kick/step1"], {
    kind: "param-toggle",
    label: "K1",
    widget: "param-toggle",
    address: "/kick/step1",
    node: "/kick",
    default: 1,
  });
  assert.strictEqual(byAddr["/hat/step3"].kind, "param-toggle");
  assert.strictEqual(byAddr["/hat/step3"].default, 1);
});

test("isGateStep accepts the 'Gate' enum AND integer 1, rejects otherwise (Correction #1)", () => {
  const seq = (gate_mode) => ({ type: "sequencer", inputs: { gate_mode, step1: 1 } });
  // The reference tested `gate_mode == 1.0` and silently failed on the string enum.
  assert.strictEqual(isGateStep(seq("Gate"), "step1"), true);
  assert.strictEqual(isGateStep(seq(1), "step1"), true);
  assert.strictEqual(isGateStep(seq("Trigger"), "step1"), false);
  assert.strictEqual(isGateStep(seq(0), "step1"), false);
  // Not a sequencer, or not a stepN param.
  assert.strictEqual(isGateStep({ type: "filter", inputs: { gate_mode: "Gate" } }, "step1"), false);
  assert.strictEqual(isGateStep(seq("Gate"), "steps"), false);
  assert.strictEqual(isGateStep(seq("Gate"), "cutoff"), false);
});

// ---------------------------------------------------------------------------------------
// (2b) CORRECTION #2 — paramless Good Button address is `/<node>/in`, not the bare node.
// ---------------------------------------------------------------------------------------

test("good-button: Brightness is /brightness/in with in-range default (Correction #2)", () => {
  const { widgets } = surfaceOf("good-button");
  const brightness = widgets.find((w) => w.label === "Brightness");
  assert.deepStrictEqual(brightness, {
    kind: "fader",
    label: "Brightness",
    widget: "fader",
    address: "/brightness/in", // NOT the bare "/brightness" — a bare address drops in resolve_port
    min: 0,
    max: 1,
    default: 0.5, // the m2s's resting `in` value, not `inputs.default`
    unit: "",
  });
});

test("good-button: Play is a note-toggle on /voicer/notes carrying note 60", () => {
  const { widgets } = surfaceOf("good-button");
  const play = widgets.find((w) => w.label === "Play C");
  assert.deepStrictEqual(play, {
    kind: "note-toggle",
    label: "Play C",
    widget: "note-toggle",
    address: "/voicer/notes",
    note: 60,
  });
});

// ---------------------------------------------------------------------------------------
// (2c) euclidean-drums — radials, per-channel group rows, a bipolar filter.
// ---------------------------------------------------------------------------------------

test("euclidean-drums: radials, per-channel group rows, bipolar filter", () => {
  const { widgets, rows } = surfaceOf("euclidean-drums");
  const controls = widgets.filter((w) => w.kind === "fader");
  assert.ok(controls.length > 0);
  assert.ok(controls.every((w) => w.widget === "radial"), "all euclidean controls are radials");

  // The kick DJ filter is bipolar: spec min/max override the m2s sentinel range.
  const kf = widgets.find((w) => w.address === "/kick_filter/in");
  assert.strictEqual(kf.min, -1);
  assert.strictEqual(kf.max, 1);
  assert.strictEqual(kf.group, "kick");

  // Consecutive per-channel controls share a group and land on their own layout row.
  const addrRows = rowsToAddresses(rows);
  assert.deepStrictEqual(addrRows[1], [
    "/kick_eu/pulses",
    "/kick_eu/steps",
    "/kick_eu/rotation",
    "/kick_env/decay",
    "/kick_filter/in",
    "/kick_level/in",
  ]);
  // Every widget in that row carries group "kick".
  assert.ok(rows[1].every((w) => w.group === "kick"));
});

// ---------------------------------------------------------------------------------------
// (2d) chord-player — 7 chord-buttons on /chord/set carrying degrees 0..6.
// ---------------------------------------------------------------------------------------

test("chord-player: 7 chord-buttons on /chord/set with degrees 0..6", () => {
  const { widgets } = surfaceOf("chord-player");
  const chords = widgets.filter((w) => w.kind === "chord-button");
  assert.strictEqual(chords.length, 7);
  assert.ok(chords.every((w) => w.address === "/chord/set" && w.widget === "chord-button"));
  assert.deepStrictEqual(
    chords.map((w) => w.degree),
    [0, 1, 2, 3, 4, 5, 6],
  );
});

// ---------------------------------------------------------------------------------------
// (2e) emit / initial — the pure binding used by render.mjs and check.mjs.
// ---------------------------------------------------------------------------------------

test("emit: fader scales x in [0,1] -> [min,max]", () => {
  const tempo = { kind: "fader", widget: "fader", address: "/clock/tempo", min: 1, max: 999, default: 120 };
  assert.deepStrictEqual(emit(tempo, 0), { address: "/clock/tempo", args: [1] });
  assert.deepStrictEqual(emit(tempo, 1), { address: "/clock/tempo", args: [999] });
  assert.deepStrictEqual(emit(tempo, 0.5), { address: "/clock/tempo", args: [500] }); // 1 + 0.5*998
});

test("emit: radial scales like a fader; bipolar filter maps x=0.5 -> 0", () => {
  const filter = { kind: "fader", widget: "radial", address: "/kick_filter/in", min: -1, max: 1, default: 0 };
  assert.deepStrictEqual(emit(filter, 0.5), { address: "/kick_filter/in", args: [0] });
});

test("emit: param-toggle passes x raw; note/chord carry the constant + gate", () => {
  const step = { kind: "param-toggle", widget: "param-toggle", address: "/kick/step1", node: "/kick", default: 1 };
  assert.deepStrictEqual(emit(step, 1), { address: "/kick/step1", args: [1] });
  assert.deepStrictEqual(emit(step, 0), { address: "/kick/step1", args: [0] });

  const play = { kind: "note-toggle", widget: "note-toggle", address: "/voicer/notes", note: 60 };
  assert.deepStrictEqual(emit(play, 1), { address: "/voicer/notes", args: [60, 1] });

  const chord = { kind: "chord-button", widget: "chord-button", address: "/chord/set", degree: 4 };
  assert.deepStrictEqual(emit(chord, 1), { address: "/chord/set", args: [4, 1] });
});

test("initial: fader default is raw (unscaled); note/chord rest at gate 0", () => {
  const tempo = { kind: "fader", widget: "fader", address: "/clock/tempo", min: 1, max: 999, default: 120 };
  assert.deepStrictEqual(initial(tempo), { address: "/clock/tempo", args: [120] });

  const step = { kind: "param-toggle", widget: "param-toggle", address: "/kick/step1", node: "/kick", default: 1 };
  assert.deepStrictEqual(initial(step), { address: "/kick/step1", args: [1] });

  const play = { kind: "note-toggle", widget: "note-toggle", address: "/voicer/notes", note: 60 };
  assert.deepStrictEqual(initial(play), { address: "/voicer/notes", args: [60, 0] });

  const chord = { kind: "chord-button", widget: "chord-button", address: "/chord/set", degree: 4 };
  assert.deepStrictEqual(initial(chord), { address: "/chord/set", args: [4, 0] });
});

// ---------------------------------------------------------------------------------------
// (2f) loadParamMeta sanity — a Float input is parsed, an audio/enum input is skipped.
// ---------------------------------------------------------------------------------------

test("loadParamMeta parses clock.tempo and skips non-numeric inputs", () => {
  assert.deepStrictEqual(paramMeta.clock.tempo, {
    min: 1,
    max: 999,
    default: 120,
    unit: "BPM",
    curve: "Linear",
  });
  // `sync` is a string-enum input (no number form) — skipped, so it has no metadata.
  assert.strictEqual(paramMeta.clock.sync, undefined);
  // `audio` on the filter is a wire-only passthrough — also skipped.
  assert.strictEqual(paramMeta.filter.audio, undefined);
  assert.ok("cutoff" in paramMeta.filter, "a numeric filter input is present");
});

// ---------------------------------------------------------------------------------------
// (2g) helper spot-checks — nodeParam coercion + layoutRows grouping.
// ---------------------------------------------------------------------------------------

test("nodeParam returns finite numbers only; booleans/wire-refs/enums fall back", () => {
  const node = { inputs: { a: 3.5, b: true, c: { from: "/x" }, d: "Gate" } };
  assert.strictEqual(nodeParam(node, "a", 0), 3.5);
  assert.strictEqual(nodeParam(node, "b", 9), 9); // boolean -> fallback
  assert.strictEqual(nodeParam(node, "c", 9), 9); // wire-ref -> fallback
  assert.strictEqual(nodeParam(node, "d", 9), 9); // enum symbol -> fallback
  assert.strictEqual(nodeParam(node, "missing", 7), 7);
  assert.strictEqual(nodeParam({}, "x", 2), 2);
});

test("layoutRows: param-toggle runs and group runs each break out of the grid", () => {
  const fader = (address, group) => ({ kind: "fader", widget: "fader", address, ...(group ? { group } : {}) });
  const toggle = (address, node) => ({ kind: "param-toggle", widget: "param-toggle", address, node });

  const rows = layoutRows([
    fader("/a"),
    toggle("/kick/step1", "/kick"),
    toggle("/kick/step2", "/kick"),
    fader("/b", "grp"),
    fader("/c", "grp"),
    fader("/d"),
  ]);
  assert.deepStrictEqual(rowsToAddresses(rows), [
    ["/a"],
    ["/kick/step1", "/kick/step2"],
    ["/b", "/c"],
    ["/d"],
  ]);
});

test("resolveControl: paramful spec overrides win over schema metadata", () => {
  // euclidean's /kick_filter names param `in` (paramful), and overrides the m2s sentinel range.
  const node = { type: "m2s", address: "/kick_filter", inputs: { in: 0.0, mode: "Smooth" } };
  const spec = { label: "Kick DJ Filter", param: "in", widget: "radial", min: -1, max: 1, default: 0, group: "kick" };
  const c = resolveControl(node, spec, paramMeta);
  assert.deepStrictEqual(c, {
    kind: "fader",
    label: "Kick DJ Filter",
    widget: "radial",
    address: "/kick_filter/in",
    min: -1,
    max: 1,
    default: 0,
    unit: "",
    group: "kick",
  });
});
