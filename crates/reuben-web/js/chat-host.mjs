// chat-host.mjs — the BROWSER assembly of the streaming reshape loop (issue #397): the one seam
// epic #350 deliberately left un-wired. The node-side proof of this exact graph is js/live-eval.mjs
// (wasmIntrospect → createToolLayer → transport → createAgentHost); this mirrors it against the
// LIVE worklet engine + the in-page tool layer (#353), targeting the reachable hosted proxy
// (ADR-0054 §1/§2) at `/api/chat`.
//
// This module owns ONLY the host graph + one turn's streaming lifecycle. It is deliberately thin
// and UI-free: it runs a turn, streams the model's plan tokens to a caller-supplied `onPlanDelta`,
// and returns the resolved turn envelope (js/agent-turn.mjs). The ROUTING of that envelope into the
// change-card (§6.1 param `resolve` vs §6.2 structural `restrike`) and the value-sweep belong to the
// browser seams (web/src/main.js, web/src/chat/spine.js) — this module stays a pure loop so a test
// can drive it against a stubbed transport with no key and no network (the merge-gating path,
// mirroring proxy/relay.mjs's self-gating posture).
//
// Session scope: ONE createChatHost IS ONE conversation. The restart-honesty gate (§6.4) is
// session-scoped inside createAgentHost, so the caller must build ONE host per spine and reuse it
// across every reshape turn — never one host per turn (that would re-arm the once-per-session line).

import { wasmIntrospect, createToolLayer } from "./tools.mjs";
import { createAgentHost, proxyTransport } from "./agent-host.mjs";

/**
 * The default same-origin proxy endpoint (ADR-0054 §2). The relay ships as a Cloudflare Pages
 * Function at this path (web/functions/api/chat.js → proxy/cloudflare.mjs); until a key is
 * provisioned it self-gates to 503 (§5) and the app stays up against a stubbed transport.
 */
export const DEFAULT_CHAT_ENDPOINT = "/api/chat";

/**
 * Assemble the in-page host graph against a live engine (js/reuben-engine.mjs). Awaits the engine's
 * shared discovery wasm exports (engine.wasmExports()) to build the real introspection adapter, so
 * describe_operators/validate/content_hash run against the SAME module the player loads — no drift.
 *
 * @param {object} deps
 * @param {import("./reuben-engine.mjs")} deps.engine - the live, worklet-wired engine.
 * @param {string} [deps.endpoint] - the proxy URL (default `/api/chat`).
 * @param {(messages: object[]) => Promise<AsyncIterable<object>>} [deps.transport] - injected
 *   transport for tests (a stubbed/mock loop); defaults to the real proxyTransport(endpoint).
 * @param {typeof fetch} [deps.fetchImpl] - injectable fetch for the default transport (tests).
 * @param {number} [deps.maxRounds] - safety bound on tool round-trips per turn (network-bound loop).
 * @returns {Promise<{ runTurn: (text: string, opts?: { onPlanDelta?: (text: string) => void }) =>
 *   Promise<import("./agent-turn.mjs").AgentTurn>, messages: object[] }>}
 */
export async function createChatHost({
  engine,
  endpoint = DEFAULT_CHAT_ENDPOINT,
  transport,
  fetchImpl,
  maxRounds = 8,
} = {}) {
  const introspect = wasmIntrospect(await engine.wasmExports());
  const toolLayer = createToolLayer({ engine, introspect });
  const relay = transport ?? proxyTransport(endpoint, fetchImpl ? { fetchImpl } : undefined);

  // ONE mutable "current turn driver" the shared transcript sink routes streamed deltas into. Turns
  // are strictly sequential (the spine's turn-in-flight guard forbids a concurrent submit), so a
  // single slot is safe: set on entry to runTurn, cleared in its finally. The host's OTHER transcript
  // callbacks (onUserTurn/onTurnStart/onTurnResolved) carry the host's INTERNAL envelope, not the
  // change-card's — the seam owns the card, so we ignore them and only forward the plan stream.
  let current = null;
  const transcript = {
    onUserTurn() {},
    onTurnStart() {},
    onPlanDelta(_turn, text) {
      current?.onPlanDelta?.(text);
    },
    onTurnResolved() {},
  };

  const host = createAgentHost({ toolLayer, transport: relay, transcript, maxRounds });

  return {
    /**
     * Run one user turn to resolution, streaming the model's plan tokens to `onPlanDelta` as they
     * arrive (§4.2 — streaming is load-bearing; the card renders the plan AS it grows). Returns the
     * resolved turn envelope: `.diff` non-null iff a `swap` ran (structural, §6.2 restrike route),
     * `.diff` null for a `send`-only param reshape (§6.1 resolve route), `.toolLog` the per-turn tool
     * provenance the seam reads to synthesize the param glow/value-sweep, `.restartHonesty` the
     * once-per-session re-strike line (§6.4).
     */
    async runTurn(text, { onPlanDelta } = {}) {
      current = { onPlanDelta };
      try {
        return await host.send(text);
      } finally {
        current = null;
      }
    },
    get messages() {
      return host.messages;
    },
  };
}
