# Why: A live Swap is wrapped in a fixed engine-side master-gain ramp: fade to zero, install, fade back up.

[Rule](../../execution-runtime.md#swap-gain-ramp)

A validated [Swap](engine-swap-unit.md) at a block boundary can still be *sonically* rude — a volume
jump between Plans, a filter opening onto a hot signal, non-survivor voices cut mid-waveform. Since
the conversational edit-while-playing loop is the product, a hard glitch on every iteration would
undermine what the swap exists to enable, so the real swap ships with its rail from day one. The
rail is an **engine-side master-gain ramp**: the callback sees the pending Engine in the install
slot but does not consume it immediately — it ramps a master output scalar to zero, installs at
zero, and ramps back up. This amends "install at the callback top" to "*begin* the ramp at the
callback top; install when it reaches zero" — still bounded, still allocation-free, still one
mailbox and one swap in flight, and install still lands at a device block boundary. The audible
result is a short duck to silence, not a click. The ramp lives with the core RT-side install slot,
so both the native callback and the web worklet inherit it; RT cost is one multiply per output
sample while ramping, nothing at steady state.

An equal-power crossfade holding both Engines was rejected: it renders both Engines every callback
for the fade — a transient 2× cost that can blow the deadline on a heavy instrument, trading a duck
for a possible xrun, and it grows the one-in-flight retire discipline a fade window. The rail is
**fixed and observable, not configurable**: one duration, one shape, hard-coded (raised-cosine,
nominal 10 ms per edge; an implementation may tune within 5–20 ms without a new decision), no
document or profile knob — configurability only if a real need appears, recorded so the temptation
has to argue with a decision. Non-survivors are silenced *under* the ramp (their hard cut is
inaudible at zero gain); survivors ride the box transplant with voice/gate state intact, so a held
note keeps sounding under the up-ramp — exactly what edit-while-playing should feel like. One
accepted edge: a note-off landing in the discard window (~15 ms) is lost and can leave a survivor
voicer's gate high — a genuinely hanging note. It is accepted because it only bites when an off
races the swap and is recoverable in-band (re-send the off, re-trigger, or voice-stealing claims
it); the fixes are worse than the disease — a panic/all-notes-off trait surface across ~40 operators
is exactly the shape [survivor-migration](survivor-migration.md) refused, and the Coordinator cannot
mint corrective offs because gate state lives inside boxes it cannot see. Documented instead as an
authoring rule of thumb.

Distilled from: ADR-0050
