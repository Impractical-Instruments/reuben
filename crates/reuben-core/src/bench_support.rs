//! Per-operator micro-benchmark bridge (#30, follow-up to #19 / ADR-0019).
//!
//! The macro layer ([`benches/macro_*`](../../benches)) benches end-to-end `render_block` of a
//! real instrument. This module is the deferred *micro* layer: it drives a single operator's
//! [`Operator::process`] directly, bypassing the graph, so a regression in one operator's
//! per-sample loop is attributable to that operator.
//!
//! `process` is fed an [`Io`], whose constructor and `with_*` builders are `pub(crate)` — the
//! "privacy bridge" ADR-0019 deferred. Rather than widen those to `pub` (a public-API leak), this
//! module — itself inside the crate — reaches them and exposes only one typed surface,
//! [`OpHarness`], gated behind the non-default `bench` feature (see [`crate`] docs). The external
//! bench crate constructs an `OpHarness` by operator kind and never touches raw `Io`.
//!
//! The single source of truth for *which* operators are benched and *how* each is driven is
//! [`WORKLOADS`]. The criterion layer iterates it at runtime; the iai layer references entries by
//! kind. The [`tests::every_operator_has_a_micro_bench_workload`] forcing function asserts
//! `WORKLOADS` covers every registered operator, so adding an operator without a workload reds CI.
//!
//! Determinism (ADR-0001): every workload is a fixed function of constants — no clock, no entropy
//! (the one RNG operator, `noise`, is seeded) — so iai instruction counts are byte-stable.

use std::sync::Arc;

use crate::descriptor::{Descriptor, LaneRule, Shape};
use crate::harmony::Harmony;
use crate::message::{Arg, Args, Emit, Event, Outbound};
use crate::operator::{CtxPublish, Io, Operator};
use crate::registry::Registry;
use crate::resources::{ResolvedRefs, ResourceStore, SampleBuffer};

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
    /// `Default`, plus a `note [60, 1]` event at block 0 — drives note-oriented operators (the
    /// voicer's voice allocation + render, the snap quantizer's resolve+emit).
    Notes,
    /// `Default`, plus a `set [0, 4, 7]` chord event at block 0 — drives the chord expander.
    ChordSet,
    /// `Default`, plus a `position [60, 1]` event at block 0 — drives the strummer.
    Position,
    /// `Default`, plus an `in [0.5]` value event at block 0 — drives the message→signal /
    /// message→message transformers (m2s, map, integrate, differentiate, osc_out).
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

/// Every built-in operator, with the recipe that exercises its real path. MUST stay in lockstep
/// with [`Registry::builtin`] — the forcing-function test fails CI if an operator is missing here.
/// Keep alphabetical for easy diffing against the registry's stable (type-name) order.
pub const WORKLOADS: &[Workload] = &[
    w("add", Recipe::Default),
    w("chord", Recipe::ChordSet),
    w("clock", Recipe::Default),
    w("context", Recipe::Default),
    w("delay", Recipe::Default),
    w("differentiate", Recipe::Value),
    w("djfilter", Recipe::Default),
    w("envelope", Recipe::Gate),
    w("filter", Recipe::Default),
    w("integrate", Recipe::Value),
    w("lfo", Recipe::Default),
    w("m2s", Recipe::Value),
    w("map", Recipe::Value),
    w("mul", Recipe::Default),
    w("noise", Recipe::Default),
    w("osc_out", Recipe::Value),
    w("oscillator", Recipe::Default),
    w("output", Recipe::Default),
    w("pan", Recipe::Default),
    w("power", Recipe::Default),
    w("reverb", Recipe::Default),
    w("sample", Recipe::Sample),
    w("sequencer", Recipe::Clocked),
    w("snap", Recipe::Notes),
    w("strum", Recipe::Position),
    w("voicer", Recipe::Notes),
];

/// The operator kinds the iai CI gate benches — the canonical mirror of `micro_iai.rs`'s
/// compile-time `#[bench::…]` list (#30, ADR-0019). iai's `harness = false` bench can't host a
/// normal test to introspect its own attributes, so the list lives here, where the
/// [`tests::iai_list_covers_every_workload`] forcing function (in the `check` job) asserts it equals
/// [`WORKLOADS`]. Adding an operator therefore reds `check` until both this list and the matching
/// `#[bench::<kind>]` attribute beside it are added. Keep both in sync (and alphabetical).
pub const MICRO_IAI_KINDS: &[&str] = &[
    "add",
    "chord",
    "clock",
    "context",
    "delay",
    "differentiate",
    "djfilter",
    "envelope",
    "filter",
    "integrate",
    "lfo",
    "m2s",
    "map",
    "mul",
    "noise",
    "osc_out",
    "oscillator",
    "output",
    "pan",
    "power",
    "reverb",
    "sample",
    "sequencer",
    "snap",
    "strum",
    "voicer",
];

/// Look up the workload for an operator kind. Panics if absent — a bench referenced a kind with no
/// workload, which the forcing-function test would also have caught.
pub fn workload(kind: &str) -> Workload {
    *WORKLOADS
        .iter()
        .find(|w| w.kind == kind)
        .unwrap_or_else(|| panic!("no micro-bench workload for operator kind {kind:?}"))
}

/// A fully-prepared single-operator bench. Built by [`OpHarness::for_kind`] *outside* the measured
/// region; only [`OpHarness::render`] is timed. Mirrors the macro layer's `BenchState`: state is
/// rebuilt per criterion iteration so nothing carries over between timings.
pub struct OpHarness {
    op: Box<dyn Operator>,
    lanes: usize,
    params: Vec<f32>,
    /// Per-slot input buffers, `Some` for each `Float` port. Indexed full-order for shaped
    /// operators (ADR-0028) and signal-order for legacy ones — matching the engine's own wiring
    /// (see [`build_inputs`]).
    in_bufs: Vec<Option<Vec<f32>>>,
    /// Held enum index per slot (full-order; empty for legacy operators, which have no enums).
    enums: Vec<usize>,
    /// Resolved context per slot (full-order for shaped, context-order for legacy).
    contexts: Vec<Harmony>,
    /// `varying` hint per slot, aligned with `in_bufs`.
    varying: Vec<bool>,
    /// One buffer per `Float` output port (declaration order).
    out_bufs: Vec<Vec<f32>>,
    /// Events injected at block 0 only (owned; borrowed into `Event`s each block).
    ev_addr: Vec<&'static str>,
    ev_args: Vec<Args>,
    /// Lane-0 sinks, mirroring the engine: every operator gets all three so emit/publish/outbound
    /// operators exercise their sink path without per-recipe wiring. Cleared each block.
    emit: Vec<Emit>,
    ctx_pub: Vec<CtxPublish>,
    outbound: Vec<Outbound>,
    /// Kept alive for the operator's lifetime — `bind_resources` clones the `Arc`.
    _store: Option<Arc<ResourceStore>>,
}

/// Whether an operator's ports are numbered sequentially (ADR-0028, the modern world) vs per-kind
/// (the legacy carrier numbering). Mirrors the macro's rule in `reuben-macros`: a port list is
/// "shaped" iff any input is a materialized `Float` or an `Enum`. The engine wires `Io`'s context /
/// enum / input slices full-order for shaped operators and per-kind for legacy ones, so the harness
/// must match or `io.harmony`/`io.signal` would read the wrong slot.
fn is_shaped(desc: &Descriptor) -> bool {
    desc.inputs
        .iter()
        .any(|p| p.shape == Shape::Enum || (p.shape == Shape::Float && p.meta.is_some()))
}

impl OpHarness {
    /// Build the bench for an operator `kind`, applying its [`WORKLOADS`] recipe. Setup only —
    /// allocation, resource decode, and event construction all happen here, never in [`render`].
    pub fn for_kind(kind: &str) -> Self {
        let reg = Registry::builtin();
        let entry = reg
            .get(kind)
            .unwrap_or_else(|| panic!("unknown operator kind {kind:?}"));
        let desc = entry.descriptor.clone();
        let mut op = (entry.make)();
        let recipe = workload(kind).recipe;
        let shaped = is_shaped(&desc);

        let WiredInputs {
            mut in_bufs,
            enums,
            contexts,
            mut varying,
        } = build_inputs(&desc, shaped);
        let out_bufs: Vec<Vec<f32>> = desc
            .outputs
            .iter()
            .filter(|p| p.shape == Shape::Float)
            .map(|_| vec![0.0; BLOCK_SIZE])
            .collect();

        let lanes = match desc.lanes {
            LaneRule::FromParam(slot) => (desc.params[slot].default.round() as usize).max(1),
            LaneRule::Inherit => 1,
        };

        // Apply the recipe's input drives + events. `set_float`/`enums` index full-order, which is
        // why every recipe-driven operator (envelope/sequencer/sample) is shaped.
        let mut ev_addr: Vec<&'static str> = Vec::new();
        let mut ev_args: Vec<Args> = Vec::new();
        let mut store = None;
        match recipe {
            Recipe::Default => {}
            Recipe::Gate => set_high(&desc, &mut in_bufs, &mut varying, "gate"),
            Recipe::Clocked => set_clock(&desc, &mut in_bufs, &mut varying, "clock"),
            Recipe::Sample => {
                set_high(&desc, &mut in_bufs, &mut varying, "gate");
                set_const(&desc, &mut in_bufs, &mut varying, "freq", 440.0);
                store = Some(bind_synthetic_sample(&desc, op.as_mut()));
            }
            Recipe::Notes => push_event(&mut ev_addr, &mut ev_args, "note", &[60.0, 1.0]),
            Recipe::ChordSet => push_event(&mut ev_addr, &mut ev_args, "set", &[0.0, 4.0, 7.0]),
            Recipe::Position => push_event(&mut ev_addr, &mut ev_args, "position", &[60.0, 1.0]),
            Recipe::Value => push_event(&mut ev_addr, &mut ev_args, "in", &[0.5]),
        }

        Self {
            op,
            lanes,
            params: desc.default_params(),
            in_bufs,
            enums,
            contexts,
            varying,
            out_bufs,
            ev_addr,
            ev_args,
            emit: Vec::new(),
            ctx_pub: Vec::new(),
            outbound: Vec::new(),
            _store: store,
        }
    }

    /// Render the full fixed workload — [`BLOCKS`] blocks of one `process` call (lane 0). Events
    /// fire at block 0 only (the macro layer's note-on-then-tail shape). Accumulates a value that
    /// depends on every block's outputs + sink activity so the optimizer cannot elide the work; the
    /// sum is the bench's return value (the caller `black_box`es it under iai).
    pub fn render(self) -> f32 {
        let Self {
            mut op,
            lanes,
            params,
            in_bufs,
            enums,
            contexts,
            varying,
            mut out_bufs,
            ev_addr,
            ev_args,
            mut emit,
            mut ctx_pub,
            mut outbound,
            _store,
        } = self;

        let mut acc = 0.0f32;
        for b in 0..BLOCKS {
            emit.clear();
            ctx_pub.clear();
            outbound.clear();

            let events: Vec<Event> = if b == 0 {
                ev_addr
                    .iter()
                    .zip(&ev_args)
                    .map(|(addr, args)| Event {
                        addr,
                        args,
                        frame: 0,
                    })
                    .collect()
            } else {
                Vec::new()
            };

            let inputs = in_bufs.iter().map(|o| o.as_deref());
            let outputs = out_bufs.iter_mut().map(|v| v.as_mut_slice());

            let mut io = Io::new(SAMPLE_RATE, BLOCK_SIZE, inputs, outputs, &params, &events)
                .with_lane(0, lanes)
                .with_contexts(&contexts)
                .with_varying(&varying)
                .with_enums(&enums)
                .with_emit(&mut emit, 0)
                .with_context_publish(&mut ctx_pub, 0)
                .with_outbound(&mut outbound, 0);
            op.process(&mut io);
            drop(io); // releases the borrows of out_bufs + the three sinks taken above

            acc += out_bufs.first().map_or(0.0, |v| v[0]);
            acc += (emit.len() + ctx_pub.len() + outbound.len()) as f32;
        }
        acc
    }
}

/// The per-slot `Io` input slices for one operator, in the layout its `process` expects.
struct WiredInputs {
    in_bufs: Vec<Option<Vec<f32>>>,
    enums: Vec<usize>,
    contexts: Vec<Harmony>,
    varying: Vec<bool>,
}

/// Build the per-slot `Io` input slices for an operator, matching the engine's numbering: full
/// (declaration) order for shaped operators, per-kind order for legacy ones. Each `Float` input
/// gets a buffer filled with its descriptor default (or `0.0` for a bare signal input with no
/// default); other shapes get `None` (events/context/enum are delivered through their own slices).
fn build_inputs(desc: &Descriptor, shaped: bool) -> WiredInputs {
    let mut in_bufs = Vec::new();
    let mut enums = Vec::new();
    let mut contexts = Vec::new();
    let mut varying = Vec::new();

    if shaped {
        // One aligned slot per input, in declaration order.
        for p in &desc.inputs {
            match p.shape {
                Shape::Float => {
                    let def = p.meta.as_ref().map_or(0.0, |m| m.default);
                    in_bufs.push(Some(vec![def; BLOCK_SIZE]));
                }
                _ => in_bufs.push(None),
            }
            enums.push(p.enum_meta.as_ref().map_or(0, |e| e.default));
            contexts.push(Harmony::default());
            varying.push(false);
        }
    } else {
        // Legacy per-kind numbering: signal inputs only in `in_bufs`/`varying`, context inputs only
        // in `contexts`, no enums. Message inputs contribute nothing (events arrive separately).
        for p in &desc.inputs {
            match p.shape {
                Shape::Float => {
                    let def = p.meta.as_ref().map_or(0.0, |m| m.default);
                    in_bufs.push(Some(vec![def; BLOCK_SIZE]));
                    varying.push(false);
                }
                Shape::Harmony => contexts.push(Harmony::default()),
                _ => {}
            }
        }
    }
    WiredInputs {
        in_bufs,
        enums,
        contexts,
        varying,
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

/// Drive a named `Float` input with a constant value (overwrites the default-filled buffer) and
/// flag it `varying` so the operator takes its recompute path.
fn set_const(
    desc: &Descriptor,
    in_bufs: &mut [Option<Vec<f32>>],
    varying: &mut [bool],
    name: &str,
    value: f32,
) {
    let i = input_index(desc, name);
    if let Some(buf) = in_bufs[i].as_mut() {
        buf.fill(value);
    }
    varying[i] = true;
}

/// Hold a named gate-like input high (1.0) — a rising edge at block 0.
fn set_high(desc: &Descriptor, in_bufs: &mut [Option<Vec<f32>>], varying: &mut [bool], name: &str) {
    set_const(desc, in_bufs, varying, name, 1.0);
}

/// Drive a named clock input as a per-block square wave: high for the first half of the block, low
/// for the second. The last sample of one block (low) → first of the next (high) gives a rising
/// edge every block, with a falling edge mid-block.
fn set_clock(
    desc: &Descriptor,
    in_bufs: &mut [Option<Vec<f32>>],
    varying: &mut [bool],
    name: &str,
) {
    let i = input_index(desc, name);
    if let Some(buf) = in_bufs[i].as_mut() {
        let half = BLOCK_SIZE / 2;
        for (f, s) in buf.iter_mut().enumerate() {
            *s = if f < half { 1.0 } else { 0.0 };
        }
    }
    varying[i] = true;
}

/// Queue an event for block 0. `addr` is the node-local address the operator matches.
fn push_event(
    ev_addr: &mut Vec<&'static str>,
    ev_args: &mut Vec<Args>,
    addr: &'static str,
    args: &[f32],
) {
    ev_addr.push(addr);
    ev_args.push(args.iter().map(|&v| Arg::Float(v)).collect());
}

/// Build a synthetic decoded sample (a 1 s sine, longer than the workload so the read loop never
/// runs dry) and bind it to the operator's first resource slot. Returns the store to keep alive.
fn bind_synthetic_sample(desc: &Descriptor, op: &mut dyn Operator) -> Arc<ResourceStore> {
    let frames = BLOCKS * BLOCK_SIZE;
    let step = std::f32::consts::TAU * 220.0 / SAMPLE_RATE;
    let channel: Vec<f32> = (0..frames).map(|i| (i as f32 * step).sin()).collect();

    let mut store = ResourceStore::new();
    let id = store.insert(
        "bench-synthetic",
        SampleBuffer::new(vec![channel], SAMPLE_RATE),
    );
    let store = Arc::new(store);

    let slot = desc
        .resources
        .first()
        .expect("Sample recipe needs a resource slot")
        .name;
    let mut refs = ResolvedRefs::new();
    refs.set(slot, id);
    op.bind_resources(&store, &refs);
    store
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
        let benched: BTreeSet<&str> = WORKLOADS.iter().map(|w| w.kind).collect();
        assert_eq!(
            registered, benched,
            "WORKLOADS is out of sync with the operator registry — add a `w(\"<kind>\", …)` entry \
             in bench_support.rs for any new operator (or remove a stale one)"
        );
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
    /// bound, sinks attached) regardless of its shape/legacy numbering.
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
