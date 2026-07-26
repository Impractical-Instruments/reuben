# Why: The envelope is a pure generator emitting a linear CV contour in [0, 1]; downstream ops interpret it, and the VCA is an explicit mul rather than a baked-in behavior.

[Rule](../../signal-time-dsp.md#envelope-emits-cv)

Coupling contour generation to VCA application costs two things at once. The gain is **linear
amplitude**, not perceptual, so a linear decay reads as an abrupt cutoff and every instrument
inherits that one baked-in curve; and an `audio in → audio out` node's contour cannot drive anything
*other* than amplitude without abusing the audio path.

The fix mirrors how modular synths work: **an EG emits CV, and downstream ops decide how to interpret
it.** The envelope becomes a pure generator emitting its ADSR level as **linear CV in `[0, 1]`** on a
`cv` output; keeping it linear makes it the flexible primitive, because linear-or-any-curve is then a
choice of a downstream op, not a property soldered into the EG. The VCA becomes an explicit `mul`:
linear amplitude is `env.cv → mul`, a natural volume envelope is `env.cv → power → mul` with the audio
on the other `mul` input — reusing the existing Signal `mul` instead of a dedicated VCA operator. The
cost is honest: the amplitude chain is now three nodes where it was one. That buys linear-or-any-curve
by composition and frees the contour for non-amplitude targets — in the groovebox the kick's *pitch*
drop is just `envelope → mul` (linear, no `power`), the same contour with a different downstream
interpretation. (The engine's `envelope` also carries a separate `active` held output — the canonical
voice-liveness source the Voicer reads to know a voice's release tail is truly finished — which is why
the EG stays a first-class node rather than collapsing into `mul`.)

Distilled from: ADR-0027
