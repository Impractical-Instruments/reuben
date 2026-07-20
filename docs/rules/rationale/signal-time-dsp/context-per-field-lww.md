# Why: Tonal context is one bundled struct of optional fields written per-field last-write-wins, and genuinely different scopes or lifetimes split into separate context nodes.

[Rule](../../signal-time-dsp.md#context-per-field-lww)

A common Rig has different publishers owning different parts of the harmony: a scale-broadcast op sets
root/scale, a chord-progression op sets the chord, tuning is a static default. If context were one
indivisible struct with a single writer, you could not express even that without funnelling everything
through one node — and you could not express a **rig-global tuning under two instruments in different
keys** without duplicating (and risking drift on) the tuning. So within one scope context is **one
bundled struct of optional fields** with **per-field last-write-wins**: static fields (tuning, root,
scale) are the node's config/params (the good-button — dial the key, pick the temperament, zero
wiring), and dynamic fields (chord, automated key changes) are driven by upstream ops writing
per-field. One struct with per-field writers covers the common Rig with no extra wiring.

Genuinely different **scopes or lifetimes** — the rig-global tuning under per-instrument keys — are
**separate context nodes** (separate wires), reusing the multiple-context mechanism
([tonal-context-is-an-operator](tonal-context-is-an-operator.md)) rather than complicating the single
struct. Cross-scope *layering* (a follower combining a global-tuning context with a local-harmony
context) is a known, deferred extension. Authority is last-write-wins: an active publisher overwrites
a manual/external set on its next write; a manual-override/latch mode is a later refinement. In the
engine the `harmony` node holds these as the `set` (held `Harmony`, its chord field adopted LWW) plus
`root`/`degrees`/`s0..s11` Value inputs, and it publishes **only on change** so steady state stays
allocation-free.

Distilled from: ADR-0013
