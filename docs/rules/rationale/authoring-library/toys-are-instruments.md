# Why: The launch Toys are beginner instruments assembled from existing operators plus a generated surface, each an ordinary instrument and never new format machinery.

[Rule](../../authoring-library.md#toys-are-instruments)

The Toys are the payoff of v1 — **instant music for a non-technical person** — and the load-bearing
decision about them is what they are *made of*: nothing new. A Toy is an **Instrument, not new format
machinery**. Each is one self-contained instrument JSON (the unit the surface generator consumes)
plus a generated surface; internally it is a graph of existing operators plus, at most, a few new
ones. This is the reuben thesis applied to itself — everything is a graph — and it keeps the beginner
tier out of the format layer entirely: no Toy earns a new JSON section, only (at most) new operators
that any instrument can use.

The slate is **depth over breadth**: three Toys, chosen to cover the three distinct player gestures
rather than to maximize count — groove box (rhythm/auto), chord player (tap-harmony), strum harp
(continuous drag). Melody-player and meta-effects were deferred because they overlap the chosen
gestures and existing fx instruments; breadth is cheap to add once the toy-construction pattern is
proven. One hard constraint shaped every gesture decision: the surface generator draws only
fader/stepper/button widgets, so any gesture a Toy needs must reduce to those (a strum is a fader
whose position stream an operator turns into notes; a chord is a button sending a degree payload) or
pay for a generator extension the disposable surface does not justify.

Two properties make the "a Toy is just an instrument" stance durable. First, because a Toy is an
ordinary document, the whole authoring surface applies to it unchanged — it migrated cleanly to
interface pipes + surface docs ([surface-docs](surface-docs.md)) with the rest of the corpus, and
its channels are exactly the material the library seeds as reusable recipes
([instrument-is-the-unit-of-reuse](instrument-is-the-unit-of-reuse.md)). Second, it proves the
composition-over-machinery bet end to end: synthesized drums (kick = oscillator + pitch-drop
envelope; snare = noise + tonal component; hat = noise → highpass → envelope) rather than committed
binary one-shots, so even the drum machine is a graph. The specific operators the Toys forced
(`noise`, `chord`, `strum`, the sequencer's `gate_mode`, the clock's `division`) live with the
operator model in [composition-operators](composition-operators.md) — what belongs here is that the
beginner-facing library is composed instruments, never bespoke format.

Distilled from: ADR-0022
