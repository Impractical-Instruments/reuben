// live-eval.mjs — NON-BLOCKING live smoke of the streaming loop against real Sonnet-5 (issue #354),
// extended (issue #356) into a small live BEHAVIORAL battery — the one place any of #356's
// Verification criteria can be checked against what a real model actually does, rather than a
// scripted transport we authored ourselves (js/agent-policy-eval.test.mjs, the merge-gating half,
// can only prove the plumbing carries the policy — it cannot prove Sonnet-5 obeys it).
//
// This is NOT a merge gate. It is run by the self-gated `web-chat-live-eval` CI job, which skips
// green when `secrets.ANTHROPIC_API_KEY` is absent (mirroring the `deploy-web` job's shape), and
// is deliberately excluded from `ci-passed`'s needs — a real regression here should still show
// red in the job's own log, but never blocks a merge on live-model variance.
//
// Session shape: ONE host (one conversation) carries three turns, so register/restart state is
// genuinely session-scoped exactly like production:
//   1. an explicit tool-use smoke (unchanged from #354) — proves the plumbing still carries a turn.
//   2. a plain parameter-style ask ("make it brighter") — should read as a live, no-restart update.
//   3. a structural ask ("add a delay") — should restart, and AT MOST ONE turn in the whole
//      session may carry the restart-honesty line (§6.4), whichever turn actually first restarts
//      already-playing sound.
// Hard assertions: the loop resolves, streams, and drives at least one tool through the layer; and
// the once-per-session restart-honesty gate (§6.4) — at most one turn carries the line. Soft (logged,
// not asserted): the forbidden-word scan (§1) — a leak is WARNED, never failed, since real-model
// language hardening is deferred to #362's acceptance bar (the hard §1 gate lives at the DOM,
// tripsLexicon); plus which tool ran per turn and the plan text itself, for the human tone read-through.
//
// Run: `ANTHROPIC_API_KEY=… node js/live-eval.mjs` (from crates/reuben-web).
// Exit 0 = pass or skip (no key); exit 1 = a genuine live-loop failure or the restart-honesty gate
// over-firing (a forbidden-word leak is a logged warning, not an exit-1 condition).

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
  // bill — output is ~5x input) well under the old 10. Hitting the cap still resolves the turn
  // (agent-host.mjs), so the lenient assertions below hold either way.
  const host = createAgentHost({ toolLayer, transport, transcript, maxRounds: 6 });

  const turns = [];
  async function runTurn(label, text) {
    const resolved = await host.send(text);
    const toolNames = resolved.toolLog.map((t) => t.name);
    console.log(`[live-eval] [${label}] model=${model} status=${resolved.status} tools=[${toolNames.join(", ")}]`);
    console.log(`[live-eval] [${label}] plan: ${resolved.plan.slice(0, 300)}`);
    if (resolved.restartHonesty) console.log(`[live-eval] [${label}] restartHonesty: "${resolved.restartHonesty}"`);
    turns.push({ label, resolved });
    return resolved;
  }

  // Turn 1: an explicit ask that should drive tool-use even without the #356 authoring policy
  // (the original #354 plumbing smoke — kept so a regression in the loop itself still shows here).
  const smoke = await runTurn(
    "plumbing-smoke",
    "You have tools to inspect and reshape a live sound. Call get_current_instrument to read " +
      "what's playing, then use send to audition a brighter cutoff, then swap to make a small " +
      "change permanent. Do it now with the tools, then tell me in one sentence.",
  );
  assert(smoke.status === "resolved", "the live turn must resolve");
  assert(transcript.deltas.length > 0, "the live turn must stream tokens");
  assert(smoke.toolLog.length > 0, "the live loop must drive at least one tool through the layer");

  // Turn 2: a plain parameter-style ask — §6.1's routing policy, unprompted (no tool named).
  await runTurn("parameter-ask", "make it a bit brighter");

  // Turn 3: a structural ask — should restart; exercises §6.4's honesty-line gate.
  await runTurn("structural-ask", "add a slow, wobbly delay");

  // Soft (a): scan every turn's plan for a forbidden engine word (§1) across the session.
  const leaks = turns.flatMap(({ label, resolved }) =>
    scanForbiddenTerms(resolved.plan).map((term) => `${label}: "${term}" in "${resolved.plan.slice(0, 200)}"`),
  );
  // Forbidden-word leaks are a WARNING, not a failure. The real model's narration is a
  // best-effort, probabilistic surface; the HARD lexicon gate lives at the DOM (tripsLexicon).
  // Real-model language hardening is deferred to #362's acceptance bar, so we surface leaks
  // loudly in the log (a regression stays visible) but never fail the job on live-model variance.
  if (leaks.length > 0) {
    console.warn(
      `[live-eval] WARN: forbidden word(s) leaked to the user — non-fatal, language hardening deferred to #362:\n${leaks.join("\n")}`,
    );
  }

  // Hard assertion (e): the once-per-session restart-honesty gate — at most one turn carries it.
  const restartTurns = turns.filter(({ resolved }) => resolved.restartHonesty).map(({ label }) => label);
  assert(
    restartTurns.length <= 1,
    `more than one turn carried the restart-honesty line this session: ${restartTurns.join(", ")}`,
  );

  console.log(
    leaks.length === 0
      ? `[live-eval] forbidden-word scan: clean across ${turns.length} turns`
      : `[live-eval] forbidden-word scan: ${leaks.length} leak(s) across ${turns.length} turns (warning only)`,
  );
  console.log(`[live-eval] restart-honesty line fired on: ${restartTurns.join(", ") || "(no structural restart this session)"}`);
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
