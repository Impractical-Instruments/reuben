//! RT-safety invariant: the render side of the swap mailbox
//! pair — `take_install` and `post_retiree` — performs **zero** heap allocation and
//! **zero** frees. Both directions are pure atomic pointer exchanges; `Box::into_raw`/
//! `Box::from_raw` are pointer conversions, and the displaced payload is handed back to
//! the Coordinator to drop off-thread (deferred free). The same window also
//! proves the ops take no locks by construction: the module's only synchronization is
//! the two `AtomicPtr`s (no `Mutex`/`Condvar`/syscall anywhere in `coordinator`), and a
//! blocking op could not complete 100k single-threaded round trips.
//!
//! Like `rt_safe.rs`, this file is its own test binary with a single test. Allocation
//! counting is armed per-thread by the shared [`rt_alloc`] harness, so the measured
//! window sees only what the render/coordinator ops themselves do on this thread — never
//! a stray allocation from a libtest harness thread interleaving under parallel load.

mod rt_alloc;

use rt_alloc::{measure, Counting};

use reuben_core::coordinator::swap_pair;

#[global_allocator]
static GLOBAL: Counting = Counting;

#[test]
fn render_side_drain_and_post_never_alloc_or_free() {
    /// Stand-in for the Engine: big enough that any accidental heap copy would be
    /// unmistakable in the counters.
    struct Payload {
        _state: [u64; 32],
    }

    // All allocation lives here, off the measured path (Swap's Instantiate side):
    // the mailbox pair, the resident payload, and one incoming payload.
    let (mut coordinator, mut render) = swap_pair::<Payload>();
    let mut current = Box::new(Payload { _state: [0; 32] });
    coordinator
        .install(Box::new(Payload { _state: [1; 32] }))
        .expect("first install");

    // Sanity: prove the counting harness is live, so a zero below can't be vacuous — an
    // ordinary Box allocation inside a measured window must register on the counter.
    let probe = measure(|| drop(Box::new(0u64)));
    assert!(
        probe.allocs > 0,
        "the counting allocator must observe an ordinary Box allocation"
    );

    // Measured window: 100k full swap cycles. The render side drains, pointer-swaps
    // its resident payload, and posts the retiree; the Coordinator reclaims and
    // recycles the same box into the next install. Steady state is heap-neutral on
    // BOTH sides — and the render-side ops in particular may not alloc or free.
    let counts = measure(|| {
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
    });

    assert_eq!(
        counts.allocs, 0,
        "mailbox drain/post/reclaim/install allocated {} time(s)",
        counts.allocs
    );
    assert_eq!(
        counts.frees, 0,
        "mailbox drain/post/reclaim/install freed {} time(s) — payloads must \
         only ever be dropped by their off-thread owner, never inside the channel",
        counts.frees
    );
}
