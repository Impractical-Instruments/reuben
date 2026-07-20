# Why: The envelope is a pure generator emitting a linear CV contour in [0, 1]; downstream ops interpret it, and the VCA is an explicit mul rather than a baked-in behavior.

[Rule](../../signal-time-dsp.md#envelope-emits-cv)

The `envelope` used to do two jobs at once — generate a linear ADSR contour **and** apply it as a VCA
(`out = audio_in * level`) — and that coupling caused two problems. The gain was **linear amplitude**,
not perceptual: the ear hears loudness roughly logarithmically, so a linear decay spends almost no time
in the quiet region (0.5 = −6 dB, 0.01 = −40 dB) and reads as an abrupt cutoff rather than a natural
tail — and every instrument inherited that one baked-in curve. And because the envelope was an
`audio in → audio out` node, its contour could not drive anything *other* than amplitude (a pitch
sweep, filter motion) without abusing the audio path.

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
