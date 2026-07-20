# Why: The engine holds a per-port zero-order-hold latch of each input's last Message so a follower reads its current value as a plain constant.

[Rule](../../execution-runtime.md#latch-service)

There is fundamentally **one edge** with two payloads — a continuous audio buffer (a Signal) and a
discrete typed Message — and over the Message wire the engine offers *read services*, not new edge
types. The latch is the central one: an engine-held per-port slot holding the zero-order-hold of
the port's last Message, so a follower reads "the current value" as a plain constant instead of
re-deriving it from a raw event stream. The operator writes through `Io`, the engine owns the
buffer, downstream reads through `Io` — exactly parallel to the Signal arena and the emit pool. A
held enum, a control float, and the tonal-context struct are all the same mechanism: the last
message's arg, held; they differ only in the kind of value, not in being a separate wire type.

The realtime constraint *forces* the value shape. The block-lifetime change list stores
`(frame, snapshot)`; if the latched value held a `Vec`/`Box`, snapshotting would clone and allocate
on the audio thread and break `tests/rt_safe.rs`. So the latched value is **`Copy`** (memcpy
snapshot, no heap), and heavy or variable data — named scales, Scala step→Hz tables — lives in an
**immutable registry** built off-RT at Instantiate and held by the Plan, never mutated during
render; symbolic args resolve to indices at write time, off the per-sample path, so reads stay pure
([render-is-allocation-free](render-is-allocation-free.md)). Changes drive follower slicing the same
way emitted Messages do — the upstream publisher runs first in topo order and fills followers'
routes, contributing a slice boundary, so a chord change at frame 40 is seen by the frame-45 note
and not the frame-37 one ([sample-accurate-timing](sample-accurate-timing.md)).

The latch sits **upstream of voice fan-out**, shared by every downstream lane. Under a fixed voice
pool a per-follower self-latch would also be correct, so the engine latch is justified by authoring
simplicity + sample-accuracy done once — and it additionally future-proofs dynamically-spawned
voices: a lane born mid-stream reads the shared slot and sees current context instantly, where a
per-lane self-latch would be empty. Address/wildcard dispatch as the transport was rejected: every
change would re-enter a String-keyed O(nodes) match; the statically-wired edge is the cheap common
case ([operator-message-emission](operator-message-emission.md)), wildcards layer on later for the
boundary, not under the audio-rate latch.

(Originally the "context read" service with its own arena; that dedicated arena has since folded
into the one per-port latch, and "context" is now the `Harmony` value — the latch + `Copy` resolver
struct are what survive and what this rule fixes.)

Distilled from: ADR-0015
