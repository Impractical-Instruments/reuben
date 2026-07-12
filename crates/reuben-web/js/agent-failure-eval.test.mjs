// agent-failure-eval.test.mjs — the merge-gating half of issue #361's failure taxonomy (spec §5),
// driven through the REAL loop (agent-host.mjs), the REAL in-page tool layer (tools.mjs), and the
// REAL turn envelope (agent-turn.mjs) over a scripted/authored model transport (the same mocking
// pattern js/agent-policy-eval.test.mjs established — no network, no key). This proves the two
// failure behaviors that are properties of the TOOL ROUND-TRIP loop (not of a live model's
// judgement, which lives in js/live-eval.mjs):
//
//   case 3 — {ok:false} validation failure → the agent SELF-CORRECTS SILENTLY: a {ok:false}
//     validate/swap is the tool WORKING (ADR-0048 §3), flows back as an ordinary result (never
//     is_error), the model repairs within its turn, and NO Diag ever reaches the user-facing plan;
//   case 3 exhausted → collapse into case 2's "can't" shape: when the model gives up, the prior
//     sound is untouched (ADR-0048 §5 — {ok:false} installs nothing) and the terminal line is plain
//     language carrying no Diag / engine word.
//
// The cases that ride D's change-card (case 1's reading + alternative chips) and the plain
// chat-turn containers (case 2/4) are UI observables — they are proven in web/tests/failure.spec.js
// against the rendered DOM, exactly as #358's change-card spec drives crafted envelopes.
//
// Run: `cd crates/reuben-web && node --test js/agent-failure-eval.test.mjs`

import test from "node:test";
import assert from "node:assert";

import { createToolLayer } from "./tools.mjs";
import { createAgentHost } from "./agent-host.mjs";
import { stubTranscript } from "./agent-turn.mjs";
import { scanForbiddenTerms } from "../proxy/system-prompt.mjs";

// --- fakes (mirroring agent-policy-eval.test.mjs) ----------------------------------------

function makeEngine({ bundle = null } = {}) {
  const calls = { send: [], loadBundle: [] };
  return {
    context: { state: "running" },
    node: {},
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

// The offending node the agent's OWN mistaken document trips — the Diag the user must never see.
const DIAG = { node: "/osc", port: "freq", message: "unknown operator type 'oscilllator'" };

// A validate that keys on the document text: a doc naming the typo'd type reports {ok:false} with a
// node/port Diag (the agent's own mistake, ADR-0048 §3); anything else validates clean. This is the
// same shape the real wasm `validate` export returns (js/check.mjs proves it end-to-end).
const makeIntrospect = () => ({
  describeOperators: () => [{ type_name: "oscillator" }, { type_name: "delay" }],
  describeInstrument: () => ({ instrument: "probe", inputs: [], outputs: [] }),
  validate: (docText) =>
    docText.includes("oscilllator")
      ? { ok: false, errors: [DIAG], warnings: [] }
      : { ok: true, errors: [], warnings: [] },
  contentHash: (docText) => `hash(${docText})`,
});

const BEFORE_DOC = {
  nodes: [
    { type: "oscillator", address: "/osc", inputs: { freq: 220 } },
    { type: "output", address: "/out", inputs: { audio: { from: "/osc" } } },
  ],
};
// The agent's first, WRONG attempt (typo'd operator type — fails validate).
const BAD_DOC = { nodes: [{ type: "oscilllator", address: "/osc" }] };
// The repaired attempt (validates clean, adds a layer).
const GOOD_DOC = {
  nodes: [
    { type: "oscillator", address: "/osc", inputs: { freq: 220 } },
    { type: "output", address: "/out", inputs: { audio: { from: "/osc" } } },
    { type: "delay", address: "/shimmer", inputs: { in: { from: "/osc" } } },
  ],
};
const bundleOf = (doc) => ({ docText: JSON.stringify(doc), resources: [] });

// --- scripted transport (identical to agent-policy-eval.test.mjs) -------------------------

function mockTransport(scripts) {
  return async () => {
    const script = scripts.shift();
    if (!script) throw new Error("mock transport exhausted (test scripted fewer rounds than the loop needed)");
    return (async function* () {
      for (const ev of script) yield ev;
    })();
  };
}

const msgStart = { type: "message_start" };
const msgStop = { type: "message_stop" };
const textStart = (i) => ({ type: "content_block_start", index: i, content_block: { type: "text", text: "" } });
const textDelta = (i, text) => ({ type: "content_block_delta", index: i, delta: { type: "text_delta", text } });
const blockStop = (i) => ({ type: "content_block_stop", index: i });
const toolStartInline = (i, id, name, input) => ({
  type: "content_block_start",
  index: i,
  content_block: { type: "tool_use", id, name, input },
});
const stopReason = (reason) => ({ type: "message_delta", delta: { stop_reason: reason } });

const textOnlyRound = (text) => [
  msgStart,
  textStart(0),
  textDelta(0, text),
  blockStop(0),
  stopReason("end_turn"),
  msgStop,
];
const toolRound = (narration, id, name, input) => [
  msgStart,
  textStart(0),
  textDelta(0, narration),
  blockStop(0),
  toolStartInline(1, id, name, input),
  blockStop(1),
  stopReason("tool_use"),
  msgStop,
];

// --- case 3: {ok:false} → silent self-correction; the user never sees a Diag ---------------

test("case 3: a {ok:false} swap is repaired WITHIN the turn — only the good document installs", async () => {
  const engine = makeEngine({ bundle: bundleOf(BEFORE_DOC) });
  const toolLayer = createToolLayer({ engine, introspect: makeIntrospect() });
  const transport = mockTransport([
    // round 1: narrate sensory, attempt the WRONG document → validate {ok:false}
    toolRound("Adding a touch of shimmer.", "tu-1", "swap", { document: BAD_DOC }),
    // round 2: (the model saw {ok:false}) silently repair → the GOOD document installs
    toolRound("Getting the shimmer just right.", "tu-2", "swap", { document: GOOD_DOC }),
    // round 3: land the turn
    textOnlyRound("Added a bright shimmer over the top."),
  ]);
  const host = createAgentHost({ toolLayer, transport, transcript: stubTranscript() });

  const resolved = await host.send("add a shimmer");

  // {ok:false} installs NOTHING (ADR-0048 §5) — only the repaired document reached the engine.
  assert.strictEqual(engine._calls.loadBundle.length, 1, "exactly one install — the {ok:false} attempt installed nothing");

  // Both swaps ran; the failing one is the tool WORKING, not is_error (ADR-0048 §3).
  const swaps = resolved.toolLog.filter((t) => t.name === "swap");
  assert.strictEqual(swaps.length, 2, "both swap attempts are recorded");
  assert.strictEqual(swaps[0].isError, false, "a {ok:false} swap is a successful call, never is_error");
  assert.strictEqual(swaps[0].result.ok, false, "the first attempt's report is {ok:false}");
  assert.strictEqual(swaps[1].result.ok, true, "the repair validated + installed");

  // The user meets only the eventual success: a normal resolved turn with the structural diff.
  assert.strictEqual(resolved.status, "resolved");
  assert.ok(resolved.diff, "the resolved turn carries the successful diff (§4.6)");
});

test("case 3: no Diag (node/port/message) ever reaches the user-facing plan", async () => {
  const engine = makeEngine({ bundle: bundleOf(BEFORE_DOC) });
  const toolLayer = createToolLayer({ engine, introspect: makeIntrospect() });
  const transport = mockTransport([
    toolRound("Adding a touch of shimmer.", "tu-1", "swap", { document: BAD_DOC }),
    toolRound("Getting the shimmer just right.", "tu-2", "swap", { document: GOOD_DOC }),
    textOnlyRound("Added a bright shimmer over the top."),
  ]);
  const host = createAgentHost({ toolLayer, transport, transcript: stubTranscript() });

  const resolved = await host.send("add a shimmer");

  // The Diag[] is agent-internal fuel: it rides the toolLog (provenance) but NONE of its text — the
  // node address, the port, or the message — may appear in what the user reads.
  assert.deepStrictEqual(scanForbiddenTerms(resolved.plan), [], `plan leaked engine vocabulary: "${resolved.plan}"`);
  assert.ok(!resolved.plan.includes(DIAG.node), "the offending node address never surfaces");
  assert.ok(!resolved.plan.includes(DIAG.message), "the raw Diag message never surfaces");
  assert.ok(!/oscilllator/i.test(resolved.plan), "the internal type name never surfaces");
});

// --- case 3 exhausted → collapse into case 2's "can't" shape (spec §5.1 / §5.3) ------------

test("case 3 exhausted: the model gives up → prior sound untouched, terminal line is plain", async () => {
  const engine = makeEngine({ bundle: bundleOf(BEFORE_DOC) });
  const toolLayer = createToolLayer({ engine, introspect: makeIntrospect() });
  const transport = mockTransport([
    // two failing attempts...
    toolRound("Working in that change.", "tu-1", "swap", { document: BAD_DOC }),
    toolRound("Trying that a different way.", "tu-2", "swap", { document: BAD_DOC }),
    // ...then the model collapses to the plain "can't make that stick" shape (case 2), NO Diag.
    textOnlyRound("I couldn't make that change stick — the sound's still going as it was."),
  ]);
  const host = createAgentHost({ toolLayer, transport, transcript: stubTranscript() });

  const resolved = await host.send("do the impossible");

  // Reshape terminal failure KEEPS the prior sound (ADR-0048 §5): nothing installed.
  assert.strictEqual(engine._calls.loadBundle.length, 0, "no install — the old sound survives");
  assert.strictEqual(resolved.diff, null, "no diff — nothing changed on the surface");
  // The terminal message is plain language, no Diag, no engine word.
  assert.deepStrictEqual(scanForbiddenTerms(resolved.plan), [], `terminal line leaked engine vocabulary: "${resolved.plan}"`);
  assert.ok(!resolved.plan.includes(DIAG.node) && !resolved.plan.includes(DIAG.message), "no Diag in the terminal line");
});
