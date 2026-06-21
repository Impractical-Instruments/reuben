//! Realtime-safety invariant: once warmed up, `Renderer::render_block` performs **zero**
//! heap allocation — neither while sustaining a held note nor while delivering note
//! messages (events are zero-copy views). A process-global counting allocator makes any
//! allocation on the audio path observable.
//!
//! This file is its own test binary with a single test, so no sibling test runs
//! concurrently to perturb the global allocation counter.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use reuben_core::message::{Arg, Message};
use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::{load, AudioConfig, Registry};

const DEFAULT_JSON: &str = include_str!("../../../instruments/default.json");

/// Number of `alloc`/`realloc` calls since process start.
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

/// System allocator that counts allocations and reallocations (i.e. heap growth).
struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        System.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        System.realloc(ptr, layout, new_size)
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

#[test]
fn render_block_is_allocation_free_after_warmup() {
    let cfg = AudioConfig::new(48_000.0, 256);
    let graph = load(DEFAULT_JSON, &Registry::builtin()).expect("load default.json");
    let mut plan = Plan::instantiate(graph, cfg).expect("instantiate");
    let mut r = Renderer::new(&plan);
    let mut out = vec![0.0f32; cfg.block_size];

    // Build the note-on message up front — its address String allocates here, off the
    // measured render path.
    let note_on = [Message::new(
        "/voicer/note",
        [Arg::Float(60.0), Arg::Float(1.0)],
        0,
    )];

    // Warm up: deliver a note and render enough blocks to grow every internal scratch
    // buffer (routes, bounds, out_scratch, order) to its steady-state capacity.
    r.render_block(&mut plan, &note_on, &mut out);
    for _ in 0..16 {
        r.render_block(&mut plan, &[], &mut out);
    }

    // Steady state: a held note with no new messages must not allocate.
    let before = ALLOCS.load(Ordering::Relaxed);
    for _ in 0..1000 {
        r.render_block(&mut plan, &[], &mut out);
    }
    let held = ALLOCS.load(Ordering::Relaxed) - before;
    assert_eq!(held, 0, "held-note render allocated {held} time(s)");

    // Message-bearing blocks must also not allocate — events are zero-copy views onto the
    // caller's Messages, not cloned.
    let before = ALLOCS.load(Ordering::Relaxed);
    for _ in 0..100 {
        r.render_block(&mut plan, &note_on, &mut out);
    }
    let with_msgs = ALLOCS.load(Ordering::Relaxed) - before;
    assert_eq!(
        with_msgs, 0,
        "message-bearing render allocated {with_msgs} time(s)"
    );
}
