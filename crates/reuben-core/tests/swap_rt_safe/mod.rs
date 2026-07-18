//! Shared callback-side install-step RT-safety skeleton for the two swap allocation tests
//! (`install_slot_rt_safe.rs` from #321, `m2_swap_harness.rs` from #324).
//!
//! Both post a full document swap to a production [`RenderSlot`], then fill the exact BLOCK-sized
//! buffers the audio callback fills across the master-gain ramp, and assert the callback-side
//! install step — drain the install bundle, run the ramp, box-transplant the survivors, post the
//! retiree — is **heap-neutral** on the render thread (ADR-0012: zero allocs, zero frees) and that
//! the measured window was **non-vacuous** (the ramp genuinely ran and completed, and a real
//! retiree came home for the Coordinator to reclaim). This module holds that skeleton, the
//! `16 blocks > 2×ramp` window constant, and the live-counter probe in one place.
//!
//! Counting is armed per-thread by [`crate::rt_alloc::measure`] (ticket #344), so a sibling test
//! allocating on another thread during the same wall-clock window can never perturb the result.
//! Every test binary that declares `mod swap_rt_safe;` must also declare `mod rt_alloc;`.
#![allow(dead_code)] // not every including binary calls every helper.

use reuben_core::coordinator::{Coordinator, RenderSlot};

use crate::rt_alloc::measure;

/// Block-fills spanning the whole ramp: `16 × 128 = 2048` frames comfortably exceeds the full
/// `2 × ramp_edge_frames()` (≈960) master-gain ramp, so a full down → install-at-zero → up ramp —
/// transplant + retiree post included — lands inside the measured window. Checked against the live
/// ramp width in [`assert_install_step_heap_neutral`], so the ">2×ramp" claim can't silently rot.
pub const RAMP_SPAN_BLOCKS: usize = 16;

/// Live-counter probe: an ordinary `Box` allocation inside a measured window must register, so the
/// zero-alloc assertions elsewhere cannot be vacuous — a dead counter would also read zero. Call
/// once before the measured swap window.
pub fn assert_counter_is_live() {
    let probe = measure(|| drop(Box::new([0u64; 8])));
    assert!(
        probe.allocs > 0,
        "the counting harness must observe an ordinary Box allocation"
    );
}

/// Measured window: fill [`RAMP_SPAN_BLOCKS`] `block`-frame buffers spanning the whole master-gain
/// ramp (down → install-at-zero → up), then assert the callback-side install step made **zero** heap
/// allocations and **zero** frees, and that the window was non-vacuous.
///
/// Call after a swap has been posted to `coord` (via [`Coordinator::swap_document`]) and `slot` has
/// been warmed to steady-state capacity off the measured path. The window is `RAMP_SPAN_BLOCKS ×
/// block` frames, asserted here to exceed the full `2 × ramp_edge_frames()` ramp so the
/// install-at-zero transplant + retiree post land strictly inside it. Consumes the retiree.
pub fn assert_install_step_heap_neutral(
    coord: &mut Coordinator,
    slot: &mut RenderSlot,
    block: usize,
) {
    let ch = slot.channels();
    let window = RAMP_SPAN_BLOCKS * block;
    let ramp = 2 * slot.ramp_edge_frames();
    assert!(
        window > ramp,
        "measured window ({window} frames) must exceed the full down+up ramp ({ramp} frames) so \
         the transplant + retiree post land inside it"
    );

    let mut buf = vec![0.0f32; block * ch];
    let mut saw_ramp = false;
    let counts = measure(|| {
        for _ in 0..RAMP_SPAN_BLOCKS {
            slot.fill(&mut buf);
            saw_ramp |= slot.is_ramping();
        }
    });

    assert_eq!(
        counts.allocs, 0,
        "the install step allocated {} time(s) — drain/ramp/transplant/post must be heap-neutral \
         on the render thread",
        counts.allocs
    );
    assert_eq!(
        counts.frees, 0,
        "the install step freed {} time(s) — the retiree is posted in the SAME box, only ever \
         dropped by its off-thread owner",
        counts.frees
    );

    // Non-vacuity: the ramp actually ran and completed inside the window, and a real retiree came
    // home — proving the transplant + post genuinely happened where we measured zero.
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
