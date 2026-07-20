# Why: The core speaks only OSC-shaped Messages; every other protocol is converted to and from OSC by isolated, removable boundary adapters.

[Rule](../../signal-time-dsp.md#osc-only-core)

reuben must interoperate with MIDI, Ableton Link, and future control protocols, and OSC is already
the lingua franca for internal Messages. Letting more than one protocol into the core would multiply
representations and force every operator to branch on protocol-specific cases — the opposite of a
graph where any output patches into any input. So the core carries **one** shape (an OSC-shaped
Message: `address + timestamp + one typed Arg`) and every foreign protocol is converted to and from
it at the I/O boundary. A MIDI note-on becomes a Message on the way in and is reconstructed as MIDI on
the way out; Ableton Link becomes Clock sync at the edge.

Two properties fall out and are why the rule holds. **Operators never protocol-branch** — they handle
one Message shape, full stop. And **each protocol is an isolated, removable adapter**, which is what
lets the same core embed behind a native shell, a web worklet, or a game host with the whole native
I/O layer detached — the removable-native-layer goal. The cost is honest and bounded: some
protocol-specific nuance (MIDI CC → an address + value) must be mapped into an OSC convention, and that
mapping lives in the adapter, never in the core. In the engine this seam is `boundary.rs`: the native
layer decodes a datagram into an address plus a flat list of primitive `Arg`s, and the boundary's
typed half turns that flat list into the single `Arg` the destination **port's declared type** wants
(a primitive wraps directly, a vocab enum resolves via its metadata, a struct type unpacks via the
converter it registered) — dest-port-type-driven, so the port is the authority and no protocol
knowledge leaks inward.

Distilled from: ADR-0007
