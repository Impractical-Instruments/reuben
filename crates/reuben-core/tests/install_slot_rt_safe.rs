//! RT-safety invariant for the install slot (ticket #321, ADR-0046 §7, ADR-0050 §2, ADR-0012):
//! a render callback driven through [`RenderSlot`] performs **zero** heap allocation and **zero**
//! frees — both at steady state (no swap pending) and across a full swap, where the callback drains
//! the install bundle, runs the master-gain ramp, box-transplants the survivors, and posts the
//! retiree. Every one of those is a pointer swap: the transplant is `mem::swap` over the operator
//! boxes (ADR-0046 §4), and the retiree is posted **in the same box** the install arrived in, so its
//! allocation is reused, never freed on the render thread (the only free is the Coordinator's
//! off-thread reclaim, outside every measured window here).
//!
//! Like `rt_safe.rs` / `coordinator_rt_safe.rs`, this file is its own single-test binary with a
//! process-global counting allocator, so no sibling test perturbs the counters. A live probe and a
//! post-window reclaim assertion keep each zero from being vacuous: the probe proves the counter is
//! live, and reclaiming a real retiree proves the slot genuinely performed the transplant+post
//! inside the measured window.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use reuben_core::coordinator::{Coordinator, RenderSlot};
use reuben_core::resources::MemoryResolver;
use reuben_core::{AudioConfig, Registry};

/// Allocations/reallocations since process start.
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
/// Frees since process start — the slot must not *drop* a bundle any more than allocate one.
static FREES: AtomicUsize = AtomicUsize::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        System.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        FREES.fetch_add(1, Ordering::Relaxed);
        System.dealloc(ptr, layout)
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        System.realloc(ptr, layout, new_size)
    }
}

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

    // Live probe: prove the counting allocator is observing, so every zero below is real.
    let before = ALLOCS.load(Ordering::Relaxed);
    let probe = Box::new([0u64; 8]);
    assert!(
        ALLOCS.load(Ordering::Relaxed) > before,
        "counting allocator must observe an ordinary Box allocation"
    );
    drop(probe);

    // (1) Steady state (no swap pending): the fast path is a bare Engine fill — no ramp, no
    // per-sample multiply, and provably no heap traffic.
    let a0 = ALLOCS.load(Ordering::Relaxed);
    let f0 = FREES.load(Ordering::Relaxed);
    for _ in 0..1_000 {
        slot.fill(&mut out);
    }
    let steady_allocs = ALLOCS.load(Ordering::Relaxed) - a0;
    let steady_frees = FREES.load(Ordering::Relaxed) - f0;
    assert_eq!(
        steady_allocs, 0,
        "steady-state fill allocated {steady_allocs} time(s)"
    );
    assert_eq!(
        steady_frees, 0,
        "steady-state fill freed {steady_frees} time(s)"
    );
    assert!(!slot.is_ramping(), "no swap ⇒ no ramp");

    // (2) A full swap. The Coordinator builds the new Engine + migration table off-thread (this
    // allocates — outside the measured window, ADR-0009). The RENDER SIDE then drains it, runs the
    // ramp, transplants the survivors, and posts the retiree across the fills below.
    let report = coord.swap_document(&doc, None);
    assert!(report.report.ok, "swap should install: {:?}", report.report);
    assert_eq!(
        report.diff.as_ref().unwrap().survived,
        2,
        "both nodes survive"
    );

    // Measured window: fills spanning the whole ramp (down → install-at-zero → up). 16 blocks =
    // 2048 frames > 2×ramp (≈960), so the transplant + post land inside the window.
    let mut block = vec![0.0f32; BLOCK * ch];
    let a1 = ALLOCS.load(Ordering::Relaxed);
    let f1 = FREES.load(Ordering::Relaxed);
    let mut saw_ramp = false;
    for _ in 0..16 {
        slot.fill(&mut block);
        saw_ramp |= slot.is_ramping();
    }
    let swap_allocs = ALLOCS.load(Ordering::Relaxed) - a1;
    let swap_frees = FREES.load(Ordering::Relaxed) - f1;
    assert_eq!(
        swap_allocs, 0,
        "install-slot swap callback allocated {swap_allocs} time(s) — drain/ramp/transplant/post \
         must be heap-neutral on the render thread"
    );
    assert_eq!(
        swap_frees, 0,
        "install-slot swap callback freed {swap_frees} time(s) — the retiree is posted in the same \
         box, only ever dropped by its off-thread owner"
    );

    // Non-vacuity: the ramp actually ran, completed, and the transplant genuinely happened —
    // otherwise no retiree would be sitting in the retire slot for the Coordinator to reclaim.
    assert!(
        saw_ramp,
        "the swap must have driven the ramp through the measured window"
    );
    assert!(!slot.is_ramping(), "the ramp completed inside the window");
    assert!(
        coord.try_reclaim().is_some(),
        "the slot posted a real retiree — the transplant/post happened inside the measured window"
    );
}
