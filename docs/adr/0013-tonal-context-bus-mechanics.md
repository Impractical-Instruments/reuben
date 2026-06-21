# Tonal-context bus: mechanics

## Context

[ADR-0008](0008-pitch-and-tuning.md) decided the two-layer pitch model (symbolic Pitch → Tuning → Hz) and said the active Tuning, key, scale, and chord "ride the tonal-context bus," queried continuously. It left the *mechanics* open: how context is transported, how followers read it, how it's represented, where symbolic pitch becomes Hz, and how it stays sample-accurate alongside sequenced notes. This ADR settles those — the design backlog's "Tonal-context / harmony engine details" thread.

The through-line: the [Clock](0006-clock-and-musical-time.md) deliberately rejected a global ambient transport (which would have killed polytempo) and became an **Operator** with wired Signal outputs. Tonal context faces the identical fork — polytonality is the polytempo analog — and resolves the same way.

## Decision

### Transport — reuse Messages; the new thing is *latched read*, not a new wire

Context travels as ordinary **Messages** (OSC-shaped, ADR-0001) over the existing wildcard dispatch (ADR-0005). No new edge type, no parallel buffer pool. Raw event-Messages are push / this-block-only, but a follower needs the *current* value ("what's the chord right now," possibly set many blocks ago). That **latch** is the only genuinely new semantic — and it already exists for params: the control-Message **block-slicing** path (ADR-0011), where an operator reads "my current value" and the engine slices the block at change boundaries. Tonal context is that exact pattern with a **structured value** instead of an f32.

Rejected: a stateless re-emit-every-block scheme (redundant work at audio rate for a musical-rate value, and a freshly-stolen Voice has no context until the next emit). The latch is cheaper (emit-on-change) and Voice-safe because the context publisher sits **upstream of per-Voice fan-out** — the latched value is shared, so a stolen Voice sees current context instantly. Latching per-follower would reset on `spawn()`; that is the trap that makes a naive message-broadcast wrong.

### The context is an Operator (a node), not a global and not loose edge magic

The tonal-context **is an Operator**, paralleling the Clock. It owns the latched struct and the resolver. Publishers wire in upstream; followers read its output. **Multiple contexts = multiple nodes** (polytonality: a D-dorian lead over a C-major pad). A single default context in the Rig — the same on-ramp as the default Clock — makes everything agree out of the box without baking *global* into the core.

Per-field **last-write-wins** lives inside the node:

- **Static fields** — tuning, root, scale — are the node's config/params (the good-button: dial the key, pick the temperament). "C major, 12-TET" needs zero upstream wiring.
- **Dynamic fields** — chord, automated key changes — are driven by **upstream ops** (a chord-progression sequencer) sending per-field write Messages.

Because scale/tuning are not scalars and params are f32-only today, they enter as **symbolic args** — `Arg::Sym("dorian")`, `Arg::Sym("rast")` — resolved through a scale/tuning **registry**; custom step-lists arrive via Scala import. Messages are already polymorphic (`Sym`/`Float`/`Int`/`Bool`), so no new arg type — but tonal context is the first non-f32 "param" and is the forcing case for any later typed-param work.

### Fields are one bundled value with per-field writers; split into separate nodes only across scopes

Within one scope, context is **one struct** (tuning + root + scale + chord) with **optional** fields, and different publishers write different fields (LWW per field): the scale-broadcast op writes root/scale, the chord-progression op writes chord, tuning is the field default. This covers the common Rig with no extra wiring.

Genuinely different **scopes/lifetimes** — e.g. a rig-global tuning under two instruments in different keys — are **separate context nodes** (separate wires), reusing the multiple-context mechanism. Cross-scope *layering* (a follower combining a global-tuning context with a local-harmony context) is a known, deferred extension.

### Representation — Scale stays symbolic→symbolic; Tuning does symbolic→Hz

```
degree --[Scale: degree → step]--> step index --[Tuning: step → Hz]--> Hz
```

- **Scale** = ordered **step-offsets within the tuning's period** (12-EDO major = `[0,2,4,5,7,9,11]`), plus a root. `degree d → root_step + scale[d mod len] + octave*period`. Swapping the Scala tuning changes Hz while the degree structure is untouched — preserving the ADR-0008 orthogonality. (A "major scale" is therefore not universal across EDOs; that is inherent to microtonality, not a flaw.) Cents/ratio scales were rejected because cents is frequency-space and would bypass Tuning.
- **Chord** = a tagged union: **scale-relative** (a set of scale degrees, e.g. `{0,2,4}`, which *re-spells diatonically* as the scale changes — the feature) **| absolute** (raw step-offsets from root, *frozen* against scale changes). The tag makes "follows key" vs "frozen" an explicit choice at the call site, defusing the silent-re-spell footgun. **One root authority** — the context root; there is no separate "chord root" coordinate.

### Resolution and snap are owned by the context (a deep module), not the follower

The context value exposes the resolver — `hz(pitch)`, `snap(pitch, policy)`, `chord_tone(n)` — so the Scale∘Tuning composition lives in one correct place and followers stay dumb (`io.context().hz(p)`, like reading a param). This keeps single-Lane authoring simple (ADR-0010) and "always in key" a single shared implementation.

**Snap** (the quantizer, *upstream* of resolve: arbitrary pitch → nearest in-scale `Pitch{degree}`):

- **Target** `Scale | Chord | ChordThenScale`, default `Scale`. `Chord` is strict (only chord tones survive); `ChordThenScale` keeps any scale tone but *prefers* chord tones when breaking ties — it does not force a valid scale tone off-scale.
- **Distance** measured in **tuning-correct cents / log-frequency**, not degree-index — so microtonal scales (unequal steps) snap correctly. The context has the tuning, so this is free.
- **Direction** `Nearest` (default) with a **deterministic down tie-break** (the determinism invariant, ADR-0001, forbids a coin-flip); `Up`/`Down` available for forced resolutions.
- **Returns** a symbolic `Pitch{degree}` (not Hz) so it re-resolves for free if the tuning swaps.
- **Policy is a caller argument**, not baked into the context — auto-tune wants `Scale/Nearest`, an arp wants `Chord`, a melody wants `ChordThenScale`.
- **Deferred:** snap *strength/gravity* (partial pull) and *hysteresis* (sticky degree under a slow drag) are follower/UX concerns, layered later.

### Timing — internal is sample-accurate and uniform; only *external* is block-quantized

Internal tonal-context changes ride the **same control-Message slicing path** (ADR-0011) as everything else, so they are **sample-accurate**: a chord change at frame 40 creates a slice boundary at 40, and a note-on event reads the context of *its* sub-block. Notes and chords therefore share one sample-accurate timeline — correct note-vs-chord ordering falls out, no manual offset juggling, no "chord race." This is the payoff of the latch-via-slicing choice over raw events or a quantized channel.

Partial sample-accuracy is worse than none (sample-accurate notes + block-quantized internal chords = guaranteed races), so internally **nothing musical is quantized** — notes, chord, key, and tuning changes are all sample-accurate.

**Only external OSC is block-quantized** — it applies at the next block boundary, because UDP arrival jitter dwarfs sample resolution (reconstructing a sub-block frame from a datagram is fake precision; see ADR-0008's note and `crates/reuben-native/src/osc.rs`). External writes are otherwise just more per-field-LWW writers on the context node's addresses, via the existing OSC-in adapter (ADR-0007) and ADR-0005 addressing; the curated exposed surface gives refactor-stable names (`/key`, `/scale`).

**Same-frame tie rule:** a context write and a note read at the *same* sample F — write-at-F is visible to read-at-F, so a downbeat chord change is heard by the downbeat note (the musically expected result). Deterministic via topological order (the context node runs upstream of followers, ADR-0001) plus "writes land before reads at equal frame."

## Considered and rejected

- **Global ambient context** (read like `sample_rate()`): simplest, but one global context kills polytonality and breaks the Clock-as-operator precedent.
- **Stateless re-emit-every-block** over pure Messages: no new machinery, but redundant audio-rate work for a musical-rate value, and a freshly-stolen Voice has no context until the next emit.
- **Followers compute degree→Hz themselves**: every author re-implements the chain → drift, bugs, and lost AI-authorability.
- **Scale defined in cents/ratios**: tuning-independent but bypasses the Tuning layer, breaking the ADR-0008 orthogonality.
- **One indivisible context struct with a single writer**: cannot express a rig-global tuning under per-instrument chords without duplicating (and risking drift on) the tuning.
- **Block-quantizing internal context** to match external: scrambles ordering against sample-accurate sequenced notes (chord races).

## Consequences

- A **context Operator** type, holding a latched struct + the resolver (`hz`/`snap`/`chord_tone`), with per-field LWW. A default instance lives in the Rig, like the default Clock.
- The first **non-f32 "param"** (scale/tuning as symbolic, registry-resolved) — nudges, but does not yet force, a typed-param model in the descriptor.
- A **scale/tuning registry** (named scales/tunings → step-offsets) plus the Scala import path for custom entries.
- **Snap policy** is a caller-supplied value type (target / direction / tie-break); strength and hysteresis are explicitly out of v1.
- The **chord-progression op is sequenced** → depends on the Clock beat grid and the sequencer operator (both V1.1); **scale-broadcast** (static key/scale) can land first.
- **Authority is last-write-wins:** an active publisher overwrites a manual/external set on its next write. A manual-override/latch mode is a later refinement.
- **Context read-back** (OSC-out so a UI shows current key/chord) is deferred to External OSC I/O (V1.5) and the introspection thread.
- Worked examples that exercise these semantics live in [tonal-context-examples.md](../tonal-context-examples.md).
