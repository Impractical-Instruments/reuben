# Web-chat authoring UX — implementation spec

**Status:** agent-ready. An implementation agent can build the in-page chat UI cold in one
~100K session from this document.
**Destination artifact of** the wayfinder map
[Web-chat authoring UX (#298)](https://github.com/Impractical-Instruments/reuben/issues/298) ·
**Assembles** decision tickets A–J (see [§0.3](#03-provenance--the-decisions-this-spec-encodes)).
**Grounds on** [ADR-0052](adr/0052-web-parity-contract-not-protocol.md) (the in-page tool layer
this UI targets) and [ADR-0048](adr/0048-mcp-tool-surface-and-contracts.md) (the contract/report
shapes it renders).

**Scope.** This is the **UX** of reuben's web-player chat authoring experience: what the user
sees and does, top to bottom, for **both** first-generation ("describe it → get a first playable
instrument") and **ongoing reshaping** (later turns editing an already-sounding instrument). It
does **not** design the agent host (auth/cost/latency), the in-page tool layer's JS, or the wasm
exports ADR-0052 §2 names — those are the downstream implementation epic's, listed in
[§10 Out of scope](#10-out-of-scope--deferred-doors). Where a decision constrains that build, it is
called out as an **observable requirement**, not an implementation.

**Visual references** (THROWAWAY mocks — fake timing, no engine; they ship nothing, cited only as
the pixel reference the way P7/B's `loading-ux-prototype` was for its SPEC):

- Cold start — [`web/bench/cold-start-prototype/prototype.html`](https://github.com/Impractical-Instruments/reuben/blob/claude/issue-298-next-ticket-0o7k5d/web/bench/cold-start-prototype/prototype.html)
  ([NOTES.md](https://github.com/Impractical-Instruments/reuben/blob/claude/issue-298-next-ticket-0o7k5d/web/bench/cold-start-prototype/NOTES.md))
- Reshape diff / thinking state — [`web/bench/reshape-diff-prototype/prototype.html`](https://github.com/Impractical-Instruments/reuben/blob/claude/wayfinder-next-issue-298-76qa0x/web/bench/reshape-diff-prototype/prototype.html)
  ([NOTES.md](https://github.com/Impractical-Instruments/reuben/blob/claude/wayfinder-next-issue-298-76qa0x/web/bench/reshape-diff-prototype/NOTES.md))

---

## 0. Ground truth

### 0.1 The persona

A **non-musician or non-technical musician**, in a browser tab, with no toolchain, describing a
sound in plain language. The `patcher` / `control-surface` skills already serve the
dev-with-a-checkout persona — that is out of scope here (the
[MCP-server map (#270)](https://github.com/Impractical-Instruments/reuben/issues/270)'s lane).
Every copy and interaction decision below is calibrated to someone who does **not** carry the
"audio must survive an edit" reflex and does **not** know reuben has an engine graph.

### 0.2 The in-page tool layer this UI drives (ADR-0052 §2)

The chat agent binds a JS tool layer over the C-ABI worklet — one tool per ADR-0048 contract,
**same report shapes** as native. The UI is a consumer of these eight contracts and **nothing
else**:

| Contract | What the UI gets from it |
|---|---|
| `describe_operators` | the capability surface — how the agent knows what reuben *can't* do (drives E's "nearest thing") |
| `describe_instrument` | the current document's structure/params — source of the change diff |
| `validate` | pre-swap check; a `{ok:false}` here is the agent's to repair, **never shown to the user** (E case 3) |
| `send` | the flat live control codec (`queue_control`) — **parameter-only** reshapes route here (F) |
| `swap` | destroy → stage → construct; **restart-swap indefinitely** on web (`survived: 0` always) — **structural** reshapes route here (F) |
| `engine_status` | worklet/audio state (playing, suspended) |
| `get_current_instrument` | the staged document the shell owns (keep-gesture source, G) |
| `get_diagnostics` | shell counters |

**Load-bearing facts these contracts fix:**

- **Web `swap` is restart-swap, indefinitely** (ADR-0052 §2). The single-threaded worklet cannot
  build an engine off-thread, so **every structural reshape audibly restarts the sound** —
  `survived: 0`, `state_reset` total. The UI must **never imply** reuben's survive-the-edit
  differentiator on web, because web genuinely cannot honor it.
- **In-page `swap` is by-value**, and **every kept swap pairs with a keep gesture** (ADR-0052 §3).
  This is an **ordering constraint on ship**, restated in [§7](#7-the-keep-gesture-g): *the chat
  window does not ship to users before the keep gesture is wired into its loop.*
- **A failed validation is a successful call** (ADR-0048 §3). `{ok:false}` with a node/port-addressed
  `Diag[]` is a **report to act on, not an error to retry** — and on this lane it is the *agent's*
  report about its own proposed document, repaired within the turn (E case 3).
- **`swap` diff summary shape** (ADR-0048 §5): `{survived, state_reset, added, removed}`, node/port
  addressed. On web, `survived`/`state_reset` are degenerate per turn (see above), so the UI's diff
  is driven by the **structural node-identity diff** — see [§4.6](#46-the-diff-the-card-actually-renders).

### 0.3 Provenance — the decisions this spec encodes

Every section below folds one resolved decision ticket. The ticket is the authority; this spec is
the executable assembly.

| § | Concern | Ticket |
|---|---|---|
| 1 | User-facing vocabulary | [A #299](https://github.com/Impractical-Instruments/reuben/issues/299) |
| 2 | Cold-start screen + starter content | [B #300](https://github.com/Impractical-Instruments/reuben/issues/300) · [I #330](https://github.com/Impractical-Instruments/reuben/issues/330) |
| 3 | Interaction model (the spine) | [C #301](https://github.com/Impractical-Instruments/reuben/issues/301) |
| 4 | Rendering the agent at work | [D #302](https://github.com/Impractical-Instruments/reuben/issues/302) |
| 5 | Ambiguity & failure | [E #303](https://github.com/Impractical-Instruments/reuben/issues/303) |
| 6 | Sonic honesty (the re-strike) | [F #304](https://github.com/Impractical-Instruments/reuben/issues/304) |
| 7 | The keep gesture | [G #305](https://github.com/Impractical-Instruments/reuben/issues/305) |
| 8 | Skill-level register | [J #336](https://github.com/Impractical-Instruments/reuben/issues/336) |

### 0.4 Code anchors (the surface this replaces)

- `web/src/main.js` — `buildPlayerScreen` (centered `.player` section, surface-mount grid),
  `doShare` (today's Share button → becomes Keep, §7), `launcherScreen` / `prefetchToy` (gallery,
  §2), the existing single centered column shell (`max-width: 60rem`, `viewport-fit=cover`).
- `web/toys.json` — the curated bundled instrument list (5 today) — the gallery's raw material and
  the chip-authoring surface (§2).

---

## 1. The lexicon (A)

**Root rule — selective surfacing.** reuben's engine graph is **fully hidden**. The chat speaks
**only in sound and musical intent**, never structure. The following are **never named to the
user**, anywhere in the UI: *Operator, input/output/port, patch/wire, swap, Plan, address,
Coordinator, voicer/voice, survivor, rig, tuning, Good Button, interface pipe, surface/widget,
param.* There is **no** progressive disclosure of the graph in this effort.

### 1.1 The words that surface

| Internal concept | The chat says |
|---|---|
| Instrument / rig / Toy | **"instrument"** — one word throughout, even when internally rig-scale. **"Toy" is retired from the chat lexicon** (this is chat-only; domain-wide retirement is out of scope, §10). |
| ready-made starters | **"example instruments"** — no new category noun; never "preset"/"template". |
| swap / reshape | **"update"** — e.g. "I updated your instrument." ("reshape" is an internal word, never surfaced.) |
| controls (umbrella) | **"controls"**, plus concrete shape-names when they help ("brightness knob", "tap pads"). |
| an Operator / a component | **no generic part-noun.** "What's in it?" → a plain capability description, never a parts inventory. |

### 1.2 Concrete music vocabulary — sensory-first, mirror the user, calibrate to skill

The chat leads with **sensory** language and **mirrors any term the user brings** (a beginner who
says "chord" gets "chord" back). It never *leads* with a term above the inferred tier
(the tier machinery is [§8](#8-skill-level-register-j)). The sensory↔theory pairs:

| Dimension | `plain` (leads with) | `theory-aware` (leads with) |
|---|---|---|
| Speed | faster / slower | tempo *(BPM always hidden)* |
| Tonality | mood words: happy / sad / dark / bright | key / chord / scale |
| Filter | brightness / muffled | cutoff / resonance |
| Oscillator | warm / harsh / buzzy tone | oscillator / waveform |
| Envelope | how it fades in/out, snappy / smooth | attack / release |
| Modulation | wobble / movement | LFO |

**Freely used at any tier** (a layperson owns them): note, beat, reverb, echo, distortion,
louder/quieter, higher/lower. **Hidden reuben music-model, surface only the plain shadow:** Pitch
(Degree/Absolute) → "higher/lower"; Harmony bus → "key / mood"; Voice/Voicer → "how many notes at
once"; **Tuning hidden entirely** (12-TET default invisible).

**Honesty rule (governs §7's wording too):** never over-promise. The chat never claims a store,
account, or capability that does not exist.

---

## 2. Cold start (B) + starter-content sourcing (I)

### 2.1 The first-run screen — gallery-first

- **Example instruments front and center.** One tap → **playing sound**. This leads with reuben's
  play-first, direct-manipulation-first nature; the research memo found **no** surveyed product
  ships a bare prompt box to a first-timer.
- **A persistent "…or describe your own" prompt bar** sits below the gallery, **always visible,
  never buried**. This bar is the *same* input line that continues into the player as C's pinned
  reshape input (§3.3) — one input, cold-start through play.

### 2.2 Gallery membership (I)

- **The gallery IS the bundled `web/toys.json` set** — **all** entries (5 today: Groovebox, Chord
  Player, Strum Harp, Euclidean Drums, Mic Space), in the file's existing `order`. `toys.json` is
  already the human-curated showcase (its whole job); **no auto-derivation from `instruments/`** in
  v1.
- **Show-all in v1** — the 5 fit one screen; `order` is the ranking. `order` already front-loads a
  self-playing instrument (Groovebox `order` 1), so B's "one tap to playing sound" is honored by
  *ordering*, not by dropping members (Mic Space stays, last, as the live-input showcase).
- **Deferred door, not built:** a featured-cap (top-N above the fold + a "more" affordance,
  `order` as the promote ranking) for when the bundled set outgrows one screen. Needs no new
  metadata.

### 2.3 Instrument-pick → proactive turn one

Tapping an example instrument starts it **playing** *and* reuben opens **chat turn one**:

1. a short greeting **naming what's playing**,
2. an invitation to reshape it,
3. **2–3 tappable quick-change chips**.

This defeats the *second* blank canvas — a newcomer who is playing but doesn't know they can talk
to the instrument — and gives a one-tap path to a first successful `update`.

**Chips (I):**

- **Hand-authored per-instrument**, stored as a new **optional `chips: [string]`** array per
  `toys.json` entry (alongside `id`/`title`/`blurb`/`kind`/`order`). Not a generic fixed set, not
  generated at cold-start (generation adds latency + non-determinism to the one impression curated
  content exists to protect).
- **Chip shape: a single sensory string that posts VERBATIM as the user's turn** — no hidden
  `{label, prompt}` pair. "What you said is what happened" must hold (C/D's model is chat *teaches*
  the controls, and the user could type the same phrase next time). Chips flow through the **normal
  reshape path**; E covers a chip the agent can't satisfy, same as any typed turn.
- **Register:** authored chips are written in **`plain`** register (§8 — the first turn carries no
  signal, so no theory-aware static content ever exists).
- **Optional, tailored-or-nothing.** Present → show the authored 2–3. **Absent → greeting-only turn
  one, no generic filler** (a new instrument can ship + appear in the gallery before its chips are
  authored). Falling back to generic would smuggle the rejected generic-fixed model back in.
- **Illustrative sets** (authoring is the implementation epic's, not fixed here):
  Groovebox `"busier beat" / "more swing" / "deeper kick"`; Euclidean Drums `"denser pattern" /
  "add a tom" / "slower"`; Chord Player `"brighter chords" / "switch to a minor key" / "warmer
  pad"`; Strum Harp `"wider range" / "softer plucks" / "change the key"`; Mic Space `"bigger room"
  / "darker tone" / "less reverb"`.

### 2.4 Describe-path → echo → build → play → land

In the "describe your own" path (typing, or tapping an example-prompt chip — the **same action**):
the user's words appear as their **first chat message** → reuben builds it → it **starts playing**
→ reuben closes the turn by **naming what it made and what they can change next** — symmetric with
the instrument-pick landing.

**Scope boundary (I):** authored `chips` cover **only the gallery-pick turn one**. The describe
path and **all** post-reshape next-change suggestions are **generated** (out of the sourcing model;
they belong to the interaction model / D).

---

## 3. The interaction model — the spine (C)

Seven decisions, top to bottom. This is the keystone; everything else renders on it.

### 3.1 Co-presence (the root)

The live instrument's controls are **never fully occluded by chat**; there is **no
play-mode/reshape-mode switch**. Reshaping happens *in view of* the running instrument. A
mode-switch is **ruled out** — it would hide the very thing reuben is.

### 3.2 Roles — the artifact is primary, chat is the editor

The **instrument surface is the product/artifact** (the thing you play, keep, share). **Chat is
the editor** bound to it. This is the Canvas/Artifacts relationship **inverted**: the **artifact is
the always-on primary**, chat is the authoring surface — "the editor for the product," not a slimmer
subordinate bar.

### 3.3 Permanent input, collapsible transcript

- The **reshape input line is always visible and anchored** — never behind a menu. This keeps
  reshaping discoverable and the controls un-buried.
- The **conversation transcript** is what expands and collapses.
- **Default state on arrival:** land **expanded** when you got here by *making* an instrument (you
  were just talking); land **collapsed to the bar** when you picked a gallery instrument to *play*
  (B's proactive turn-one still fires, then settles).

### 3.4 Modeless; controls stay live through the agent's turn

Hands are always live on the controls; words are always available; **nothing switches a mode.**
During the agent's turn (the seconds between Send and the reshape landing), the currently-sounding
instrument **keeps playing and keeps responding to the user's hands** — **no freeze**. The **only**
interruption is the single restart-swap moment at swap-time ([§6](#6-sonic-honesty--the-re-strike-f)),
not a dead zone spanning the whole think.

### 3.5 Reshapes land on the surface (the reverse-Lovable move)

A worded edit that touches a control **that already exists on the surface** (e.g. "make it darker"
lowers a filter cutoff that *is* a knob) **highlights/animates that control on the instrument
itself** — the **surface is the primary place a change is shown**; the transcript carries the
natural-language summary in parallel. Lovable lets you *click an element* to edit it; reuben does
the **mirror** — you edit with *words* and the surface shows you *which control that was*, teaching
the non-musician the bridge between "darker" and the knob. (The highlight/diff treatment itself is
[§4](#4-rendering-the-agent-at-work-d).)

### 3.6 Stable layout by default; directed & suggested re-layout

- Controls that **survived** a reshape **hold their position** across the update — the board
  *evolves* rather than reshuffles. **Layout keys on node identity** (the visual counterpart to the
  survivor model). Added controls animate in (highlighted, §3.5); removed controls animate out.
- **Exceptions:** re-layout is an **explicit user-directable action** ("rearrange this / clean up
  the layout"), and the agent **proactively suggests re-layout** for big structural changes (the
  same ~4–5-change threshold D uses, §4.4) or when a different arrangement would play better.
- **Observable requirement for implementation:** surface layout **keys on node identity, not a
  fresh sort each render.** A board that reshuffles every turn re-buries the knob the user just
  learned.

### 3.7 One responsive layout

**Not** two divergent spines — **one responsive layout that scales**:

- the **surface owns the top/center at every width**;
- the **chat input line is pinned to the bottom** (thumb-reachable on a phone, fine on desktop);
- the **transcript expands upward as a collapsible sheet**.

Small-screen concession is **partial, collapsible occlusion of the *lower* surface** — **never a
full-screen takeover** (that is the ruled-out mode-switch). One spine, one mental model, one build,
phone → desktop; consistent with the existing single centered column shell.

---

## 4. Rendering the agent at work (D)

Six parts. Renders on the §3 spine.

### 4.1 Diff home — surface highlight + a persistent change-card (A+B hybrid)

Two **linked** representations of every reshape:

- **On the surface:** affected controls animate/glow keyed on node identity — **added pulses in,
  changed sweeps its value + glows, removed animates out** (§3.5's reverse-Lovable).
- **In the transcript:** a compact, persistent **change-card** — a scannable, scroll-back record,
  **one row per change** (added / changed / removed).
- **Linkage:** hovering/tapping a card row **echo-highlights its control** on the surface; the
  surface glow fades after a beat, the **card persists**.

(Rejected: pure-surface — nothing to scroll back to once badges fade; pure-card — loses the "which
knob was that" teaching.)

### 4.2 Thinking state — a streaming plan that resolves in place into the card

On Send, the card **appears immediately** showing a natural-language **plan** ("Lowering the tone,
adding a shimmer…"), then **resolves in the same object** into the final rows once the update lands
(the v0 / Copilot-Workspace shape — intent shown *before* the sound restarts). The instrument keeps
playing and stays hand-live throughout (§3.4); only the restart-swap moment interrupts (§6).

(Rejected: a quiet ghost-line — emptier wait; a surface ghost-preview — presumes the outcome before
validation.)

### 4.3 No-knob changes — a card row, no surface echo

Not every added/removed/changed node maps to a surface control (added reverb with no knob, an
internal reroute, a paramless node). Such a change still gets a **sensory card row** ("Added reverb
— more space") but has **no control to echo**. **The card is the complete record; the surface
highlight is the bonus that fires only when a control exists.**

### 4.4 Big diffs — enumerate small, collapse past a threshold

Small reshapes enumerate every row. Once a change crosses **~4–5 changes** (the same "big
structural change" threshold §3.6 flags for a proactive re-layout suggestion), the card collapses to
a **summary headline** ("Rebuilt — essentially a new instrument") with an expandable "show all
changes." Keeps the common small case crisp and the rare rebuild from flooding the transcript.

### 4.5 Card language is sensory-only

Rows read **"Added Shimmer" / "Brighter → darker"**, never node/operator names. The card **is the
translation layer** from the node-addressed diff to A's sound/intent vocabulary (§1), and calibrates
to the register (§8).

### 4.6 The diff the card actually renders

Because web `swap` is restart-swap (`survived: 0` always), **survivor / `state_reset` stats are
degenerate per turn and are NOT shown** (that everything resets sonically is §6's honesty, not a
per-control fact). The card is driven by the **structural node-identity diff** computed from the
before/after documents:

- `added` (nodes in the new document, absent from the old) → **added rows** + surface pulse-in
  where a control exists;
- `removed` (nodes in the old, absent from the new) → **removed rows** + surface animate-out;
- **computed "changed"** (params differing on nodes present in **both** documents) → **changed
  rows** + surface value-sweep glow.

Layout keys on node identity (§3.6): survivors hold position.

### 4.7 Restart-honesty is D's slot, F's content

The card **reserves a render location at its foot** for the "the sound restarted to apply this"
honesty. **D fixes *where* it renders; [§6](#6-sonic-honesty--the-re-strike-f) decides how loud it
is and what it says.**

---

## 5. Ambiguity & failure (E)

**Core principle: the user-facing failure surface has essentially ONE shape, and only ambiguity
acts.** Everything the agent knows about *why* something went wrong — capability gaps, node/port
`Diag`s — stays **internal**. The user meets sound and intent only (§1).

### 5.1 The four cases

**1. Ambiguous but actionable** ("make it warmer") → **act-then-react (best-effort).** The agent
picks the most likely reading and makes the change, which **plays immediately** (asking a text
question would stall the play-first loop). The guess is surfaced **on D's change-card**: a one-line
sensory *"how I read it"* preface + **1–2 tappable alternative-interpretation chips** (reusing B's
quick-change chip pattern). One tap re-reshapes toward the other reading — **a wrong guess is one
tap from fixed, no typing.** The chips double as teaching.

**2. Unsatisfiable** (reuben genuinely can't — "add a saxophone I hum in") → **"can't — but here's
the nearest thing."** Plain, sound/intent-framed: *"I can't bring in a saxophone, but I can make the
lead breathier and more reedy — want that?"* The nearest achievable move is a **tappable action**.
**The engine reason is never exposed** — the agent knows the capability surface via
`describe_operators` and reframes. Sound unchanged → **chat-only turn, no change-card, no surface
highlight.**

**3. Validation failure (`{ok:false}`)** → **agent self-corrects silently; the user never sees a
`Diag`.** The agent has `validate` *and* `swap`, so it validates its own proposed document before
swapping. A `{ok:false}` with node/port `Diag[]` is the *agent's* mistake, not the user's request
being wrong — and per **ADR-0048 §3 it is "a report to act on, not an error to retry."** The
`Diag[]` is agent-internal fuel: the agent repairs/retries **within its turn**. The user sees only
the eventual success (a normal change-card) or, if attempts are exhausted, a terminal plain-language
message that **collapses into case 2's shape** ("I couldn't make that change stick") — never
node/port language.

**4. Empty / no-op / off-topic** (empty send, gibberish, "what's the weather") → **gentle
re-orient.** Not an error — the user doesn't know what to say. Friendly nudge + tappable **starter
directions / quick-change chips** (B's cold-start content as a *permanent* safety net; off-topic
gets a light *"I make sounds — want to try making this brighter?"* redirect). Sound untouched. The
suggested directions draw from the **same source I (#330)** defines.

### 5.2 Container rule (resolves C's & D's hand-off)

- Best-effort guess / caveat / alternative chips **ride D's change-card**.
- "Can't, nearest thing" / terminal failure / re-orient are **plain chat turns** in the transcript.
- **The rule: no surface change ⇒ no change-card — it's a chat turn.**

### 5.3 The one phase divergence

The taxonomy and posture are **identical** at first-generation and at reshape. The **only**
creation-vs-reshape difference is the **terminal-failure fallback state**:

- **Reshape** terminal failure → the **prior sound keeps playing** (ADR-0048 §5: `{ok:false}` ⇒
  nothing installed, the old sound survives).
- **First-creation** terminal failure (reachable only via the describe path from cold-start, nothing
  yet playing) → land the user **back at the gallery/cold-start** (§2) with *"I couldn't build that
  from scratch — want to start from one of these and shape it?"* — reinforcing the gallery-first bet.

---

## 6. Sonic honesty — the re-strike (F)

**Stance: embrace the restart as a deliberate re-strike.** **No** warn-before, **no** apply-gate,
**no** masking, **no** per-turn apology. A structural reshape is presented as *the instrument
replaying with your change, from the top* — which is literally what happens: every node comes back
cold (ADR-0046 §10), so the clock/sequencer restarts at step 0. **The phase reset — not the ~100ms
silence — is the salient event.** The non-musician persona doesn't carry the "audio must survive an
edit" reflex: "I asked for a change → it played the new thing" *is* a re-strike. (Masking is
impossible anyway — the single-threaded worklet can't hold two engines for a crossfade.)

### 6.1 The caveat that shrinks the restart — `send` vs `swap`

Only **structural** reshapes restart:

- **Parameter-only reshape** (existing graph, new values) → routed through **`send`**
  (`queue_control`) → **truly live: no gap, no phase reset.** The control moves under the user's
  eyes (§3.5). Gets D's card row; **no re-strike.**
- **Structural reshape** (add / remove / rewire nodes) → **`swap`** → restart. **Only this** triggers
  the re-strike.

The `send`-vs-`swap` split is **invisible plumbing — never named to the user** (naming "parameter
vs graph" would leak the engine graph §1 retired). The two behaviors read as **magnitude-appropriate**
with no explanation: a small tweak staying live vs. adding a part replaying-from-the-top matches
naive intuition.

**Observable requirement for implementation:** the tool layer/agent **routes a parameter-only
reshape through `send`, a structural reshape through `swap`.** The UI's job is to render each as its
magnitude-appropriate behavior (live sweep vs. re-strike).

### 6.2 Making the re-strike read as intentional, not broken

Principle: **a gap with a co-timed visible cause reads as "the change landed"; a gap with no visible
cause reads as a glitch.** D's flow already streams intent *before* the restart and resolves the card
*at* it, so:

1. **Co-timed cause** — the change-card commits and the surface animates (§4.6's diff: added pulses
   in, removed animates out) **exactly** as the sound drops and returns. Cause and effect
   simultaneous.
2. **Replay-from-top is shown** — any transport/playhead **visibly returns to the start**, so the
   phase reset reads as "playing the new version from the beginning."
3. **A decisive, consistent gesture** every structural change — learned as this product's
   punctuation. **No spinner / "loading…" over the gap** — 100ms of loading reads as jank; it's a
   *beat*, not a *wait*.
4. **A clean declicked duck** — the output **fades to silence and back** over a few ms, **never a
   hard cut / click.** A click is the single thing most likely to read as broken. *(Observable
   requirement: no click; a deliberate duck. Ramp length/shape is implementation's — apply
   ADR-0050's raised-cosine declick philosophy to the restart edges.)*

### 6.3 Timing

**Instant re-strike for v1**, carried by §6.2's gesture. **Quantize-to-downbeat** (hold the swap
until the loop's bar line) is a **named future enhancement — not v1**: it is engine-scheduling
machinery, helps only clocked instruments (a drone has no downbeat), and adds up-to-a-bar latency
that can read as unresponsive.

### 6.4 Words — D's reserved honesty slot (§4.7)

**First-run only.** **One** light, positive framing line the **first** time a structural change
restarts the sound **in a session** — owning the replay ("here's the new version, from the top") —
then **wordless on every repeat.** A per-turn line re-pathologizes a restart we've decided is
intentional; a one-time line inoculates a beginner meeting their first restart, then gets out of the
way. Phrasing inherits §1's register and calibrates per §8.

**Consequences:**

- **Param-only reshapes never carry a restart line** (they don't restart) — they still get D's card
  row.
- **If nothing is currently sounding** when a structural change lands, there's no restart to be
  honest about — it just **builds and is ready**; the re-strike framing applies only when the
  instrument is actively playing.

---

## 7. The keep gesture (G)

The keep gesture is **the ADR-0042 Share-link mechanism, re-presented as a save** — exactly what
ADR-0052 §3's ship-gate asks for (in-page `swap` is by-value; page memory is volatile). Eight
decisions:

### 7.1 Frame — keep-to-not-lose, not share-with-others

One mechanism (the self-contained bundle URL) serves both, but v1 **leads with loss-prevention**:
the link is your *save*; sharing rides along on the same link ("…or paste it anywhere to share"),
never the headline.

### 7.2 Word — "Keep"

The affordance is labeled **Keep**, carrying a persistent state: **"Not kept yet" → "Kept ✓"**.
Chosen over "Save" (over-promises a cloud/account store that doesn't exist — §1's honesty rule
forbids it; the reality is a link you bookmark) and over today's "Share" (foregrounds an audience
the first-timer may not have, and gives volatility no word).

### 7.3 Placement — the bottom-anchored chrome, by the input line

A persistent control **paired with the pinned reshape input at the bottom** of §3's spine — always
visible whether the transcript sheet is expanded or collapsed, thumb-reachable on a phone. (Moves
**off** today's top-header Share button.) The state it reports is a property of the top/center
artifact, but the *action* lives in the bottom chrome per §3.

### 7.4 Ephemerality — passive state teaches it; a leave-guard catches real loss

The always-visible "Not kept yet / Kept ✓" state teaches volatility passively (no nag). A
**navigate-away leave-guard** is the loss-side safety net, and it fires **only when there is
diverged, un-re-findable, unkept work.** Scoping lever: a **gallery instrument the user hasn't
touched is re-findable** (still in the gallery), so nothing is truly at risk until the instrument
**diverges** — the user described their own, or reshaped an example.

### 7.5 Proactivity — one-time visual pulse, no chat turn

On the **first divergence**, the "Not kept yet" chip **announces itself with a single subtle
pulse/highlight** (the same visual language as §4's surface highlights), then sits quiet. Proactive
**exactly once**, on the chrome, **never in the transcript** — no recurring nag. The leave-guard is
the only other proactive moment, and it is **loss-triggered, not satisfaction-triggered.**

### 7.6 What Keep produces (v1) — copy the link AND make reload/bookmark work

Tap Keep → **write the snapshot to `location.hash`** (so **reload restores it** and **bookmarking
the page** becomes a store the non-musician already understands) **and** copy the link. A brief
non-modal confirm: *"Kept ✓ — bookmark this page to come back, or paste the link anywhere to
share."* This is **one snapshot write** — inside ADR-0042 §2's deferral boundary. **Live re-encoding
as-you-play (updating the hash on every gesture) stays deferred** and is **not** in scope.

### 7.7 Staleness after a later reshape — the unsaved-changes model

Keep-state is **relative to the live instrument.** A diverging reshape after a keep flips **"Kept ✓"
→ "Not kept yet"** and **re-arms the leave-guard** (no re-pulse — §7.5's one-time pulse already
fired). The written hash **stays at the last keep** (reload = last keep). Keeping again
re-snapshots. Familiar "you have unsaved changes."

### 7.8 Agent role — reuben points to Keep, the user's tap performs it

Keep is a **page gesture, not a ninth contract** — not among ADR-0052's eight, and the browser
requires a **user gesture** to write the clipboard, so the agent *cannot* mint the link itself. When
the user asks about saving/sharing in chat, reuben answers in one line and **highlights/pulses the
Keep control** so the eye goes to it; the mint happens on the user's tap. **Agent directs, never
performs.**

### 7.9 Samples edge (stated so it isn't re-litigated)

ADR-0042 §3 refuses sample-bearing links at mint. This **cannot arise in v1's chat lane**: the
cold-start gallery is sample-free and the chat lane has no sample-upload path yet. So **Keep is
always available in v1.** When user samples arrive, Keep inherits ADR-0042 §3's refusal shape and
needs a non-musician wording pass then — out of scope now.

**Ship-gate satisfied:** a keep gesture is wired into the chat loop (bottom-chrome "Keep",
ephemerality-aware), and it is the ADR-0042 Share-link mechanism presented as a save. Per ADR-0052
§3, the chat window can ship once this is built — and **not before.**

---

## 8. Skill-level register (J)

The cross-cutting register every other turn calibrates against (B's greeting, D's card verbs, E's
copy, F's restart line, next-change suggestions). It sits **on top of** §1's mirror rule — the two
together are the complete adaptation story.

### 8.1 Shape — binary, governs only what the chat *leads* with

Register is a **binary: `plain` (default) vs `theory-aware`.** Not a spectrum, not N tiers. The
finer, per-term gradation is **already handled reactively by §1's mirror rule** (the chat echoes any
term the user brings). Register therefore decides **only what the chat leads with proactively** when
it has no user term to mirror — the greeting, next-change suggestions, card verbs, caveats. Every
concrete divergence in §1.2 is itself a binary pair, so a smoother spectrum would be redundant.

### 8.2 Signals — start `plain`; bump on unprompted theory vocabulary

- **Default:** everyone starts `plain`. Leading with jargon to a beginner is alienating; a
  beginner-friendly phrasing to an expert is at worst mildly slow (and mirroring upgrades any term
  they use instantly).
- **Sole active bump signal:** the user reaching for **genuine music-theory vocabulary, unprompted**
  ("put it in a minor key", "swung 6/8", "add a fifth"). This is the **same** signal §1's mirror rule
  keys on — **no separate detector.**
- **Volunteered self-description** ("I'm a producer") is honored → jump to `theory-aware`. But the
  chat **never asks** "what's your skill level?" — that's the quiz the persona came to avoid.
- **Rejected signal — the cold-start example pick.** A beginner may tap the most complex-sounding
  instrument; picking ≠ vocabulary. **No inference from it.**

### 8.3 Shift — ratchet up only, session-scoped

- **Monotonic upgrade; never auto-demote.** Once a user has shown they know "tempo", snapping back
  to "faster/slower" reads as condescending. The cost of *staying* `theory-aware` is near-zero
  because mirroring still meets them turn-by-turn.
- **False-bump guard:** only **user-originated, unprompted** terms count. If reuben led with "tempo"
  and the user echoes it, that's them mirroring *us*, not evidence — otherwise one theory-aware chip
  bootstraps the whole session's tier (a feedback loop).
- **Persistence: session-scoped**, resets on a fresh session. v1's lane is account-free (the keep
  gesture is a link snapshot; no profile to store a tier).

### 8.4 Scope — vocabulary only, not verbosity

Register selects the **plain-vs-theory *term*** from a pair and nothing else. **How *much* the chat
says** stays a per-ticket concern already fixed elsewhere (D's card is terse by design; F's restart
line is first-run-only). Register is one clean, composable axis.

**Derived:** since everyone starts `plain` and the first turn carries no signal, **all pre-authored
starter content is written in `plain` register** (B's greeting, I's chips). The `theory-aware` branch
only ever appears *after* a live signal, so it never lives in static content.

---

## 9. Acceptance criteria

An implementation satisfies this spec when:

1. **Vocabulary.** No engine word from §1's forbidden list ever reaches the user, in any state
   (happy path, failure, thinking, keep). Chat copy is sensory-first and mirrors user terms.
2. **Cold start.** The first screen is the show-all `toys.json` gallery (one tap → playing) with a
   persistent "…or describe your own" bar. An instrument pick starts sound **and** fires a proactive
   turn one carrying 2–3 authored chips (or greeting-only if `chips` absent). Chips post verbatim as
   the user's turn.
3. **Spine.** Surface top/center, chat input pinned bottom, transcript an upward collapsible sheet,
   one responsive layout phone→desktop. Controls stay hand-live through the agent's turn; only the
   structural restart interrupts. Surface layout keys on **node identity** (survivors hold position).
4. **Agent-at-work.** Every reshape shows a change-card (plan streams in, resolves in place) and, for
   controls that exist, a co-timed surface highlight. Card rows are sensory-only. No-knob changes get
   a card row and no surface echo. >~4–5 changes collapse to a summary + expandable list. The diff is
   the structural node-identity diff (added/removed/computed-changed); survivor stats are not shown.
5. **Failure.** Ambiguous → act-then-react best-effort + "how I read it" + alternative chips on the
   card. Unsatisfiable → "nearest thing" as a chat turn, engine reason never shown. `{ok:false}` →
   agent self-corrects silently (user never sees a `Diag`); exhausted → collapses to the "can't"
   shape. Empty/off-topic → gentle re-orient with starter chips. **No surface change ⇒ chat turn, no
   card.** Reshape terminal failure keeps the prior sound; first-creation terminal failure returns to
   the gallery.
6. **Sonic honesty.** Parameter-only reshapes route through `send` (live, no gap); structural
   reshapes `swap`/re-strike. The re-strike has a co-timed visible cause, shows replay-from-top, uses
   a consistent gesture with **no spinner**, and **never clicks** (a declicked duck). A one-time
   framing line the first structural restart per session; wordless thereafter.
7. **Keep.** A bottom-chrome "Keep" control with "Not kept yet → Kept ✓". Tap writes the snapshot to
   `location.hash` **and** copies the link. First divergence pulses the chip once; a leave-guard
   fires only on diverged, un-re-findable, unkept work; a later reshape reverts to "Not kept yet".
   Agent points, user's tap performs. **The chat window ships only once Keep is wired in.**
8. **Register.** Binary `plain`/`theory-aware`, starts `plain`, ratchets up on unprompted
   user-originated theory vocabulary (never demotes, never asks), session-scoped, vocabulary-only.
   All static starter content is `plain`.

---

## 10. Out of scope — deferred doors

Named here so they are not accidentally pulled into the v1 build:

- **Agent-host / auth / cost architecture** (client-side key vs backend proxy, billing, rate limits).
  Orthogonal to UX; ADR-0052 named it the web-chat effort's own separate question. This spec assumes
  *some* agent host exists. If its answer later changes latency/reliability characteristics, §3/§4/§6
  may need a revisit (a dependency, not a blocker).
- **Implementation of the spec** — the chat UI's JS, the in-page tool layer, the wasm exports
  ADR-0052 §2 names. The downstream execution epic.
- **Native MCP / Claude Code authoring UX** — the dev-with-a-checkout persona
  ([MCP-server map #270](https://github.com/Impractical-Instruments/reuben/issues/270)).
- **TouchOSC / control-surface redesign** — a generated instrument's surface projection stays what it
  is today (`surfaces/*.json`, the `control-surface` skill).
- **Domain-wide retirement of "Toy"** — §1 retired it from the *chat lexicon* only; removing it from
  reuben's language entirely (ADR-0022, `web/toys.json`, code refs, README, `CONTEXT.md`) is a
  separate code/data/docs effort.
- **Deferred v1 sub-doors** already named inline: auto-derived gallery membership + featured-cap
  (§2.2); generated cold-start chips (§2.4); live re-encoding of the keep snapshot as-you-play (§7.6);
  quantize-to-downbeat re-strike timing (§6.3); user-sample upload + Keep's sample refusal wording
  (§7.9).
