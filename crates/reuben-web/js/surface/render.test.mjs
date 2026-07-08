// Unit tests for the DOM renderer + engine binding (issue #225).
//
// Pure JS, `node --test`, NO browser and NO wasm. render.mjs is import-safe (it touches
// `document` only inside functions), so we install a MINIMAL fake `document` on globalThis
// — just enough surface for renderSurface: createElement returning a node with children,
// classList, style, setAttribute, textContent, value, and addEventListener/dispatchEvent
// so a test can synthesise an "input"/"click"/"pointerdown" event. A fake engine records
// every send(address, args). The stub is torn down in an `after` hook so it can't leak into
// sibling test files.
//
// Run: `cd crates/reuben-web && node --test js/surface/render.test.mjs`

import test from "node:test";
import assert from "node:assert";
import { after } from "node:test";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

import { loadParamMeta, buildSurface } from "./infer.mjs";
import { renderSurface, sendInitialDefaults } from "./render.mjs";

// --- minimal fake DOM --------------------------------------------------------------------

// One node type covers every element render.mjs creates. dispatch(type) invokes the
// listeners registered for that event type, mimicking element.dispatchEvent.
function makeElement(tagName) {
  const listeners = new Map(); // type -> handler[]
  const el = {
    tagName: String(tagName).toUpperCase(),
    children: [],
    style: {},
    value: undefined,
    textContent: "",
    attrs: {},
    _classes: new Set(),
    classList: {
      add(...names) {
        for (const n of names) el._classes.add(n);
      },
      remove(...names) {
        for (const n of names) el._classes.delete(n);
      },
      contains(n) {
        return el._classes.has(n);
      },
    },
    appendChild(child) {
      el.children.push(child);
      return child;
    },
    setAttribute(name, val) {
      el.attrs[name] = String(val);
    },
    getAttribute(name) {
      return el.attrs[name];
    },
    addEventListener(type, handler) {
      if (!listeners.has(type)) listeners.set(type, []);
      listeners.get(type).push(handler);
    },
    dispatch(type) {
      for (const h of listeners.get(type) ?? []) h();
    },
  };
  return el;
}

function installFakeDocument() {
  const doc = {
    createElement: (tag) => makeElement(tag),
    getElementById: () => null, // no <head> style injection under the stub
    head: null,
  };
  const prev = globalThis.document;
  globalThis.document = doc;
  return () => {
    globalThis.document = prev; // restore (undefined if there was none) so nothing leaks
  };
}

// A fake engine that records the exact (address, args) pairs the binding sends.
function makeEngine() {
  const sends = [];
  return { sends, send: (address, args) => sends.push({ address, args }) };
}

// Depth-first search of the rendered tree for the first element matching `pred`.
function find(root, pred) {
  for (const child of root.children) {
    if (pred(child)) return child;
    const deeper = find(child, pred);
    if (deeper) return deeper;
  }
  return null;
}
const isTag = (t) => (el) => el.tagName === t;

// --- fixtures (real instruments + schema, parsed off disk — no wasm) ---------------------

const here = dirname(fileURLToPath(import.meta.url));
const SCHEMA_PATH = join(here, "../../../reuben-core/schema/instrument.schema.json");
const INSTRUMENT_DIR = join(here, "../../../../instruments");
const readJson = (p) => JSON.parse(readFileSync(p, "utf8"));

const paramMeta = loadParamMeta(readJson(SCHEMA_PATH));
const surfaceOf = (name) => buildSurface(readJson(join(INSTRUMENT_DIR, `${name}.json`)), paramMeta);

// Tear the global stub down once, no matter which test installed it last.
const restore = installFakeDocument();
after(restore);

// -----------------------------------------------------------------------------------------
// good-button: a note-toggle + a fader; assert the binding routes through emit().
// -----------------------------------------------------------------------------------------

test("good-button renders a note-toggle button + a fader, each bound through emit()", () => {
  const engine = makeEngine();
  const container = makeElement("div");
  renderSurface(surfaceOf("good-button"), engine, container);

  // The fader (Brightness -> /brightness/in): dispatch an input at slider value 0.5.
  const range = find(container, (el) => el.tagName === "INPUT" && el.attrs.type === "range");
  assert.ok(range, "a range input was rendered for the Brightness fader");
  range.value = "0.5";
  range.dispatch("input");
  assert.deepStrictEqual(engine.sends.at(-1), { address: "/brightness/in", args: [0.5] });

  // The Play button (note-toggle -> /voicer/notes note 60): hold = [60,1], release = [60,0].
  const play = find(container, (el) => el.tagName === "BUTTON" && el.textContent === "Play C");
  assert.ok(play, "a button was rendered for the Play C note-toggle");
  play.dispatch("pointerdown");
  assert.deepStrictEqual(engine.sends.at(-1), { address: "/voicer/notes", args: [60, 1] });
  play.dispatch("pointerup");
  assert.deepStrictEqual(engine.sends.at(-1), { address: "/voicer/notes", args: [60, 0] });
});

// -----------------------------------------------------------------------------------------
// groovebox: the 3 step lanes each render as their own row of 16 toggles; a click emits.
// -----------------------------------------------------------------------------------------

test("groovebox lays out the 3 step lanes as their own 16-cell rows", () => {
  const engine = makeEngine();
  const container = makeElement("div");
  renderSurface(surfaceOf("groovebox"), engine, container);

  // Rows: [tempo(1), kick(16), snare(16), hat(16), 4 faders] — 5 rows total.
  assert.strictEqual(container.children.length, 5, "5 layout rows");
  const lanes = [1, 2, 3]; // kick / snare / hat
  for (const i of lanes) {
    const row = container.children[i];
    assert.strictEqual(row.children.length, 16, `lane row ${i} has 16 step cells`);
    assert.ok(row.classList.contains("step-lane"), `lane row ${i} is a step-lane`);
    assert.ok(
      row.children.every((c) => c.tagName === "BUTTON"),
      `lane row ${i} is all toggle buttons`,
    );
  }
});

test("groovebox step-toggle click sends emit() on its param address", () => {
  const engine = makeEngine();
  const container = makeElement("div");
  renderSurface(surfaceOf("groovebox"), engine, container);

  // /kick/step1 rests at default 1 (pressed); one click flips it off -> emit [0].
  const step1 = container.children[1].children[0];
  step1.dispatch("click");
  assert.deepStrictEqual(engine.sends.at(-1), { address: "/kick/step1", args: [0] });

  // /kick/step2 rests at default 0 (off); one click turns it on -> emit [1].
  const step2 = container.children[1].children[1];
  step2.dispatch("click");
  assert.deepStrictEqual(engine.sends.at(-1), { address: "/kick/step2", args: [1] });
});

// -----------------------------------------------------------------------------------------
// sendInitialDefaults: one default send per widget, post-ready (see render.mjs lifecycle).
// -----------------------------------------------------------------------------------------

test("sendInitialDefaults(good-button) fires each widget's default exactly once", () => {
  const engine = makeEngine();
  sendInitialDefaults(surfaceOf("good-button"), engine);

  // Two widgets: note-toggle (resting gate 0) and the Brightness fader (raw default 0.5).
  assert.strictEqual(engine.sends.length, 2);
  const byAddr = Object.fromEntries(engine.sends.map((s) => [s.address, s.args]));
  assert.deepStrictEqual(byAddr["/brightness/in"], [0.5]);
  assert.deepStrictEqual(byAddr["/voicer/notes"], [60, 0]);
});

// -----------------------------------------------------------------------------------------
// An instrument with no control blocks renders an empty (present, non-crashing) surface.
// -----------------------------------------------------------------------------------------

test("a surface with no widgets renders an empty container without crashing", () => {
  const engine = makeEngine();
  const container = makeElement("div");
  renderSurface({ widgets: [], rows: [] }, engine, container);
  assert.strictEqual(container.children.length, 0);
  assert.strictEqual(engine.sends.length, 0);
});
