//! RT-safety invariant for the install slot: a render callback driven through [`RenderSlot`]
//! performs **zero** heap allocation and **zero** frees — at steady state and across a full
//! swap (drain the install bundle, run the master-gain ramp, box-transplant the survivors, carry
//! their live input values, post the retiree). Every one of those is a pointer swap: the transplant is `mem::swap` over the
//! operator boxes, and the retiree is posted **in the same box** it arrived in, so its
//! allocation is reused, never freed on the render thread — the only free is the Coordinator's
//! off-thread reclaim, outside every measured window here.
//!
//! Counted by the shared [`rt_alloc`] harness; window sizing and live-probe/reclaim checks live
//! in the shared [`swap_rt_safe`] helper.

mod rt_alloc;
mod swap_rt_safe;

use rt_alloc::{measure, Counting};
use swap_rt_safe::{assert_counter_is_live, assert_install_step_heap_neutral};

use reuben_core::coordinator::RenderSlot;
use reuben_core::message::Arg;
use reuben_core::{AudioConfig, Registry};
use reuben_document::coordinator::Coordinator;
use reuben_document::resources::MemoryResolver;

#[global_allocator]
static GLOBAL: Counting = Counting;

const BLOCK: usize = 128;

fn cfg() -> AudioConfig {
    AudioConfig::new(48_000.0, BLOCK)
}

/// A simple envelope → output rig (no voicer/sample/hosted sub-plans), so a *freshly built* Engine
/// renders allocation-free from its very first block — the up-ramp renders the new Engine before any
/// warmup, and only a graph with no first-render pool growth keeps that window clean. The envelope's
/// held CV is the master output.
fn envelope_doc(env_addr: &str) -> String {
    format!(
        r#"{{ "format_version": 3, "instrument": "eg",
             "interface": {{ "outputs": {{ "out": {{ "from": "/out.audio" }} }} }},
             "nodes": [
               {{ "type": "envelope", "address": "{env_addr}",
                  "inputs": {{ "gate": 1.0, "attack": 0.5, "decay": 0.01,
                               "sustain": 0.8, "release": 0.5 }} }},
               {{ "type": "output", "address": "/out",
                  "inputs": {{ "audio": {{ "from": "{env_addr}.cv" }} }} }} ] }}"#
    )
}

#[test]
fn install_slot_callback_never_allocates_or_frees() {
    let doc = envelope_doc("/env");
    let (mut coord, side, _w) = Coordinator::install_initial(
        &doc,
        Registry::builtin(),
        Box::new(MemoryResolver::new()),
        cfg(),
    )
    .expect("initial install");
    let mut slot = RenderSlot::new(side);
    let ch = slot.channels();

    // Warm the resident Engine to steady-state capacity (off the measured path), exactly as
    // `rt_safe.rs` does before measuring — grow every internal scratch buffer.
    let mut out = vec![0.0f32; 64 * BLOCK * ch];
    for _ in 0..8 {
        slot.fill(&mut out);
    }

    // Live probe: prove the counting harness is observing, so every zero below is real.
    assert_counter_is_live();

    // (1) Steady state (no swap pending): the fast path is a bare Engine fill — no ramp, no
    // per-sample multiply, and provably no heap traffic.
    let steady = measure(|| {
        for _ in 0..1_000 {
            slot.fill(&mut out);
        }
    });
    assert_eq!(
        steady.allocs, 0,
        "steady-state fill allocated {} time(s)",
        steady.allocs
    );
    assert_eq!(
        steady.frees, 0,
        "steady-state fill freed {} time(s)",
        steady.frees
    );
    assert!(!slot.is_ramping(), "no swap ⇒ no ramp");

    // (2) A full swap. The Coordinator builds the new Engine + migration table off-thread (this
    // allocates — outside the measured window). The RENDER SIDE then drains it, runs the
    // ramp, transplants the survivors, and posts the retiree across the fills the shared helper
    // measures below.
    let report = coord.swap_document(&doc);
    assert!(report.report.ok, "swap should install: {:?}", report.report);
    assert_eq!(
        report.diff.as_ref().unwrap().survived,
        2,
        "both nodes survive"
    );

    // Measured window: fills spanning the whole ramp (down → install-at-zero → up), asserting the
    // drain/ramp/transplant/post is heap-neutral and that the window was non-vacuous.
    assert_install_step_heap_neutral(&mut coord, &mut slot, BLOCK);
}

/// `level` (a Signal pipe) feeds `/m.a`, and `/m2.a` too when `fan_out`, so it renders as a node of
/// its own or dissolves into `/m.a`; `k` (a held pipe) feeds `/v.a`.
fn piped_doc(b: f32, fan_out: bool) -> String {
    let second = if fan_out {
        r#", { "type": "mul_f32_signal", "address": "/m2",
               "inputs": { "a": { "from": "/level" }, "b": 1.0 } }"#
    } else {
        ""
    };
    format!(
        r#"{{ "format_version": 3, "instrument": "p",
             "interface": {{
               "inputs": {{ "level": {{ "type": "f32_buffer", "default": 0.3 }},
                            "k": {{ "type": "f32", "default": 0.5 }} }},
               "outputs": {{ "out": {{ "from": "/out.audio" }} }} }},
             "nodes": [
               {{ "type": "mul_f32_signal", "address": "/m",
                  "inputs": {{ "a": {{ "from": "/level" }}, "b": {b} }} }},
               {{ "type": "mul_f32_value", "address": "/v",
                  "inputs": {{ "a": {{ "from": "/k" }}, "b": 1.0 }} }},
               {{ "type": "output", "address": "/out",
                  "inputs": {{ "audio": {{ "from": "/m.out" }} }} }} {second} ] }}"#
    )
}

#[test]
fn carrying_live_values_across_a_swap_never_allocates_or_frees() {
    let (kept, dissolved) = (piped_doc(1.0, true), piped_doc(2.0, false));
    let (mut coord, side, _w) = Coordinator::install_initial(
        &kept,
        Registry::builtin(),
        Box::new(MemoryResolver::new()),
        cfg(),
    )
    .expect("initial install");
    let mut slot = RenderSlot::new(side);
    let ch = slot.channels();
    let mut out = vec![0.0f32; 64 * BLOCK * ch];
    slot.queue_osc("/level/in", &[Arg::F32(0.7)]);
    slot.queue_osc("/k/in", &[Arg::F32(0.9)]);
    for _ in 0..8 {
        slot.fill(&mut out);
    }
    assert_counter_is_live();

    let mut block = vec![0.0f32; BLOCK * ch];
    for next in [&dissolved, &kept, &dissolved] {
        let report = coord.swap_document(next);
        assert!(report.report.ok, "swap should install: {:?}", report.report);
        assert_install_step_heap_neutral(&mut coord, &mut slot, BLOCK);
        // The first blocks after the install render the carried values.
        let after = measure(|| {
            for _ in 0..4 {
                slot.fill(&mut block);
            }
        });
        assert_eq!((after.allocs, after.frees), (0, 0), "post-install blocks");
    }
    slot.fill(&mut block);
    let peak = block.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
    assert!(
        (peak - 1.4).abs() < 1e-4,
        "level=0.7 carried through: peak {peak}"
    );
}
