# Composition & operator model

> The one recursive graph — how operators declare and register their contract, how all data flows as one Message/Arg substrate in Value, Event, and Signal forms, and how instruments nest and expose interface pipes.

## Now

reuben is **one recursive graph**. An **Operator** is the smallest node — a unit of DSP behavior; an
**Instrument** is a named subgraph that exposes an **interface** and is reused inside another graph as
if it were an operator; a **Rig** is just the outermost graph played at the top. There is one node
model, one port model, one connection rule, and one file schema from operator to rig — learn it once,
apply it at every scale. An operator is authored the simplest possible way: a single-voice,
single-channel stream, one block at a time — "given one input block and my state, produce one output
block." It never sees the fan-out matrix. Cross-cutting work (voicing, mixing, panning) lives *above*
the operator layer as structural constructs, so an operator stays small and cannot botch the parts
the engine owns.

An operator's contract is **single-sourced and self-registering**. `register_operator!` submits each
built-in at its own definition site — `inventory` gathers them at link time, so there is no central
list to merge-conflict on. `operator_contract!` takes one declaration of the ports, constants, and
metadata and emits both the runtime `Descriptor` and a **typed port handle** per port
(`In<SignalF32>`, `In<Held<f32>>`, `Out<Event<Note>>`, …); `io.read`/`io.write` dispatch on the
handle, whose type fixes the port's form and carries its declared default, so a wrong-form read does
not compile and no default can drift. The stateless-pointwise math family goes one level further —
`number_operator_contract!` generates a whole value/signal operator family from a single scalar
function — and the same census-macro idea gives every **product vocab type** its field-destructure
operator: one `unpack_op!(vocab::Note)` line mints `unpack_note`, which reads a `Note` **event** stream
on `in` and emits each field (`pitch`, `velocity`) as a held Value that defaults to the type's
`Default`, so a mono voice can be wired as a patch instead of hidden inside the Voicer.

All data on the graph is **one substrate**: a `Message = { address, frame, Arg }` carrying exactly
one `Arg` (OSC primitives, shared vocab types like `Note`/`Harmony`, an all-unit enum's erased index,
or the dense `Buffer`). A vocab enum that carries a payload — `Pitch` (`Degree | Absolute`) — is not
erasable without dropping that payload, so it is promoted to its own named `Arg` leaf and rides a wire
on its own; `#[derive(ArgValue)]` routes each enum by whether any variant carries a payload. A `Signal` is just a Message whose Arg is a Buffer; a held control is the
zero-order-hold of a port's last Arg (the engine's latch service, see
[execution-runtime](execution-runtime.md)). The address is a boundary/debug label — internal edges
route by wired port, never by name. Every port declares one of three **forms** by its value type:
**Value** (`f32`/`enum`/`harmony`/`i32` — latched, held, sparse), **Event** (`note` — unlatched,
frame-stamped), or **Signal** (`f32_buffer` — dense per-sample). The planner does no propagation: it
checks each wire locally, materializing Value→Signal and widening `i32`→`f32` as the only implicit
coercions and hard-erroring every other crossing (the sanctioned bridge is an explicit converter
operator). A node's whole surface is inputs, outputs, **constants**, and resources — a Constant is a
plan-time immutable port (changing it rebuilds the graph); the old "param" concept is gone.

Composition nests two ways, split on **cardinality**. A statically-nested instrument (fixed
build-time count) is referenced by a `subpatch` node and **inlined**: at build its nodes splice into
the parent's flat schedule under an address prefix, boundary wires rewire to the inner targets, and
the node dissolves — zero runtime cost, per-reuse identity and state for free. Runtime-varying
cardinality is **hosted**: the **Voicer** builds N standalone voice patches and renders the active
ones per block, the sole runtime host. Either way a graph's edge is crossed by **interface pipes** —
the single boundary mechanism at every level: an input pipe mints an address internal nodes wire
from, an output pipe is fed from an internal port, each pipe declares its own `Arg` type, and
N-channel hardware I/O is N mono pipes bound to logical channels that a separate device profile — not
the patch — maps onto the rig.

## Rules

<a id="recursive-composition"></a>
### Operator, Instrument, and Rig are one recursive concept: an Instrument is a named subgraph that exposes an interface and is reused as if it were an operator, with its own identity and state per use.

[why](rationale/composition-operators/recursive-composition.md)

<a id="single-stream-operators"></a>
### An operator is authored as one single-voice, single-channel stream — one input block plus its state to one output block — and cross-cutting fan-out lives in structural constructs above the operator layer, never inside an operator.

[why](rationale/composition-operators/single-stream-operators.md)

<a id="operator-self-registration"></a>
### Each built-in operator registers itself at its own definition site through `register_operator!`/inventory, gathered into the built-in set at link time, so there is no central operator list to edit.

[why](rationale/composition-operators/operator-self-registration.md)

<a id="single-source-contract"></a>
### An operator declares its ports, constants, and metadata once in `operator_contract!`, which emits both the typed port handles and the runtime `Descriptor` from the same tokens.

[why](rationale/composition-operators/single-source-contract.md)

<a id="pointwise-number-operators"></a>
### A stateless pointwise number operator is declared once as a scalar function in `number_operator_contract!`, which generates its whole value/signal (and future number-type) operator family.

[why](rationale/composition-operators/pointwise-number-operators.md)

<a id="message-arg-substrate"></a>
### All data is one `Message` carrying exactly one closed-enum `Arg`, a `Signal` is a Message whose Arg is a `Buffer`, and the address labels the OSC boundary and debug only — internal edges route by wired port, never by address.

[why](rationale/composition-operators/message-arg-substrate.md)

<a id="declared-port-forms"></a>
### Every port carries one of three forms — Value (latched, held, sparse), Event (unlatched, multi-valued), or Signal (dense per-sample buffer) — fixed at authoring by the port's declared type, not inferred from the graph.

[why](rationale/composition-operators/declared-port-forms.md)

<a id="per-wire-form-check"></a>
### The planner resolves forms with a local per-wire check whose only implicit coercions are Value→Signal materialization and `i32`→`f32` widening; every other crossing, including Signal→Value and `f32`→`i32`, is a hard error requiring an explicit converter operator.

[why](rationale/composition-operators/per-wire-form-check.md)

<a id="constants-are-immutable-ports"></a>
### A node's surface is inputs, outputs, constants, and resources; a Constant is a plan-time immutable port whose change rebuilds the graph, structurally distinct from a runtime Input, with no separate param concept.

[why](rationale/composition-operators/constants-are-immutable-ports.md)

<a id="typed-port-handles"></a>
### An operator reads and writes each port through a typed handle whose type fixes the port's form and carries its declared default, so a wrong-form read cannot compile and every declared Signal input is a dense buffer of exactly `frames` samples.

[why](rationale/composition-operators/typed-port-handles.md)

<a id="nesting-inline-or-host"></a>
### A nested instrument with fixed build-time cardinality is inlined and dissolved into the parent's flat schedule for zero runtime cost, while runtime-varying cardinality is hosted as live sub-plans — the Voicer's polyphony being the sole host.

[why](rationale/composition-operators/nesting-inline-or-host.md)

<a id="interface-pipes"></a>
### A graph's boundary is named interface pipes — an input pipe mints an address internal nodes wire from and an output pipe is fed from an internal port, each pipe declares its own `Arg` type, and N-channel I/O is N mono pipes bound to logical channels that a device profile, not the patch, maps to hardware.

[why](rationale/composition-operators/interface-pipes.md)

<a id="payload-enum-arg-leaves"></a>
### A vocab enum that carries a payload is promoted to its own named `Arg` variant as an opaque `Copy` leaf, while an all-unit enum type-erases to `Arg::Enum(index)` — `#[derive(ArgValue)]` routes each enum by whether any variant carries a payload.

[why](rationale/composition-operators/payload-enum-arg-leaves.md)

<a id="product-type-unpack-operators"></a>
### Each product vocab type gets a generated `unpack_<type>` operator from a one-line `unpack_op!` census entry that reuses the shared contract internals and self-registers through inventory, emitting every field as a ZOH-held Value defaulting to the type's `Default`.

[why](rationale/composition-operators/product-type-unpack-operators.md)

## Terms

- **Operator** — the smallest node: a unit of DSP behavior, authored as one single-voice, single-channel block-at-a-time stream that the engine schedules.
- **Instrument** — a named subgraph that exposes an interface and is reused inside another graph as if it were an operator, with its own identity and state per use.
- **Rig** — the outermost graph, the one actually played at top level.
- **Message** — the one data unit: `{ address, frame, Arg }`, carrying exactly one `Arg`.
- **Arg** — the single closed-enum payload a Message carries: OSC primitives, shared vocab types (`Note`, `Harmony`), an erased enum index, or the dense `Buffer`.
- **Signal** — a Message whose `Arg` is a `Buffer`; the dense, per-sample port form (`f32_buffer`).
- **Value** — a latched, held, single-valued port form (`f32`/`enum`/`harmony`/`i32`), read as a constant within a `process` call via zero-order-hold.
- **Event** — an unlatched, multi-valued, frame-stamped port form (`note`), read as a stream and never sliced.
- **Constant** — a plan-time immutable port whose value is fixed at instantiate; changing it rebuilds the graph.
- **interface pipe** — a named boundary entry, the one boundary mechanism at every graph level: an input pipe mints an address, an output pipe is fed from an internal port.
- **subpatch** — a node referencing a nested instrument, inlined and dissolved into the parent graph at build.
- **logical channel** — the device-independent channel index a signal pipe binds; a device profile, not the patch, maps it to hardware.
