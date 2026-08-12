# Intent vocabulary — word → move

<!-- GENERATED from docs/agents/vocabulary.json by `cargo run -p reuben-core --example gen_vocabulary`; guarded by `committed_rendered_view_is_in_sync`, which re-renders from that source. Do not hand-edit. -->

Ambiguous ask → act on the most likely reading, then name the alternative you passed over ("took *darker* as tone color; say the word and I'll take it minor instead").

Unsatisfiable ask → apply the nearest achievable move and state the gap plainly; never fake the effect, and don't stall for clarification when a conservative move exists (act, then react).

## Timbral

- **warmer** — filter.cutoff down (slightly); saturator.warmth up; reverb.damp up (slightly). ↔ brighter
- **brighter** — filter.cutoff up; resonator.brightness up [a resonator voices the tone]; reverb.damp down (slightly). ↔ warmer, darker
- **darker** — filter.cutoff down; saturator.warmth up (slightly). ↔ brighter
- **dirtier** — saturator.drive up; filter.resonance up (slightly). ↔ cleaner
- **cleaner** — saturator.drive down; reverb.mix down (slightly); delay.feedback down (slightly). ↔ dirtier
- **harsher** — saturator.drive up; filter.cutoff up; filter.resonance up. ↔ softer
- **softer** — envelope.attack up; saturator.drive down; filter.cutoff down (slightly). ↔ harsher, punchier
- **punchier** — envelope.attack down; envelope.sustain down (slightly); saturator.drive up (slightly) [synth-land punch — the registry has no compressor]. ↔ softer
- **airier** — reverb.mix up (slightly); reverb.damp down; filter.cutoff up [opens the top only where the filter is lowpass].
- **wetter** — reverb.mix up; delay.mix up [a delay is the space in the voice]. ↔ drier
- **drier** — reverb.mix down; delay.mix down. ↔ wetter
- **bigger** — reverb.room up; delay.time up (slightly); saturator.warmth up (slightly).

## Rhythmic

- **busier** — euclid.pulses up [a euclid drives the pattern]; clock.division up [a clock sets the rate]. ↔ sparser
- **sparser** — euclid.pulses down [a euclid drives the pattern]; clock.division down [a clock sets the rate]. ↔ busier
- **faster** — clock.tempo up. ↔ slower
- **slower** — clock.tempo down. ↔ faster
- **longer** — envelope.release up; envelope.decay up. ↔ shorter
- **shorter** — envelope.release down; envelope.decay down. ↔ longer
- **tighter** — envelope.attack down; envelope.release down; reverb.mix down (slightly) [only if smear is the complaint]. ↔ looser
- **looser** — envelope.attack up (slightly); m2s.time up [an m2s glides the pitch — the registry has no timing-jitter seat, so this is the laid-back feel, not literal swing]. ↔ tighter
- **more syncopated** — euclid.rotation up (by 1) [a euclid drives the pattern]. ↔ straighter
- **straighter** — euclid.rotation set 0 [back onto the grid a euclid drives]. ↔ more syncopated

## Tonal

- **sadder** — harmony.s2 set 3 [a minor 3rd]; harmony.s5 set 8 [a minor 6th]; harmony.s6 set 10 [a minor 7th]. ↔ happier
- **happier** — harmony.s2 set 4 [a major 3rd]; harmony.s5 set 9 [a major 6th]; harmony.s6 set 11 [a major 7th]. ↔ sadder
- **richer** — chord.size up [3 → 4, adds the 7th].
- **more dissonant** — snap.target set Scale [off Chord — chord-snap is the most consonant policy]; harmony.s1 set 1 [a flat 2nd, when more spice is wanted]. ↔ more consonant
- **more consonant** — snap.target set Chord [snap to chord tones]. ↔ more dissonant
- **darker** — harmony.s2 set 3 [a minor 3rd]; harmony.s5 set 8 [a minor 6th]. ↔ brighter

## Fallback — direction-only

- Tie-breaker: when the word is in the table above, the table wins. Otherwise pick ONE conservative move from the families below, apply it, and let the user react.
- High-frequency-energy words (crisp, sparkly, glassy, sizzly, thin, tinny) → brightness family: filter cutoff up, reverb damp down.
- Weight / size-down words (deep, fat, thick, huge, heavy) → filter cutoff down slightly, saturator warmth up, reverb room up.
- Aggression words (aggressive, crunchy, fuzzy, gnarly) → saturator drive up.
- Gentleness words (mellow, smooth, dreamy, lush) → envelope attack up, saturator drive down, reverb mix up slightly.
- Distance words (distant, cavernous ↔ close, intimate) → reverb mix and room up ↔ down.
- Motion words (wobbly, shimmery, vibrato, pulsing) → lfo rate and depth.
- Register words (higher, lower, an octave up / down) → transpose amount ±12, or harmony root.
- Energy words (energetic, frantic ↔ chill, laid-back) → clock tempo, then the busier / sparser family.
- Width words (wider, stereo ↔ narrower, mono) → pan spread apart ↔ together, granulator spray up ↔ down. No single width seat exists — move per voice and say so.
- Fatness words (fatter, thicker) → saturator warmth up and drive up slightly; there is no detune / sub-oscillator seat, so state that the body is coming from saturation.
- Mood words beyond the table (moody, ominous, mysterious) → the minor-mode family (see sadder), plus filter cutoff down slightly.
- Material words (clickier, woodier, metallic) → resonator structure, brightness, and damping, when a resonator is in the voice.
- Swing words (swung, shuffled, groovy) → no direct seat today; the nearest is stepping euclid rotation off the grid — apply it and state the gap.
- Nothing fits → name what the document lacks and offer the nearest table move rather than refusing.
