// End-to-end tests for the streaming conversation loop (issue #354) — the ticket's Verification
// section, over a DETERMINISTIC MOCK model (no network, no key; the merge-gating path). The mock
// transport emits a scripted Anthropic event stream (text deltas + tool_use for `send` and for
// `swap`); the loop dispatches into the REAL in-page tool layer (js/tools.mjs) over a fake
// engine/introspect. Asserts: a typed turn drives send AND swap through the layer and streams a
// resolved turn envelope back; a tool error surfaces to the AGENT (not the user); streaming renders
// INCREMENTALLY (multiple deltas, not one blob).
//
// Run: `cd crates/reuben-web && node --test js/agent-host.test.mjs`

import test from "node:test";
import assert from "node:assert";

import { createToolLayer } from "./tools.mjs";
import { createAgentHost } from "./agent-host.mjs";
import { stubTranscript } from "./agent-turn.mjs";

// --- fakes (mirroring js/tools.test.mjs) -------------------------------------------------

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

function makeIntrospect({ validateReport = { ok: true, errors: [], warnings: [] } } = {}) {
  return {
    describeOperators: () => [{ type_name: "oscillator" }],
    describeInstrument: () => ({ instrument: "probe", inputs: [], outputs: [] }),
    validate: () => validateReport,
    contentHash: (docText) => `hash(${docText})`,
  };
}

const BEFORE_DOC = {
  nodes: [
    { type: "oscillator", address: "/osc", inputs: { freq: 220 } },
    { type: "output", address: "/out", inputs: { audio: { from: "/osc" } } },
  ],
};
const AFTER_DOC = {
  nodes: [
    { type: "oscillator", address: "/osc", inputs: { freq: 440 } },
    { type: "output", address: "/out", inputs: { audio: { from: "/osc" } } },
    { type: "oscillator", address: "/lfo", inputs: { freq: 5 } },
  ],
};
const bundleOf = (doc) => ({ docText: JSON.stringify(doc), resources: [] });

// --- mock transport: a scripted Anthropic event stream per round -------------------------

function mockTransport(scripts) {
  const calls = [];
  const t = async (messages) => {
    calls.push(JSON.parse(JSON.stringify(messages))); // snapshot for assertions
    const script = scripts.shift();
    if (!script) throw new Error("mock transport exhausted (loop ran more rounds than scripted)");
    return (async function* () {
      for (const ev of script) yield ev;
    })();
  };
  t.calls = calls;
  return t;
}

// Event builders (the Anthropic message-stream wire shape).
const msgStart = { type: "message_start" };
const msgStop = { type: "message_stop" };
const textStart = (i) => ({ type: "content_block_start", index: i, content_block: { type: "text", text: "" } });
const textDelta = (i, text) => ({ type: "content_block_delta", index: i, delta: { type: "text_delta", text } });
const blockStop = (i) => ({ type: "content_block_stop", index: i });
const toolStart = (i, id, name) => ({
  type: "content_block_start",
  index: i,
  content_block: { type: "tool_use", id, name },
});
const inputJson = (i, partial_json) => ({
  type: "content_block_delta",
  index: i,
  delta: { type: "input_json_delta", partial_json },
});
const toolStartInline = (i, id, name, input) => ({
  type: "content_block_start",
  index: i,
  content_block: { type: "tool_use", id, name, input },
});
const stopReason = (reason) => ({ type: "message_delta", delta: { stop_reason: reason } });

test("a typed turn drives send AND swap through the tool layer and streams a resolved envelope", async () => {
  const engine = makeEngine({ bundle: bundleOf(BEFORE_DOC) });
  const introspect = makeIntrospect();
  const toolLayer = createToolLayer({ engine, introspect });
  const transcript = stubTranscript();

  // send's input arrives as streamed input_json_delta fragments; swap's arrives inline — exercises
  // both accumulation paths.
  const sendJson = JSON.stringify({ messages: [{ address: "/voice1/cutoff", args: [1200] }] });
  const mid = Math.floor(sendJson.length / 2);

  const transport = mockTransport([
    // Round 1: stream a plan, then a tool_use for send and one for swap.
    [
      msgStart,
      textStart(0),
      textDelta(0, "Warming "),
      textDelta(0, "it "),
      textDelta(0, "up"),
      blockStop(0),
      toolStart(1, "tu-send", "send"),
      inputJson(1, sendJson.slice(0, mid)),
      inputJson(1, sendJson.slice(mid)),
      blockStop(1),
      toolStartInline(2, "tu-swap", "swap", { document: AFTER_DOC }),
      blockStop(2),
      stopReason("tool_use"),
      msgStop,
    ],
    // Round 2: the reshape lands.
    [msgStart, textStart(0), textDelta(0, "Done."), blockStop(0), stopReason("end_turn"), msgStop],
  ]);

  const host = createAgentHost({ toolLayer, transport, transcript });
  const resolved = await host.send("make it brighter");

  // The turn resolved in place (§4.2) and carries the streamed plan.
  assert.strictEqual(resolved.status, "resolved");
  assert.match(resolved.plan, /Warming it up/);
  assert.match(resolved.plan, /Done\./);

  // send went through the real layer to the engine.
  assert.strictEqual(engine._calls.send.length, 1);
  assert.deepStrictEqual(engine._calls.send[0], { address: "/voice1/cutoff", args: [1200] });

  // swap installed by value and the resolved envelope carries the structural diff (§4.6).
  assert.strictEqual(engine._calls.loadBundle.length, 1);
  assert.strictEqual(engine._calls.loadBundle[0].docText, JSON.stringify(AFTER_DOC));
  assert.ok(resolved.diff, "a resolved swap attaches its diff to the turn");
  assert.strictEqual(resolved.diff.survived, 0, "restart-swap: survived is ALWAYS 0");
  assert.deepStrictEqual(resolved.diff.added, ["/lfo"]);
  assert.deepStrictEqual(resolved.diff.changed, ["/osc"]);
  assert.deepStrictEqual(resolved.diff.removed, []);

  // The tool round-trips are recorded as provenance, neither errored.
  assert.deepStrictEqual(resolved.toolLog.map((t) => t.name), ["send", "swap"]);
  assert.ok(resolved.toolLog.every((t) => t.isError === false));

  // The second model request carried the tool_results back (the loop fed them to the agent).
  const secondReq = transport.calls[1];
  const toolResultTurn = secondReq[secondReq.length - 1];
  assert.strictEqual(toolResultTurn.role, "user");
  const ids = toolResultTurn.content.map((b) => b.tool_use_id).sort();
  assert.deepStrictEqual(ids, ["tu-send", "tu-swap"]);
});

test("a tool error surfaces to the AGENT (a tool_result), never to the user", async () => {
  // An unreachable engine makes `send` throw (isError, ADR-0048 §3). The loop must feed that back
  // to the model as a tool_result with is_error — and must NOT leak it into the user-facing plan.
  const engine = makeEngine({ hasNode: false }); // not reachable
  const toolLayer = createToolLayer({ engine, introspect: makeIntrospect() });
  const transcript = stubTranscript();

  const transport = mockTransport([
    [
      msgStart,
      textStart(0),
      textDelta(0, "Let me nudge it."),
      blockStop(0),
      toolStartInline(1, "tu-send", "send", { messages: [{ address: "/a", args: [1] }] }),
      blockStop(1),
      stopReason("tool_use"),
      msgStop,
    ],
    [msgStart, textStart(0), textDelta(0, "Hm, try again?"), blockStop(0), stopReason("end_turn"), msgStop],
  ]);

  const host = createAgentHost({ toolLayer, transport, transcript });
  const resolved = await host.send("nudge it");

  // Nothing was dispatched (the layer refused an unreachable send).
  assert.strictEqual(engine._calls.send.length, 0);

  // The error went to the AGENT: the second request's tool_result carries is_error + the reason.
  const secondReq = transport.calls[1];
  const toolResultTurn = secondReq[secondReq.length - 1];
  const tr = toolResultTurn.content.find((b) => b.tool_use_id === "tu-send");
  assert.strictEqual(tr.is_error, true);
  assert.match(tr.content, /not reachable/);

  // The error is provenance in toolLog, NOT user-facing copy: the resolved plan is model text only.
  assert.strictEqual(resolved.toolLog[0].isError, true);
  assert.doesNotMatch(resolved.plan, /not reachable/);
  assert.match(resolved.plan, /Let me nudge it\.Hm, try again\?/);
});

test("streaming renders INCREMENTALLY — multiple deltas, not one final blob (§4.2)", async () => {
  const toolLayer = createToolLayer({ engine: makeEngine(), introspect: makeIntrospect() });
  const transcript = stubTranscript();

  const tokens = ["A", "warm", " ", "round", " ", "pad"];
  const transport = mockTransport([
    [
      msgStart,
      textStart(0),
      ...tokens.map((tok) => textDelta(0, tok)),
      blockStop(0),
      stopReason("end_turn"),
      msgStop,
    ],
  ]);

  const host = createAgentHost({ toolLayer, transport, transcript });
  const resolved = await host.send("a warm pad");

  // One delta per streamed token — the card can render the plan AS it grows (not on completion).
  assert.strictEqual(transcript.deltas.length, tokens.length);
  assert.ok(transcript.deltas.length > 1, "must stream incrementally, not as a single blob");
  assert.deepStrictEqual(
    transcript.deltas.map((d) => d.text),
    tokens,
  );
  assert.strictEqual(resolved.plan, tokens.join(""));
});

// --- restart-honesty gating (issue #356, spec §6.4) --------------------------------------

test("the FIRST genuine restart of already-playing sound in a session carries F's line; the second does not (§6.4)", async () => {
  // Bundle starts loaded (BEFORE_DOC): every swap below restarts an ALREADY-PLAYING sound, so
  // `restarted` is true both times — only the ONCE-PER-SESSION gate should tell them apart.
  const engine = makeEngine({ bundle: bundleOf(BEFORE_DOC) });
  const toolLayer = createToolLayer({ engine, introspect: makeIntrospect() });

  const swapRound = (id, doc) => [
    msgStart,
    textStart(0),
    textDelta(0, "Updating."),
    blockStop(0),
    toolStartInline(1, id, "swap", { document: doc }),
    blockStop(1),
    stopReason("tool_use"),
    msgStop,
  ];
  const landRound = [msgStart, textStart(0), textDelta(0, "Done."), blockStop(0), stopReason("end_turn"), msgStop];

  // One host = one session (agent-host.mjs's `restartHonestyGiven` closure). Two `send()` calls
  // on the SAME instance, each with a structural swap that genuinely restarts playing sound.
  const host = createAgentHost({
    toolLayer,
    transport: mockTransport([swapRound("tu-1", BEFORE_DOC), landRound, swapRound("tu-2", AFTER_DOC), landRound]),
    transcript: stubTranscript(),
  });

  const firstTurn = await host.send("bring back the old layer");
  assert.strictEqual(
    firstTurn.restartHonesty,
    "Here's the new version, from the top.",
    "the session's first genuine restart carries the line",
  );

  const secondTurn = await host.send("add the layer again");
  assert.strictEqual(secondTurn.restartHonesty, null, "the session's second restart stays wordless (§6.4)");
});

// Unlike `makeEngine` (a STATIC fake — `currentBundle()` never reflects a prior `loadBundle`,
// fine for the single-swap-per-test cases above), this scenario needs the SECOND swap to see the
// FIRST swap's install — so this local fake actually tracks state, mirroring the real engine.
function makeStatefulEngine({ bundle = null, contextState = "running", hasNode = true } = {}) {
  const calls = { send: [], loadBundle: [] };
  let current = bundle;
  return {
    context: { state: contextState },
    node: hasNode ? {} : null,
    send(address, args) {
      calls.send.push({ address, args });
    },
    async loadBundle(arg) {
      calls.loadBundle.push(arg);
      current = { docText: arg.docText, resources: arg.resources ?? [] };
    },
    currentBundle() {
      return current;
    },
    _calls: calls,
  };
}

test("a first install into silence never carries the line, and doesn't spend the once-per-session slot", async () => {
  const engine = makeStatefulEngine({ bundle: null }); // nothing loaded yet
  const toolLayer = createToolLayer({ engine, introspect: makeIntrospect() });

  const swapRound = (id, doc) => [
    msgStart,
    textStart(0),
    textDelta(0, "Building."),
    blockStop(0),
    toolStartInline(1, id, "swap", { document: doc }),
    blockStop(1),
    stopReason("tool_use"),
    msgStop,
  ];
  const landRound = [msgStart, textStart(0), textDelta(0, "Ready."), blockStop(0), stopReason("end_turn"), msgStop];

  const host = createAgentHost({
    toolLayer,
    transport: mockTransport([
      swapRound("tu-a", BEFORE_DOC), // first creation: nothing was sounding -> no restart to be honest about
      landRound,
      swapRound("tu-b", AFTER_DOC), // NOW something is playing -> this IS the session's first restart
      landRound,
    ]),
    transcript: stubTranscript(),
  });

  const created = await host.send("make me a pad");
  assert.strictEqual(created.restartHonesty, null, "first install into silence carries no line (§6.4)");

  const reshaped = await host.send("add a layer");
  assert.strictEqual(
    reshaped.restartHonesty,
    "Here's the new version, from the top.",
    "the slot was NOT spent by the silent first install, so this genuine restart still gets it",
  );
});
