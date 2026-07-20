# Why: Every port carries one of three forms — Value (latched, held, sparse), Event (unlatched, multi-valued), or Signal (dense per-sample buffer) — fixed at authoring by the port's declared type, not inferred from the graph.

[Rule](../../composition-operators.md#declared-port-forms)

On the one Message/Arg substrate ([message-arg-substrate](message-arg-substrate.md)), a wire still has
to carry data in one of a few shapes. There are exactly **three**, and they fall out of two
independent axes — *latched?* and *single-valued?* ([plan.rs](../../../../crates/reuben-core/src/plan.rs)):
**Value** (latched ∧ single: a scalar/enum/`Harmony` held by zero-order-hold, block-sliced so it reads
as a constant within a `process` call), **Event** (unlatched ∧ multi: `Note`, delivered frame-stamped
and never sliced), and **Signal** (dense per-sample buffer, audio). The other two combinations are
nonsense, so the set is closed at three. Slicing is *derived*, not declared: slice = latched ∧
single-valued = Value.

The decision that matters is that the numeric form is **declared at authoring, not inferred from the
graph**. An earlier design resolved each `f32` port's form with a plan-time topological propagation
pass — a forward solver, a feedback back-edge rule, a two-arm read API — all complexity spent to avoid
ever asking the author which form a port is. Scrubbing it against real DSP showed that avoidance *was*
the single thing making the model hard to hold. So the author writes one keyword: **`f32` = Value,
`f32_buffer` = Signal** (`enum`/`harmony` are Value-only, `note` Event-only). The numeric type is the
only one with a choice, and it is a one-keyword fact about what the port *is*, not something to
discover: declare `f32_buffer` where stepped values would sound wrong (a swept `filter.cutoff`, an
`oscillator.freq`, an audio wire); declare `f32` where the data is sparse, held, or event-like (a
`tempo` knob, a `gate`/trigger, a pitch latched once per hit). The discriminator throughout: *does the
value vary per-sample in a musically required way?*

This corrects a real mistake — the prior "a `Float` is always materialized into a buffer" was carried
forward without weighing the cost, and it made a rarely-changing knob pay a per-sample price it does
not owe (a `frames`-length buffer allocated and filled every block, 48k iterations/second for a value
that changes twice). Now buffers are allocated only for a declared-Signal port or a materialized
Value→Signal edge; a Value port gets a latch slot and does **zero** per-sample work. A port that must
be modulatable is simply declared `f32_buffer` — it accepts both a Signal source and a materialized
constant, so nothing is lost. The library *does* fork for math ops (value-math vs signal-math nodes),
and that is a feature, not a tax ([pointwise-number-operators](pointwise-number-operators.md)).

Distilled from: ADR-0031, ADR-0030
