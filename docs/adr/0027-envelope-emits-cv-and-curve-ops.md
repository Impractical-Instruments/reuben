# Envelope emits linear CV; curve ops shape it; VCA is a `mul`

## Context

The `envelope` operator did two jobs at once: it generated a linear ADSR contour **and**
applied it as a VCA (`out = audio_in * level`). Two problems followed from that coupling:

- The gain was **linear amplitude**, not perceptual. The ear hears loudness roughly
  logarithmically, so a linear amplitude decay/release spends almost no time in the quiet
  region (0.5 = −6 dB, 0.1 = −20 dB, 0.01 = −40 dB) and reads as an abrupt cutoff rather than
  a natural tail. Every instrument inherited that one baked-in curve.
- The envelope was an `audio in → audio out` node, so its contour could not be reused to drive
  anything *other* than amplitude (pitch sweeps, filter motion) without abusing the audio path.

Raised on the `v1.3-strum-harp` branch; tracked as issue #40.

## Decision

Mirror how modular synths work: an **EG emits CV**, and **downstream ops decide how to
interpret it**. Concretely:

1. **`envelope` becomes a pure generator.** It drops its `audio` input and emits the ADSR
   level contour as **linear CV in `[0, 1]`** on a `cv` output. Keeping the EG linear makes it
   the flexible primitive — linear *or* any curve is a choice of downstream op, not a property
   baked into the EG. This is a **breaking change**; all bundled instruments are migrated.

2. **Curve ops are named for their precise math, not a generic "curve" knob.** The first is
   **`power`** — `out = x^exponent` (unipolar; negatives clamp to 0 so a fractional exponent
   can't yield NaN). Default `exponent = 2`. Future shapes (`logarithmic`, …) get their own
   named ops rather than overloading one operator with a mode param.

3. **The VCA is an explicit `mul`.** Linear amplitude is `env.cv → mul`. A natural volume
   envelope is `env.cv → power → mul`, with the audio on the other `mul` input. This reuses the
   existing Signal `mul` (ADR-0017) instead of adding a dedicated VCA operator.

### Why `power` (x^k) and not a true exponential (e^{kx})

Both track perceived loudness far better than linear. `x^k` was chosen because it maps `0 → 0`
and `1 → 1` exactly — a release reaches **true silence** and a peak reaches **unity** with no
floor parameter to fudge — and it is cheaper (one `powf`, no normalization). `x²` is perceptually
close to an exponential decay over the audible range. A true `e^{kx}` curve never reaches 0 and
would need a `−60 dB`-style floor + renormalization to be usable as a release; the power curve
avoids that entirely. The name stays honest: it is a *power* curve, so the op is `power`.

## Consequences

- **Amplitude chain is now three nodes** (`envelope` → `power` → `mul`) where it was one. That
  is the cost of decoupling; it buys linear-or-any-curve by composition and frees the contour to
  drive non-amplitude targets. In the groovebox, the kick's **pitch**-drop envelope is just
  `envelope → mul` (linear, no `power`) — the same contour, a different downstream interpretation.
- The `envelope` descriptor changed (ports `audio,gate → audio` became `gate → cv`), so the
  golden descriptor snapshot and the generated instrument schema were re-blessed.
- Instruments that wired audio *through* the envelope (`default`, `groovebox`, `strum-harp`,
  `echo`, `reverb`, `chord-player`, and the rest) were migrated to the new chain; volume
  envelopes use `power` for an exponential-style curve.
