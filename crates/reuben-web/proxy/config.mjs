// config.mjs — the proxy's env → config surface (issue #403 / Live-loop/B; ADR-0054 §4/§5).
//
// One place to turn the Cloudflare Pages env into the proxy's typed config: the model id (§4) and
// the per-session RESHAPE CEILING N (§5). The ceiling is surfaced HERE so the abuse floor
// (Live-loop/C, #404) reads a single config field instead of re-parsing env — this ticket only
// EXPOSES it (a default + a `REUBEN_CHAT_RESHAPE_CEILING` override); C is what enforces it.
//
// Runtime-agnostic and side-effect-free: pass any env-shaped object (Cloudflare `context.env`,
// `process.env`, or a test double). Never throws on bad input — an unparseable override falls back
// to the default, keeping the "app stays up / self-gates green" posture (§5) rather than 500-ing a
// page load over a mistyped variable.

import { MODEL_DEFAULT } from "./relay.mjs";

/**
 * Default per-session reshape ceiling N (ADR-0054 §5). A SOFT cap on "updates" — the user-facing
 * word for reshapes (§1) — never on sound: at the cap the instrument keeps playing and stays
 * hand-live; only a *new* update is withheld. A "session" is the lifetime of a page-load-minted
 * token, so a reload resets the count. This value is a cost-sanity / UX knob, not the scripting
 * defense — that is the abuse floor (allow-list · rate-limit · session token, Live-loop/C). Tune
 * per environment with `REUBEN_CHAT_RESHAPE_CEILING`; this constant is the only place it lives.
 */
export const RESHAPE_CEILING_DEFAULT = 25;

/** Parse a positive-integer env override; fall back to `fallback` on absent/blank/non-integer. */
function positiveIntOr(raw, fallback) {
  if (raw == null || raw === "") return fallback;
  const n = Number(raw);
  return Number.isInteger(n) && n > 0 ? n : fallback;
}

/**
 * Resolve the proxy config from a server env object.
 *
 * @param {Record<string, string|undefined>} [env] - server env (a Pages `context.env`).
 * @returns {{ apiKey: string|undefined, model: string, reshapeCeiling: number }}
 *   `apiKey` absent ⇒ the relay self-gates to a clean 503 (§5); `model` defaults to the Sonnet-5
 *   tier (§4); `reshapeCeiling` is N for the abuse floor (§5).
 */
export function readProxyConfig(env = {}) {
  return {
    // The server-side key — a Pages SECRET, never in the bundle (§2). Absent ⇒ self-gated 503 (§5).
    apiKey: env.ANTHROPIC_API_KEY,
    // The Sonnet-5-tier id is configuration, not an ADR constant (§4).
    model: env.REUBEN_CHAT_MODEL || MODEL_DEFAULT,
    // The per-session reshape ceiling N (§5) — surfaced for the abuse floor (Live-loop/C, #404).
    reshapeCeiling: positiveIntOr(env.REUBEN_CHAT_RESHAPE_CEILING, RESHAPE_CEILING_DEFAULT),
  };
}
