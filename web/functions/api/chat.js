// web/functions/api/chat.js — the Cloudflare Pages Function that exposes the web-chat agent proxy
// same-origin at POST /api/chat (issue #397; the deploy wiring in crates/reuben-web/proxy/README.md).
//
// It re-exports the portable adapter (crates/reuben-web/proxy/cloudflare.mjs), which reads the
// ANTHROPIC_API_KEY server SECRET, declares the system prompt + the eight generated tool schemas +
// the model id (ADR-0054 §2/§3), and streams Claude's SSE straight through to the browser. The key
// lives ONLY in the Function's server env — never in web/dist, never client-reachable (ADR-0054 §1).
//
// Self-gating (ADR-0054 §5): with no key set, the adapter returns a clean 503 and the app stays up
// against the deterministic stubbed transport — so the browser loop is fully testable with no key.
//
// Local dev (issue #397, the bring-your-own dev key path): build once, then run the app behind the
// Function so the browser POSTs same-origin —
//   cd web && npm run build
//   npx wrangler pages dev dist --binding ANTHROPIC_API_KEY=$ANTHROPIC_API_KEY_REUBEN_DEV
//   open http://localhost:8788/?chat=1
// `--binding` maps a dev key into the Function's env under the name the adapter reads, keeping it in
// the wrangler process only (never in the built bundle). Without it, /api/chat self-gates to 503.
//
// `onRequest` (the method-guarded catch-all) handles the route: POST relays, everything else 405s.

export { onRequest } from "../../../crates/reuben-web/proxy/cloudflare.mjs";
