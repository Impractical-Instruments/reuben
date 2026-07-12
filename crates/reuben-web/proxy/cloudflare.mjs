// cloudflare.mjs — the Cloudflare Pages Functions adapter for the chat relay (issue #354).
//
// Matches the existing Cloudflare Pages deploy (the `deploy-web` job in .github/workflows/ci.yml
// ships web/dist to Pages). Deploying this as a Pages Function co-locates the relay with the app
// under the same origin, so the browser POSTs same-origin `/api/chat` and the SSE streams back
// with no CORS. See ./README.md for the wiring (copy/symlink into web/functions/api/chat.js).
//
// Reads the key from server env (ADR-0054 §2): `context.env.ANTHROPIC_API_KEY` is a Pages project
// SECRET, never in the bundle. Self-gates green when absent (ADR-0054 §5) — the relay returns 503,
// the Function never crashes. Provisioning the origin, the key secret, and the abuse-floor
// escalation (Turnstile-class challenge, §5) are ready-for-human follow-ups (ADR-0054 §7).

import { createRelay, MODEL_DEFAULT } from "./relay.mjs";
import { SYSTEM_PROMPT } from "./system-prompt.mjs";
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

  // Abuse-floor SEAM (ADR-0054 §5): origin/Referer allow-list, per-IP token bucket, and a
  // short-lived signed session token belong here. v1 provisioning (incl. a Turnstile-class
  // invisible challenge) is a ready-for-human follow-up (ADR-0054 §7); left as a documented no-op.

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
    apiKey: env.ANTHROPIC_API_KEY,
    systemPrompt: SYSTEM_PROMPT,
    tools: artifact.tools,
    model: env.REUBEN_CHAT_MODEL || MODEL_DEFAULT,
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
