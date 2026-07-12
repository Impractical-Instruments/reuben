# Anthropic key rotation — the hosted web-chat proxy

The operational half of [issue #402](https://github.com/Impractical-Instruments/reuben/issues/402)
(Live-loop/A): once the `ANTHROPIC_API_KEY` that funds the hosted chat proxy exists, someone has to
own rotating it. This is that written-down story — how to rotate, who owns it, how often. It grounds
on [ADR-0054](../../../docs/adr/0054-web-chat-agent-host.md) §1 (key is a **server-side secret**,
never client-reachable), §5 (spend ceiling + minimal logging), and §7 (this provisioning follow-up).

## The key lives in two independent stores

The same `ANTHROPIC_API_KEY` string is set in **two separate places**, for two different jobs.
Rotating means replacing it in **both** — one does not update the other.

| Store | Where | Read by | Purpose |
| --- | --- | --- | --- |
| **Cloudflare Pages secret** | `reuben-web-player` project → Settings → Environment variables & Secrets (encrypted) | `crates/reuben-web/proxy/cloudflare.mjs` at runtime (`env.ANTHROPIC_API_KEY`) | The live hosted proxy that serves `/api/chat`. **This is #402's core deliverable.** |
| **GitHub Actions repo secret** | Repo → Settings → Secrets and variables → Actions | The non-blocking `web-chat-live-eval` CI job (`.github/workflows/ci.yml`) | The live Sonnet-5 CI smoke. Optional; self-gates green when unset. |

Both stores **self-gate cleanly when the key is absent or invalid**: the proxy returns
`503 proxy_unconfigured` and never crashes (ADR-0054 §5; `proxy/relay.mjs`), and the CI job skips
green. That is what makes rotation low-risk — a momentary gap degrades gracefully rather than
breaking the app.

## Who owns this

- **Billing owner / rotation owner:** the **billing owner** — the person with billing authority named
  in #402. They hold the Anthropic Console login and are the single point of accountability for the
  key's lifecycle.
- **Second pair of hands:** anyone with admin on the `reuben-web-player` Pages project and on the
  GitHub repo's Actions secrets can perform the Cloudflare/GitHub half; only the billing owner mints
  and revokes keys in the Anthropic Console.

## Cadence

- **Scheduled:** **every 30 days.** Put a recurring reminder on the billing owner's calendar; there
  is no automated expiry.
- **Immediate (out of cycle), no debate:** a suspected leak (key seen in a log, a bundle, a
  screen-share, a paste), an unexplained spend spike (see the §5 ceiling / billing alerts below), or
  any personnel change that removes someone who had Console or Pages-admin access.

## Scheduled rotation (zero-downtime)

Do it in this order so the proxy is never keyless. The **new key is live before the old one dies.**

1. **Mint a new key.** Anthropic Console → **API keys** → *Create key*. Name it so the active key is
   obvious (e.g. `reuben-web-proxy-2026Q3`). **Do not revoke the old key yet.**
2. **Update the Cloudflare Pages secret.** `reuben-web-player` → Settings → Environment variables &
   Secrets → edit **`ANTHROPIC_API_KEY`** (encrypted). Update every environment the proxy runs in —
   **Production** always; **Preview** too if staging (`dev`) serves a live model.
3. **Redeploy — required.** Cloudflare Pages binds secrets **at deploy time**, so the change only
   takes effect on the **next deployment**. Trigger one: re-run the `deploy-web` job on the target
   branch, or Cloudflare dashboard → the project → **Deployments → Retry deployment**. The running
   deployment keeps the *old* key until this completes.
4. **Verify the new key serves.** Once *Live-loop/B* has mounted `web/functions/api/chat.js`, a POST
   to `/api/chat` should return a real SSE stream. **Before B is mounted** there is nothing to smoke
   — confirm instead that the secret is present and the redeploy went green; the proxy stays
   `503`-gated by design until B lands.
5. **Update the GitHub Actions secret.** Repo → Settings → Secrets and variables → Actions → update
   **`ANTHROPIC_API_KEY`**. (Takes effect on the next workflow run; no redeploy.)
6. **Revoke the old key.** Only after 2–5 are confirmed: Anthropic Console → API keys → revoke the
   previous key. Rotation is complete when the old key no longer exists.

## Emergency rotation (key compromised)

Reverse the priority: **kill first, restore second.** A brief keyless window is acceptable — the
proxy self-gates to `503` and the current instrument keeps playing hand-live (ADR-0054 §5; the §6
host-failure class renders as the sensory "I lost the thread — try that again?", never a crash).

1. **Revoke the compromised key immediately** in the Anthropic Console.
2. Mint a replacement and run **Scheduled rotation steps 1–5** to restore service.
3. Check **Usage** in the Console for anomalous spend during the exposure window; if material, note
   it for the billing owner.

## Spend guardrails (ADR-0054 §5)

Rotation and cost defense are the same concern — a leaked key shows up as spend. Keep a **monthly
budget + usage alert** configured in the Anthropic Console, sized to the §5 per-session **reshape
ceiling** (cost is metered on reshapes, soft-capped per page-load session). An alert firing is one
of the immediate-rotation triggers above. Logging stays **minimal** (§5): aggregate abuse/cost
metrics only, no retention of prompt or instrument content.

## What this doc does *not* cover

- **Mounting the proxy Function** (`web/functions/api/chat.js`) — that's *Live-loop/B*, not #402.
- **The abuse floor** (origin allow-list, per-IP token bucket, signed session token, Turnstile) —
  ADR-0054 §5/§7, separate work; see `proxy/README.md` and the `cloudflare.mjs` seam.
- **Flipping the ship-gate flag** (`web/src/chat/flag.js`) — the go-live ticket.
