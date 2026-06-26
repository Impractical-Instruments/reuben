//! Per-operator micro-benchmark bridge (#30, follow-up to #19 / ADR-0019; unified model, ADR-0030).
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
//! Faithful to the engine (ADR-0030): the harness wires the same per-input-port arrays the Render
//! loop builds — a held [`Arg`] **latch** (read via `io.last`), the sparse [`Event`] **streams**
//! (read via `io.stream`), the per-sample materialized **buffers** (read via `io.signal`), and the
//! `varying` hints — all in input-port declaration order, plus the single Lane-0 **emit** sink that
//! now subsumes the former harmony-publish / outbound sinks.
//!
//! Determinism (ADR-0001): every workload is a fixed function of constants — no clock, no entropy
//! (the one RNG operator, `noise`, is seeded) — so iai instruction counts are byte-stable.

use std::sync::Arc;

use crate::descriptor::{Descriptor, LaneRule, Port, PortType};
use crate::message::{Arg, Emit, Event};
use crate::operator::{Io, Operator};
use crate::registry::Registry;
use crate::resources::{ResolvedRefs, ResourceStore, SampleBuffer};
use crate::vocab::harmony::Harmony;
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
    "transpose",
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
    /// Per-input buffer, `Some` for each `Float` (materialized) and `Buffer` (audio) input — the
    /// dense block `io.signal` reads — and `None` for held / event ports. Input-port order, matching
    /// the engine's per-Lane wiring.
    in_bufs: Vec<Option<Vec<f32>>>,
    /// The held (ZOH) [`Arg`] per input port — the unified latch (ADR-0030) `io.last` reads. In
    /// input-port order; collapses the former Harmony / enum / param lanes.
    latched: Vec<Arg>,
    /// `varying` hint per input port, aligned with `latched` (false ⇒ a const-folding op may reuse
    /// cached coefficients).
    varying: Vec<bool>,
    /// One buffer per `Buffer` output port (signal-output ordinal order — the index `io.signal_mut`
    /// uses).
    out_bufs: Vec<Vec<f32>>,
    /// Events injected at block 0 only: `(input port, node-local address, payload)`. Borrowed into
    /// per-port [`Event`] streams each block.
    events: Vec<(usize, &'static str, Arg)>,
    /// Lane-0 emit sink (ADR-0030): every operator gets one so emit/publish/outbound operators
    /// exercise their sink path without per-recipe wiring. Cleared each block.
    emit: Vec<Emit>,
    /// Kept alive for the operator's lifetime — `bind_resources` clones the `Arc`.
    _store: Option<Arc<ResourceStore>>,
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

        // Per-input-port arrays, in declaration order — the layout the Render loop wires
        // (ADR-0030): a dense buffer for materialized/audio ports, a held Arg latch for every port,
        // a `varying` hint for every port.
        let mut in_bufs: Vec<Option<Vec<f32>>> = desc.inputs.iter().map(input_buffer).collect();
        let mut latched: Vec<Arg> = desc.inputs.iter().map(default_latch).collect();
        let mut varying: Vec<bool> = vec![false; desc.inputs.len()];

        let out_bufs: Vec<Vec<f32>> = desc
            .outputs
            .iter()
            .filter(|p| matches!(p.ty, PortType::Buffer))
            .map(|_| vec![0.0; BLOCK_SIZE])
            .collect();

        let lanes = match desc.lanes {
            LaneRule::FromParam(slot) => (desc.params[slot].default.round() as usize).max(1),
            LaneRule::Inherit => 1,
        };

        // Apply the recipe's input drives + events.
        let mut events: Vec<(usize, &'static str, Arg)> = Vec::new();
        let mut store = None;
        match recipe {
            Recipe::Default => {}
            Recipe::Gate => set_const(&desc, &mut in_bufs, &mut latched, &mut varying, "gate", 1.0),
            Recipe::Clocked => set_clock(&desc, &mut in_bufs, &mut varying, "clock"),
            Recipe::Sample => {
                set_const(&desc, &mut in_bufs, &mut latched, &mut varying, "gate", 1.0);
                set_const(
                    &desc,
                    &mut in_bufs,
                    &mut latched,
                    &mut varying,
                    "freq",
                    440.0,
                );
                store = Some(bind_synthetic_sample(&desc, op.as_mut()));
            }
            Recipe::Notes => push_note(
                &mut events,
                &desc,
                "notes",
                Note::new(Pitch::Absolute(60.0), 1.0),
            ),
            Recipe::ChordSet => {
                push_note(&mut events, &desc, "set", Note::new(Pitch::Degree(0), 1.0))
            }
            Recipe::Position => set_const(
                &desc,
                &mut in_bufs,
                &mut latched,
                &mut varying,
                "position",
                0.5,
            ),
            // `in` is a `Float` control on the dense transformers but a `Note` port on `osc_out`;
            // drive each in its own type (ADR-0030 split the formerly-uniform value event).
            Recipe::Value => {
                let i = input_index(&desc, "in");
                if matches!(desc.inputs[i].ty, PortType::F32) {
                    set_const(&desc, &mut in_bufs, &mut latched, &mut varying, "in", 0.5);
                } else {
                    push_note(
                        &mut events,
                        &desc,
                        "in",
                        Note::new(Pitch::Absolute(60.0), 1.0),
                    );
                }
            }
        }

        Self {
            op,
            lanes,
            in_bufs,
            latched,
            varying,
            out_bufs,
            events,
            emit: Vec::new(),
            _store: store,
        }
    }

    /// Render the full fixed workload — [`BLOCKS`] blocks of one `process` call (lane 0). Events
    /// fire at block 0 only (the macro layer's note-on-then-tail shape). Accumulates a value that
    /// depends on every block's outputs + emit activity so the optimizer cannot elide the work; the
    /// sum is the bench's return value (the caller `black_box`es it under iai).
    pub fn render(self) -> f32 {
        let Self {
            mut op,
            lanes,
            in_bufs,
            latched,
            varying,
            mut out_bufs,
            events,
            mut emit,
            _store,
        } = self;

        let n_inputs = latched.len();

        // Block-0 event streams, built once: one per-port [`Event`] vec borrowing the owned `events`
        // payloads, plus an empty set for every other block — so the per-block loop allocates
        // nothing for streams.
        let mut ev_per_port: Vec<Vec<Event>> = (0..n_inputs).map(|_| Vec::new()).collect();
        for (port, addr, arg) in &events {
            ev_per_port[*port].push(Event {
                address: addr,
                arg,
                frame: 0,
            });
        }
        let block0_streams: Vec<&[Event]> = ev_per_port.iter().map(|v| v.as_slice()).collect();
        let empty_streams: Vec<&[Event]> = vec![&[]; n_inputs];

        let mut acc = 0.0f32;
        for b in 0..BLOCKS {
            emit.clear();
            let streams = if b == 0 {
                &block0_streams
            } else {
                &empty_streams
            };

            let inputs = in_bufs.iter().map(|o| o.as_deref());
            let outputs = out_bufs.iter_mut().map(|v| v.as_mut_slice());

            let mut io = Io::new(SAMPLE_RATE, BLOCK_SIZE, inputs, outputs)
                .with_latched(&latched)
                .with_streams(streams)
                .with_varying(&varying)
                .with_lane(0, lanes)
                .with_emit(&mut emit, 0);
            op.process(&mut io);
            drop(io); // releases the borrows of out_bufs + the emit sink taken above

            acc += out_bufs.first().map_or(0.0, |v| v[0]);
            acc += emit.len() as f32;
        }
        acc
    }
}

/// The per-input dense buffer the engine would hand `process`: a `Float` control materialized to
/// its default, a `Buffer` audio input as silence (a wired source the recipe may overwrite), or
/// `None` for a held / event port (delivered through the latch / streams instead).
fn input_buffer(p: &Port) -> Option<Vec<f32>> {
    match p.ty {
        PortType::F32 => Some(vec![p.meta.as_ref().map_or(0.0, |m| m.default); BLOCK_SIZE]),
        PortType::Buffer => Some(vec![0.0; BLOCK_SIZE]),
        _ => None,
    }
}

/// The held (ZOH) [`Arg`] the engine would latch for a port at rest (ADR-0030): a scalar control's
/// default, a vocab enum's default variant, the default `Harmony`. Buffer / event / non-default
/// ports get a harmless `F32(0.0)` placeholder — `io.last` is never the read for those.
fn default_latch(p: &Port) -> Arg {
    match &p.ty {
        PortType::F32 => Arg::F32(p.meta.as_ref().map_or(0.0, |m| m.default)),
        PortType::Vocab {
            enum_meta: Some(e), ..
        } => e
            .resolve_arg(&Arg::I32(e.default as i32))
            .unwrap_or(Arg::I32(e.default as i32)),
        PortType::Vocab { name, .. } if *name == "Harmony" => Arg::Harmony(Harmony::default()),
        _ => Arg::F32(0.0),
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

/// Drive a named `Float`/`Buffer` input with a constant value (overwrites the default-filled
/// buffer), keep the latch in sync (for the const-fold / ZOH read path), and flag it `varying` so
/// the operator takes its recompute path.
fn set_const(
    desc: &Descriptor,
    in_bufs: &mut [Option<Vec<f32>>],
    latched: &mut [Arg],
    varying: &mut [bool],
    name: &str,
    value: f32,
) {
    let i = input_index(desc, name);
    if let Some(buf) = in_bufs[i].as_mut() {
        buf.fill(value);
    }
    if matches!(desc.inputs[i].ty, PortType::F32) {
        latched[i] = Arg::F32(value);
    }
    varying[i] = true;
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

/// Queue a `Note` event for block 0 on the named event input. `addr` is the node-local address the
/// engine carries (the port name); the payload rides a single [`Arg::Note`] (ADR-0030).
fn push_note(
    events: &mut Vec<(usize, &'static str, Arg)>,
    desc: &Descriptor,
    name: &str,
    note: Note,
) {
    let i = input_index(desc, name);
    events.push((i, desc.inputs[i].name, Arg::Note(note)));
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
