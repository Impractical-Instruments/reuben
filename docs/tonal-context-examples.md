# Tonal-context: worked examples

Concrete scenarios that exercise the tonal-context mechanics ([ADR-0013](adr/0013-tonal-context-bus-mechanics.md)), built to grow intuition. Notation: scale degrees are 0-based (`0` = root). MIDI is float (60.0 = middle C). "step" = a step in the active tuning's period (12 in 12-TET).

---

## 1. Resolution chain (degree → Hz)

Context: root = C (midi 60), scale = major `[0,2,4,5,7,9,11]`, tuning = 12-TET (A4 = 440).

| degree | → step | → midi | → Hz |
|---|---|---|---|
| 0 | 60 + 0 | 60 (C4) | 261.63 |
| 2 | 60 + 4 | 64 (E4) | 329.63 |
| 4 | 60 + 7 | 67 (G4) | 392.00 |
| 7 | 60 + 12 | 72 (C5) | 523.25 |
| -1 | 60 + (−1) | 59 (B3) | 246.94 |

Degree 7 wraps the 7-note scale: `7 mod 7 = 0`, `octave = 1` → `root + scale[0] + 1*period = 60 + 0 + 12`. Degree −1 wraps downward to the prior octave's leading tone.

## 2. Retune without re-spelling (the orthogonality)

Same context as §1, but swap **only** the tuning to quarter-comma meantone. Degree structure is untouched; Hz moves:

| degree | step | 12-TET Hz | meantone Hz |
|---|---|---|---|
| 0 | 60 | 261.63 | 261.63 |
| 2 (third) | 64 | 329.63 | **327.03** |
| 4 (fifth) | 67 | 392.00 | **391.21** |

The follower asked for "degree 2" both times. Only the Tuning layer changed. This is why Scale lives in step-space, not cents (ADR-0013).

## 3. Diatonic chord motion — the feature

Context: C major. Chord is **scale-relative**.

| chord (degrees) | sounds | name |
|---|---|---|
| `{0,2,4}` | C E G | I |
| `{1,3,5}` | D F A | ii |
| `{4,6,8}` | G B D | V |

Shifting the degree set walks the diatonic chords with no recompute — `{4,6,8}` is `{0,2,4}+4`, and degree 8 wraps to D5. This is exactly the relativity that §4 warns about.

## 4. The re-spell footgun — and the fix

Start: C major, chord = scale-relative `{0,2,4}` → **C E G**.

A key-change op rewrites the *scale* field to C **minor** `[0,2,3,5,7,8,10]`:

| chord encoding | before (C major) | after (C minor) | desired? |
|---|---|---|---|
| scale-relative `{0,2,4}` | C E G | **C E♭ G** | yes *if* you wanted the chord to follow the key |
| absolute `[0,4,7]` (steps from root) | C E G | **C E G** | yes *if* you wanted a frozen Cmaj |

Same notes before, different after. The **tag** (`scale-relative \| absolute`) makes "follows key" vs "frozen" an explicit call-site choice — not a default you trip over. One root authority (the context root) means there's no second "chord root" to disagree about.

## 5. Snap-to-scale — direction and ties

Context: C major. Input is an arbitrary float-MIDI gesture.

| input | policy | result | why |
|---|---|---|---|
| 64.3 | Scale / Nearest | **E (64)** | closer to E(64) than F(65) |
| 64.8 | Scale / Nearest | **G (67)?** no → **F (65)** | nearest scale tones are F(65)/E(64); 64.8 closest to F(65) |
| 66.0 (F♯) | Scale / Nearest | **F (65)** | exactly between F(65)/G(67) → tie → **down** |
| 66.0 (F♯) | Scale / **Up** | **G (67)** | forced upward (leading-tone resolution) |
| 66.0 (F♯) | Scale / **Down** | **F (65)** | forced downward |
| 62.0 (D) | Scale | **D (62)** | already in scale → unchanged |

The tie-break is **deterministic down** (ADR-0001 forbids a coin-flip on exact ties).

## 6. Snap target — `Chord` vs `ChordThenScale`

Context: C major, chord = Cmaj `{0,2,4}` → tones C(60) E(64) G(67).

| input | policy | result | why |
|---|---|---|---|
| 62.0 (D) | **Chord** (strict) | **C (60)** | D isn't a chord tone; C,E both dist 2 → tie down → C |
| 62.0 (D) | **ChordThenScale** | **D (62)** | D *is* a scale tone → kept; chord-preference only breaks ties, never forces a valid scale tone off-scale |
| 63.0 (D♯) | **Chord** | **C (60)** | nearest chord tone among C,E (both dist 3) → tie down |
| 63.0 (D♯) | **ChordThenScale** | **E (64)** | nearest *scale* tones are D(62)/E(64), tie → but E is a chord tone → chord breaks the tie up to E |
| 65.0 (F) | **Chord** | **E (64)** | F not a chord tone; nearest chord tone E(64) |
| 65.0 (F) | **ChordThenScale** | **F (65)** | F is in scale → kept |

The distinction to hold onto: **`Chord` is strict** (off-chord tones are pulled to chord tones); **`ChordThenScale` is permissive** (any scale tone survives; chord tones only win *ties*).

## 7. Microtonal snap — why distance is cents, not degree-index

Context: root = C, tuning = a Rast-like set with a neutral 3rd ~350¢ above the root, and a 4th at 500¢. Scale degrees near the input: neutral-3rd (≈350¢) and 4th (500¢).

A gesture lands at **430¢** above the root.

- **By cents (correct):** |430−350| = 80, |430−500| = 70 → snaps **up to the 4th**.
- **By degree-index (wrong):** would treat the two candidates as "1 step apart" and mis-pick using index midpoints, ignoring that the steps are 150¢ apart and unequal.

The context owns the tuning, so the cents path is free. A 12-TET-only snap would be silently wrong here.

## 8. Multi-publisher, one context (per-field LWW)

One default context node in the Rig. Three writers, no extra wiring:

| writer | field written | example value |
|---|---|---|
| scale-broadcast op | root, scale | C, dorian |
| chord-progression op | chord | `{0,2,4}` then `{3,5,0}` … |
| (node config) | tuning | 12-TET (default) |

A melody follower reading this one context gets the merged snapshot `{tuning: 12-TET, root: C, scale: dorian, chord: …}`. The scale op never touches `chord`; the chord op never touches `scale`. Last write per field wins.

## 9. Two scopes — global tuning, local keys (separate nodes)

Rig-global tuning under two instruments in different keys = **two context nodes**:

| node | tuning | root | scale | used by |
|---|---|---|---|---|
| context-A | maqam X | D | dorian | the lead |
| context-B | maqam X | A | mixolydian | the pad |

Both carry the same tuning (set in each node, or — later — via cross-scope layering). The lead and pad snap/resolve against their own node. This is the polytonality analog of polytempo; it reuses the multiple-context mechanism, no special machinery.

## 10. Sample-accurate ordering — no chord race

One Render block, `n` frames. A chord-progression op writes the new chord at frame **40**; a sequencer emits note-ons at frames **37** and **45**.

The engine slices the block at 40 (the context write is on the control-slicing path, ADR-0011):

```
sub-block A = [0, 40)   context = OLD chord   → note-on @37 reads OLD  ✓
sub-block B = [40, n)   context = NEW chord   → note-on @45 reads NEW  ✓
```

Notes and chord share one sample-accurate timeline; ordering falls out. Contrast a (rejected) block-quantized internal chord: the @37 note might read a chord that "should" have changed earlier or later in the block — a race.

## 11. Same-frame tie — downbeat chord change

Chord change and a note-on both resolve to the **same** sample F (the downbeat). Rule: **write-at-F is visible to read-at-F**, so the downbeat note plays the **new** chord — the musically expected result. Deterministic via topological order (context node upstream of the follower) + write-before-read at equal frame.

## 12. External (block-quantized) vs internal (sample-accurate)

A human nudges `/key` over OSC from a controller, datagram arriving mid-block.

- The change applies at the **next block boundary** — block-quantized, because UDP arrival jitter already dwarfs sample resolution (no honest sub-block frame to recover).
- Inaudible: harmony moves at musical rate; ~2.7 ms (128 @ 48 kHz) is nothing.
- Internally the *same* key change driven by a sequenced automation lane would be **sample-accurate** (§10's path). Only the *external* boundary quantizes.

If a chord-progression op is actively driving the chord, a manual OSC chord set is overwritten on the op's **next** write (last-write-wins). Manual-override/latch is a later refinement.
