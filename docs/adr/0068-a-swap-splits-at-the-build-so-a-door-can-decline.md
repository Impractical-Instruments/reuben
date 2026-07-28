# ADR-0068 — a Swap splits at the build, so a door can decline what it cannot carry

## Context

`Coordinator::swap_document` validates, builds the whole new Engine, **and** fills the install
mailbox, all before it returns. The first moment a caller can read the new Engine's geometry is
therefore a moment *after* the render side has already been handed it.

That is fine for a door that will carry whatever builds — the native shell republishes its device
output map at whatever logical width the swap produced. It is not fine for a door whose render
buffers are a fixed size. The browser door carries fixed planar quanta: its initial construct and
its render-side adopt both refuse a wider engine, because both hold it before anything is
committed. Its swap cannot refuse, because by then the engine is in the mailbox. Its only remaining
move was to report the swap unplayable and make its render path total, so an engine it cannot carry
produces silence rather than a trap — and the gate that actually protects it ended up being a
construct on a separate discovery context, one language and one context away from the thing it
guards.

The engine had no verb for "build it, and let me look before you hand it over". A door that needed
one had to either write a private path around the Coordinator, or accept the fait accompli.

## Decision

**A Swap splits at the build.** `Coordinator::prepare_document` does everything `swap_document`
does except fill the mailbox, and returns a `PreparedSwap` — a built Engine the caller can measure
(`channels`, `input_channels`, `block_size`, `sample_rate`, `content_hash`, `warnings`).
`Coordinator::commit_swap` posts it. `swap_document` is exactly those two back to back, unchanged
for every caller that already had one.

**Declining is dropping.** A `PreparedSwap` the caller does not commit is freed where every other
Coordinator-side drop happens — off the audio thread. Nothing was published, so the install slot
never closed and there is no retiree to reclaim. There is no `cancel` verb because there is nothing
to cancel.

**The migration table is the commit's work, not the build's.** It pairs Plan indices against the
Engine this swap will displace, which is only known when the swap commits. Computing it at prepare
would make a prepared swap that is held across another install transplant against the wrong Engine
— out-of-range indices in a loop that runs on the render thread. So `PreparedSwap` carries no
table, and the commit diffs against whatever is installed at that moment. This is what lets a
prepared swap be held, queued, or committed after another one without a generation stamp policing
it.

**The commit refuses a swap prepared against a different `AudioConfig`.** The one-call form could
not get this wrong — it always built with its own config. Splitting the verb makes "prepare on one
Coordinator, commit on another" expressible, and the door this ADR exists for already runs two
Coordinators, so it is the first thing a caller will try. An Engine instantiated at one rate and
committed into a Coordinator running another renders at the wrong speed under a declick ramp sized
for a rate it does not run at, and nothing in the document can detect it after the fact. So the
commit compares the prepared Engine's rate and block size against its own and rejects with a real
diagnostic. Two Coordinators that *share* a config remain free to hand prepared swaps to each
other: the Engine is self-consistent by then, and the table is diffed by the committing side.

**The one-call form still reclaims before it builds.** `swap_document` is prepare + commit, and the
commit reclaims — but reclaiming only there would hold the retiree alive across the build, raising
the call's peak from live + new to live + retiree + new. A whole extra Engine is a real cost to a
wasm door under a memory cap, which is the same door this ADR is for. So `swap_document` keeps its
own reclaim as its first statement and the commit's becomes a no-op load on an empty slot.

**The structure channel keeps calling the single-call form.** A host serving that channel has
already accepted whatever width arrives — the device-map republish is part of the seam it fills —
so splitting the verb there would be a decision no one at that seam is positioned to make.

The alternative shape considered was a validation hook the build consults. It was rejected for
being narrower: a callback can only answer with what the door knows synchronously at build time,
while a prepared swap lets the door do arbitrary work — resize its buffers, ask its own host, defer
to a user — and then commit or drop. It also keeps the refusal in the door's own control flow
rather than inverting it.

## Consequences

- The property holds at the seam that needs it: no door is handed an Engine it has not had the
  chance to refuse.
- Additive at the type level and at the call: `swap_document` keeps its signature, its ordering
  (reclaim, build, install) and its peak memory, so no consumer has to move, and a door that adopts
  the split does so when its own workaround becomes worth retiring. The equivalence is not free — it
  is held by `swap_document` reclaiming before it delegates, which is a line that exists only to
  preserve it and would be easy to delete as redundant.
- The rejection report is the `Err` half of `prepare_document`, so a declining door still has the
  same report vocabulary to answer its client in — including the hash of what keeps playing.
- A commit refused — for a swap in flight, or for a foreign audio config — consumes the prepared
  swap, exactly as the single-call form drops what it built. A door for which the build is expensive
  reclaims first; with the slot open the in-flight refusal is unreachable.
- Two verbs now do what one did, and the two-call form has a state the one-call form did not: a
  built Engine alive on the caller's side. It is inert — no thread has seen it — but it is real
  memory, and a door that prepares without ever committing holds a whole Plan. In the two-call form
  that Engine is also alive across the caller's own decision, so a door that prepares while a
  retiree is still out does briefly hold three.
- A new pairing constraint exists that did not before: a `PreparedSwap` is only valid at its own
  audio config. The commit enforces it rather than the type system — encoding the config in the type
  would put a parameter on `Coordinator` that every consumer would carry for one refusal.

## Open

- Whether the structure-channel `swap` verb should eventually route through the split, with the
  geometry veto expressed on the host seam. Nothing needs it today; the door that found this
  drives the Coordinator directly.
