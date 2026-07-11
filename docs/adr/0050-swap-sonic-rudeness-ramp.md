# ADR-0050: Swap sonic-rudeness policy: a fixed master-gain ramp around install

## Status

Accepted (2026-07-11). The swap-rudeness decision of the reuben MCP server effort —
wayfinder ticket [MCP/G (#277)](https://github.com/Impractical-Instruments/reuben/issues/277)
on map [#270](https://github.com/Impractical-Instruments/reuben/issues/270), closing
[#220](https://github.com/Impractical-Instruments/reuben/issues/220)'s open question 5.
**Rides on** [ADR-0046](0046-coordinator-swap-engine-unit.md) (the Swap mechanism this
papers over; resolves the policy slot §3 left open) and follows
[ADR-0038](0038-interface-pipes-and-the-device-layer.md) §9's *fixed and observable, not
configurable* philosophy. Feeds the epic's M2 tickets (MCP/J).

## Context

A validated Swap at a block boundary can still be sonically rude: a volume jump between
Plans, a filter opening onto a hot signal, non-survivor voices cut mid-waveform.
ADR-0046 §3 installs the new Engine at the callback top, discards the retiring Engine's
rendered residue and pending Messages, and left audible-rudeness policy to this decision —
noting the shell could briefly hold both Engines if a crossfade were wanted. Facts that
bore on the choice:

- **No master gain exists.** The master is bare per-channel sum buffers
  (`crates/reuben-core/src/render.rs`); there is no gain stage, ramp, or smoothing
  machinery anywhere on the master path. Any rail is new machinery.
- **A server-side rail has no address to target.** `send` control messages address node
  params — there is no global master-volume OSC address — and pending Messages are dropped
  at install (ADR-0045 §5, ADR-0046 §3), so a fade `send` racing the swap is clobbered by
  design.
- **A crossfade renders both Engines** every callback for its duration: transient 2×
  render cost on the audio thread.
- **Voice/gate state lives inside the operator boxes** (`operators/voicer.rs`): a survivor
  voicer carries its ringing notes across the transplant, and no panic/all-notes-off
  surface exists on any operator.
- **M1's restart-swap is inherently rude** (~100ms silence, streams reopen, every node
  cold) and ADR-0046 §10 already documents that as the interim's tolerance, not the
  contract's.

## Decision

### 1. Rails ship with M2's mailbox swap; M1 stays rude

The conversational loop is the product: a hard glitch on every iteration undermines what
M2 exists to build, so the real swap ships with its rail from day one. M1's restart-swap
keeps its documented gap unchanged — no M1 work item.

**Considered and rejected:** *no rails in the epic* (defers the product's core feel past
the milestone built to deliver it, for little savings); *rails in M1 too* (a fade wrapped
around stream teardown is gold-plating on throwaway machinery — it dies with restart-swap
in M2).

### 2. The rail is an engine-side master-gain ramp: fade-down → install → fade-up

The callback sees the pending Engine in the install slot but does not consume it
immediately: it ramps a master output scalar to zero, installs at zero, and ramps back up.
This amends ADR-0046 §3's "install at the callback top" to "**begin the ramp at the
callback top; install when it reaches zero**" — still bounded, still allocation-free, one
mailbox, one swap in flight, and install still lands at a device block boundary. The
audible result is a ~20ms duck to silence, not a click. The ramp lives with the core
RT-side slot (ADR-0046 §7), so both shells — native callback and web worklet (MCP/I) —
inherit it. RT cost: one multiply per output sample while ramping, nothing at steady
state.

**Considered and rejected:** *equal-power crossfade holding both Engines* (sonically
seamless, but both Engines render every callback for the fade — transient 2× cost that can
blow the deadline on a heavy instrument, trading a duck for a possible xrun, and §2's
one-in-flight retire discipline grows a fade window); *server-side gain wraps* (no master
address to target, and racy by design — see Context); *fade-in only* (leaves §3 untouched
but hard-cuts the old Engine mid-waveform — half the click survives).

### 3. Fixed and observable, not configurable

One duration, one shape, hard-coded: **raised-cosine, nominal 10ms per edge** — long
enough to declick any transient, short enough to read as instant. The epic's
implementation ticket may tune within **5–20ms** without a new decision. No document or
profile knob, no opt-out. Same clause as ADR-0038 §9: configurability only if a real need
appears — recorded here so the temptation has to argue with an ADR.

**Considered and rejected:** *a `swap_ramp_ms` knob* (documents behave differently across
engines, a config surface to test, and nobody has asked); *a named config seam without the
knob* (adds nothing — the seam is one constant).

### 4. Non-survivors are silenced under the ramp; survivors keep ringing

A non-survivor's fresh box starts cold and its displaced instance retires off-thread —
mechanically a hard cut, but inaudible: the master is at zero when it happens. Nothing
extra is built. Survivors ride the box transplant with their voice/gate state intact — a
held note keeps sounding under the up-ramp, which is exactly what edit-while-playing
should feel like. There is no ring-out: the retired Plan is never rendered post-install
(the crossfade that would allow it is rejected above).

### 5. The hanging-note window is accepted and documented

A note-off landing in the discard window (pending Messages dropped at install — ≤ one
block plus the down-ramp, ~15ms) is lost, leaving a survivor voicer's gate high: a
genuinely hanging note. Accepted: the window only bites when an off races the swap, and
the failure is recoverable in-band — re-send the off, re-trigger the note, or voice
stealing claims it; the agent driving the loop can follow a swap with a corrective `send`.
The fixes are worse than the disease: a panic/all-notes-off trait surface across ~40
operators is exactly the shape ADR-0046 §4 refused, and the Coordinator cannot mint
corrective offs — gate state lives inside boxes it cannot see. Documented as an authoring
rule of thumb (required `reuben://guide/authoring` content, → MCP/H).

## Consequences

- ADR-0046 §3's open policy slot is filled: an M2 swap is a fixed ~20ms raised-cosine duck
  at a block boundary. §3's option to hold both Engines for a crossfade is consciously
  declined.
- The epic's M2 rail ticket falls out of §2: the master ramp joins the RT-side install
  slot in core, inherited by both shells.
- Authoring guidance gains two lines: *a swap ducks the output for ~20ms* and *a note-off
  racing a swap can hang a note — re-send the off* (→ MCP/H's guide sourcing).
- M1's restart-swap rudeness is unchanged and stays documented as interim tolerance
  (ADR-0046 §10).
