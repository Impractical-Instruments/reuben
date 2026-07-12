// agent-policy-eval.test.mjs — the merge-gating half of issue #356's eval harness: a BATTERY of
// user turns driven through the REAL loop (agent-host.mjs), the REAL in-page tool layer
// (tools.mjs), and the REAL turn envelope (agent-turn.mjs), over a scripted/authored model
// transport (the same mocking pattern js/agent-host.test.mjs already established — no network, no
// key). This proves the PLUMBING can carry every happy-path behavior #356 commits to:
//
//   (a) no forbidden word ever reaches the turn envelope's user-facing text
//   (b) a parameter-style ask routes through `send`; a structural ask routes through `swap`
//   (d) a chip's exact text posts verbatim as the user turn's plan
//
// What this file DELIBERATELY does not attempt:
//   (c) register ratcheting is a property of a live model's OWN judgement across a conversation —
//       it cannot be proven against a scripted transport we authored ourselves. What CAN be proven
//       here is that our own theory-aware vocabulary never collides with the forbidden list (below).
//       Real ratchet verification is `js/live-eval.mjs`'s job against a live model.
//   (e) the first-restart-only honesty line is fully covered as REAL CODE (not a scripted
//       assumption) in `js/agent-host.test.mjs` ("restart-honesty gating" tests) — not duplicated
//       here.
//
// Run: `cd crates/reuben-web && node --test js/agent-policy-eval.test.mjs`

import test from "node:test";
import assert from "node:assert";

import { createToolLayer } from "./tools.mjs";
import { createAgentHost } from "./agent-host.mjs";
import { stubTranscript } from "./agent-turn.mjs";
import { FORBIDDEN_TERMS, PLAIN_THEORY_PAIRS, scanForbiddenTerms } from "../proxy/system-prompt.mjs";

// --- fakes (mirroring agent-host.test.mjs / tools.test.mjs) ------------------------------

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

const makeIntrospect = () => ({
  describeOperators: () => [{ type_name: "oscillator" }],
  describeInstrument: () => ({ instrument: "probe", inputs: [], outputs: [] }),
  validate: () => ({ ok: true, errors: [], warnings: [] }),
  contentHash: (docText) => `hash(${docText})`,
});

const BEFORE_DOC = {
  nodes: [
    { type: "oscillator", address: "/osc", inputs: { freq: 220 } },
    { type: "output", address: "/out", inputs: { audio: { from: "/osc" } } },
  ],
};
const AFTER_DOC = {
  nodes: [
    { type: "oscillator", address: "/osc", inputs: { freq: 220 } },
    { type: "output", address: "/out", inputs: { audio: { from: "/osc" } } },
    { type: "delay", address: "/shimmer", inputs: { in: { from: "/osc" } } },
  ],
};
const bundleOf = (doc) => ({ docText: JSON.stringify(doc), resources: [] });

// --- scripted transport: an AUTHORED "policy-compliant" reply per round ------------------

function mockTransport(scripts) {
  const t = async () => {
    const script = scripts.shift();
    if (!script) throw new Error("mock transport exhausted (test scripted fewer rounds than the loop needed)");
    return (async function* () {
      for (const ev of script) yield ev;
    })();
  };
  return t;
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

// One round of plain text (no tool call), then end_turn.
const textOnlyRound = (text) => [
  msgStart,
  textStart(0),
  textDelta(0, text),
  blockStop(0),
  stopReason("end_turn"),
  msgStop,
];

// One round: narrate, then call exactly one tool; the NEXT round lands the turn.
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

// --- (a) + (b): routing battery ------------------------------------------------------------

const ROUTING_BATTERY = [
  {
    label: "a parameter-only ask ('brighter') routes through send, live, no restart",
    userText: "make it brighter",
    narration: "Brightening it up.",
    landing: "Brighter now.",
    tool: { name: "send", id: "tu-send", input: { messages: [{ address: "/osc/cutoff", args: [1800] }] } },
  },
  {
    label: "a structural ask ('add a shimmer') routes through swap and carries a diff",
    userText: "add a shimmer",
    narration: "Adding a touch of shimmer.",
    landing: "Added some shimmer.",
    tool: { name: "swap", id: "tu-swap", input: { document: AFTER_DOC } },
  },
];

for (const c of ROUTING_BATTERY) {
  test(`routing: ${c.label}`, async () => {
    const engine = makeEngine({ bundle: bundleOf(BEFORE_DOC) });
    const toolLayer = createToolLayer({ engine, introspect: makeIntrospect() });
    const transport = mockTransport([
      toolRound(c.narration, c.tool.id, c.tool.name, c.tool.input),
      textOnlyRound(c.landing),
    ]);
    const host = createAgentHost({ toolLayer, transport, transcript: stubTranscript() });

    const resolved = await host.send(c.userText);

    assert.deepStrictEqual(
      resolved.toolLog.map((t) => t.name),
      [c.tool.name],
      "exactly the expected tool ran",
    );
    // (a) no forbidden word anywhere in what the user actually sees.
    assert.deepStrictEqual(scanForbiddenTerms(resolved.plan), [], `plan text leaked engine vocabulary: "${resolved.plan}"`);

    if (c.tool.name === "send") {
      assert.strictEqual(engine._calls.send.length, 1, "send dispatched to the live engine");
      assert.strictEqual(engine._calls.loadBundle.length, 0, "a parameter-only reshape never installs a document");
      assert.strictEqual(resolved.diff, null, "no structural diff on a send-only turn");
    } else {
      assert.strictEqual(engine._calls.loadBundle.length, 1, "swap installed a document");
      assert.ok(resolved.diff, "a structural reshape carries the node-identity diff (§4.6)");
    }
  });
}

// --- (d): turn-one chips post verbatim -----------------------------------------------------

// Illustrative authored chips straight from spec §2.3 (Groovebox / Euclidean Drums / Chord Player).
const ILLUSTRATIVE_CHIPS = [
  "busier beat",
  "more swing",
  "deeper kick",
  "denser pattern",
  "add a tom",
  "switch to a minor key",
];

for (const chip of ILLUSTRATIVE_CHIPS) {
  test(`turn-one chip posts verbatim as the user's turn: "${chip}"`, async () => {
    const toolLayer = createToolLayer({ engine: makeEngine(), introspect: makeIntrospect() });
    const transport = mockTransport([textOnlyRound("Got it.")]);
    const host = createAgentHost({ toolLayer, transport, transcript: stubTranscript() });

    await host.send(chip);

    // The loop posts the raw text as the user message with NO transformation (agent-turn.mjs
    // `userTurn(text)` sets `plan: text` untouched) — "what you said is what happened" (spec §2.3).
    const userMessage = host.messages[0];
    assert.strictEqual(userMessage.role, "user");
    assert.strictEqual(userMessage.content, chip, "the chip text reaches the model byte-for-byte");
  });
}

// --- (c), partial: our OWN theory-aware vocabulary never collides with the forbidden list -----
// This does NOT prove a live model ratchets register correctly (see file header) — it proves the
// authored §1.2 pairs this policy leads with are internally consistent: nothing in the
// theory-aware column is itself on the never-say list (which would make the register ratchet
// self-contradicting), and vice versa for the plain column.

test("no plain-register term in §1.2's table is itself a forbidden word", () => {
  for (const pair of PLAIN_THEORY_PAIRS) {
    assert.deepStrictEqual(scanForbiddenTerms(pair.plain), [], `plain term for ${pair.dimension} collides with the forbidden list`);
  }
});

test("no theory-aware term in §1.2's table is itself a forbidden word", () => {
  for (const pair of PLAIN_THEORY_PAIRS) {
    assert.deepStrictEqual(
      scanForbiddenTerms(pair.theory),
      [],
      `theory-aware term for ${pair.dimension} collides with the forbidden list`,
    );
  }
});

test("scanForbiddenTerms catches inflected forms (plurals/verb forms), not just exact stems", () => {
  assert.deepStrictEqual(scanForbiddenTerms("I added a new Operators list"), ["operator"]);
  assert.deepStrictEqual(scanForbiddenTerms("the wires got patched"), ["patch", "wire"]);
  assert.deepStrictEqual(scanForbiddenTerms(`that's a nice ${FORBIDDEN_TERMS[0]}`), [FORBIDDEN_TERMS[0]]);
});

test("scanForbiddenTerms is clean on ordinary sensory copy", () => {
  const sensory = [
    "I made it brighter and a little warmer.",
    "Here's a snappier attack with a touch of shimmer.",
    "This instrument now has more notes ringing at once.",
    "Switched it into a minor key for a darker mood.",
  ];
  for (const line of sensory) {
    assert.deepStrictEqual(scanForbiddenTerms(line), [], `false positive on: "${line}"`);
  }
});
