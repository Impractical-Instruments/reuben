// live-eval.mjs — NON-BLOCKING live smoke of the streaming loop against real Sonnet-5 (issue #354).
//
// This is NOT a merge gate. It is run by the self-gated `web-chat-live-eval` CI job, which skips
// green when `secrets.ANTHROPIC_API_KEY` is absent (mirroring the `deploy-web` job's shape). It
// wires the REAL relay (proxy/relay.mjs) → real Anthropic → the REAL agent-host loop over a FAKE
// engine (no wasm/browser needed), and asserts the loop runs end-to-end and drives at least one
// tool through the in-page layer. Assertions are deliberately lenient — the authoring policy that
// makes tool-use reliable is issue #356; here we only prove the plumbing carries a live turn.
//
// Run: `ANTHROPIC_API_KEY=… node js/live-eval.mjs` (from crates/reuben-web).
// Exit 0 = pass or skip (no key); exit 1 = the live loop failed.

import { readFileSync } from "node:fs";

import { createToolLayer } from "./tools.mjs";
import { createAgentHost, sseEvents } from "./agent-host.mjs";
import { stubTranscript } from "./agent-turn.mjs";
import { createRelay } from "../proxy/relay.mjs";
import { SYSTEM_PROMPT_PLACEHOLDER } from "../proxy/system-prompt.mjs";

const apiKey = process.env.ANTHROPIC_API_KEY;
if (!apiKey) {
  console.log("[live-eval] ANTHROPIC_API_KEY not set — skipping (green).");
  process.exit(0);
}

const artifact = JSON.parse(
  readFileSync(new URL("./tool-schemas.generated.json", import.meta.url), "utf8"),
);
const model = process.env.REUBEN_CHAT_MODEL || artifact.model_default;

// A tiny starting instrument the model can inspect + reshape (no engine needed to smoke the loop).
const START_DOC = {
  nodes: [
    { type: "oscillator", address: "/osc", inputs: { freq: 220 } },
    { type: "output", address: "/out", inputs: { audio: { from: "/osc" } } },
  ],
};

function fakeEngine() {
  const calls = { send: [], loadBundle: [] };
  let bundle = { docText: JSON.stringify(START_DOC), resources: [] };
  return {
    context: { state: "running" },
    node: {},
    send(address, args) {
      calls.send.push({ address, args });
    },
    async loadBundle(arg) {
      calls.loadBundle.push(arg);
      bundle = { docText: arg.docText, resources: arg.resources ?? [] };
    },
    currentBundle() {
      return bundle;
    },
    _calls: calls,
  };
}

// A permissive fake introspect: every document validates, hashes are deterministic — enough for a
// plumbing smoke (the engine is the real validation authority in production, not this fake).
const fakeIntrospect = {
  describeOperators: () => artifact.instrument_schema?.$defs?.node?.properties?.type?.enum
    ? artifact.instrument_schema.$defs.node.properties.type.enum.map((type_name) => ({ type_name }))
    : [{ type_name: "oscillator" }, { type_name: "output" }],
  describeInstrument: () => ({ instrument: "smoke", inputs: [], outputs: [] }),
  validate: () => ({ ok: true, errors: [], warnings: [] }),
  contentHash: (docText) => `h${docText.length}`,
};

async function main() {
  const engine = fakeEngine();
  const toolLayer = createToolLayer({ engine, introspect: fakeIntrospect });
  const transcript = stubTranscript();

  const relay = createRelay({ apiKey, systemPrompt: SYSTEM_PROMPT_PLACEHOLDER, tools: artifact.tools, model });
  const transport = async (messages) => {
    const res = await relay({ messages });
    if (!res.ok) throw new Error(`relay responded ${res.status}`);
    return sseEvents(res);
  };

  const host = createAgentHost({ toolLayer, transport, transcript, maxRounds: 10 });

  // An explicit ask that should drive tool-use even without the #356 authoring policy.
  const resolved = await host.send(
    "You have tools to inspect and reshape a live sound. Call get_current_instrument to read " +
      "what's playing, then use send to audition a brighter cutoff, then swap to make a small " +
      "change permanent. Do it now with the tools, then tell me in one sentence.",
  );

  const toolNames = resolved.toolLog.map((t) => t.name);
  console.log(`[live-eval] model=${model} status=${resolved.status} tools=[${toolNames.join(", ")}]`);
  console.log(`[live-eval] deltas streamed=${transcript.deltas.length}`);
  console.log(`[live-eval] plan: ${resolved.plan.slice(0, 200)}`);

  assert(resolved.status === "resolved", "the live turn must resolve");
  assert(transcript.deltas.length > 0, "the live turn must stream tokens");
  assert(resolved.toolLog.length > 0, "the live loop must drive at least one tool through the layer");
  console.log("[live-eval] PASS");
}

function assert(cond, msg) {
  if (!cond) {
    console.error(`[live-eval] FAIL: ${msg}`);
    process.exit(1);
  }
}

main().catch((err) => {
  console.error("[live-eval] FAIL:", err && err.stack ? err.stack : err);
  process.exit(1);
});
