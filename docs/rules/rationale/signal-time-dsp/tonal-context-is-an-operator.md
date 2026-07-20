# Why: Tonal context (tuning, root, scale, chord) is an Operator like the Clock — a default instance grooves a Rig into one key and multiple context nodes give polytonality — never a global.

[Rule](../../signal-time-dsp.md#tonal-context-is-an-operator)

The two-layer pitch model said the active tuning/key/scale/chord "ride the tonal-context bus," queried
continuously, but left open *what* the bus is. The Clock faced the identical fork and settled it: a
global ambient transport was rejected because it would kill polytempo, so the Clock became an
**Operator** with wired outputs ([clock-is-an-operator](clock-is-an-operator.md)). Tonal context is
the exact analog — polytonality is the polytempo analog — and resolves the same way. The context **is
an Operator** (a node), not a global and not loose edge magic: it owns the latched struct and the
resolver, publishers wire in upstream, followers read its output. A single default context node in the
Rig is the same on-ramp as the default Clock — "C major, 12-TET" with zero wiring — so everything
agrees out of the box **without baking *global* into the core**.

**Multiple contexts are just multiple nodes**: a D-dorian lead over a C-major pad is two context nodes,
which a single global could not express. A global was rejected on exactly this (one context kills
polytonality and breaks the Clock precedent); making followers compute degree→Hz themselves was
rejected too (every author re-implements the chain → drift, bugs, lost AI-authorability). In the engine
this is the `harmony` operator: it owns the latched `Harmony` and publishes it on a `harmony` output
port that followers (the Voicer's degree resolution, a snap op) read as "what's the key/chord right
now." The transport is an ordinary wired Message edge and the read is the engine's per-port latch —
both owned by the [execution-runtime](../../execution-runtime.md) topic (its latch-service rule); this
rule only fixes that context is a *node*.

Distilled from: ADR-0013, ADR-0008
