// live-eval.mjs — NON-BLOCKING live smoke of the streaming loop against real Sonnet-5 (issue #354).
//
// This is the ONE CI job that spends real Anthropic tokens (ADR-0054 §5), so it holds ONLY what a
// mock cannot stand in for: the real relay → Sonnet-5 → SSE → in-page tool layer path, driven
// end-to-end. Everything DETERMINISTIC is already covered — with no tokens — by the mock-transport
// tests in the `web` job, and re-checking it against a live model buys nothing but spend:
//   - the loop's tool dispatch + incremental streaming — js/agent-host.test.mjs
//   - send-vs-swap routing + verbatim chip posting     — js/agent-policy-eval.test.mjs (ROUTING_BATTERY)
//   - the once-per-session restart-honesty latch (§6.4) — js/agent-host.test.mjs ("restart-honesty
//     gating"): it is a plain closure flag in agent-host.mjs (`restartHonestyGiven`), so "at most one
//     line per session" holds BY CONSTRUCTION and is proven there exactly-once, ordered, plus the
//     install-into-silence edge — a live ≤1 re-check is structurally incapable of adding signal.
// So this file was slimmed from a four-turn behavioral battery down to the single turn that
// genuinely requires a live model. Do NOT re-add turns to "restore coverage" — the coverage lives
// in the mock tests above; extra live turns only raise the token bill.
//
// NOT a merge gate. Run by the self-gated `web-chat-live-eval` CI job, which skips green when
// `secrets.ANTHROPIC_API_KEY` is absent (mirroring `deploy-web`), and is excluded from `ci-passed`'s
// needs — a regression here shows red in the job's own log but never blocks a merge on live variance.
//
// The single turn: one explicit ask drives the real model through the whole loop — read → send →
// swap → one-sentence reply (~4 rounds). Because the loop re-sends the accumulated history +
// tool_result blocks on every round, this one turn also proves the real API accepts follow-up
// requests carrying tool results (the "second request" failure relay.mjs' thinking note warns of).
// Hard assertions: the turn resolves, streams tokens, and drives at least one tool through the layer.
// Soft (logged, never fails): the forbidden-word scan (§1) — a real-model word-choice leak is WARNED,
// not failed, since the HARD lexicon gate lives at the DOM (tripsLexicon) and language hardening is
// deferred to #362. It stays because a live model's narration is the one thing a mock can't observe.
//
// Run: `ANTHROPIC_API_KEY=… node js/live-eval.mjs` (from crates/reuben-web).
// Exit 0 = pass or skip (no key); exit 1 = a genuine live-loop failure (a forbidden-word leak is a
// logged warning, not an exit-1 condition).

import { readFileSync } from "node:fs";

import { createToolLayer } from "./tools.mjs";
import { createAgentHost, sseEvents } from "./agent-host.mjs";
import { stubTranscript } from "./agent-turn.mjs";
import { createRelay } from "../proxy/relay.mjs";
import { SYSTEM_PROMPT, scanForbiddenTerms } from "../proxy/system-prompt.mjs";

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

  // Cost caps for the non-blocking smoke (this job is billed to us): `maxTokens` is a plumbing
  // smoke, not a real reshape, so 2048 is ample for a tool call + one sentence while bounding a
  // runaway generation — the production default (8192) stays put in relay.mjs. Caching (relay.mjs'
  // cacheControl default) makes the re-sent prefix cheap across rounds.
  const relay = createRelay({
    apiKey,
    systemPrompt: SYSTEM_PROMPT,
    tools: artifact.tools,
    model,
    maxTokens: 2048,
  });
  const transport = async (messages) => {
    const res = await relay({ messages });
    if (!res.ok) {
      // Surface the upstream error body (the Anthropic {type:"error", error:{...}} payload the
      // relay passes straight through) so a non-2xx names the exact failing field in the
      // non-blocking live-eval CI log — not just a bare status code.
      const body = await res.text().catch(() => "");
      throw new Error(`relay responded ${res.status}: ${body.slice(0, 900)}`);
    }
    return sseEvents(res);
  };

  // The asserted path is get_current_instrument → send → swap → one-sentence reply (~4 rounds); 6
  // leaves slack for model variance while capping the loop's round count (and thus its output-token
  // bill — output is ~5x input). Hitting the cap still resolves the turn (agent-host.mjs), so the
  // lenient assertions below hold either way.
  const host = createAgentHost({ toolLayer, transport, transcript, maxRounds: 6 });

  // The single live turn: an explicit ask that drives the real model through the whole loop — read,
  // audition via send, make it permanent via swap, then narrate in one sentence. This is the ONE
  // thing a mock can't stand in for. The loop re-sends the accumulated history + tool_result blocks
  // on every round, so this turn also proves the real API accepts follow-up requests with tool
  // results — no separate multi-turn session is needed to exercise that.
  const smoke = await host.send(
    "You have tools to inspect and reshape a live sound. Call get_current_instrument to read " +
      "what's playing, then use send to audition a brighter cutoff, then swap to make a small " +
      "change permanent. Do it now with the tools, then tell me in one sentence.",
  );
  const toolNames = smoke.toolLog.map((t) => t.name);
  console.log(`[live-eval] model=${model} status=${smoke.status} tools=[${toolNames.join(", ")}]`);
  console.log(`[live-eval] plan: ${smoke.plan.slice(0, 300)}`);

  assert(smoke.status === "resolved", "the live turn must resolve");
  assert(transcript.deltas.length > 0, "the live turn must stream tokens");
  assert(smoke.toolLog.length > 0, "the live loop must drive at least one tool through the layer");

  // Soft (logged, never fails): scan the real model's plan for engine vocabulary (§1). A leak is a
  // WARNING, not a failure — the HARD lexicon gate lives at the DOM (tripsLexicon) and real-model
  // language hardening is deferred to #362's acceptance bar. Kept because a live model's word choice
  // is the one thing a mock can't observe; everything deterministic is owned by the mock tests.
  const leaks = scanForbiddenTerms(smoke.plan);
  if (leaks.length > 0) {
    console.warn(
      `[live-eval] WARN: forbidden word(s) leaked to the user — non-fatal, language hardening ` +
        `deferred to #362: ${leaks.join(", ")} in "${smoke.plan.slice(0, 200)}"`,
    );
  } else {
    console.log("[live-eval] forbidden-word scan: clean");
  }

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
