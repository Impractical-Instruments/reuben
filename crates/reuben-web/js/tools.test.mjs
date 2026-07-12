// Unit tests for the in-page tool layer (issue #353, ADR-0048 §5, ADR-0052 §2) — the eight
// contract tools the chat agent binds. Pure JS, `node --test`, NO wasm and NO browser: the
// engine-bound tools (send/swap/status/...) are exercised against a FAKE engine and a FAKE
// introspect adapter, so the contract logic is proven without an AudioWorklet. The REAL
// wasmIntrospect adapter over the real exports is covered separately by check.mjs (it needs the
// real module); the engine-bound tools can't run in node at all (no AudioWorklet), so fakes are
// the only way to gate their logic in CI.
//
// Run: `cd crates/reuben-web && node --test js/tools.test.mjs`

import test from "node:test";
import assert from "node:assert";

import { createToolLayer } from "./tools.mjs";

// A fake engine recording send/loadBundle calls; currentBundle + context.state + node are
// configurable so engine_status / get_current_instrument / swap can be driven deterministically.
function makeEngine({ bundle = null, contextState = "running", hasNode = true } = {}) {
  const calls = { send: [], loadBundle: [] };
  return {
    context: { state: contextState },
    node: hasNode ? {} : null,
    send(address, args) {
      calls.send.push({ address, args });
    },
    async loadBundle(arg) {
      calls.loadBundle.push(arg);
    },
    currentBundle() {
      return bundle;
    },
    _calls: calls,
  };
}

// A fake introspect adapter: contentHash is deterministic and injective on its input
// ("hash(<text>)"), so a before/after hash pair is distinguishable; validate returns a
// configured Report; any method can be told to throw (the isError mapping).
function makeIntrospect({
  validateReport = { ok: true, errors: [], warnings: [] },
  throwOn = {},
} = {}) {
  const calls = { describeOperators: [], describeInstrument: [], validate: [], contentHash: [] };
  return {
    describeOperators(name) {
      calls.describeOperators.push(name);
      if (throwOn.describeOperators) throw new Error(throwOn.describeOperators);
      return [{ type_name: "oscillator" }];
    },
    describeInstrument(docText) {
      calls.describeInstrument.push(docText);
      if (throwOn.describeInstrument) throw new Error(throwOn.describeInstrument);
      return { instrument: "probe", inputs: [], outputs: [] };
    },
    validate(docText) {
      calls.validate.push(docText);
      if (throwOn.validate) throw new Error(throwOn.validate);
      return validateReport;
    },
    contentHash(docText) {
      calls.contentHash.push(docText);
      if (throwOn.contentHash) throw new Error(throwOn.contentHash);
      return `hash(${docText})`;
    },
    _calls: calls,
  };
}

const BEFORE_DOC = {
  nodes: [
    { type: "oscillator", address: "/osc", inputs: { freq: 220 } },
    { type: "output", address: "/out", inputs: { audio: { from: "/osc" } } },
  ],
};
// /osc changed (freq), /out survives, /lfo added, nothing removed.
const AFTER_DOC = {
  nodes: [
    { type: "oscillator", address: "/osc", inputs: { freq: 440 } },
    { type: "output", address: "/out", inputs: { audio: { from: "/osc" } } },
    { type: "oscillator", address: "/lfo", inputs: { freq: 5 } },
  ],
};
const bundleOf = (doc) => ({ docText: JSON.stringify(doc), resources: [] });

// --- send (batch, min 1) -----------------------------------------------------------------

test("send dispatches one engine.send per message and returns { sent: N }", () => {
  const engine = makeEngine();
  const tools = createToolLayer({ engine, introspect: makeIntrospect() });

  const out = tools.send({ messages: [{ address: "/a/in", args: [1] }, { address: "/b/in" }] });
  assert.deepStrictEqual(out, { sent: 2 });
  assert.strictEqual(engine._calls.send.length, 2);
  assert.deepStrictEqual(engine._calls.send[0], { address: "/a/in", args: [1] });
  // args defaults to [] when a message omits it.
  assert.deepStrictEqual(engine._calls.send[1], { address: "/b/in", args: [] });
});

test("send throws on an empty or non-array messages list (ADR-0048 §5 min 1)", () => {
  const engine = makeEngine();
  const tools = createToolLayer({ engine, introspect: makeIntrospect() });
  assert.throws(() => tools.send({ messages: [] }));
  assert.throws(() => tools.send({ messages: undefined }));
  assert.strictEqual(engine._calls.send.length, 0, "nothing dispatched on a rejected batch");
});

test("send is probe-first: an unreachable engine throws, nothing dispatched (ADR-0048 §5)", () => {
  // A closed context (or a torn-down node) is unreachable — posting to it is not "sent".
  const closed = makeEngine({ contextState: "closed" });
  const t1 = createToolLayer({ engine: closed, introspect: makeIntrospect() });
  assert.throws(() => t1.send({ messages: [{ address: "/a/in", args: [1] }] }), /not reachable/);
  assert.strictEqual(closed._calls.send.length, 0, "nothing dispatched to a closed context");

  const noNode = makeEngine({ hasNode: false });
  const t2 = createToolLayer({ engine: noNode, introspect: makeIntrospect() });
  assert.throws(() => t2.send({ messages: [{ address: "/a/in" }] }), /not reachable/);
  assert.strictEqual(noNode._calls.send.length, 0, "nothing dispatched with no node");
});

// --- validate (a failed validation is a SUCCESSFUL call, ADR-0048 §3) --------------------

test("validate returns the Report and does NOT throw on ok:false", () => {
  const report = { ok: false, errors: [{ node: "/osc", message: "unknown type" }], warnings: [] };
  const tools = createToolLayer({ engine: makeEngine(), introspect: makeIntrospect({ validateReport: report }) });
  const out = tools.validate({ document: AFTER_DOC });
  assert.deepStrictEqual(out, report, "the {ok:false} report is the deliverable, not an error");
});

// --- describe_* map introspect throws to isError (a real throw) --------------------------

test("describe_operators returns { operators } and throws on an unknown name", () => {
  const ok = createToolLayer({ engine: makeEngine(), introspect: makeIntrospect() });
  assert.deepStrictEqual(ok.describe_operators({ name: "oscillator" }), {
    operators: [{ type_name: "oscillator" }],
  });
  const bad = createToolLayer({
    engine: makeEngine(),
    introspect: makeIntrospect({ throwOn: { describeOperators: "unknown operator: nope" } }),
  });
  assert.throws(() => bad.describe_operators({ name: "nope" }), /unknown operator/);
});

test("describe_instrument returns the boundary and throws when the doc fails to load", () => {
  const ok = createToolLayer({ engine: makeEngine(), introspect: makeIntrospect() });
  assert.strictEqual(ok.describe_instrument({ document: BEFORE_DOC }).instrument, "probe");
  const bad = createToolLayer({
    engine: makeEngine(),
    introspect: makeIntrospect({ throwOn: { describeInstrument: "does not load" } }),
  });
  assert.throws(() => bad.describe_instrument({ document: "{ not json" }), /does not load/);
});

// --- swap: valid, invalid, and expect-conflict ------------------------------------------

test("swap on a valid doc installs by value, reports survived:0 + structural diff, no state_reset", async () => {
  const engine = makeEngine({ bundle: bundleOf(BEFORE_DOC) });
  const introspect = makeIntrospect();
  const tools = createToolLayer({ engine, introspect });

  const out = await tools.swap({ document: AFTER_DOC, resources: [] });

  assert.strictEqual(out.ok, true);
  assert.strictEqual(out.diff.survived, 0, "restart-swap: survived is ALWAYS 0 (ADR-0052 §2)");
  assert.deepStrictEqual(out.diff.added, ["/lfo"]);
  assert.deepStrictEqual(out.diff.changed, ["/osc"]);
  assert.deepStrictEqual(out.diff.removed, []);
  assert.ok(!("state_reset" in out.diff), "no state_reset key on the web diff (#353)");
  assert.strictEqual(out.restarted, true, "something was already playing — this is a genuine restart (#356)");

  // content_hash is the NEW document's hash, not the installed one.
  const newText = JSON.stringify(AFTER_DOC);
  assert.strictEqual(out.content_hash, `hash(${newText})`);
  assert.notStrictEqual(out.content_hash, `hash(${JSON.stringify(BEFORE_DOC)})`);

  // Installed by value: loadBundle got the new docText verbatim.
  assert.strictEqual(engine._calls.loadBundle.length, 1);
  assert.strictEqual(engine._calls.loadBundle[0].docText, newText);
  assert.deepStrictEqual(engine._calls.loadBundle[0].resources, []);
});

test("swap on an invalid doc installs NOTHING and returns no diff (old sound keeps playing)", async () => {
  const report = { ok: false, errors: [{ node: "/osc", message: "bad" }], warnings: [] };
  const engine = makeEngine({ bundle: bundleOf(BEFORE_DOC) });
  const introspect = makeIntrospect({ validateReport: report });
  const tools = createToolLayer({ engine, introspect });

  const out = await tools.swap({ document: AFTER_DOC });

  assert.strictEqual(out.ok, false);
  assert.deepStrictEqual(out.errors, report.errors);
  assert.ok(!("diff" in out), "a rejected swap installs nothing, so no diff");
  // content_hash names what KEEPS playing (the installed doc), not the rejected one.
  assert.strictEqual(out.content_hash, `hash(${JSON.stringify(BEFORE_DOC)})`);
  assert.strictEqual(engine._calls.loadBundle.length, 0, "nothing installed");
});

test("swap with a mismatched expect returns a conflict and installs nothing (no validate)", async () => {
  const engine = makeEngine({ bundle: bundleOf(BEFORE_DOC) });
  const introspect = makeIntrospect();
  const tools = createToolLayer({ engine, introspect });
  const installedHash = `hash(${JSON.stringify(BEFORE_DOC)})`;

  const out = await tools.swap({ document: AFTER_DOC, expect: "stale-token" });

  assert.strictEqual(out.ok, false);
  assert.deepStrictEqual(out.conflict, { expected: "stale-token", actual: installedHash });
  assert.strictEqual(out.content_hash, installedHash);
  assert.ok(!("diff" in out));
  assert.strictEqual(engine._calls.loadBundle.length, 0, "no install on conflict");
  assert.strictEqual(introspect._calls.validate.length, 0, "conflict short-circuits before validate");
});

test("swap with a matching expect proceeds to install", async () => {
  const engine = makeEngine({ bundle: bundleOf(BEFORE_DOC) });
  const introspect = makeIntrospect();
  const tools = createToolLayer({ engine, introspect });
  const installedHash = `hash(${JSON.stringify(BEFORE_DOC)})`;

  const out = await tools.swap({ document: AFTER_DOC, expect: installedHash });
  assert.strictEqual(out.ok, true);
  assert.strictEqual(engine._calls.loadBundle.length, 1, "matching expect installs");
});

test("swap from nothing loaded: beforeText null → installedHash empty, before treated as no nodes", async () => {
  const engine = makeEngine({ bundle: null });
  const introspect = makeIntrospect();
  const tools = createToolLayer({ engine, introspect });

  const out = await tools.swap({ document: AFTER_DOC });
  assert.strictEqual(out.ok, true);
  // Every node is `added` (there was no before document).
  assert.deepStrictEqual(out.diff.added, ["/lfo", "/osc", "/out"]);
  assert.deepStrictEqual(out.diff.changed, []);
  assert.strictEqual(out.restarted, false, "a first install into silence is not a restart (#356, spec §6.4)");
  assert.deepStrictEqual(out.diff.removed, []);
});

// --- engine_status / get_current_instrument / get_diagnostics ---------------------------

test("engine_status reflects the AudioContext state and reachability", () => {
  const running = createToolLayer({ engine: makeEngine({ contextState: "running" }), introspect: makeIntrospect() });
  assert.deepStrictEqual(running.engine_status(), { reachable: true, state: "running" });

  const suspended = createToolLayer({ engine: makeEngine({ contextState: "suspended" }), introspect: makeIntrospect() });
  assert.deepStrictEqual(suspended.engine_status(), { reachable: true, state: "suspended" });

  const closed = createToolLayer({ engine: makeEngine({ contextState: "closed" }), introspect: makeIntrospect() });
  assert.deepStrictEqual(closed.engine_status(), { reachable: false, state: "closed" });

  const noNode = createToolLayer({ engine: makeEngine({ hasNode: false }), introspect: makeIntrospect() });
  assert.strictEqual(noNode.engine_status().reachable, false, "no node ⇒ not reachable");
});

test("get_current_instrument throws with nothing loaded, returns {document, content_hash} with a bundle", () => {
  const empty = createToolLayer({ engine: makeEngine({ bundle: null }), introspect: makeIntrospect() });
  assert.throws(() => empty.get_current_instrument(), "nothing loaded is isError");

  const engine = makeEngine({ bundle: bundleOf(BEFORE_DOC) });
  const tools = createToolLayer({ engine, introspect: makeIntrospect() });
  const out = tools.get_current_instrument();
  assert.deepStrictEqual(out.document, BEFORE_DOC, "document is the parsed staged doc");
  assert.strictEqual(out.content_hash, `hash(${JSON.stringify(BEFORE_DOC)})`);
});

test("get_diagnostics returns the four DiagnosticsReport counters, all zero (the seam)", () => {
  const tools = createToolLayer({ engine: makeEngine(), introspect: makeIntrospect() });
  assert.deepStrictEqual(tools.get_diagnostics(), {
    output_xruns: 0,
    input_ring_underruns: 0,
    input_ring_overruns: 0,
    input_ring_producer_drops: 0,
  });
});

// --- the roster is the exact snake_case contract names (M1's agent schemas key on these) --

test("the tool layer exposes exactly the eight ADR-0048 contract names", () => {
  const tools = createToolLayer({ engine: makeEngine(), introspect: makeIntrospect() });
  assert.deepStrictEqual(Object.keys(tools).sort(), [
    "describe_instrument",
    "describe_operators",
    "engine_status",
    "get_current_instrument",
    "get_diagnostics",
    "send",
    "swap",
    "validate",
  ]);
});
