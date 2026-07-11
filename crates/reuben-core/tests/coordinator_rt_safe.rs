//! RT-safety invariant (ADR-0046 §2, ADR-0012): the render side of the swap mailbox
//! pair — `take_install` and `post_retiree` — performs **zero** heap allocation and
//! **zero** frees. Both directions are pure atomic pointer exchanges; `Box::into_raw`/
//! `Box::from_raw` are pointer conversions, and the displaced payload is handed back to
//! the Coordinator to drop off-thread (deferred free, ADR-0009). The same window also
//! proves the ops take no locks by construction: the module's only synchronization is
//! the two `AtomicPtr`s (no `Mutex`/`Condvar`/syscall anywhere in `coordinator`), and a
//! blocking op could not complete 100k single-threaded round trips.
//!
//! Like `rt_safe.rs`, this file is its own test binary with a single test, so no
//! sibling test perturbs the process-global allocation counters.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use reuben_core::coordinator::swap_pair;

/// Number of `alloc`/`realloc` calls since process start.
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
/// Number of `dealloc` calls since process start.
static FREES: AtomicUsize = AtomicUsize::new(0);

/// System allocator that counts allocations, reallocations, and frees — the
/// `rt_safe.rs` `Counting` pattern, extended with a free counter because the mailbox
/// contract forbids the render side to *drop* a payload, not just to allocate one.
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

#[test]
fn render_side_drain_and_post_never_alloc_or_free() {
    /// Stand-in for the Engine: big enough that any accidental heap copy would be
    /// unmistakable in the counters.
    struct Payload {
        _state: [u64; 32],
    }

    // All allocation lives here, off the measured path (Swap's Instantiate side,
    // ADR-0009): the mailbox pair, the resident payload, and one incoming payload.
    let (mut coordinator, mut render) = swap_pair::<Payload>();
    let mut current = Box::new(Payload { _state: [0; 32] });
    coordinator
        .install(Box::new(Payload { _state: [1; 32] }))
        .expect("first install");

    // Sanity: prove the counting harness is live, so a zero below can't be vacuous.
    let probe_before = ALLOCS.load(Ordering::Relaxed);
    let probe = Box::new(0u64);
    assert!(
        ALLOCS.load(Ordering::Relaxed) > probe_before,
        "the counting allocator must observe an ordinary Box allocation"
    );
    drop(probe);

    // Measured window: 100k full swap cycles. The render side drains, pointer-swaps
    // its resident payload, and posts the retiree; the Coordinator reclaims and
    // recycles the same box into the next install. Steady state is heap-neutral on
    // BOTH sides — and the render-side ops in particular may not alloc or free.
    let allocs_before = ALLOCS.load(Ordering::Relaxed);
    let frees_before = FREES.load(Ordering::Relaxed);
    for _ in 0..100_000 {
        // Render side (the audio-callback ops under test).
        let next = render.take_install().expect("install is published");
        let displaced = std::mem::replace(&mut current, next);
        if render.post_retiree(displaced).is_err() {
            panic!("retire slot must be vacant under one-in-flight");
        }

        // Coordinator side: reclaim the retiree and recycle it as the next install.
        let retiree = coordinator.try_reclaim().expect("retiree is home");
        coordinator.install(retiree).expect("recycled install");
    }
    let allocs = ALLOCS.load(Ordering::Relaxed) - allocs_before;
    let frees = FREES.load(Ordering::Relaxed) - frees_before;

    assert_eq!(
        allocs, 0,
        "mailbox drain/post/reclaim/install allocated {allocs} time(s)"
    );
    assert_eq!(
        frees, 0,
        "mailbox drain/post/reclaim/install freed {frees} time(s) — payloads must \
         only ever be dropped by their off-thread owner, never inside the channel"
    );
}
