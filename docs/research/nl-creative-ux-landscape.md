# Natural-language creative-UX landscape: patterns for a future web-chat effort

**Date:** 2026-07-11 (all sources accessed this day) ·
**Not tied to a wayfinder ticket.** This is exploratory research, run ahead of any ticket,
to ground the **future web-chat effort** that [ADR-0052](../adr/0052-web-parity-contract-not-protocol.md)
names but explicitly declines to design ("designing the chat window, its agent host, … —
that is the web-chat effort's lane; this ADR fixes the architecture it starts from," §2).
This memo does not design that window either. It surveys how existing products handle
natural-language creative authoring, so that future design work starts from evidence about
what has already been tried, rather than a blank page.

**Research question:** Across (a) text-to-audio/music products, (b) AI app/UI builders that
produce a live directly-editable system, (c) chat-driven editors of one persistent artifact,
and (d) generative-UI conventions for rendering what an agent did — what interaction shapes
already exist for "natural language produces or reshapes a thing you keep working on," and
which of those shapes plausibly transfer to reuben's target shape: a non-musician, in a
browser tab, describing a sound/system in plain language, where the output is not a file but
a **live, patchable, real-time-rendering graph** (Operators, typed ports, a `swap`/survivor
model that must not click) that the same person keeps touching — by knob and by language —
across sessions?

## Method

**Primary source** here means: a vendor's own product docs, help-center articles, official
blog/changelog, or a first-party spec repository (e.g. `modelcontextprotocol/ext-apps`) —
not a listicle, a "how to prompt X" affiliate guide, or Reddit/Medium commentary. Where only
the latter was findable for a claim, this memo says so explicitly rather than presenting it
as verified.

**A sandbox constraint shaped what could be directly fetched.** This environment's outbound
network goes through a proxy gateway that allow-lists a short list of hosts (npm, PyPI,
crates.io, GitHub, `*.anthropic.com`/`claude.com`, a few others); a direct `curl`/`WebFetch`
to an arbitrary vendor domain (`suno.com`, `elevenlabs.io`, `v0.app`, `lovable.dev`,
`replit.com`, `openai.com`, `figma.com`, `help.openai.com`, etc.) returns a **CONNECT-tunnel
403 at the gateway itself** — confirmed by running the same request through both `WebFetch`
and a raw `curl`, both failing identically, which rules out a per-site bot-block and points
at gateway policy. GitHub (including `raw.githubusercontent.com`) and Anthropic's own domains
fetched normally and are cited as directly-read primary sources below. For every other vendor,
the evidence comes through the **WebSearch tool**, which fetches and quotes the vendor's own
page server-side and returns dated snippets attributed to the vendor's URL — the *source* is
still the vendor's own docs/blog, but this memo could not independently re-render the raw page
itself, and says so wherever it matters. Claims resting only on secondary commentary (an
affiliate guide, a Medium post, a forum thread) are flagged inline as such.

---

## 1. Text-to-audio/music web products

| Product | Interaction shape | Is the output ever parametric/editable, or only re-renderable? | Primary source |
|---|---|---|---|
| **Suno** | Not chat-iterative in the conversational sense: a menu of discrete regeneration actions — **Remix** (Cover / Extend / Reuse Prompt), **Get Stems** (Edit menu), and the **Song Editor** (drag-select a time range on the waveform, optionally supply new lyrics/tags, regenerate just that range) | Stems and the Song Editor's range-replace give **region-level** post-hoc editing without a full re-render — "post-generation editing without re-creating the entire song." But there is no symbolic/parametric handle (no note list, no mix parameter) — the addressable unit is a time range of an already-rendered waveform, not a graph of named things. | [suno.com/release-notes](https://suno.com/release-notes); [help.suno.com](https://help.suno.com/en/categories/550017) (via WebSearch synthesis — direct fetch blocked, §Method) |
| **Udio** | Same shape: **Inpainting** (select up to 4 regions, regenerate just those), **Remix**, **Extend** (30s increments) | Region-scoped regeneration, same as Suno — no parametric/stem-independent mixing surface described in the changelog itself. | [help.udio.com changelog](https://help.udio.com/en/articles/10748731-changelog-what-s-new-with-udio) (via WebSearch synthesis) |
| **ElevenLabs Music** | The most *structured* of the three: a `composition_plan` is a JSON object of named sections (each with its own style/duration/lyrics), not a prose blob; **in-painting** regenerates one section via a `source_from` parameter while the rest is held fixed (documented as enterprise-only at time of writing) | Because the plan is section-structured JSON, it is closer to a parametric document than a rendered blob — but it is exposed as a compose-time API artifact, not an end-user chat object; **stem separation** is a one-way downstream endpoint (returns a ZIP of files) with no path back into further composition | [elevenlabs.io/docs/api-reference/music/compose](https://elevenlabs.io/docs/api-reference/music/compose); [.../composition-plans](https://elevenlabs.io/docs/eleven-api/guides/how-to/music/composition-plans); [.../separate-stems](https://elevenlabs.io/docs/api-reference/music/separate-stems) (all via WebSearch synthesis) |

**Pattern, common to all three:** none of these is "a chat thread that keeps refining one
persistent instrument." Each is *one-shot-generate, then a fixed menu of regeneration
actions* (extend / inpaint-a-region / stem-split / cover) applied to the **same rendered
audio artifact** — the object being edited is a waveform, addressable only by *time range*,
never by *parameter name*. None of the three lets a user directly nudge a mix parameter (a
filter cutoff, a reverb send) the way a knob would — every edit is "regenerate this region
differently," never "turn this value." This is the sharpest available contrast with reuben:
there is no "instrument keeps sounding while you reshape it" analog anywhere in this section
— every edit action stops, regenerates, and re-presents a new static rendering.

---

## 2. AI app/UI builders producing a live, directly-editable system

| Product | Interaction loop shape | How changes are surfaced to a non-technical user | Primary source |
|---|---|---|---|
| **v0 (Vercel)** | Chat pane + live preview + code pane, side by side | Each chat turn includes a stated **plan** ("what it is going to do") followed by a **step-by-step breakdown** of the change in prose (features implemented, how it works) — a natural-language account, not a raw diff or tool log | [v0.app/docs](https://v0.app/docs) (via WebSearch synthesis — direct fetch blocked) |
| **Bolt.new (StackBlitz)** | Chat drives **WebContainers** — a full in-browser Node.js runtime with filesystem/terminal/package-manager access — "AI models [get] complete control over the entire environment," not code-assist-only; live preview updates as the container rebuilds | The README itself does not detail a diff/changelog UI convention explicitly. One data point *could not be verified against a primary source*: a StackBlitz community-forum thread (secondary, not a vendor statement) describes a beta "diff" apply-mode being reverted in favor of full-file rewrites after users hit more breakage — suggestive, not confirmed by the vendor, that whole-artifact re-emission was found safer than a computed delta even by a team optimizing hard for token cost | [github.com/stackblitz/bolt.new](https://github.com/stackblitz/bolt.new) (fetched directly, primary) |
| **Lovable** | Chat edits, **plus** a first-party direct-manipulation mode: an "Edit" toggle sits next to the chat input; selecting it lets you click any rendered element in the live preview and adjust sizing/color/text/margin/padding/font/Tailwind classes on the spot, no prose required | Per Lovable's own engineering post: on save, the edited JSX/TSX is regenerated from the modified AST, a **diff is computed to touch only the precisely modified lines**, and an HMR event fires back to the session — i.e. Lovable explicitly computes a scoped diff for this path (unlike Bolt.new's anecdotal full-rewrite-only account above) | [lovable.dev/blog/introducing-visual-edits](https://lovable.dev/blog/introducing-visual-edits); [lovable.dev/blog/visual-edits](https://lovable.dev/blog/visual-edits) ("How we built the Visual Edits feature"); [docs.lovable.dev/features/visual-edit](https://docs.lovable.dev/features/visual-edit) (via WebSearch synthesis) |
| **Replit Agent** | Chat + live app preview; **checkpoints** are automatic snapshots (code + workspace + conversation context + connected-DB state) at each meaningful step, surfaced in the Agent tab, Git pane, and a History view; **rollback** restores any checkpoint in one click; **App History** additionally lets you *live-preview* an old checkpoint in the browser without touching the current app/DB | Checkpoints themselves *are* the change-surfacing unit — a labeled point in history rather than a diff view; **Plan Mode** asks clarifying questions on vague requests before acting (official docs' own example: "build out the other links" → agent asks which links) | [docs.replit.com/core-concepts/agent/checkpoints-and-rollbacks](https://docs.replit.com/core-concepts/agent/checkpoints-and-rollbacks) (via WebSearch synthesis) |
| **GitHub Copilot Workspace** | Task (an issue/PR/idea) → **specification** → **plan** → implementation; distinctively, the **plan is itself an editable artifact** — a numbered, natural-language step list you can edit *before* any code is generated, not just a diff to review after | "Everything Copilot Workspace proposes — from the plan to the code — is fully editable, allowing you to iterate until you're confident in the path ahead" (GitHub's own framing); a workspace is shareable via link for teammates to view or fork | [github.blog/news-insights/product-news/github-copilot-workspace](https://github.blog/news-insights/product-news/github-copilot-workspace/); [githubnext.com/projects/copilot-workspace](https://githubnext.com/projects/copilot-workspace/) (via WebSearch synthesis) |

**Cross-vendor pattern:** every one of these gives the user a **natural-language account of
the change** — v0's prose step breakdown, Copilot Workspace's editable plan-before-code,
Lovable's computed scoped diff, Replit's labeled checkpoints — and **none of the surveyed
consumer-facing tools expose a raw tool-call/function-call log** to the end user. This maps
directly onto reuben's own contract: ADR-0048 §4–5 already specifies `Report = {ok, errors,
warnings}` (`Diag[]`, node/port-addressed) plus, for `swap`, a **diff summary**
`{survived, state_reset, added, removed}` — reuben converged on the same "structured natural
summary, not a log" posture independently, and the evidence here says that posture is the
market consensus, not a guess.

---

## 3. Chat-driven editing of a persistent artifact

| Product | Pattern for "one enduring object refined across turns" | Primary source |
|---|---|---|
| **ChatGPT Canvas** | A side panel, distinct from the chat thread, holding one document/code artifact. Edits arrive three ways: (1) plain chat instruction, (2) highlight a passage or click a block-comment icon → inline request scoped to that selection, (3) click into the canvas and type directly. A **"Show changes"** toolbar button renders additions/deletions between versions; a back button restores a prior version. | [help.openai.com/en/articles/9930697](https://help.openai.com/en/articles/9930697-what-is-the-canvas-feature-in-chatgpt-and-how-do-i-use-it); [openai.com/index/introducing-canvas](https://openai.com/index/introducing-canvas/) (via WebSearch synthesis) |
| **Claude.ai Artifacts** | The artifact renders in its own panel; re-prompting produces a new version, with Claude "maintain[ing] context about what you've built and why." A **version-history dropdown** lets you open any past version read-only or **Restore** it to become the current head. Editing a *prior chat message* forks the whole conversation (and its artifact lineage) rather than just the artifact. Text-based artifact types (Markdown/code/plain text) support a direct in-place **Edit content → Save** path that creates a new version without going through chat; image/PDF/HTML/table artifacts do not. | [support.claude.com/en/articles/9487310](https://support.claude.com/en/articles/9487310-what-are-artifacts-and-how-do-i-use-them) (via WebSearch synthesis) |
| **Figma "First Draft"** | Generates from a text prompt not into an image but into a **native Figma document** — real layers, auto-layout, actual components — by assembling from one of four curated component/wireframe libraries rather than free-form pixel synthesis. Once generated, the object is edited with Figma's **ordinary direct-manipulation tools**; no further natural language is required, and none is privileged over direct editing — the generation step and the editing step use entirely separate surfaces. | [figma.com/blog/figma-ai-first-draft](https://www.figma.com/blog/figma-ai-first-draft/) ("Building a better First Draft for designers"); [help.figma.com — Use First Draft](https://help.figma.com/hc/en-us/articles/23955143044247-Use-First-Draft-with-Figma-AI) (via WebSearch synthesis) |
| **Notion AI** | Generate-then-iterate inside Notion's ordinary page/block model ("don't worry about getting it perfect on the first try… iterate until you've got exactly what you need") — closest of the four to plain prose refinement, least differentiated for a structured-object question; noted briefly for completeness. | [notion.com/help/guides/everything-you-can-do-with-notion-ai](https://www.notion.com/help/guides/everything-you-can-do-with-notion-ai) (via WebSearch synthesis) |

**One finding worth flagging prominently, and only partially verified:** multiple 2026
secondary sources (news/aggregator sites, not OpenAI itself) converge on OpenAI removing
Canvas from GPT-5.5 Instant/Thinking on **2026-05-28**, folding writing/code editing into
inline "writing blocks" and "code blocks" in the main chat thread, reportedly for
cross-surface (phone/tablet/desktop) rendering consistency. **This memo could not verify the
claim against an OpenAI primary source** — `help.openai.com`'s release-notes page could not
be fetched directly (§Method), and no first-party OpenAI blog post confirming the change
turned up in search. Treat the fact of the change as plausible-but-unconfirmed, but treat the
*shape* of the move (separate persistent side-panel → inline-in-thread rendering) as itself a
useful signal either way: it means the "artifact lives beside the chat, not in it" design is
not settled even at the product that popularized it.

---

## 4. Generative UI / agent-action-surfaced-in-chat patterns

**MCP Apps (SEP-1865)** — a first-party MCP extension, fetched directly from the spec source
and the announcement blog (both primary, not proxy-blocked): tools declare an associated
interactive UI ahead of time via `_meta.ui.resourceUri`, pointing at a `ui://`-scheme
resource (MIME `text/html;profile=mcp-app`) the server bundles. The host fetches and renders
that resource in a **sandboxed iframe**; the iframe and host talk over **MCP's ordinary
JSON-RPC base protocol carried over `postMessage`** — the same audit trail as any other tool
call — and a UI element can trigger a further tool call via `callServerTool()`. The design
explicitly keeps the model in the loop: a tool's `visibility` metadata can hide it from the
model while leaving it callable from the UI, but "tools MUST return meaningful content array
even when UI is available" so the model's own text-context stays intact regardless of what's
rendered. Ships as part of the same 2026-07-28 spec revision the sibling
[MCP/A memo](mcp-rust-server-landscape.md) already tracked (locked 2026-05-21).
([blog.modelcontextprotocol.io/posts/2026-01-26-mcp-apps](https://blog.modelcontextprotocol.io/posts/2026-01-26-mcp-apps/);
[github.com/modelcontextprotocol/ext-apps, apps.mdx](https://github.com/modelcontextprotocol/ext-apps/blob/main/specification/2026-01-26/apps.mdx) — both fetched directly)

**OpenAI Apps SDK** — the ChatGPT-native counterpart, "later standardized as MCP Apps, so you
can build once and run your UI across MCP Apps-compatible hosts" (OpenAI's own framing, via
WebSearch synthesis): "UI components turn structured tool results from your MCP server into a
human-friendly UI," running in an iframe that "talk[s] to the host via the MCP Apps bridge
(JSON-RPC over postMessage)" and rendering **inline in the conversation, before the model's
own text response** — i.e. the widget shows the artifact and the model's prose narrates or
summarizes it, not the reverse.
([developers.openai.com/apps-sdk](https://developers.openai.com/apps-sdk),
[.../concepts/ui-guidelines](https://developers.openai.com/apps-sdk/concepts/ui-guidelines) — via WebSearch synthesis)

**Anthropic's own tool-use docs** (`platform.claude.com/docs/en/agents-and-tools/tool-use`,
fetched directly — a redirect target of `docs.claude.com`) define the wire-level
`tool_use`/`tool_result` loop, client vs. server tools, and `isError`-style semantics — but
this page is developer/API-facing, not an end-user UI-rendering guideline. **Could not verify
against a primary source:** a written Anthropic document specifically prescribing end-user
rendering conventions for tool results (as distinct from the API contract) did not turn up in
this search; the closest first-party evidence of Anthropic's own convention is Claude.ai's
observed product behavior (§3's version-history UI, and computer-use screenshots rendered
inline) rather than a stated design-pattern doc.

**Relevance to reuben:** ADR-0048's three-layer error model (protocol error / `isError` /
ordinary result including `{ok:false}`) and its `Report`/`Diag`/diff-summary shapes are
structurally the same premise MCP Apps and the Apps SDK build on — a tool result is a
renderable artifact, not a log line. ADR-0052 §1 has already ruled out MCP-the-protocol
reaching the browser tab specifically (no iframe, no `ui://` resource, no postMessage bridge)
— but the *shape* MCP Apps proves out (tool result → structured, model-visible, user-facing
render) is exactly what the in-page tool layer (ADR-0052 §2) would need to reinvent locally:
a future chat window rendering `swap`'s diff summary as a small visual list, in the page's own
UI, is "MCP Apps' idea, contract-ported rather than protocol-ported" — consistent with
ADR-0052's "the contract ports, not the protocol" framing applied one level up the stack.

---

## 5. Cross-cutting synthesis

**(a) Where does direct manipulation coexist with chat, and how is the escape hatch signaled?**
Lovable is the cleanest example: an explicit "Edit" toggle sits *in the chat input area
itself*, so the bypass-language affordance is discoverable from the conversational surface
without leaving it. Figma First Draft is the opposite extreme — a **total** handoff: once
generated, the object is an ordinary Figma document and the chat/generation surface is simply
gone; there is no persistent coexistence, only a one-time transition. ChatGPT Canvas sits in
the middle: direct in-canvas typing and chat-driven edits act on the *same* object
concurrently, for text/code content types only. None of the three music products offer true
parameter-level direct manipulation of the audio itself — only *region selection* (drag a
waveform range) as a precursor to a further generative action, never a manual nudge of a
mix/synthesis parameter.

**(b) How do these products solve the blank-canvas cold start?** Every code-generation
product in §2 ships a template gallery and/or example prompts (v0's template catalog at
`v0.app/templates`; Bolt/Lovable/Replit similarly, per their own marketing surfaces). The
music products front-load **structured prompt formulas** rather than templates — Suno's own
help content still points beginners toward "genre + mood + instrument cue + structure goal,"
and ships a dedicated "AI Music Starter Kit" — turning a blank text box into a fill-in-the-
blank exercise. Figma sidesteps the problem structurally: because First Draft only ever
assembles from a fixed component library, there is no truly-blank failure mode to design
around. The common thread: **no surveyed product ships a bare, unguided prompt box** to a
first-time, non-expert user.

**(c) How is failure or ambiguity in the request surfaced?** Two distinct first-party
patterns turned up, and no third: (1) **ask a clarifying question before acting** — Replit
Agent's Plan Mode, per its own docs, asks what "the other links" means before proceeding on a
vague request; (2) **surface an editable intent artifact before the real work happens** —
GitHub Copilot Workspace's plan-before-code, v0's stated "what it's going to do" pre-step.
Neither of these is "silent best-effort plus a caveat" — both are visible-before-committing
gates. This memo did **not** find a first-party statement of a silent-partial-success pattern
in any surveyed product; if it exists, it wasn't in vendor docs prominent enough to surface
through search. reuben's own ADR-0048 §3 posture — "a failed validation is a successful tool
call," `{ok:false}` is a diagnostic report a model should act on, not an error to retry blindly
— has no directly surveyed precedent here, for or against; it appears to be reuben's own
synthesis rather than an adopted market pattern (flagged, not claimed as either novel or
unprecedented — simply not found in this survey).

---

## 6. Where reuben's shape has no precedent

- **Nothing surveyed keeps a perceptual output continuously running while a conversation
  edits it.** Every music product discards-and-rerenders the full artifact (or a selected
  region of it) on every edit — there is no state that must survive an edit without an
  audible/visible glitch. reuben's `swap`/survivor model (ADR-0046: a node survives if its
  address, Operator type, and instantiate-time identity all match across the swap, so its
  *state* — not just its output — crosses the edit uninterrupted) has no counterpart in
  anything found here. The nearest structural cousin is Replit's live app preview +
  checkpoints — a running process you can roll back — but a web server's request/response
  cycle tolerates a redeploy gap in a way a real-time audio callback cannot; none of the
  surveyed "live preview" mechanisms (Bolt's WebContainer rebuild, v0's preview refresh,
  Lovable's HMR patch) are answering to a hard-realtime, allocation-free render loop where
  dropping one block audibly clicks. A "regenerate and replace" model — adequate for a
  rendered song or a reloaded web page — would be **audibly inadequate** for reuben by
  construction; this is precisely why ADR-0046/0048 built restart-swap-in-M1 with an honest
  `survived: 0` rather than pretending regenerate-and-replace is good enough long-term.
- **The typed port/contract system (Value/Signal/Event) has no analog in anything surveyed.**
  Every builder in §2 edits source text or a DOM tree; every music product edits/regenerates
  audio samples. None expose a graph of named, individually-addressable, typed inputs/outputs
  a user (or an agent) can target one at a time the way reuben's Operators expose ports over
  OSC addresses. Figma First Draft's "generate from a constrained component library, edit
  the real document after" is the *closest philosophical* cousin — compose from known,
  well-typed building blocks rather than freeform synthesis — and it maps cleanly onto
  reuben's own `patcher` skill, which already composes from the live, introspected Operator
  set rather than inventing structure. This is a pattern that plausibly transfers, but it is
  an analogy to reuben's *existing* authoring posture, not evidence about the *chat* layer
  specifically.
- **Patterns that plausibly do transfer, concretely:** (1) a natural-language change summary
  in place of a raw tool/function-call log — near-universal across §2–3, and reuben already
  has the matching contract shape (`Report`/`Diag`, the `swap` diff summary) waiting for a
  chat surface to render it; (2) template/starter-prompt cold-start design — reuben's existing
  Toys and example rigs (README's table: groovebox, chord-player, strum-harp, ...) are already
  the raw material for a chat cold-start screen, needing curation/surfacing rather than
  invention; (3) a chat-surface-adjacent escape hatch into direct manipulation, on Lovable's
  model — except reuben's escape hatch already exists structurally (the knob/pad *is* the
  native mode; `send`/interface pipes predate any chat effort) and the open design question
  a future chat window actually faces is closer to "how does the chat surface make the
  player-facing controls feel reachable/discoverable," which is closer to *Lovable in
  reverse* (Lovable added direct manipulation to a chat-first product; reuben would be adding
  chat to a direct-manipulation-first product) than to any single pattern surveyed here.

---

## Closing synthesis

Every product surveyed that produces something *live and directly manipulable* (§2, Figma
First Draft) converges on the same three moves: a natural-language summary of what changed
in place of a raw log, some form of starter content to defeat the blank canvas, and — where a
direct-manipulation surface exists at all — a chat-adjacent, discoverable toggle into it
rather than a hidden menu. reuben's own MCP/ADR work (ADR-0048's `Report`/`Diag`/diff-summary
contract, ADR-0052's in-page tool layer) already anticipates the first move independently.
None of the text-to-audio products (§1) offer a useful behavioral model for reuben's central
claim — that the NL-authored object is a *running system*, not a file to regenerate — because
none of them have a running system to begin with; their "edit" is always "re-render," and the
adequacy question ADR-0046/0048 already answered (restart-swap is honestly inadequate long-
term; the real survivor machinery is what M2 is for) has no precedent to lean on precisely
because reuben is one of the only surveyed cases where the naive "regenerate and replace"
model would be **audible**, not just slow. The one genuinely new pattern this research
surfaced and worth carrying forward explicitly is **MCP Apps / the OpenAI Apps SDK's "tool
result is a rendered, model-visible artifact, not a log"** (§4) — a shape reuben's contract
layer already fits, and one the future web-chat effort could adopt *in spirit* (render
`swap`'s diff as a small visual list in the page's own chat UI) without adopting the protocol
ADR-0052 already declined to bring to the browser.

---

## Sources

All accessed 2026-07-11. Method note repeated: entries marked "fetched directly" were read as
raw pages (GitHub/`raw.githubusercontent.com`/`*.anthropic.com`/`*.claude.com`, and
`blog.modelcontextprotocol.io` + `github.com/modelcontextprotocol/ext-apps`, none of which hit
the sandbox's proxy allow-list wall); all others came through the WebSearch tool's server-side
fetch-and-summarize of the cited vendor URL (§Method explains why direct `WebFetch`/`curl` to
these domains 403'd at the gateway in this sandbox).

**Text-to-audio/music:**
- Suno — https://suno.com/release-notes , https://help.suno.com/en/categories/550017
- Udio — https://help.udio.com/en/articles/10748731-changelog-what-s-new-with-udio
- ElevenLabs Music — https://elevenlabs.io/docs/api-reference/music/compose ,
  https://elevenlabs.io/docs/eleven-api/guides/how-to/music/composition-plans ,
  https://elevenlabs.io/docs/api-reference/music/separate-stems ,
  https://elevenlabs.io/blog/eleven-music-new-tools-for-exploring-editing-and-producing-music-with-ai

**AI app/UI builders:**
- v0 (Vercel) — https://v0.app/docs , https://v0.app/templates
- Bolt.new (StackBlitz) — https://github.com/stackblitz/bolt.new (fetched directly)
- Lovable — https://lovable.dev/blog/introducing-visual-edits ,
  https://lovable.dev/blog/visual-edits , https://docs.lovable.dev/features/visual-edit
- Replit Agent — https://docs.replit.com/core-concepts/agent/checkpoints-and-rollbacks
- GitHub Copilot Workspace — https://github.blog/news-insights/product-news/github-copilot-workspace/ ,
  https://githubnext.com/projects/copilot-workspace/

**Chat-driven persistent-artifact editors:**
- ChatGPT Canvas — https://help.openai.com/en/articles/9930697-what-is-the-canvas-feature-in-chatgpt-and-how-do-i-use-it ,
  https://openai.com/index/introducing-canvas/
- Canvas-removal claim (unverified against an OpenAI primary source; secondary aggregation
  only) — https://www.krasa.ai/news/openai-gpt-5-5-instant-writing-coding-blocks-canvas-removed-may-2026 ,
  https://www.aicerts.ai/news/chatgpt-canvas-sunset-key-dates-impacts-migration-guidance/
  (attempted direct fetch of https://help.openai.com/en/articles/9624314-model-release-notes — 403 at the sandbox gateway)
- Claude.ai Artifacts — https://support.claude.com/en/articles/9487310-what-are-artifacts-and-how-do-i-use-them
- Figma First Draft — https://www.figma.com/blog/figma-ai-first-draft/ ,
  https://help.figma.com/hc/en-us/articles/23955143044247-Use-First-Draft-with-Figma-AI
- Notion AI — https://www.notion.com/help/guides/everything-you-can-do-with-notion-ai

**Generative UI / tool-action rendering:**
- MCP Apps announcement — https://blog.modelcontextprotocol.io/posts/2026-01-26-mcp-apps/ (fetched directly)
- MCP Apps spec (SEP-1865) — https://github.com/modelcontextprotocol/ext-apps/blob/main/specification/2026-01-26/apps.mdx (fetched directly)
- OpenAI Apps SDK — https://developers.openai.com/apps-sdk , https://developers.openai.com/apps-sdk/concepts/ui-guidelines
- Anthropic tool use docs — https://platform.claude.com/docs/en/agents-and-tools/tool-use/overview (fetched directly; redirect target of docs.claude.com)

**reuben (local, read 2026-07-11):** docs/adr/0052-web-parity-contract-not-protocol.md;
docs/adr/0048-mcp-tool-surface-and-contracts.md; docs/research/mcp-rust-server-landscape.md;
README.md; CONTEXT.md.

**Network-policy note:** sandbox proxy allow-list confirmed via `curl`
(`CONNECT tunnel failed, response 403`) for `example.com`, `elevenlabs.io`, `suno.com`;
`GET https://code.claude.com/docs/en/mcp` and `raw.githubusercontent.com` succeeded directly
in the same session, isolating the failure to gateway policy rather than vendor-side blocking.
