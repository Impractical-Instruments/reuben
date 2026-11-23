//! Per-operator micro-benchmark bridge.
//!
//! The macro layer ([`benches/macro_*`](../../benches)) benches end-to-end `render_block` of a
//! real instrument; this module drives one operator's [`Operator::process`] directly, through the
//! real engine via [`OpDriver`](crate::op_driver), so a regression is attributable to that
//! operator. Every kind in [`crate::bench_census::kinds`] is benched;
//! [`WORKLOADS`](crate::bench_support::WORKLOADS) names only the ones that need a recipe other than
//! [`Recipe::Default`](crate::bench_support::Recipe::Default), and
//! [`all_workloads`](crate::bench_support::all_workloads) applies it.

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use num_traits::Float;

use crate::bench_census;
use crate::descriptor::{Descriptor, PortType};
use crate::op_driver::OpDriver;
use crate::registry::Registry;
use crate::resources::SampleBuffer;
use crate::vocab::pitch::{Note, Pitch};

/// 48 kHz — the real shipped sample rate (matches the macro layer).
pub const SAMPLE_RATE: f32 = 48_000.0;
/// 128-frame blocks — the real shipped default.
pub const BLOCK_SIZE: usize = 128;
/// `375 * 128 == 48_000` == exactly 1 s of audio, the same fixed schedule the macro layer renders.
pub const BLOCKS: usize = 375;

/// How an operator's inputs are driven for its micro bench. The variant captures the *minimum* a
/// given operator needs to exercise its real per-sample path rather than an early-out idle path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recipe {
    /// Every `Float` input held at its descriptor default (constant ⇒ `varying = false`, so a
    /// const-folding operator benches its fast path), enums at their default, no events. Fits any
    /// operator whose `process` does real work every sample without a trigger (oscillators,
    /// filters, math, the LFO, the clock, …).
    Default,
    /// `Default`, plus the `audio` input held at a constant 0.5 — for shapers whose per-sample
    /// cost is dominated by a transcendental on the input (the saturator's tanh has a libm
    /// early-out at 0.0, so a silent bench underreports).
    Audio,
    /// `Default`, plus a constant-magnitude AC waveform driven into the `sidechain` key — for the
    /// compressor, whose detector idles below threshold. A held/DC key would be removed by the key
    /// high-pass; an above-threshold AC key runs the real path (peak log + ballistics + makeup exp)
    /// every sample.
    Sidechain,
    /// `Default`, plus the `gate` input held high — a rising edge at block 0 opens the envelope so
    /// its attack/decay/sustain math runs (an ungated envelope idles at zero).
    Gate,
    /// `Default`, plus the `clock` input driven as a per-block square wave, so the sequencer sees a
    /// rising + falling edge each block and walks its step table (a flat clock never advances it).
    Clocked,
    /// `Default`, plus a synthetic decoded sample bound to the resource slot, the `gate` held high
    /// (rising edge ⇒ trigger), and `freq` set positive — so the sample player's read loop runs.
    Sample,
    /// `Sample`, plus `loop` on over a short region — so the sample player's seam path (crossfade
    /// entry, the equal-power fade, the wrap) runs about twenty times in the second, alongside the
    /// plain runs between seams.
    SampleLoop,
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
    /// transformers (m2s, map, integrate, differentiate), or a `Note` event for the operators whose
    /// `in` is a `Note` port (the `osc_out` sink; `unpack_note`, whose event drives its latch).
    Value,
    /// `Default`, plus every numbered source input (`in1`, `in2`, …) held at a constant 0.5 — for
    /// the `mix` bus, which skips a silent source, so a default-held bench would sum nothing — and
    /// the first half of its numbered `gain` inputs driven per sample, so the bench pays for both
    /// the held-gain and the per-sample-gain paths.
    Sources,
    /// `Default`, plus a pattern set on `pat` and a clock driven into `phase` and `beat` (a beat
    /// every 3000 frames, so the second crosses four cycles) — for the `pattern`
    /// Operator, which fires nothing on an empty pattern or a still clock.
    Pattern,
}

/// One micro-bench workload: its kind and how to drive it.
#[derive(Debug, Clone, Copy)]
pub struct Workload {
    /// The operator's [`Descriptor::type_name`] / registry key, or a bench-only kind.
    pub kind: &'static str,
    pub recipe: Recipe,
}

const fn w(kind: &'static str, recipe: Recipe) -> Workload {
    Workload { kind, recipe }
}

/// The kinds whose micro bench needs a recipe other than [`Recipe::Default`]. Every other kind in
/// [`bench_census::kinds`] is benched with `Default` and has no line here. Keep alphabetical.
pub const WORKLOADS: &[Workload] = &[
    w("chord", Recipe::ChordSet),
    w("compressor", Recipe::Sidechain),
    w("differentiate_f32_signal", Recipe::Value),
    w("envelope", Recipe::Gate),
    w("euclid", Recipe::Clocked),
    w("granulator", Recipe::Grains),
    w("integrate_f32_signal", Recipe::Value),
    w("m2s", Recipe::Value),
    w("mix", Recipe::Sources),
    w("osc_out", Recipe::Value),
    w("pattern", Recipe::Pattern),
    w("plucked_string", Recipe::Gate),
    w("resonator", Recipe::Gate),
    w("sample", Recipe::Sample),
    w(SAMPLE_LOOP, Recipe::SampleLoop),
    w("saturator", Recipe::Audio),
    w("sequencer", Recipe::Clocked),
    w("snap", Recipe::Notes),
    w("strum", Recipe::Position),
    w("transpose", Recipe::Notes),
    // `unpack_note`'s `in` is a `Note` port (like `osc_out`), so `Value` pushes an event into it —
    // one note runs the frame-sorted latch + field emits rather than the idle baseline.
    w("unpack_note", Recipe::Value),
    w("voicer", Recipe::Notes),
];

/// `overhead` — the zero-DSP measurement point (follow-up to the OpDriver reframe).
///
/// Every micro case measures `step_node` = the operator's own `process` **plus** the engine's
/// per-node overhead (edge clear, routing, materialize, `Io` build). That overhead is a constant
/// offset per case, so a change to it smears across all fifty-odd cases as small uniform deltas —
/// visible only as "+6% on every cheap value op" — instead of failing one case whose name says
/// what regressed. This operator pins it down: a **no-op `process`** behind a typical port shape
/// (two Value inputs, one Signal output), so its entire instruction count *is* the per-node
/// stepping overhead, gated by the same 3%/10% thresholds as any operator.
///
/// It is deliberately **not registered** (no `operator_census!` entry names it): it isn't part of
/// the instrument format, must not appear in the schema / `describe` / a patchable graph, and the
/// committed schema stays identical with and without the `bench` feature. The census forcing
/// function carves out exactly this one kind, and [`OpHarness::for_kind`] constructs it directly
/// instead of through the registry.
pub mod overhead {
    use alloc::boxed::Box;

    use crate::descriptor::Descriptor;
    use crate::operator::{Io, Operator};

    /// The workload/bench kind string — a name the registry census must never claim.
    pub const KIND: &str = crate::bench_census::OVERHEAD;

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

pub use crate::bench_census::{SAMPLE_LOOP, VARIANTS};

/// The recipe `kind` is benched with: its [`WORKLOADS`] line, else [`Recipe::Default`].
pub fn recipe(kind: &str) -> Recipe {
    WORKLOADS
        .iter()
        .find(|w| w.kind == kind)
        .map_or(Recipe::Default, |w| w.recipe)
}

/// Every micro-bench workload: each kind in [`bench_census::kinds`] with its [`recipe`], sorted by
/// kind.
///
/// That includes `subpatch`. It registers no ports and dissolves at build, so the loader never lets
/// one reach a Plan; the harness constructs it directly and steps its no-op `process`, benching the
/// format anchor for census completeness.
pub fn all_workloads() -> Vec<Workload> {
    bench_census::kinds(&Registry::builtin())
        .into_iter()
        .map(|kind| w(kind, recipe(kind)))
        .collect()
}

/// A fully-prepared single-operator bench. Built by [`OpHarness::for_kind`] *outside* the measured
/// region; only [`OpHarness::render`] is timed. Rides on a real [`OpDriver`]: the recipe is applied
/// through the driver, and `render` steps the operator through the engine's real per-node path, so
/// the bench cannot drift from production stepping (the OpDriver reframe).
pub struct OpHarness {
    driver: OpDriver,
    /// Frames rendered per timed call — the fixed 1 s schedule (`BLOCKS * BLOCK_SIZE`).
    frames: usize,
}

impl OpHarness {
    /// Build the bench for an operator `kind`, applying its [`recipe`] through a real
    /// [`OpDriver`]. Setup only — plan instantiation, resource decode, and event/buffer construction
    /// all happen here, never in [`render`](Self::render).
    pub fn for_kind(kind: &str) -> Self {
        use crate::operator::Operator;
        // `overhead` is bench-only and deliberately absent from `Registry::builtin` (see
        // [`overhead`]) — layer it onto this local lookup copy through the embedder seam,
        // so every kind resolves through one uniform path.
        let mut reg = Registry::builtin();
        reg.register(
            || Box::new(overhead::Overhead::new()),
            overhead::Overhead::descriptor(),
        );
        let op = VARIANTS
            .iter()
            .find(|(variant, _)| *variant == kind)
            .map_or(kind, |(_, op)| *op);
        let entry = reg
            .get(op)
            .unwrap_or_else(|| panic!("unknown operator kind {op:?}"));
        let desc = entry.descriptor.clone();
        let mut driver = OpDriver::from_boxed((entry.make)(), desc.clone(), SAMPLE_RATE);
        apply_recipe(&mut driver, &desc, recipe(kind));
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
/// needs to exercise its real per-sample path rather than an early-out idle path.
fn apply_recipe(driver: &mut OpDriver, desc: &Descriptor, recipe: Recipe) {
    match recipe {
        Recipe::Default => {}
        Recipe::Audio => set_const(driver, desc, "audio", 0.5),
        Recipe::Sidechain => drive_ac(driver, desc, "sidechain", 0.5),
        Recipe::Gate => set_const(driver, desc, "gate", 1.0),
        Recipe::Clocked => drive_clock(driver, desc, "clock"),
        Recipe::Sample => {
            set_const(driver, desc, "gate", 1.0);
            set_const(driver, desc, "freq", 440.0);
            bind_synthetic_sample(driver, desc);
        }
        Recipe::SampleLoop => {
            apply_recipe(driver, desc, Recipe::Sample);
            set_const(driver, desc, "loop", 1.0);
            set_const(driver, desc, "loop_start", 0.1);
            set_const(driver, desc, "loop_end", 0.2);
        }
        Recipe::Grains => bind_synthetic_sample(driver, desc),
        Recipe::Notes => push_note(driver, desc, "notes", Note::new(Pitch::Absolute(60.0), 1.0)),
        Recipe::ChordSet => push_note(driver, desc, "set", Note::new(Pitch::Degree(0), 1.0)),
        Recipe::Position => set_const(driver, desc, "position", 0.5),
        // `in` is a `Float` control on the dense transformers but a `Note` port on `osc_out`; drive
        // each in its own type (the formerly-uniform value event is now split by type).
        Recipe::Value => {
            let i = input_index(desc, "in");
            if matches!(desc.inputs[i].ty, PortType::F32) {
                set_const(driver, desc, "in", 0.5);
            } else {
                push_note(driver, desc, "in", Note::new(Pitch::Absolute(60.0), 1.0));
            }
        }
        Recipe::Pattern => drive_pattern(driver, desc),
        Recipe::Sources => {
            for (i, p) in desc.inputs.iter().enumerate() {
                let numbered = p
                    .name
                    .strip_prefix("in")
                    .is_some_and(|k| !k.is_empty() && k.bytes().all(|b| b.is_ascii_digit()));
                if numbered {
                    driver.set(i, 0.5);
                }
            }
            let frames = BLOCKS * BLOCK_SIZE;
            let swept: Vec<f32> = (0..frames).map(|f| 0.5 + (f % 64) as f32 / 64.0).collect();
            for (i, p) in desc.inputs.iter().enumerate() {
                let first_half = p
                    .name
                    .strip_prefix("gain")
                    .and_then(|k| k.parse::<usize>().ok())
                    .is_some_and(|k| (1..=8).contains(&k));
                if first_half {
                    driver.drive(i, &swept);
                }
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

/// Drive a named Signal input with a period-4 ±`amp` square across the whole fixed schedule — a
/// constant-magnitude AC waveform (12 kHz at 48 kHz, inside a sidechain high-pass's passband) that
/// keeps a level detector on its real above-threshold path rather than an idle early-out.
fn drive_ac(driver: &mut OpDriver, desc: &Descriptor, name: &str, amp: f32) {
    let frames = BLOCKS * BLOCK_SIZE;
    let buf: Vec<f32> = (0..frames)
        .map(|i| if (i / 2) % 2 == 0 { amp } else { -amp })
        .collect();
    driver.drive(input_index(desc, name), &buf);
}

/// Queue a `Note` event for block 0 on the named event input (the engine routes it to the port's `io.read` stream).
fn push_note(driver: &mut OpDriver, desc: &Descriptor, name: &str, note: Note) {
    driver.push(input_index(desc, name), 0, note);
}

/// Drive a named clock input as a per-block square wave: high for the first half of every 128-frame
/// block, low for the second. The clock is a held **Value**, fed by edges rather than a
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

/// Frames per beat in [`Recipe::Pattern`]'s clock: 16 beats in the fixed second.
const PATTERN_BEAT: usize = 3_000;

/// Set `pat` to a pattern compiled by the port's own preparer, and drive `phase` and `beat` as a
/// Clock would: `phase` ramping over each [`PATTERN_BEAT`] frames, `beat` stepping where it wraps.
fn drive_pattern(driver: &mut OpDriver, desc: &Descriptor) {
    let pat = input_index(desc, "pat");
    let preparer = desc.inputs[pat]
        .preparer()
        .expect("the Pattern recipe needs a prepared `pat` input");
    let text = "0 [3 5] <7 ~> 2*2, 0(3,8) [1 ~]/2";
    let prepared = preparer
        .prepare(&text.into())
        .expect("the bench pattern compiles");
    driver.set(pat, crate::message::Arg::Prepared(prepared));
    let frames = BLOCKS * BLOCK_SIZE;
    let phase: Vec<f32> = (0..frames)
        .map(|f| (f % PATTERN_BEAT) as f32 / PATTERN_BEAT as f32)
        .collect();
    driver.drive(input_index(desc, "phase"), &phase);
    let beat = input_index(desc, "beat");
    for f in (PATTERN_BEAT..frames).step_by(PATTERN_BEAT) {
        driver.push(beat, f, (f / PATTERN_BEAT) as i32);
    }
}

/// Build a synthetic decoded sample (a 1 s sine, longer than the workload so the read loop never
/// runs dry) and bind it to the operator's first resource slot through the real loader path.
fn bind_synthetic_sample(driver: &mut OpDriver, desc: &Descriptor) {
    let frames = BLOCKS * BLOCK_SIZE;
    let step = core::f32::consts::TAU * 220.0 / SAMPLE_RATE;
    let channel: Vec<f32> = (0..frames).map(|i| Float::sin(i as f32 * step)).collect();
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
    use alloc::collections::BTreeSet;

    /// Every [`WORKLOADS`] line overrides a kind that is benched, with a recipe that differs from
    /// the one it would get anyway — so a removed or renamed operator cannot leave a dead override,
    /// and the table cannot drift back into restating the registry.
    #[test]
    fn every_workload_override_is_live() {
        let benched: BTreeSet<&str> = bench_census::kinds(&Registry::builtin())
            .into_iter()
            .collect();
        let mut seen = BTreeSet::new();
        for w in WORKLOADS {
            assert!(
                benched.contains(w.kind),
                "WORKLOADS names {:?}, which is not a micro-bench kind — remove the stale line",
                w.kind
            );
            assert!(
                w.recipe != Recipe::Default,
                "WORKLOADS names {:?} with Recipe::Default, which every kind gets without a line — \
                 remove it",
                w.kind
            );
            assert!(seen.insert(w.kind), "WORKLOADS names {:?} twice", w.kind);
        }
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

    /// Every workload builds and renders a full block schedule without panicking — a cheap smoke
    /// test that the harness wires each operator's `Io` correctly (right slice lengths, resource
    /// bound, latch/streams attached) regardless of its port shape.
    #[test]
    fn the_pattern_recipe_fires_notes_every_cycle() {
        let mut h = OpHarness::for_kind("pattern");
        h.driver.render(h.frames);
        let ons = (h.driver.emits().iter())
            .filter(|e| matches!(e.arg, crate::message::Arg::Note(n) if !n.is_off()))
            .count();
        // Per cycle: `0 [3 5] <7 ~> 2*2` plays five or six, `0(3,8)` three, `[1 ~]/2` one in two.
        assert!(ons >= 4 * 8, "the recipe plays its pattern: {ons} note-ons");
    }

    #[test]
    fn every_workload_renders() {
        for w in all_workloads() {
            let out = OpHarness::for_kind(w.kind).render();
            assert!(
                out.is_finite(),
                "{} produced a non-finite accumulator",
                w.kind
            );
        }
    }
}
