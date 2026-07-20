//! Behavioral contract of the Coordinator ↔ Render mailbox pair:
//! exactly-once drain, one swap in flight, caller-clocked timeout, and cross-thread
//! delivery. RT-safety (zero alloc/free on the render side) is asserted separately in
//! `coordinator_rt_safe.rs`, which needs a private global allocator.

use reuben_core::coordinator::swap_pair;

#[test]
fn install_is_drained_exactly_once() {
    let (mut coordinator, mut render) = swap_pair::<u32>();

    // Nothing published yet: the render-side drain sees an empty slot.
    assert!(
        render.take_install().is_none(),
        "empty slot must drain None"
    );

    coordinator
        .install(Box::new(7))
        .expect("install into empty pair");

    // The one publish is drained exactly once...
    assert_eq!(render.take_install().as_deref(), Some(&7));
    // ...and the slot is empty again afterwards.
    assert!(
        render.take_install().is_none(),
        "a drained install must not be observable twice"
    );
}

#[test]
fn retiree_rides_the_retire_slot_back() {
    let (mut coordinator, mut render) = swap_pair::<&str>();

    // No swap has happened: nothing to reclaim.
    assert!(coordinator.try_reclaim().is_none());

    // A full round trip: install, drain, post the displaced payload back.
    coordinator
        .install(Box::new("new engine"))
        .expect("install");
    let incoming = render.take_install().expect("drain the install");
    render
        .post_retiree(Box::new("old engine"))
        .expect("retire slot is empty by the one-in-flight discipline");

    // Defensive branch: a second post before the Coordinator reclaims must be refused,
    // handing the payload straight back. The occupied retire slot is left untouched —
    // nothing is dropped or leaked — so the first retiree still rides home intact.
    let bounced = render
        .post_retiree(Box::new("double post"))
        .expect_err("retire slot is occupied: the second post must be refused");
    assert_eq!(
        *bounced, "double post",
        "the refused payload is handed back untouched"
    );

    drop(incoming);

    // The Coordinator gets exactly the retiree back — the *first* post's payload, proving
    // the refused double-post neither clobbered the slot nor was silently dropped. The
    // deferred free happens on its thread on reclaim, never on the render side.
    assert_eq!(coordinator.try_reclaim().as_deref(), Some(&"old engine"));
    assert!(
        coordinator.try_reclaim().is_none(),
        "a reclaimed retiree must not be observable twice"
    );
}

/// Never publish the next install until the prior retiree came back. The
/// swap stays "in flight" through all three intermediate states — published, drained,
/// and posted-but-unreclaimed — because only reclaim guarantees the retire slot is
/// vacant for the *next* swap's retiree.
#[test]
fn second_install_is_refused_until_the_retiree_is_reclaimed() {
    let (mut coordinator, mut render) = swap_pair::<u32>();

    coordinator.install(Box::new(1)).expect("first install");

    // Published, not yet drained: refused, payload handed back intact.
    let refused = coordinator
        .install(Box::new(2))
        .expect_err("undrained install: swap still in flight");
    assert_eq!(*refused.rejected, 2);
    assert_eq!(
        refused.to_string(),
        "previous swap is still in flight; reclaim its retiree before installing the next payload"
    );

    // Drained, retiree not yet posted: still refused.
    let _incoming = render.take_install().expect("drain first install");
    let refused = coordinator
        .install(refused.rejected)
        .expect_err("retiree not returned: swap still in flight");

    // Retiree posted but not yet reclaimed: still refused.
    render.post_retiree(Box::new(0)).expect("post retiree");
    let refused = coordinator
        .install(refused.rejected)
        .expect_err("retiree not reclaimed: swap still in flight");

    // Reclaim completes the swap; the second install is now accepted and delivered.
    assert_eq!(coordinator.try_reclaim().as_deref(), Some(&0));
    coordinator
        .install(refused.rejected)
        .expect("after reclaim the next install is accepted");
    assert_eq!(render.take_install().as_deref(), Some(&2));
}

/// Core is OS-free, so `reclaim` takes the clock from the caller: a `timed_out`
/// predicate polled between drain attempts. A consumer that never drains (audio not
/// running) trips the actionable error instead of wedging the Coordinator forever.
#[test]
fn never_draining_consumer_trips_the_timeout() {
    let (mut coordinator, mut render) = swap_pair::<u32>();
    coordinator.install(Box::new(1)).expect("install");

    // The caller's "clock" here expires after 32 polls.
    let mut polls = 0u32;
    let err = coordinator
        .reclaim(|| {
            polls += 1;
            polls > 32
        })
        .expect_err("nobody is draining: reclaim must time out");
    assert_eq!(
        err.to_string(),
        "engine isn't consuming swaps; is audio running?"
    );

    // Timing out reports; it does not corrupt the protocol. The swap is still in
    // flight (the slot still holds the payload), so the next install stays refused...
    let refused = coordinator
        .install(Box::new(2))
        .expect_err("timed-out swap is still in flight");

    // ...and a late-waking render side completes the swap normally.
    let _incoming = render.take_install().expect("late drain still delivers");
    render.post_retiree(Box::new(0)).expect("post retiree");

    // An already-returned retiree wins over an already-expired deadline: reclaim must
    // check the slot before consulting the clock.
    let retiree = coordinator
        .reclaim(|| true)
        .expect("retiree is home: no timeout");
    assert_eq!(*retiree, 0);
    coordinator
        .install(refused.rejected)
        .expect("swap completed: next install accepted");
}

/// The real crossing: a Coordinator thread swapping against a render thread
/// that only ever drains, exchanges pointers, and posts. Every displaced payload comes
/// back exactly once, in order — 10k rounds of the full protocol.
#[test]
fn swaps_cross_threads_exactly_once_in_order() {
    use std::time::{Duration, Instant};

    const ROUNDS: u64 = 10_000;
    const STOP: u64 = u64::MAX;

    let (mut coordinator, mut render) = swap_pair::<u64>();

    let renderer = std::thread::spawn(move || {
        // The callback's resident engine: a mailbox install always displaces one
        // (the first Swap's predecessor is the empty Plan).
        let mut current: Box<u64> = Box::new(0);
        loop {
            match render.take_install() {
                Some(next) => {
                    let stop = *next == STOP;
                    let displaced = std::mem::replace(&mut current, next);
                    render
                        .post_retiree(displaced)
                        .expect("retire slot is vacant by the one-in-flight discipline");
                    if stop {
                        break;
                    }
                }
                None => std::thread::yield_now(),
            }
        }
    });

    // Test-side clock (tests may use the OS; core may not).
    let deadline = || {
        let expires = Instant::now() + Duration::from_secs(30);
        move || Instant::now() >= expires
    };

    for i in 1..=ROUNDS {
        coordinator
            .install(Box::new(i))
            .expect("previous swap completed: install accepted");
        let retiree = coordinator
            .reclaim(deadline())
            .expect("render thread is draining");
        assert_eq!(
            *retiree,
            i - 1,
            "each displaced payload returns exactly once, in install order"
        );
    }

    coordinator.install(Box::new(STOP)).expect("stop install");
    let last = coordinator.reclaim(deadline()).expect("final retiree");
    assert_eq!(*last, ROUNDS);
    renderer.join().expect("render thread exits cleanly");
}

/// Tearing the pair down mid-swap must not leak: a payload still sitting in either
/// slot is freed when the endpoints drop. That free is why `RenderMailbox` requires
/// off-thread teardown (it happens after the stream stops, never on the render thread).
#[test]
fn occupied_slots_free_their_payload_on_teardown() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct Counted(Arc<AtomicUsize>);
    impl Drop for Counted {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    // (a) An install nobody ever drained.
    let drops = Arc::new(AtomicUsize::new(0));
    {
        let (mut coordinator, render) = swap_pair::<Counted>();
        coordinator
            .install(Box::new(Counted(Arc::clone(&drops))))
            .expect("install");
        drop(render);
        drop(coordinator);
    }
    assert_eq!(
        drops.load(Ordering::Relaxed),
        1,
        "an undrained install must be freed at teardown, not leaked"
    );

    // (b) A retiree nobody ever reclaimed.
    let drops = Arc::new(AtomicUsize::new(0));
    {
        let (mut coordinator, mut render) = swap_pair::<Counted>();
        coordinator
            .install(Box::new(Counted(Arc::clone(&drops))))
            .expect("install");
        let incoming = render.take_install().expect("drain");
        assert!(
            render
                .post_retiree(Box::new(Counted(Arc::clone(&drops))))
                .is_ok(),
            "post retiree into a vacant slot"
        );
        drop(incoming); // the drained payload, dropped by its new owner: +1
        drop(coordinator);
        drop(render);
    }
    assert_eq!(
        drops.load(Ordering::Relaxed),
        2,
        "an unreclaimed retiree must be freed at teardown, not leaked"
    );
}
