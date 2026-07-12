// cloudflare.mjs — the Cloudflare Pages Functions adapter for the chat relay (issue #354).
//
// Matches the existing Cloudflare Pages deploy (the `deploy-web` job in .github/workflows/ci.yml
// ships web/dist to Pages). Deploying this as a Pages Function co-locates the relay with the app
// under the same origin, so the browser POSTs same-origin `/api/chat` and the SSE streams back
// with no CORS. The mount is `web/functions/api/chat.js`, which re-exports `onRequestPost` (issue
// #403 / Live-loop/B); see ./README.md for the deploy wiring.
//
// Config comes from server env via `readProxyConfig` (./config.mjs): the key (ADR-0054 §2 — a Pages
// SECRET, never in the bundle), the model id (§4), and the per-session reshape ceiling N (§5). When
// the key is absent the relay self-gates green (ADR-0054 §5) — it returns 503 and the Function never
// crashes. The abuse-floor build (allow-list · rate-limit · session token · the ceiling) is
// Live-loop/C (#404); the Turnstile-class escalation is a ready-for-human follow-up (ADR-0054 §7).

import { createRelay } from "./relay.mjs";
import { SYSTEM_PROMPT } from "./system-prompt.mjs";
import { readProxyConfig } from "./config.mjs";
// The generated tool-schema artifact (ADR-0054 §3) — declared to the model verbatim; the in-page
// layer executes the same names. esbuild/wrangler bundles this JSON import at deploy time.
import artifact from "../js/tool-schemas.generated.json" with { type: "json" };

/**
 * Cloudflare Pages Functions entrypoint for POST /api/chat. The chat feature is behind a ship-gate
 * flag (M1 global decision), so nothing routes here until the spine ticket enables it.
 *
 * @param {{ request: Request, env: Record<string, string> }} context
 * @returns {Promise<Response>}
 */
export async function onRequestPost(context) {
  const { request, env } = context;
  const config = readProxyConfig(env);

  // Abuse-floor SEAM (ADR-0054 §5): the origin/Referer allow-list, per-IP token bucket, short-lived
  // signed session token, and the per-session reshape ceiling (`config.reshapeCeiling`, N) belong
  // here — layered and invisible to the persona (no wall before first sound). Building them is
  // Live-loop/C (#404); the Turnstile-class escalation is a ready-for-human follow-up (§7). Left a
  // documented no-op so this mount ships self-gated-green before the floor lands.

  let body;
  try {
    body = await request.json();
  } catch {
    return new Response(JSON.stringify({ code: "bad_request", error: "Body must be JSON." }), {
      status: 400,
      headers: { "content-type": "application/json" },
    });
  }

  const relay = createRelay({
    apiKey: config.apiKey,
    systemPrompt: SYSTEM_PROMPT,
    tools: artifact.tools,
    model: config.model,
  });
  return relay(body);
}

// Only POST is meaningful; reject other verbs cleanly (a Pages Function catch-all).
export function onRequest(context) {
  if (context.request.method !== "POST") {
    return new Response("method not allowed", { status: 405 });
  }
  return onRequestPost(context);
}
