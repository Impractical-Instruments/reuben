# Why: Snap quantizes an arbitrary pitch to the nearest in-scale degree — tuning-correct distance, deterministic down tie-break, policy supplied per call — returning a symbolic degree that re-resolves if the tuning swaps.

[Rule](../../signal-time-dsp.md#pitch-snap)

**Snap** is the quantizer that sits *upstream* of resolution: an arbitrary pitch → the nearest
in-scale `Pitch{degree}`. It is the "always in key" good-button, and it is a note transformer on the
message graph (`Note` in, `Note` out), so it composes between a note source (external play, a
sequencer) and a Voicer. Four choices make it correct and reusable:

- **Target** `Scale | Chord | ChordThenScale` (default `Scale`). `Chord` is strict — only chord tones
  survive; `ChordThenScale` is permissive — any scale tone survives but chord tones win ties, so it
  never forces a note off-scale.
- **Distance** in **tuning-correct cents / log-frequency**, not degree-index, so unequal-step
  microtonal scales snap correctly — the context has the tuning, so this is free. (Today's 12-TET-only
  slice measures in semitone space, which *is* cents in 12-TET; the cents-correct path rides the same
  seam when non-12-TET tunings land.)
- **Direction** `Nearest` (default) with a **deterministic down tie-break** — the determinism
  invariant forbids a coin-flip — plus `Up`/`Down` for forced resolutions.
- **Returns a symbolic `Pitch{degree}`** (not Hz), so it re-resolves for free if the tuning swaps.

Crucially, **policy is a caller argument**, not baked into the context: auto-tune wants
`Scale/Nearest`, an arp wants `Chord`, a melody wants `ChordThenScale`. Baking a policy into the
context would force one snap behavior on every follower of that context; supplying it per call lets one
shared context serve all three. Keeping the resolver (`hz`) and the quantizer (`snap`) as distinct
operations — snap upstream, resolve at the Voicer — is what makes the symbolic-return trick pay off.
Snap *strength/gravity* (partial pull) and *hysteresis* (sticky degree under a slow drag) are
follower/UX concerns, explicitly deferred.

Distilled from: ADR-0013
