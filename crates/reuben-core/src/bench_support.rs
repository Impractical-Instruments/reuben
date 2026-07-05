//! Per-operator micro-benchmark bridge (#30, follow-up to #19 / ADR-0019; unified model, ADR-0030).
//!
//! The macro layer ([`benches/macro_*`](../../benches)) benches end-to-end `render_block` of a
//! real instrument. This module is the deferred *micro* layer: it drives a single operator's
//! [`Operator::process`] directly, bypassing the graph, so a regression in one operator's
//! per-sample loop is attributable to that operator.
//!
//! The operator is driven through the **real engine** by [`OpDriver`](crate::op_driver): the harness
//! applies its recipe with `set`/`push`/`drive`/`bind`, and [`OpHarness::render`] times
//! `Renderer::step_node` over the fixed schedule. So this layer can never drift from how the engine
//! actually seeds and steps a node — and the engine per-node overhead it now includes (edge clear,
//! routing, materialize, `Io` build) is a *constant* per-operator offset, so regression detection
//! survives the shift from "process cost" to "per-node cost" (the OpDriver reframe of ADR-0019).
//! That constant offset is also measured *by itself*: the bench-only [`overhead`] case is a no-op
//! operator behind a typical port shape, so a change to the engine's stepping cost fails one case
//! whose name says so instead of smearing small deltas across every cheap operator.
//! The external bench crate constructs an `OpHarness` by operator kind and never touches raw `Io`.
//!
//! The single source of truth for *which* operators are benched and *how* each is driven is
//! [`WORKLOADS`]. The criterion layer iterates it at runtime; the iai layer references entries by
//! kind. The [`tests::every_operator_has_a_micro_bench_workload`] forcing function asserts
//! `WORKLOADS` covers every registered operator, so adding an operator without a workload reds CI.
//!
//! Determinism (ADR-0001): every workload is a fixed function of constants — no clock, no entropy
//! (the one RNG operator, `noise`, is seeded) — so iai instruction counts are byte-stable.

use crate::descriptor::{Descriptor, PortType};
use crate::op_driver::OpDriver;
use crate::registry::Registry;
use crate::resources::SampleBuffer;
use crate::vocab::pitch::{Note, Pitch};

/// 48 kHz — the real shipped sample rate (matches the macro layer, ADR-0019).
pub const SAMPLE_RATE: f32 = 48_000.0;
/// 128-frame blocks — the real shipped default.
pub const BLOCK_SIZE: usize = 128;
/// `375 * 128 == 48_000` == exactly 1 s of audio, the same fixed schedule the macro layer renders.
pub const BLOCKS: usize = 375;

/// How an operator's inputs are driven for its micro bench. One per [`WORKLOADS`] entry; the
/// variant captures the *minimum* a given operator needs to exercise its real per-sample path
/// rather than an early-out idle path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recipe {
    /// Every `Float` input held at its descriptor default (constant ⇒ `varying = false`, so a
    /// const-folding operator benches its fast path), enums at their default, no events. Fits any
    /// operator whose `process` does real work every sample without a trigger (oscillators,
    /// filters, math, the LFO, the clock, …).
    Default,
    /// `Default`, plus the `gate` input held high — a rising edge at block 0 opens the envelope so
    /// its attack/decay/sustain math runs (an ungated envelope idles at zero).
    Gate,
    /// `Default`, plus the `clock` input driven as a per-block square wave, so the sequencer sees a
    /// rising + falling edge each block and walks its step table (a flat clock never advances it).
    Clocked,
    /// `Default`, plus a synthetic decoded sample bound to the resource slot, the `gate` held high
    /// (rising edge ⇒ trigger), and `freq` set positive — so the sample player's read loop runs.
    Sample,
    /// `Default`, plus a synthetic decoded sample bound to the resource slot — for the free-running
    /// granulator, which needs no gate/note: at default density it spawns grains automatically, so
    /// the bound sample alone exercises its real grain-summing path.
    Grains,
    /// `Default`, plus a `note [60, 1]` event at block 0 — drives note-oriented operators (the
    /// voicer's voice allocation + render, the snap quantizer's resolve+emit).
    Notes,
    /// `Default`, plus a `set` degree-`Note` event at block 0 — drives the chord expander.
    ChordSet,
    /// `Default`, plus the `position` control held at a non-default value — its first sample crosses
    /// a string boundary (the strummer seeds `prev_string` at -1), so the strummer plucks.
    Position,
    /// `Default`, plus driving the `in` port — a held `0.5` for the dense message→signal
    /// transformers (m2s, map, integrate, differentiate), or a `Note` event for the message sink
    /// (osc_out, whose `in` is a `Note` port).
    Value,
}

/// One operator's micro-bench workload: its registry `kind` and how to drive it. The whole table
/// is `const`, so it doubles as the forcing-function census (see [`tests`]).
#[derive(Debug, Clone, Copy)]
pub struct Workload {
    /// The operator's [`Descriptor::type_name`] / registry key.
    pub kind: &'static str,
    pub recipe: Recipe,
}

const fn w(kind: &'static str, recipe: Recipe) -> Workload {
    Workload { kind, recipe }
}

/// Every built-in operator, with the recipe that exercises its real path, plus the bench-only
/// [`overhead`] case. MUST stay in lockstep with [`Registry::builtin`] — the forcing-function test
/// fails CI if an operator is missing here (`overhead` is its one carved-out exception: benched but
/// deliberately unregistered). Keep alphabetical for easy diffing against the registry's stable
/// (type-name) order.
pub const WORKLOADS: &[Workload] = &[
    w("abs_f32_signal", Recipe::Default),
    w("abs_f32_value", Recipe::Default),
    w("add_f32_signal", Recipe::Default),
    w("add_f32_value", Recipe::Default),
    w("chord", Recipe::ChordSet),
    w("clamp_f32_signal", Recipe::Default),
    w("clamp_f32_value", Recipe::Default),
    w("clock", Recipe::Default),
    w("delay", Recipe::Default),
    w("differentiate_f32_signal", Recipe::Value),
    w("div_f32_signal", Recipe::Default),
    w("div_f32_value", Recipe::Default),
    w("djfilter", Recipe::Default),
    w("envelope", Recipe::Gate),
    w("euclid", Recipe::Clocked),
    w("filter", Recipe::Default),
    w("granulator", Recipe::Grains),
    w("harmony", Recipe::Default),
    w("integrate_f32_signal", Recipe::Value),
    w("lfo", Recipe::Default),
    w("m2s", Recipe::Value),
    w("map_f32_signal", Recipe::Default),
    w("map_f32_value", Recipe::Default),
    w("max_f32_signal", Recipe::Default),
    w("max_f32_value", Recipe::Default),
    w("min_f32_signal", Recipe::Default),
    w("min_f32_value", Recipe::Default),
    w("modulo_f32_signal", Recipe::Default),
    w("modulo_f32_value", Recipe::Default),
    w("mul_f32_signal", Recipe::Default),
    w("mul_f32_value", Recipe::Default),
    w("negate_f32_signal", Recipe::Default),
    w("negate_f32_value", Recipe::Default),
    w("noise", Recipe::Default),
    w("osc_out", Recipe::Value),
    w("oscillator", Recipe::Default),
    w("output", Recipe::Default),
    // Bench-only, not a registered operator (see the [`overhead`] module): the zero-DSP point that
    // isolates the engine's per-node stepping overhead as its own gated, attributable case.
    w(overhead::KIND, Recipe::Default),
    w("pan", Recipe::Default),
    w("power_f32_signal", Recipe::Default),
    w("power_f32_value", Recipe::Default),
    w("reciprocal_f32_signal", Recipe::Default),
    w("reciprocal_f32_value", Recipe::Default),
    w("resonator", Recipe::Gate),
    w("reverb", Recipe::Default),
    w("sample", Recipe::Sample),
    w("sequencer", Recipe::Clocked),
    w("snap", Recipe::Notes),
    w("strum", Recipe::Position),
    w("sub_f32_signal", Recipe::Default),
    w("sub_f32_value", Recipe::Default),
    // `subpatch` registers no ports and dissolves at build (ADR-0034 §2), so the loader never
    // lets one reach a Plan; the harness constructs it directly and steps its no-op `process`,
    // benching the format anchor for census completeness.
    w("subpatch", Recipe::Default),
    w("transpose", Recipe::Notes),
    w("voicer", Recipe::Notes),
];

/// The operator kinds the iai CI gate benches — the canonical mirror of `micro_iai.rs`'s
/// compile-time `#[bench::…]` list (#30, ADR-0019). iai's `harness = false` bench can't host a
/// normal test to introspect its own attributes, so the list lives here, where the
/// [`tests::iai_list_covers_every_workload`] forcing function (in the `check` job) asserts it equals
/// [`WORKLOADS`]. Adding an operator therefore reds `check` until both this list and the matching
/// `#[bench::<kind>]` attribute beside it are added. Keep both in sync (and alphabetical).
pub const MICRO_IAI_KINDS: &[&str] = &[
    "abs_f32_signal",
    "abs_f32_value",
    "add_f32_signal",
    "add_f32_value",
    "chord",
    "clamp_f32_signal",
    "clamp_f32_value",
    "clock",
    "delay",
    "differentiate_f32_signal",
    "div_f32_signal",
    "div_f32_value",
    "djfilter",
    "envelope",
    "euclid",
    "filter",
    "granulator",
    "harmony",
    "integrate_f32_signal",
    "lfo",
    "m2s",
    "map_f32_signal",
    "map_f32_value",
    "max_f32_signal",
    "max_f32_value",
    "min_f32_signal",
    "min_f32_value",
    "modulo_f32_signal",
    "modulo_f32_value",
    "mul_f32_signal",
    "mul_f32_value",
    "negate_f32_signal",
    "negate_f32_value",
    "noise",
    "osc_out",
    "oscillator",
    "output",
    "overhead",
    "pan",
    "power_f32_signal",
    "power_f32_value",
    "reciprocal_f32_signal",
    "reciprocal_f32_value",
    "resonator",
    "reverb",
    "sample",
    "sequencer",
    "snap",
    "strum",
    "sub_f32_signal",
    "sub_f32_value",
    "subpatch",
    "transpose",
    "voicer",
];

/// `overhead` — the zero-DSP measurement point (ADR-0019 follow-up to the OpDriver reframe).
///
/// Every micro case measures `step_node` = the operator's own `process` **plus** the engine's
/// per-node overhead (edge clear, routing, materialize, `Io` build). That overhead is a constant
/// offset per case, so a change to it smears across all fifty-odd cases as small uniform deltas —
/// visible only as "+6% on every cheap value op" — instead of failing one case whose name says
/// what regressed. This operator pins it down: a **no-op `process`** behind a typical port shape
/// (two Value inputs, one Signal output), so its entire instruction count *is* the per-node
/// stepping overhead, gated by the same 3%/10% thresholds as any operator.
///
/// It is deliberately **not registered** (`register_operator!` is never invoked): it isn't part of
/// the instrument format, must not appear in the schema / `describe` / a patchable graph, and the
/// committed schema stays identical with and without the `bench` feature. The census forcing
/// function carves out exactly this one kind, and [`OpHarness::for_kind`] constructs it directly
/// instead of through the registry.
pub mod overhead {
    use crate::descriptor::Descriptor;
    use crate::operator::{Io, Operator};

    /// The workload/bench kind string — a name the registry census must never claim.
    pub const KIND: &str = "overhead";

    // A representative surface, not a used one: two held Values + a Signal out is the modal
    // operator shape, so the engine builds/clears/materializes what it would for a real node.
    crate::operator_contract!(Overhead {
        inputs:  { a: f32 { 0.0..=1.0, default 0.0, "", lin },
                   b: f32 { 0.0..=1.0, default 0.0, "", lin } },
        outputs: { out: f32_buffer },
    });

    /// Stateless: the whole point is that `process` contributes zero instructions.
    #[derive(Default)]
    pub struct Overhead;

    impl Overhead {
        pub fn new() -> Self {
            Self
        }
    }

    impl Operator for Overhead {
        fn descriptor() -> Descriptor {
            Self::contract()
        }

        /// Deliberately empty — everything the bench counts happens in the engine around it.
        fn process(&mut self, _io: &mut Io) {}

        fn spawn(&self) -> Box<dyn Operator> {
            Box::new(Self::new())
        }
    }
}

/// Look up the workload for an operator kind. Panics if absent — a bench referenced a kind with no
/// workload, which the forcing-function test would also have caught.
pub fn workload(kind: &str) -> Workload {
    *WORKLOADS
        .iter()
        .find(|w| w.kind == kind)
        .unwrap_or_else(|| panic!("no micro-bench workload for operator kind {kind:?}"))
}

/// A fully-prepared single-operator bench. Built by [`OpHarness::for_kind`] *outside* the measured
/// region; only [`OpHarness::render`] is timed. Rides on a real [`OpDriver`]: the recipe is applied
/// through the driver, and `render` steps the operator through the engine's real per-node path, so
/// the bench cannot drift from production stepping (the OpDriver reframe of ADR-0019).
pub struct OpHarness {
    driver: OpDriver,
    /// Frames rendered per timed call — the fixed 1 s schedule (`BLOCKS * BLOCK_SIZE`).
    frames: usize,
}

impl OpHarness {
    /// Build the bench for an operator `kind`, applying its [`WORKLOADS`] recipe through a real
    /// [`OpDriver`]. Setup only — plan instantiation, resource decode, and event/buffer construction
    /// all happen here, never in [`render`](Self::render).
    pub fn for_kind(kind: &str) -> Self {
        use crate::operator::Operator;
        // `overhead` is bench-only and deliberately absent from `Registry::builtin` (see
        // [`overhead`]) — layer it onto this local lookup copy through the embedder seam
        // (ADR-0004), so every kind resolves through one uniform path.
        let mut reg = Registry::builtin();
        reg.register(
            || Box::new(overhead::Overhead::new()),
            overhead::Overhead::descriptor(),
        );
        let entry = reg
            .get(kind)
            .unwrap_or_else(|| panic!("unknown operator kind {kind:?}"));
        let desc = entry.descriptor.clone();
        let mut driver = OpDriver::from_boxed((entry.make)(), desc.clone(), SAMPLE_RATE);
        apply_recipe(&mut driver, &desc, workload(kind).recipe);
        Self {
            driver,
            frames: BLOCKS * BLOCK_SIZE,
        }
    }

    /// Render the full fixed workload — [`BLOCKS`] real `step_node` blocks, threading
    /// operator state across them; events fire at block 0 only (the note-on-then-tail shape).
    /// Accumulates a value depending on the outputs + emit activity so the optimizer cannot elide
    /// the work; the sum is the bench's return value (the caller `black_box`es it under iai).
    pub fn render(mut self) -> f32 {
        self.driver.render(self.frames);
        let mut acc = 0.0f32;
        for out in self.driver.outputs() {
            acc += out.first().copied().unwrap_or(0.0);
        }
        acc + self.driver.emits().len() as f32
    }
}

/// Apply a [`Recipe`]'s input drives + events to a freshly-built driver — the minimum each operator
/// needs to exercise its real per-sample path rather than an early-out idle path (ADR-0030).
fn apply_recipe(driver: &mut OpDriver, desc: &Descriptor, recipe: Recipe) {
    match recipe {
        Recipe::Default => {}
        Recipe::Gate => set_const(driver, desc, "gate", 1.0),
        Recipe::Clocked => drive_clock(driver, desc, "clock"),
        Recipe::Sample => {
            set_const(driver, desc, "gate", 1.0);
            set_const(driver, desc, "freq", 440.0);
            bind_synthetic_sample(driver, desc);
        }
        Recipe::Grains => bind_synthetic_sample(driver, desc),
        Recipe::Notes => push_note(driver, desc, "notes", Note::new(Pitch::Absolute(60.0), 1.0)),
        Recipe::ChordSet => push_note(driver, desc, "set", Note::new(Pitch::Degree(0), 1.0)),
        Recipe::Position => set_const(driver, desc, "position", 0.5),
        // `in` is a `Float` control on the dense transformers but a `Note` port on `osc_out`; drive
        // each in its own type (ADR-0030 split the formerly-uniform value event).
        Recipe::Value => {
            let i = input_index(desc, "in");
            if matches!(desc.inputs[i].ty, PortType::F32) {
                set_const(driver, desc, "in", 0.5);
            } else {
                push_note(driver, desc, "in", Note::new(Pitch::Absolute(60.0), 1.0));
            }
        }
    }
}

/// Index of input `name` (declaration order). Panics if absent — a recipe named an input the
/// operator doesn't have, a wiring bug worth failing loudly on in setup.
fn input_index(desc: &Descriptor, name: &str) -> usize {
    desc.inputs
        .iter()
        .position(|p| p.name == name)
        .unwrap_or_else(|| panic!("{:?} has no input {name:?}", desc.type_name))
}

/// Hold a named control (or constant audio-in) at `value` — the held (ZOH) `io.read` value for a
/// `Float`/enum, a constant materialized buffer for an audio input. Sticky across blocks.
fn set_const(driver: &mut OpDriver, desc: &Descriptor, name: &str, value: f32) {
    driver.set(input_index(desc, name), value);
}

/// Queue a `Note` event for block 0 on the named event input (the engine routes it to the port's `io.read` stream).
fn push_note(driver: &mut OpDriver, desc: &Descriptor, name: &str, note: Note) {
    driver.push(input_index(desc, name), 0, note);
}

/// Drive a named clock input as a per-block square wave: high for the first half of every 128-frame
/// block, low for the second. The clock is a held **Value** (ADR-0031), fed by edges rather than a
/// per-sample buffer, so push a level change at each 0.5-threshold crossing — a rising edge every
/// block (last-low → first-high) with a falling edge mid-block, so a sequencer walks its step table.
fn drive_clock(driver: &mut OpDriver, desc: &Descriptor, name: &str) {
    let half = BLOCK_SIZE / 2;
    let port = input_index(desc, name);
    let mut prev = 0.0f32;
    for f in 0..BLOCKS * BLOCK_SIZE {
        let level = if f % BLOCK_SIZE < half { 1.0 } else { 0.0 };
        if (prev < 0.5) != (level < 0.5) {
            driver.push(port, f, level);
            prev = level;
        }
    }
}

/// Build a synthetic decoded sample (a 1 s sine, longer than the workload so the read loop never
/// runs dry) and bind it to the operator's first resource slot through the real loader path.
fn bind_synthetic_sample(driver: &mut OpDriver, desc: &Descriptor) {
    let frames = BLOCKS * BLOCK_SIZE;
    let step = std::f32::consts::TAU * 220.0 / SAMPLE_RATE;
    let channel: Vec<f32> = (0..frames).map(|i| (i as f32 * step).sin()).collect();
    let slot = desc
        .resources
        .first()
        .expect("Sample recipe needs a resource slot")
        .name;
    driver.bind(slot, SampleBuffer::new(vec![channel], SAMPLE_RATE));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Forcing function, half 1 (#30): every registered operator must have a [`WORKLOADS`] entry,
    /// so a new operator can't silently escape the micro layer. Runs in the `check` job under
    /// `--features bench`.
    #[test]
    fn every_operator_has_a_micro_bench_workload() {
        let registered: BTreeSet<&str> = Registry::builtin().type_names().collect();
        let mut benched: BTreeSet<&str> = WORKLOADS.iter().map(|w| w.kind).collect();
        // The one deliberate registry/bench asymmetry: `overhead` is benched but never registered
        // (bench-only, not part of the instrument format — see the `overhead` module docs). Remove
        // it before comparing; if it ever *does* get registered, the duplicate-name panic in
        // `Registry::builtin` and the assert below both have it covered.
        assert!(
            benched.remove(overhead::KIND),
            "the bench-only `overhead` workload disappeared from WORKLOADS"
        );
        assert!(
            !registered.contains(overhead::KIND),
            "`overhead` is bench-only and must never be a registered operator — registering it \
             would leak it into the schema and patchable graphs"
        );
        assert_eq!(
            registered, benched,
            "WORKLOADS is out of sync with the operator registry — add a `w(\"<kind>\", …)` entry \
             in bench_support.rs for any new operator (or remove a stale one)"
        );
    }

    /// The overhead case is a true zero-DSP point: a typical port surface (two Values in, one
    /// Signal out) around a `process` that writes nothing — driven through the real engine, its
    /// output is silence and its cost is pure per-node stepping overhead.
    #[test]
    fn overhead_probe_is_a_silent_noop_with_a_typical_surface() {
        use crate::operator::Operator;
        let d = overhead::Overhead::descriptor();
        assert_eq!(d.type_name, overhead::KIND);
        assert_eq!(
            d.inputs.len(),
            2,
            "two Value inputs (the modal operator shape)"
        );
        assert_eq!(d.outputs.len(), 1, "one Signal output");

        let mut driver = OpDriver::for_type(overhead::Overhead::new(), SAMPLE_RATE);
        driver.render(BLOCK_SIZE * 4);
        assert!(
            driver.output(0).iter().all(|&s| s == 0.0),
            "overhead must write nothing — silence out"
        );
        assert!(driver.emits().is_empty(), "overhead must emit nothing");
    }

    /// Forcing function, half 2 (#30): the iai CI gate ([`MICRO_IAI_KINDS`] / `micro_iai.rs`'s
    /// `#[bench::…]` list) must cover every workload, so a new operator can't be benched locally
    /// (criterion auto-iterates `WORKLOADS`) yet escape the gate. Lives here, not beside the iai
    /// bench, because a `harness = false` bench can't host a libtest. On failure: add the missing
    /// `#[bench::<kind>(args = ("<kind>",), setup = OpHarness::for_kind)]` attribute in
    /// `micro_iai.rs` *and* the matching [`MICRO_IAI_KINDS`] entry.
    #[test]
    fn iai_list_covers_every_workload() {
        let benched: BTreeSet<&str> = WORKLOADS.iter().map(|w| w.kind).collect();
        let gated: BTreeSet<&str> = MICRO_IAI_KINDS.iter().copied().collect();
        assert_eq!(
            benched, gated,
            "MICRO_IAI_KINDS (and micro_iai.rs's #[bench] list) is out of sync with WORKLOADS"
        );
    }

    /// Every workload builds and renders a full block schedule without panicking — a cheap smoke
    /// test that the harness wires each operator's `Io` correctly (right slice lengths, resource
    /// bound, latch/streams attached) regardless of its port shape.
    #[test]
    fn every_workload_renders() {
        for w in WORKLOADS {
            let out = OpHarness::for_kind(w.kind).render();
            assert!(
                out.is_finite(),
                "{} produced a non-finite accumulator",
                w.kind
            );
        }
    }
}
