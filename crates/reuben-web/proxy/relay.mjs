// relay.mjs — the hosted proxy relay, portable core (issue #354, ADR-0054 §1/§2).
//
// browser ── {messages} ──▶ proxy ──▶ Claude ──(SSE)──▶ proxy ──▶ browser
//
// The proxy owns the MODEL-FACING side (ADR-0054 §2): it holds the Anthropic key server-side,
// declares the system prompt + the eight tool schemas + the model id, and passes the SSE stream
// straight through to the browser. The browser owns the ENGINE-FACING side (js/agent-host.mjs +
// js/tools.mjs). This file is the runtime-agnostic handler; `cloudflare.mjs` adapts it to the
// existing Cloudflare Pages deploy.
//
// Self-gating (ADR-0054 §5, and the coordinator's global decision): the key is read from server
// env by the ADAPTER and injected here. When it is ABSENT this returns a clean 503 with a telemetry
// code and NEVER throws — the app stays up and the browser loop stays testable against a MOCK
// transport with no key. Logging posture is minimal (§5): aggregate/diagnostic only, no retention
// of prompt or instrument content.
//
// The tool schemas are NOT hand-authored here — they are the generated artifact
// `js/tool-schemas.generated.json` (ADR-0054 §3), passed in by the adapter, so the declared
// contract and the in-page executed contract cannot drift.

const ANTHROPIC_URL = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION = "2023-06-01";
/** ADR-0054 §4: the Sonnet-5 tier is the decision; the exact id is config (default here). */
export const MODEL_DEFAULT = "claude-sonnet-5";

/** A JSON Response helper with the reuben telemetry envelope (machine-readable `code`). */
function jsonResponse(status, body) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

/**
 * Build the chat-relay handler. Runtime-agnostic: the adapter supplies the key (from env), the
 * generated tool schemas, and — for tests — a `fetchImpl`.
 *
 * @param {object} cfg
 * @param {string|undefined} cfg.apiKey - ANTHROPIC_API_KEY (server env). Absent ⇒ every call 503s.
 * @param {string} cfg.systemPrompt - the model-facing system prompt (a bare placeholder in M1; the
 *   authoring policy is issue #356). Server-authoritative + cache-stable (ADR-0054 §2/§3).
 * @param {Array<{name:string,description:string,input_schema:object}>} cfg.tools - the generated
 *   tool schemas (`js/tool-schemas.generated.json` → `.tools`). Declared to the model verbatim.
 * @param {string} [cfg.model] - the Sonnet-5-tier model id (config; ADR-0054 §4).
 * @param {number} [cfg.maxTokens]
 * @param {typeof fetch} [cfg.fetchImpl] - injectable upstream fetch (defaults to global fetch).
 * @param {string} [cfg.anthropicUrl] - injectable upstream URL (defaults to the real endpoint).
 * @returns {(body: unknown) => Promise<Response>} a handler taking the parsed browser request body.
 */
export function createRelay({
  apiKey,
  systemPrompt,
  tools,
  model = MODEL_DEFAULT,
  maxTokens = 8192,
  // Sonnet-5 runs adaptive thinking ON by default (ADR-0054 §4 makes thinking/effort a config /
  // deferred-tuning lever, not an architecture constant). M1 is plumbing-only, so default it OFF:
  // an emitted `thinking` block must be echoed back unchanged (its `thinking` field + `signature`)
  // on the next round, which the browser loop (agent-host.mjs `consumeStream`) does not yet
  // assemble — leaving it on 400s the SECOND request with
  // `messages.N.content.0.thinking.thinking: Field required`. Turning adaptive thinking on is the
  // authoring-policy ticket's call (#356), paired there with thinking-block round-tripping.
  thinking = { type: "disabled" },
  fetchImpl = fetch,
  anthropicUrl = ANTHROPIC_URL,
}) {
  return async function handle(body) {
    // Self-gate on the key (ADR-0054 §5). Clean 503 + telemetry code; never throw, never crash.
    if (!apiKey) {
      console.warn("[reuben-chat-proxy] ANTHROPIC_API_KEY not set — returning 503 (self-gated).");
      return jsonResponse(503, {
        code: "proxy_unconfigured",
        error: "The chat relay has no server-side key configured.",
      });
    }

    const messages = body && typeof body === "object" ? body.messages : undefined;
    if (!Array.isArray(messages) || messages.length === 0) {
      return jsonResponse(400, {
        code: "bad_request",
        error: "Request body must be { messages: [...] } with at least one message.",
      });
    }

    // The server-authoritative, per-turn-invariant prefix (tools + system + model) that caches
    // perfectly (ADR-0054 §3); the volatile tail is the conversation.
    const upstreamBody = {
      model,
      max_tokens: maxTokens,
      thinking,
      system: systemPrompt,
      tools,
      stream: true,
      messages,
    };

    let upstream;
    try {
      upstream = await fetchImpl(anthropicUrl, {
        method: "POST",
        headers: {
          "content-type": "application/json",
          "x-api-key": apiKey,
          "anthropic-version": ANTHROPIC_VERSION,
        },
        body: JSON.stringify(upstreamBody),
      });
    } catch (err) {
      // Network drop to the model (ADR-0054 §6 host/transport failure). Aggregate-only log.
      console.error("[reuben-chat-proxy] upstream fetch failed:", String((err && err.message) || err));
      return jsonResponse(502, { code: "upstream_unreachable", error: "The model host is unreachable." });
    }

    // Pass the stream STRAIGHT through (ADR-0054 §2): SSE on success, the upstream JSON error on
    // failure — the browser transport checks response.ok before parsing, so a non-2xx never reaches
    // the SSE parser. Forward the upstream status + content-type verbatim.
    const contentType = upstream.headers.get("content-type") || "text/event-stream";
    return new Response(upstream.body, {
      status: upstream.status,
      headers: { "content-type": contentType, "cache-control": "no-store" },
    });
  };
}
