# Why: A single default Clock grooves every Toy together out of the box, but Clocks are Operators, so polytempo, clock division, and independent timing are patched when wanted.

[Rule](../../signal-time-dsp.md#clock-is-an-operator)

Two failure modes bound the design. A **global-only transport** is simple but rigid — polytempo and
independent grooves become awkward — while **fully decentralized clocks** are flexible but nothing
syncs by default, which is beginner-hostile. The hybrid takes the good half of each: a single default
Clock exists so any two Toys dropped in a Rig groove together out of the box (the on-ramp), *and*
Clocks are ordinary Operators, so polytempo, clock division, and independent timing are patched when
wanted. Default sync, optional divergence.

The Clock provides **base timing only** — tempo, meter, position — and nothing else; that minimalism
is what lets groove and feel be separate, composable operators rather than knobs buried in transport.
Making the Clock an Operator (not an ambient global) is also the precedent the tonal-context node
follows for the identical reason: polytonality is the polytempo analog, and both resolve the same way
([tonal-context-is-an-operator](tonal-context-is-an-operator.md)). In the engine the Clock is where
sample-accuracy actually lives: it free-runs on the deterministic sample timeline, advancing a beat
phase by `tempo / 60 / sample_rate` beats per sample, so beat boundaries land on exact samples
regardless of block size — the precision external OSC arrival times cannot honestly give. The phase is
held in `f64` because `f32` accumulation slips audibly off the sample grid within seconds of a long
session. `division` (gate subdivisions per beat) is a wired, block-sliced Value input — a 16th-note
grid is `division` 4 — the thin slice of the ADR's deferred subdivision that actually shipped.
External tempo sync (Link, MIDI clock, OSC) feeds the Clock, but only through boundary adapters
([osc-only-core](osc-only-core.md)).

Distilled from: ADR-0006
