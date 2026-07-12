// web/functions/api/chat.js — the mounted Cloudflare Pages Function for POST /api/chat
// (issue #403 / Live-loop/B). This file IS the mount: it re-exports the built adapter's POST
// handler so the Pages deploy serves the chat relay same-origin. The browser reaches it via
// `proxyTransport("/api/chat")` (crates/reuben-web/js/agent-host.mjs) with no CORS.
//
// Every model-facing decision lives in the shared adapter (crates/reuben-web/proxy/cloudflare.mjs),
// so this stays a one-line seam and cannot drift from the tested relay: the SYSTEM_PROMPT, the
// GENERATED tool-schema artifact (ADR-0054 §3), and the model id + reshape ceiling read from config
// (crates/reuben-web/proxy/config.mjs — REUBEN_CHAT_MODEL / REUBEN_CHAT_RESHAPE_CEILING).
//
// Exporting `onRequestPost` (not `onRequest`) makes Pages route ONLY POST here and auto-405 other
// verbs. Until the `ANTHROPIC_API_KEY` Pages secret is set (Live-loop/A, #402) it self-gates to a
// clean 503 `{code:"proxy_unconfigured"}` and never crashes — the site stays up (ADR-0054 §5),
// exactly like `deploy-web` self-gates green without the Cloudflare secrets.
export { onRequestPost } from "../../../crates/reuben-web/proxy/cloudflare.mjs";
