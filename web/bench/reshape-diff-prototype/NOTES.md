# Reshape diff-summary + thinking-state treatment — verdict (UX/D, #302)

**Question:** how does the web-chat UI render (a) the agent's "thinking" state and
(b) the reshape diff-summary, on C's spine (#301)?

**Prototype:** [`prototype.html`](./prototype.html) — a THROWAWAY mock (fake timing, no
engine) of three structurally-different treatments (A surface-loud / B change-card /
C spatial-annotation), switchable via `?variant=` or the floating bar. All three are kept
as the comparison record; the winning treatment is a hybrid. Delete once #302 folds into
the H (#306) spec.

## Verdict (decided HITL via `/prototype`)

1. **Diff home = A+B hybrid.** Surface highlights the affected controls (added pulses in,
   changed sweeps + glows, removed animates out — node identity, reverse-Lovable) **and** the
   transcript keeps a persistent, scannable **change-card** (one row per add/change/remove).
   Hover a row → echo-highlights its control. Card persists; surface glow fades.
2. **Thinking state = streaming plan that resolves into the card.** The card appears on Send
   with a natural-language plan, then resolves in place into the final rows once the swap
   lands. Controls stay hand-live throughout (C §4); only the restart-swap interrupts.
3. **No-knob changes = card row, no surface echo.** A sound change with no surface control
   still gets a sensory row; the surface highlight is the bonus that only fires when a control
   exists. The card is the complete record.
4. **Big diffs = enumerate small, collapse past a threshold** (~>4–5 changes → summary
   headline + expandable "show all changes").
5. **Card language = sensory-only** (inherited from A #299 — never node/operator names).
6. **Restart-honesty line = D's slot, F's content** (#304 decides how loud / what it says).

**Translation the card performs:** web `swap` is restart-swap (`survived: 0` always,
ADR-0052 §2), so survivor stats / `state_reset` are degenerate per-turn and NOT shown. The
card is driven by the structural node-identity diff: `added`/`removed` + a computed "changed"
(params differing on nodes in both docs). Grounding: ADR-0048 §4–5, ADR-0052 §2, C (#301),
research memo §2/§4, A (#299).

Full decision record: [#302 resolution comment](https://github.com/Impractical-Instruments/reuben/issues/302).
