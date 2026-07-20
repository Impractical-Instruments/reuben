# Why: All data is one `Message` carrying exactly one closed-enum `Arg`, a `Signal` is a Message whose Arg is a `Buffer`, and the address labels the OSC boundary and debug only — internal edges route by wired port, never by address.

[Rule](../../composition-operators.md#message-arg-substrate)

The north star is that the core speaks only **OSC-shaped Messages**. Everything after that drifted:
the engine accreted seven distinct internal carriers — a dense f32 arena, a sparse emit pool, a
`Harmony` struct arena, an enum latch, a param lane, materialized-float buffers, the outbound lane.
The insight that collapses them: most are not different *kinds of data*, they are different *read
styles over one thing*. A held enum is "the last `/mode` message's arg"; Harmony is "the last
`/harmony` message's args, decoded"; a control float is "the zero-order-hold of the last `/cutoff`
message." One carrier — an OSC-shaped Message stream — read three ways.

So there is one type: `Message = { address, frame, Arg }`. Three divergences from OSC are deliberate
and recorded so a future reader does not "fix" them
([message.rs](../../../../crates/reuben-core/src/message.rs)): an internal `frame` timestamp (a sample
offset; external OSC has none and is stamped "now"); **exactly one `Arg`**, not many — which is *why*
concrete-type Args exist, since two scalars (a note's pitch + velocity) cannot be two args, so they
pack into one `Arg::Note`; and concrete-type Args instead of OSC primitives-or-blob, which keeps
values human-readable and a compile-time data contract. `Arg` is **one closed, central enum**: OSC
primitives (`F32`/`I32`/`Str`), shared *vocab* types (`Note`, `Harmony`, and every enum type-erased to
one `Enum(index)` variant — type identity moves to the port descriptor's `EnumMeta`, so adding a vocab
enum touches no central engine file), and the dense `Buffer`. A **`Signal` is just a Message whose Arg
is a `Buffer`** — shorthand, not a second type; `Buffer` is the only Arg with no OSC form, which is
how audio is kept off the wire *by construction*.

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
