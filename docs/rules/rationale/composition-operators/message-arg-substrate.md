# Why: All data is one `Message` carrying exactly one closed-enum `Arg`, a `Signal` is a Message whose Arg is a `Buffer`, and the address labels the OSC boundary and debug only — internal edges route by wired port, never by address.

[Rule](../../composition-operators.md#message-arg-substrate)

The core speaks only **OSC-shaped Messages**. The insight that gets it there: a held enum, a decoded
`Harmony`, and a control float are not different *kinds of data*, they are different *read styles
over one thing* — "the last `/mode` message's arg", "the last `/harmony` message's args, decoded",
"the zero-order-hold of the last `/cutoff` message". One carrier, read three ways.

So there is one type: `Message = { address, frame, Arg }`. Three divergences from OSC are deliberate
and recorded so a future reader does not "fix" them
([message.rs](../../../../crates/reuben-core/src/message.rs)): an internal `frame` timestamp (a sample
offset; external OSC has none and is stamped "now"); **exactly one `Arg`**, not many — which is *why*
concrete-type Args exist, since two scalars (a note's pitch + velocity) cannot be two args, so they
pack into one `Arg::Note`; and concrete-type Args instead of OSC primitives-or-blob, which keeps
values human-readable and a compile-time data contract. `Arg` is **one closed, central enum**: OSC
primitives (`F32`/`I32`/`Str`), shared *vocab* types (`Note`, `Harmony`, and every **all-unit** enum
type-erased to one `Enum(index)` variant — type identity moves to the port descriptor's `EnumMeta`, so
adding a unit enum touches no central engine file; a payload-carrying enum instead rides its own named
leaf variant, see [payload-enum-arg-leaves](payload-enum-arg-leaves.md)), and the dense `Buffer`. A **`Signal` is just a Message whose Arg
is a `Buffer`** — shorthand, not a second type. `Buffer` has no OSC form — which is how audio is kept
off the wire *by construction* — as do the wire-internal vocab types (`Harmony`, `Pitch`) that
register no converter.

The **address is boundary-only**. It is kept for OSC shape, boundary routing, and debug — **never**
internal dispatch. Address routing as the internal primitive would put a `String` and an O(nodes)
match on the audio hot path; instead every internal edge is an addressless, statically-wired port
connection resolved once at Instantiate, and the `address` field drops out of the internal hot path
entirely ([declared-port-forms](declared-port-forms.md)). External OSC routes *by address* to a
node/port at the boundary, then the value travels internally by connection, never by name. The
`Copy`-normalized last-Arg latch that serves held reads is the engine's latch service — see
[execution-runtime](../../execution-runtime.md). Rejected: `Arg::Blob` for structs (loses
human-readability and the compile-time contract) and per-operator enum types (a code smell — a
`FilterMode` is reused everywhere — and it would force `Arg` open; shared vocab keeps it closed).

Distilled from: ADR-0030
