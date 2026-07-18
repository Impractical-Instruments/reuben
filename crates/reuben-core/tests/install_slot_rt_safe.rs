//! RT-safety invariant for the install slot (ticket #321, ADR-0046 §7, ADR-0050 §2, ADR-0012):
//! a render callback driven through [`RenderSlot`] performs **zero** heap allocation and **zero**
//! frees — both at steady state (no swap pending) and across a full swap, where the callback drains
//! the install bundle, runs the master-gain ramp, box-transplants the survivors, and posts the
//! retiree. Every one of those is a pointer swap: the transplant is `mem::swap` over the operator
//! boxes (ADR-0046 §4), and the retiree is posted **in the same box** the install arrived in, so its
//! allocation is reused, never freed on the render thread (the only free is the Coordinator's
//! off-thread reclaim, outside every measured window here).
//!
//! Like `rt_safe.rs` / `coordinator_rt_safe.rs`, this file is its own single-test binary. Counting
//! is armed per-thread by the shared [`rt_alloc`] harness (ticket #344) — each measured window arms
//! this thread only around the ops under test — so an allocation on a libtest harness thread cannot
//! interleave into a window and perturb the count under parallel load. The full-swap window, its
//! `16 blocks > 2×ramp` sizing, and the live-probe/reclaim non-vacuity checks it shares with
//! `m2_swap_harness.rs` live in the shared [`swap_rt_safe`] helper.

mod rt_alloc;
mod swap_rt_safe;

use rt_alloc::{measure, Counting};
use swap_rt_safe::{assert_counter_is_live, assert_install_step_heap_neutral};

use reuben_core::coordinator::{Coordinator, RenderSlot};
use reuben_core::resources::MemoryResolver;
use reuben_core::{AudioConfig, Registry};

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
    // allocates — outside the measured window, ADR-0009). The RENDER SIDE then drains it, runs the
    // ramp, transplants the survivors, and posts the retiree across the fills the shared helper
    // measures below.
    let report = coord.swap_document(&doc, None);
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
