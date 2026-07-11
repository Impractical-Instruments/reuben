# Cold-start / first-run UX — prototype verdict (Web-chat UX/B, #300)

**Throwaway prototype:** [`prototype.html`](./prototype.html) — three cold-start shapes on a
switcher, each with a seeded first-chat-turn state. No engine, no audio; fonts fall back to the
storybook stack's system substitutes. Delete once the web-chat spec (#306) is implemented.

**Map:** [#298](https://github.com/Impractical-Instruments/reuben/issues/298) ·
**Ticket:** [#300](https://github.com/Impractical-Instruments/reuben/issues/300) ·
**Vocabulary locked to:** [Web-chat UX/A #299](https://github.com/Impractical-Instruments/reuben/issues/299)
("instrument", "example instruments", "controls", "update"; never "Toy").

## Question

What does the first-run screen show a non-musician who has never seen reuben — a gallery of
example instruments, a prompt box with example prompts, or both — and how does picking a
starter seed the first chat turn?

## Verdict (settled HITL with @charliehuge)

1. **Cold-start shape = gallery-first (variant A).** Example instruments front and center — one
   tap to *playing sound*. A persistent "…or describe your own" prompt bar sits below the
   gallery (always visible, not buried). Leads with reuben's play-first nature; avoids the bare
   prompt box the research memo (§5b) says no product ships to a first-timer. The bundled
   instruments (`web/toys.json`) are the gallery's raw material.

2. **Instrument-pick seed = proactive greeting + quick-change chips.** Tapping an example
   instrument starts it playing *and* reuben opens chat turn one: a short greeting naming what's
   playing, an invitation to reshape it, and 2–3 tappable concrete changes
   ("make it faster" / "make it darker" / "add a bassline" / "what can I change?"). This defeats
   the *second* blank canvas — a newcomer who's playing but doesn't know they can talk to it —
   and gives a one-tap path to a first successful `update`.

3. **Describe-path seed = echo → build → play → land.** In the "describe your own" path (typing,
   or tapping an example-prompt chip), the user's words appear as their first chat message,
   reuben builds it, it starts playing, and reuben closes the turn by naming what it made and
   what they can change next — symmetric with the instrument-pick landing.

## Explicit hand-offs (not decided here)

- **Where the chat sits relative to the instrument's controls** → the interaction-model keystone
  [Web-chat UX/C #301]. B fixes only *what turn one says*, not the screen's spine. B's constraint
  on C: the cold-start lands with the instrument playing **and** a chat able to post a proactive
  turn one with tappable chips.
- **Empty / unsatisfiable descriptions** (clarify vs. best-effort) → [Web-chat UX/E #303].
- **Gallery membership + where the quick-change chips come from** (bundled set + hand-authored
  chips vs. an auto-derivation pipeline) → graduated to its own sourcing ticket.
