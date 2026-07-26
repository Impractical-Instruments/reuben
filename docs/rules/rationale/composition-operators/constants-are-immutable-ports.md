# Why: A node's surface is inputs, outputs, constants, and resources; a Constant is a plan-time immutable port whose change rebuilds the graph, structurally distinct from a runtime Input, with no separate param concept.

[Rule](../../composition-operators.md#constants-are-immutable-ports)

A node has exactly two kinds of surface, split on a **mutability axis**: **Inputs** are *runtime* —
values the engine can change while the patch runs (wired, or latched from a default); **Constants**
are *plan-time* — fixed when the graph is instantiated, and changing one rebuilds the graph (the
canonical case is the Voicer's `voices` pool size, which sets buffer allocation and topology). A
third concept — **param**, a runtime `f32` control predating the Message/Arg substrate — conflated
the two: once every runtime param became a materialized input
([declared-port-forms](declared-port-forms.md)) it hosted exactly one value, `voices`, which is a
Constant.

The fix deletes param and makes a **Constant an immutable port**, reusing the `Port` struct wholesale;
membership in the `constants` list vs `inputs` *is* the runtime/plan-time distinction, and constants
stay out of every loop that walks inputs (edges, buffers, materialization) so they never acquire a
wire or a per-sample buffer ([descriptor.rs](../../../../crates/reuben-core/src/descriptor.rs)). The
boundary is now **structural and hard to re-violate** — a node's whole surface is inputs, outputs,
constants, resources. The runtime was *already* generic: every message-rate value is one `Arg` in the
latch. Only the cold authoring layer was still bifurcated by type (`input_overrides` for f32 vs
`enum_overrides` for enum index, with `Str`/`I32`/`Harmony` having *no* override channel at all); that
collapses to one `value_overrides: Vec<(usize, Arg)>` plus a symmetric `constant_overrides`, upserted
through one `Port::coerce` seam that owns the only type-switch — so `Str`/`I32` inputs become settable
for free.

One detail worth keeping. `voices` is a true `i32` (OSC-native; the `1..=32` range enforced by
`coerce` is the real guard against insane values, not the integer width — `usize` is platform-width
and not wire-serializable), which is the settable integer type the `i32` interface pipe reuses
([interface-pipes](interface-pipes.md), [per-wire-form-check](per-wire-form-check.md)).

Distilled from: ADR-0035
