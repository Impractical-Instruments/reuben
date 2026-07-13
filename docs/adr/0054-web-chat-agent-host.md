# ADR-0054: The web-chat agent host — a hosted proxy, tiered toward bring-your-own

> **Superseded by [ADR-0056](0056-web-product-extracted-to-private-repo.md)** as a decision of
> *this* repo. The proxy, the relay, the API key, the abuse floor, the model tier, the cost ceiling,
> and every "ready-for-human follow-up" below are product and operations decisions of the private
> `reuben-web` repo — the public BSD engine hosts no proxy, funds no account, and ships no chat
> agent. One clause outlives the move as a constraint on public core: §3's rule that the tool
> schemas are **generated from `reuben-core`'s serde types**, so the contract has one source. The
> generator itself moved with the crate. Kept as history.

## Status

Accepted (2026-07-11). The agent-host decision of the web-chat authoring UX effort —
ticket [#351](https://github.com/Impractical-Instruments/reuben/issues/351), the deferred
question [ADR-0052 §2](0052-web-parity-contract-not-protocol.md) explicitly declined
("the chat window's agent host — client-side API key vs backend proxy — auth, cost, product
questions") and [spec §10](../web-chat-authoring-ux-spec.md#10-out-of-scope--deferred-doors)
listed out of scope. This ADR **is** that question, and it is a **design decision, not a build**:
it gates the agent-loop ticket (the first M1 root of the downstream execution epic). **Grounds on**
the [Web-chat authoring UX spec](../web-chat-authoring-ux-spec.md) (the UX this host serves),
[ADR-0052](0052-web-parity-contract-not-protocol.md) (the in-page tool layer — eight contracts
over the C-ABI worklet — this host drives, and §5's "one schema, many doors" type discipline this
ADR extends), and [ADR-0042](0042-share-links.md) (the account-free share-link lane the keep
gesture rides — orthogonal to, and unaffected by, this host). **Amends none.** Names
`ready-for-human` provisioning follow-ups (§7) and one bounded spec revisit (§6) so the build
tickets start from a settled architecture.

## Context

- The spec designs the UX on top of *some* agent host and never picks one. The persona
  ([spec §0.1](../web-chat-authoring-ux-spec.md#01-the-persona)) is a non-musician in a browser
  tab, no toolchain, here to **play** — describe a sound in plain language and hear it. Every copy
  and interaction decision is calibrated to someone who did not come to answer a setup quiz.
- Two spec rules bound the host from the UX side. The **lexicon** (§1) speaks only in sound and
  musical intent and forbids naming an account, a key, or a capability that does not exist. The
  **account-free lane** is a deliberate bet: v1 has no login and no profile (the keep gesture is a
  link snapshot, ADR-0042; the skill-register is session-scoped, §8.3). Any host that puts a
  signup or API-key wall in front of first sound breaks both.
- The host drives the in-page tool layer ADR-0052 §2 fixed: eight JS tools over the C-ABI worklet,
  **executing in the browser** (the worklet *is* the engine; nothing else can run the tools). The
  reshape loop is agentic — `describe_instrument` → decide `send` vs `swap` (§6.1) → often
  `validate` → emit a new document + sensory card language → self-repair on `{ok:false}` within the
  turn (§5.1 case 3). It is also **live**: a human waits with sound playing, and it **streams**
  (§4.2's plan-resolves-in-place needs token streaming).
- The candidate hosts each carry a cost. A **client-side key** baked in the bundle has no setup
  wall but is trivially extracted and billed to us with no server-side chokepoint — the abuse
  magnet the issue flags. **Bring-your-own** (BYO) is zero-cost and opens no abuse surface we own,
  but the key-entry step is exactly the wall the persona came to avoid, and the account-free lane
  has nowhere durable to keep a key. A **backend proxy** we host reaches first sound with no wall
  and gives us a server-side control point — at the cost of an origin/service, our token bill, and
  an abuse-defense obligation, because an unauthenticated hosted LLM proxy is nearly the same abuse
  magnet as the exposed key.

## Decision

### 1. A hosted backend proxy is the v1 host; the architecture is tiered toward BYO

The v1 host is a **backend proxy we host**. It holds the Anthropic key server-side and relays the
chat + eight-contract tool loop. This is the only option that reaches first sound with **zero
setup wall** while honoring the lexicon and the account-free lane: the persona describes a sound
and hears it, and the chat never names a key, an account, or a store.

The **architecture is tiered** — free proxied sessions are the entry, **bring-your-own-key** is the
long-run escape hatch when free quota is exhausted — but **v1 builds the proxy only**. BYO is a
**named `ready-for-human` follow-up** (§7), not a v1 build: a BYO handoff must say "your own key"
in *some* surface, which is engine/toolchain vocabulary the lexicon (§1) retired, and it needs a
key-entry surface, secret storage the account-free lane has nowhere to put, and a second
client-side agent path — a meaningfully separate effort in a non-chat register. Recording the
tiered target here lets the ceiling (§5) be designed as the future BYO seam without building it now.

**What "your own key" is — an Anthropic API account, not a Claude.ai subscription.** BYO means the
user brings their own **Claude Developer Platform** key (`console.anthropic.com` — a distinct account
with its own token billing, used via the `x-api-key` header the proxy already sends,
`crates/reuben-web/proxy/relay.mjs`). A **Claude.ai subscription** (Pro/Max) is a *different product*:
it grants the Claude apps and Claude Code, mints **no** API key, and cannot authenticate the Messages
API here — a subscriber has nothing to bring. (A subscription-scoped OAuth path to the Messages API
technically exists for Anthropic's own first-party tooling, but its tokens are short-lived and its
third-party use is unsupported — not a foundation to build BYO on.) This sharpens the tier's shape:
the BYO audience is the narrow, more-technical slice who hold or will create an API account, **not**
the play-first persona (§0.1) — so BYO is an escape hatch for power users, never a cost-shift for the
mass free tier, and the free-proxy economics (§5) stay the load-bearing lever.

**Considered and rejected — as the v1 front door:** the **client-side baked key** (no wall, but
un-defensible — scraped from the bundle, billed to us, no chokepoint; effectively the abuse magnet
this effort exists to avoid); **BYO as the entry** (contradicts §0.1 and the account-free bet — the
setup wall in front of a play-first persona); the **full tier in v1** (proxy + real BYO fallback —
best day-one economics, but drags key-entry chrome, secret storage, and a client-side bypass path
into the account-free launch, for a bigger and later ship). The tiered *architecture* is accepted;
the BYO *build* defers.

### 2. The loop: proxy owns the model-facing side, browser owns the engine-facing side

The tools execute in the browser (ADR-0052 §2), but the model conversation runs through the proxy,
so the reshape turn is a **relayed, client-side-execution loop**:

```
browser ── user turn ──▶ proxy ──▶ Claude
                                     │ emits tool_use
browser ◀── tool_use ──── proxy ◀────┘
   │ JS layer executes against the live worklet
   └── tool_result ──▶ proxy ──▶ Claude ──▶ … ──▶ streamed reshape lands
```

The **proxy owns the model-facing side**: the system prompt (the §1 lexicon rules, the §8 register,
the eight-contract usage guidance), the tool-schema declaration (§3 below), the model id (§4), and
the SSE stream it passes straight through to the browser (§4.2's streaming is satisfied here, not a
model choice). The **browser owns the engine-facing side**: the JS tool layer executing against the
worklet, returning `tool_result`s. Each `tool_use`/`tool_result` is a browser↔proxy↔Claude
round-trip — the loop is network-bound, which §6 addresses.

**Considered and rejected — the browser declaring the model-facing contract:** a client that sends
its own system prompt or tool schemas is tamperable (inject tools, reshape the prompt) and
cache-hostile (a per-turn-varying prefix never caches). The trust boundary and the cache both put
the model-facing contract server-side.

### 3. Tool schemas are declared server-side, generated from core types — the third door

The proxy declares the eight tool schemas in every request's `tools` array
(**server-authoritative**). The schemas are **generated from the shared `reuben-core` serde types**
into a single JSON artifact that **both** the proxy (declares to the model) **and** the in-page JS
layer (executes) consume — so the declared contract and the executed contract **cannot drift**. This
is [ADR-0052 §5](0052-web-parity-contract-not-protocol.md)'s "contract types live OS-free in core,
one schema, many doors" extended to a **third door**: native MCP, the in-page layer, and now the
proxy's model-facing declaration all derive from one source.

A server-owned, position-0 `tools` + `system` prefix that never varies per turn also **caches
perfectly** (prompt-caching is a prefix match; the volatile per-turn content is the conversation
tail).

**Considered and rejected — hand-authoring the schemas in the proxy:** server-authoritative but a
second hand-maintained copy alongside the in-page layer, free to drift silently — exactly the
divergence ADR-0052 §5 exists to prevent.

### 4. Model tier: Sonnet 5 by default; the id is config, not an ADR constant

The live reshape loop runs on the **Sonnet-5 tier** by default. It carries near-Opus quality on
this exact shape of work (agentic tool-use + structured document output) at roughly half the Opus
per-token cost and lower latency — both of which are load-bearing when **we** pay for every token
and a human waits on live sound. Cheaping to the Haiku tier is a false economy here: a weaker model
produces invalid documents → more `validate`/repair round-trips → *higher* latency and cost, and a
worse instrument on the one first impression the curated content exists to protect (§2.3).

The **exact model id is a configuration value, not fixed by this ADR** — the tier and its rationale
are the decision. Routing hard cases up to the Opus tier or cheap sub-steps down to Haiku is a
**deferred tuning lever**, not a v1 commitment. `effort` and streaming settings are likewise config.

### 5. Cost, abuse, and the ceiling

**Per-session ceiling — a soft cap on reshapes, never on sound.** The cost ceiling is metered on
**reshapes** (legible to the user as "updates," §1), not raw tokens. At the cap it is a **soft
stop**: the current instrument **keeps playing and stays hand-live** (§3.4, §6 — the sound is never
broken); only a *new* update is withheld, surfaced as one sensory "resting" line ("this instrument's
had a big workout — give it a moment, or keep playing what you've got"). The exact N is
configuration. A "session" is the lifetime of a page-load-minted token (below), not a persistent
account; a reload mints a fresh token and resets the reshape count — acceptable leakage for a legit
player, because the abuse layer, not the ceiling, defends against scripting. This ceiling **is the
future BYO seam**: when BYO ships (§7), the rest line grows a "…or bring your own power" chrome
affordance in a non-chat register.

**Abuse floor — layered and invisible.** An unauthenticated hosted LLM proxy is an abuse magnet
(free Claude on our bill), and the account-free lane gives us no user identity to rate-limit. The
v1 floor is therefore layered and **invisible to the persona** (a visible CAPTCHA before first
sound is the wall we are avoiding): (1) an **origin/Referer allow-list**; (2) a **per-IP
token-bucket rate limit**; (3) a **short-lived signed session token** minted server-side on page
load and required by the proxy, tying requests to a real page load and blunting trivial replay. An
**invisible bot challenge** (Cloudflare Turnstile-class, no-interaction) is **named as the
escalation layer** and a `ready-for-human` provisioning follow-up (§7), pulled in if/when abuse
materializes — proportionate defense now, a heavier gate on standby.

**Logging posture — minimal.** The proxy is now an intermediary of user prompt content. The v1
default is **minimal logging**: aggregate abuse/cost metrics only, **no retention of prompt or
instrument content** — keeping faith with the low-ceremony, account-free persona. A fuller privacy
pass is a follow-up if the posture ever needs to change.

## Consequences

- The downstream execution epic starts the agent-loop ticket from a settled architecture: a
  proxy relay (§2), a server-authoritative generated schema artifact (§3), a default model tier
  (§4), and a ceiling + abuse floor (§5) — instead of a blank host.
- **§6's re-strike is untouched by the host.** The restart-swap is a *local* audio event in the
  worklet, and §6.2's load-bearing co-timing (card commit ↔ sound restart) is entirely client-side
  at swap-time. The proxy's network latency lives in the §4.2 "thinking" phase *before* the swap,
  not in the re-strike gap, so the re-strike reads identically to a native in-process loop. Build
  tickets must not touch §6 for latency.
- **§3.4 is reinforced.** "Controls stay hand-live through the turn, no freeze" mattered before;
  with the turn now a multi-round network loop, it matters more — the reinforcement is a
  strengthened requirement, not a revisit.
- The generated schema artifact (§3) becomes a shared build dependency of the proxy and the in-page
  layer, and a fourth consumer keeping the ADR-0052 §5 core types honest.

## The one bounded spec revisit (§6 of the issue)

ADR-0052 §2 flagged that the host's latency/reliability answer may force a revisit of spec §3/§4/§6.
The proxy introduces exactly one gap, bounded and named here so build tickets know — **not** a new
decision ticket:

- **§4/§5 gain a host/transport failure class.** §5's failure taxonomy (E) is all *agent/engine*
  failure — ambiguous, unsatisfiable, `{ok:false}`, off-topic — and exposes no engine reason. The
  proxy adds failures that are neither: a network drop mid-loop, a proxy 5xx, model overload, or the
  reshape soft-cap tripping mid-turn. These **collapse into §5.3's existing terminal-failure
  shapes** — a reshape terminal failure keeps the prior sound playing; a first-creation terminal
  failure lands back at the gallery — rendered in the sensory lexicon with the network/host reason
  **never exposed** ("I lost the thread — try that again?"). No new UX door.
- **§4.2's thinking state is stretched** to tolerate a longer, multi-phase, network-variable wait
  (multiple client-side tool round-trips per reshape), rather than a single uninterrupted stream.
- **§6 holds; §3.4 is reinforced** (both above) — recorded so build tickets do not over-engineer.

## Ready-for-human follow-ups this decision spawns

Separate from this design decision:

- Provision a **proxy origin/service** to host the relay.
- Create and fund an **Anthropic account**; store its key as a **server-side secret**.
- Provision a **Turnstile-class invisible challenge** for the abuse escalation layer (§5).
- Build the **BYO lane** (§1): key-entry chrome, key storage, the client-side agent path, and a
  non-chat-register wording pass — the tier's second half, deferred from v1. Two constraints from
  §1's "what your own key is" clarification: the wording pass points the user at **creating an
  Anthropic API key** (an API account, not a "log in with Claude" subscription flow); and a
  sub-decision to settle at build time is whether the user's key calls Anthropic **directly from the
  browser** (the key stays client-side — a CORS/exposure surface) or still flows **through the
  proxy** (which then holds a user credential — a trust/liability shift from the v1 posture where the
  proxy never sees a user key).
