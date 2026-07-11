// Unit tests for the structural node-identity diff (issue #353, spec §4.6) — the diff the
// change-card renders. Pure JS, `node --test`, NO wasm and NO browser (the diff is a plain
// function over two parsed documents), following the codec.test.mjs / share.test.mjs precedent.
//
// The diff keys on node.address: `added`/`removed` catch whole-node identity changes,
// `changed` catches content edits on a node present in BOTH documents (param/config/voice/
// sample/patch — anything in the node object). Survivors (present in both, byte-equal) hold
// position and appear in none of the three buckets (spec §4.6).
//
// Run: `cd crates/reuben-web && node --test js/diff.test.mjs`

import test from "node:test";
import assert from "node:assert";

import { structuralDiff } from "./diff.mjs";

// A three-node base document. Each test derives its `after` from this so the expected buckets
// come from the crafted edit, not from recomputing the diff the way the code does.
function baseDoc() {
  return {
    nodes: [
      { type: "oscillator", address: "/osc", inputs: { freq: 220.0 } },
      { type: "filter", address: "/filt", inputs: { cutoff: 1000.0 }, config: { poles: 2 } },
      { type: "output", address: "/out", inputs: { audio: { from: "/filt" } } },
    ],
  };
}

test("an added node lands in `added` and nowhere else", () => {
  const before = baseDoc();
  const after = baseDoc();
  after.nodes.push({ type: "oscillator", address: "/lfo", inputs: { freq: 5.0 } });

  const d = structuralDiff(before, after);
  assert.deepStrictEqual(d.added, ["/lfo"]);
  assert.deepStrictEqual(d.removed, []);
  assert.deepStrictEqual(d.changed, []);
});

test("a dropped node lands in `removed` and nowhere else", () => {
  const before = baseDoc();
  const after = baseDoc();
  after.nodes = after.nodes.filter((n) => n.address !== "/filt");
  // /out still references /filt, but the diff is purely structural (node identity), not a
  // validation — a dangling wire is validate's concern, not the diff's.

  const d = structuralDiff(before, after);
  assert.deepStrictEqual(d.removed, ["/filt"]);
  assert.deepStrictEqual(d.added, []);
  assert.deepStrictEqual(d.changed, []);
});

test("a param-only edit lands in `changed`, not added/removed", () => {
  const before = baseDoc();
  const after = baseDoc();
  after.nodes.find((n) => n.address === "/osc").inputs.freq = 440.0;

  const d = structuralDiff(before, after);
  assert.deepStrictEqual(d.changed, ["/osc"]);
  assert.deepStrictEqual(d.added, []);
  assert.deepStrictEqual(d.removed, []);
});

test("a config edit counts as `changed` (not just inputs)", () => {
  const before = baseDoc();
  const after = baseDoc();
  after.nodes.find((n) => n.address === "/filt").config.poles = 4;

  const d = structuralDiff(before, after);
  assert.deepStrictEqual(d.changed, ["/filt"]);
});

test("a type change on the same address counts as `changed`", () => {
  const before = baseDoc();
  const after = baseDoc();
  after.nodes.find((n) => n.address === "/osc").type = "noise";

  const d = structuralDiff(before, after);
  assert.deepStrictEqual(d.changed, ["/osc"]);
});

test("an untouched node appears in none of the three buckets (survivor holds position)", () => {
  const before = baseDoc();
  const after = baseDoc();
  after.nodes.find((n) => n.address === "/osc").inputs.freq = 440.0; // only /osc changes

  const d = structuralDiff(before, after);
  // /filt and /out are untouched: they must not appear anywhere.
  for (const bucket of [d.added, d.removed, d.changed]) {
    assert.ok(!bucket.includes("/filt"), "/filt is a survivor");
    assert.ok(!bucket.includes("/out"), "/out is a survivor");
  }
});

test("key order does not manufacture a `changed` (stable, key-sorted comparison)", () => {
  const before = {
    nodes: [{ type: "oscillator", address: "/osc", inputs: { freq: 220.0, gain: 0.5 } }],
  };
  // Same node content, keys emitted in a different order (both at the node level and inside
  // `inputs`). A naive JSON.stringify would flag this as changed; the diff must not.
  const after = {
    nodes: [{ inputs: { gain: 0.5, freq: 220.0 }, address: "/osc", type: "oscillator" }],
  };

  const d = structuralDiff(before, after);
  assert.deepStrictEqual(d.added, []);
  assert.deepStrictEqual(d.removed, []);
  assert.deepStrictEqual(d.changed, []);
});

test("all three buckets at once, each sorted deterministically", () => {
  const before = {
    nodes: [
      { type: "oscillator", address: "/b", inputs: { freq: 100 } },
      { type: "oscillator", address: "/a", inputs: { freq: 200 } }, // changed below
      { type: "filter", address: "/gone", inputs: {} }, // removed
      { type: "output", address: "/keep", inputs: {} }, // survivor
    ],
  };
  const after = {
    nodes: [
      { type: "oscillator", address: "/b", inputs: { freq: 100 } }, // survivor
      { type: "oscillator", address: "/a", inputs: { freq: 999 } }, // changed
      { type: "output", address: "/keep", inputs: {} }, // survivor
      { type: "oscillator", address: "/zed", inputs: {} }, // added
      { type: "oscillator", address: "/mid", inputs: {} }, // added
    ],
  };

  const d = structuralDiff(before, after);
  assert.deepStrictEqual(d.added, ["/mid", "/zed"], "added is sorted");
  assert.deepStrictEqual(d.removed, ["/gone"]);
  assert.deepStrictEqual(d.changed, ["/a"]);
});

test("missing or empty `.nodes` is treated as no nodes (defensive)", () => {
  const doc = baseDoc();
  // before has no nodes at all -> every after-node is `added`.
  assert.deepStrictEqual(structuralDiff({}, doc).added, ["/filt", "/osc", "/out"]);
  assert.deepStrictEqual(structuralDiff(undefined, doc).added, ["/filt", "/osc", "/out"]);
  // after has no nodes -> every before-node is `removed`.
  assert.deepStrictEqual(structuralDiff(doc, { nodes: [] }).removed, ["/filt", "/osc", "/out"]);
  // both empty -> empty diff.
  assert.deepStrictEqual(structuralDiff({}, {}), { added: [], removed: [], changed: [] });
});

test("nodes lacking an `address` are ignored, not thrown on", () => {
  const before = { nodes: [{ type: "oscillator" }, { type: "filter", address: "/f" }] };
  const after = { nodes: [{ type: "noise" }, { type: "filter", address: "/f" }] };
  // The address-less nodes are dropped entirely; /f is an untouched survivor.
  const d = structuralDiff(before, after);
  assert.deepStrictEqual(d, { added: [], removed: [], changed: [] });
});
