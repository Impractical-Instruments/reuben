# reuben web-chat proxy relay

The hosted backend proxy for the in-browser chat authoring agent (issue #354, **ADR-0054**).

```
browser ── {messages} ──▶ proxy ──▶ Claude ──(SSE)──▶ proxy ──▶ browser
```

The proxy owns the **model-facing** side (ADR-0054 §2): it holds the Anthropic key server-side,
declares the system prompt + the eight tool schemas + the model id, and passes the SSE stream
straight through to the browser. The browser owns the **engine-facing** side — the eight tools
execute in the page against the live worklet (`js/tools.mjs`, `js/agent-host.mjs`).

## Files

| File | Role |
| --- | --- |
| `relay.mjs` | **Portable core.** `createRelay({ apiKey, systemPrompt, tools, model, fetchImpl })` → a handler that takes the parsed browser body `{messages}` and returns a streaming `Response`. Runtime-agnostic; unit-tested against a mock upstream (`relay.test.mjs`). |
| `cloudflare.mjs` | **Cloudflare Pages Functions adapter.** Reads `env.ANTHROPIC_API_KEY`, injects the generated tool schemas, delegates to `createRelay`. |
| `system-prompt.mjs` | The **authoring policy** (issue #356): the §1 lexicon, §8 register, §6.1 send-vs-swap routing, §4.2 narration contract, §2.3/§2.4 turn-one shapes, §6.4 first-run re-strike line — the model-facing `SYSTEM_PROMPT` string, plus the shared `FORBIDDEN_TERMS`/`scanForbiddenTerms` the eval harness scans with. |
| `../js/tool-schemas.generated.json` | The **generated** tool-schema artifact (ADR-0054 §3), consumed by BOTH this proxy (declares to the model) and the in-page layer (executes). Regenerate with `cargo run --example gen_tool_schemas`. |

## Model

Default **`claude-sonnet-5`** (ADR-0054 §4: the Sonnet-5 tier is the decision; the exact id and
`max_tokens` are config). Override per environment with `REUBEN_CHAT_MODEL`.

## Self-gating (ADR-0054 §5)

When `ANTHROPIC_API_KEY` is **absent** the relay returns a clean `503` (`code: proxy_unconfigured`)
and never throws — the app stays up, and the browser loop stays testable against the deterministic
mock transport (no key, no network). This mirrors the `deploy-web` job's skip-green-without-secrets
posture. Logging is minimal: aggregate/diagnostic only, **no retention of prompt or instrument
content** (§5).

## Deploy path (do NOT provision here)

The app already deploys to Cloudflare Pages (`deploy-web` in `.github/workflows/ci.yml`, shipping
`web/dist`). To run the relay as a same-origin Pages Function:

1. **Expose the Function.** Add a Pages Function at `web/functions/api/chat.js` that re-exports the
   adapter (Pages picks up `functions/` at build):

   ```js
   // web/functions/api/chat.js
   export { onRequest } from "../../../crates/reuben-web/proxy/cloudflare.mjs";
   ```

   The browser talks to it via `proxyTransport("/api/chat")` (`js/agent-host.mjs`).

2. **Set the secret** (Pages project → Settings → Environment variables, encrypted):
   `ANTHROPIC_API_KEY` (and optionally `REUBEN_CHAT_MODEL`). Until it is set the Function self-gates
   to `503`, exactly like `deploy-web` self-gates without the Cloudflare secrets.

3. **Abuse floor (ADR-0054 §5 / §7 — ready-for-human follow-up, NOT built here).** Add the
   origin/Referer allow-list, per-IP token bucket, and short-lived signed session token in
   `cloudflare.mjs`'s documented seam; provision a Turnstile-class invisible challenge as the
   escalation layer if abuse materializes.

Nothing is provisioned by this ticket. The relay is runnable and self-gates green without a key.

## Testing

- Merge-gating: `node --test proxy/relay.test.mjs` (from `crates/reuben-web`) — deterministic,
  no key, mock upstream. Runs in the `web` CI job.
- Merge-gating policy eval (issue #356): `node --test proxy/system-prompt.test.mjs
  js/agent-policy-eval.test.mjs` — asserts the prompt text covers every §1/§8/§6.1/§6.4 rule, and
  runs a battery of scripted (mock-model) user turns through the REAL loop/tool-layer/turn-envelope
  asserting no forbidden word ever appears, chips post verbatim, and the first-restart-only line
  fires once per session. This proves the plumbing carries the policy; it cannot prove a live model
  obeys the prompt — that's the live smoke below.
- Live smoke (non-blocking, self-gated on `secrets.ANTHROPIC_API_KEY`): the `web-chat-live-eval`
  CI job runs `node js/live-eval.mjs` against real Sonnet-5, now including a short live session that
  scans every streamed turn for a forbidden word and checks the first-restart line fires at most
  once.
