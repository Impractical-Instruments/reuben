# Ritual: the web-chat demo bar — "make it darker and add a shimmer", kept and restored

The web-chat epic's **terminal** acceptance ritual ([issue #362](https://github.com/Impractical-Instruments/reuben/issues/362),
spec §9, ADR-0052 §3) — the same top-level bar the native epic's `docs/rituals/m2-demo-bar.md`
holds for the engine, raised here to the in-page conversational player. A human opens the web player
cold, picks a fixed instrument, hands the in-page agent one fixed prompt, and judges **by ear and by
eye** whether the loop landed: the edit is audible, the re-strike is honest (a declicked duck, the
playhead visibly from the top), the transcript reads warm and jargon-free, and **Keep** truly
persists — reload the page and the instrument comes back.

**This is the highest bar in the epic.** Every automated per-ticket gate — the gallery / spine /
change-card / re-strike / failure / keep specs in `web/tests/`, the consolidated acceptance suite
`web/tests/acceptance.spec.js`, and the host-side round-trip tests `crates/reuben-web/js/*.test.mjs`
— sits **below** this ritual. They prove the loop is behaviorally correct in the DOM; this ritual is
the one thing they cannot judge: whether, through real speakers, the whole conversational loop
*sounds* honest and *reads* human.

## What is automated vs. what stays human

The **automatable halves are automated** in `web/tests/acceptance.spec.js` and run in CI:

- the whole loop end to end (cold-start → play → parameter reshape → structural reshape → keep →
  reload restore), at **phone and desktop** viewports;
- the **consolidated forbidden-word guard** — one DOM/transcript sweep across every state
  (cold-start, happy path, thinking, failure, keep), the spec §1 hard gate;
- the **ship gate** (ADR-0052 §3): Keep present, wired, and performing (a tap writes the durable
  hash);
- the tool-call round-trips and keep-hash restore-on-reload.

What stays **human** are the two perceptual halves no automated gate should stand in for:

- **the re-strike honesty BY EAR** — the declicked duck (the *shape* is machine-checked in
  `crates/reuben-web/js/declick.test.mjs`; the *audibility* is your ear) and the sense that the
  structural change read as an intentional replay-from-top, not a glitch or a gap;
- **the transcript tone read-through** — that the reuben side reads warm, sensory, and jargon-free,
  and that the whole exchange feels like shaping a sound with someone, not operating a synth.

## The fixed fixture (checked in — do not improvise)

- **Starting instrument:** **Groovebox** — the first gallery card (`web/toys.json`, `order: 1`), a
  `self-playing` toy that sounds the instant it loads. Its assets are prefetched at the splash, so
  the pick is instant, and because it is already sounding you have a steady reference to judge the
  edit against **and** a continuous bed whose behavior across the structural re-strike you can hear
  directly. The acceptance suite asserts the first gallery card **is** groovebox, so this fixture
  can't silently re-order out from under the ritual.
- **Prompt (verbatim):**

  > make it darker and add a shimmer

  Two moves in one breath, on purpose: **"darker"** is a **parameter** reshape (routes through the
  live `send` path — no gap, no restart, §6.1), and **"add a shimmer"** is a **structural** reshape
  (a `swap`/re-strike — the declicked duck and replay-from-top, §6.2). One prompt exercises both
  sonic-honesty routes so you can hear the difference between them in a single turn.

## Setup

You need speakers or headphones (this ritual is about sound), the built web player, and — since
the loop is now wired to a real model (issue #397) — **a reachable agent host**. For a local pass,
serve the built `dist/` behind the proxy Pages Function (`web/functions/api/chat.js`) with
`wrangler pages dev`, binding your Anthropic key into the Function's server env. The key lives only
in the wrangler process — never in `dist`, never in the bundle (ADR-0054 §1).

```sh
cd web
npm ci
npm run build
# Serve dist + the /api/chat Function, mapping your dev key into the Function's binding.
# (Use whatever env var holds your key; the Function reads ANTHROPIC_API_KEY.)
npx wrangler pages dev dist --binding ANTHROPIC_API_KEY=$ANTHROPIC_API_KEY_REUBEN_DEV
# → prints a http://localhost:8788 URL
```

Open the printed URL with the chat flag on: **`http://localhost:8788/?chat=1`** (`chat/flag.js`).

> Without a key the Function self-gates to `503` (ADR-0054 §5): the app still runs, but a reshape
> collapses to the §5.3 terminal line ("I lost the thread…") instead of driving a real edit. If you
> see that on every turn, the key binding didn't take. (`vite preview` serves no Function at all, so
> `/api/chat` 404s there — use `wrangler pages dev` for this ritual, not `npm run preview`.)

## Run

### 1. Cold start → pick Groovebox

You land on the splash. Tap **Start** (the one gesture that unlocks audio). You arrive at the
**gallery** — every toy shown, in order, with a persistent "…or describe your own" bar beneath.

Tap the **first card, Groovebox**. It starts playing immediately — a steady groove — and the view
becomes the **spine**: the instrument's controls top/center, the chat input pinned to the bottom, a
proactive turn-one greeting naming what's playing plus two or three tappable quick-change chips.

Listen: a steady, moving groove. Keep it playing — its continuity is half of what you are about to
judge.

### 2. Hand over the one prompt

In the bottom chat input, type the prompt **verbatim**:

> make it darker and add a shimmer

Hand the agent nothing else — no hints about controls, nodes, or tools. Reproducibility lives in the
same fixed start and the same fixed words every run.

### 3. Watch and listen as the agent works

The agent narrates a short **plan** into a change-card that streams in and resolves in place. It will
land the edit as **two moves**:

- **"darker" → a parameter reshape.** The relevant control sweeps live on the surface with a co-timed
  highlight; the groove **does not stop** — the tone just darkens under your ear. No playhead reset,
  no restart line. This is the gapless `send` path (§6.1).
- **"add a shimmer" → a structural reshape.** The agent re-strikes: a change-card row commits, the
  transport playhead **visibly returns to the start**, and — the first structural restart of the
  session only — a one-line honesty framing appears ("Here's the new version, from the top." or
  similar). The sound drops under a soft ~20 ms **duck** and returns carrying the new shimmer layer.

### 4. Judge the re-strike — this is the whole point

- **The darkening was gapless.** The groove darkened without ever stopping — a live sweep, not a
  restart.
- **The shimmer arrived on an honest re-strike.** The structural change dropped the sound under a
  soft duck and replayed **from the top** — a visible, co-timed cause (the card commit + the playhead
  snap), not a silent glitch.
- **No click.** The duck is a smooth dip to silence and back — no click, pop, or zipper noise on
  either edge. Repeat the prompt a couple of times if you need to; no edge should tick.
- **No spinner.** The gap is a *beat*, not a *wait* — no "loading…" chrome ever flashes over it.
- **The transcript reads human.** Scroll the transcript open (the upward sheet). The reuben side is
  warm and sensory throughout — "darker", "shimmer", "from the top" — and never leaks an engine word
  (no "swap", "operator", "port", "patch", "node", …). It reads like shaping a sound with someone.

### 5. Keep it, then reload

Tap **Keep** in the bottom chrome. It flips from "Not kept yet" to "Kept ✓", writes the snapshot to
the page's address (the `#r1.…` fragment), and copies the link. A short non-modal confirm invites you
to bookmark the page.

Now **reload the browser tab.** You land back on the splash; tap **Start / Play** once. The **same
darkened, shimmering Groovebox comes back** — Keep persisted the edited instrument, and the reload
restored it from the address. (This is the ADR-0052 §3 ship gate made tangible: the kept swap paired
with a keep gesture, and the gesture is what survived the reload.)

## Pass criteria (human judgment)

- [ ] **Cold start → playing in one tap.** The Groovebox pick starts audible sound immediately.
- [ ] **Darker, gaplessly.** The tone darkened while the groove kept playing — no stop, no restart.
- [ ] **Shimmer present, on an honest re-strike.** A new shimmer layer is audible, arriving under a
      soft duck with a visible replay-from-top — an intentional re-strike, not a glitch.
- [ ] **Seamless, not a click.** The duck is a smooth dip to silence and back — no click/pop/zipper
      on either edge.
- [ ] **No spinner over the gap.** The re-strike showed no loading chrome — a beat, not a wait.
- [ ] **The transcript reads warm and jargon-free.** Sensory language throughout; no engine word ever
      surfaces.
- [ ] **Keep persists across a reload.** After Keep + reload + Start, the edited instrument restores.

If the agent's edit came back unsatisfiable or it stumbled, that is the conversational loop doing its
job — read what it says and, this being a demo of the loop, let it try again or nudge it. The bar is
whether the loop *recovers gracefully and sounds honest*, not whether the first attempt is perfect.

### 6. Shut down

`Ctrl-C` the `preview` server.

## Why this is scripted, not automated

The setup — the exact starting toy and the exact prompt — is pinned so the scenario is the same every
time, and everything the DOM can observe **is** automated (`web/tests/acceptance.spec.js`). What is
**not** automated is the ear and the read: whether the duck is inaudible, whether the shimmer is
musically present, and whether the transcript *feels* human are perceptual judgments — deliberately
the one human check sitting above every automated per-ticket gate, exactly as the native epic's
`m2-demo-bar.md` keeps the by-ear seam-continuity call human.
