//! Single-slot atomic mailbox pair — the lock-free RT crossing a Swap rides (ADR-0046 §2).
//!
//! Two hand-rolled single-slot mailboxes on [`AtomicPtr`]: an **install slot** the
//! Coordinator fills and the render side drains, and a **retire slot** the render side
//! fills and the Coordinator drains. The payload is generic/opaque — the install bundle
//! (Engine + output map) is a later ticket; this is just the channel primitive.

use std::fmt;
use std::marker::PhantomData;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Arc;

/// Create a connected mailbox pair: the Coordinator's end and the render side's end.
///
/// Allocates (two empty slots behind an [`Arc`]) — call it at Swap-machinery setup time,
/// off the audio thread, like all Instantiate-phase work (ADR-0009).
pub fn swap_pair<T: Send>() -> (CoordinatorMailbox<T>, RenderMailbox<T>) {
    let shared = Arc::new(Shared {
        install: Slot::empty(),
        retire: Slot::empty(),
    });
    (
        CoordinatorMailbox {
            shared: Arc::clone(&shared),
            in_flight: false,
        },
        RenderMailbox { shared },
    )
}

/// One single-slot mailbox: an [`AtomicPtr`] owning the boxed payload it holds.
struct Slot<T> {
    ptr: AtomicPtr<T>,
    /// `Slot` logically owns a `T` when occupied; the raw pointer erases that from the
    /// auto traits, so ownership is restated here and in the manual `Send`/`Sync` impls.
    _owns: PhantomData<*mut T>,
}

impl<T> Slot<T> {
    fn empty() -> Self {
        Slot {
            ptr: AtomicPtr::new(ptr::null_mut()),
            _owns: PhantomData,
        }
    }
}

impl<T> Drop for Slot<T> {
    /// Free a payload stranded in the slot at teardown (an install nobody drained, a
    /// retiree nobody reclaimed). Runs where the *last* endpoint drops — endpoint
    /// teardown is a Coordinator-side, non-RT act, matching deferred free (ADR-0009).
    fn drop(&mut self) {
        // `&mut self`: both endpoints are gone, no atomics needed.
        let raw = *self.ptr.get_mut();
        if !raw.is_null() {
            drop(unsafe { Box::from_raw(raw) });
        }
    }
}

// SAFETY: a `Slot` is a single-slot channel — it transfers *ownership* of a `T` between
// threads (fill on one, drain on the other), so `T: Send` is exactly the required
// bound. No `&T` is ever shared across threads through the slot (each end only ever
// exchanges whole pointers under `&mut self`), so `T: Sync` is not needed. The
// `PhantomData<*mut T>` above suppresses the auto impls (`AtomicPtr` alone would be
// unconditionally `Send + Sync`, unsound for owned payloads) so that these are the
// only source of thread-safety for the mailboxes.
unsafe impl<T: Send> Send for Slot<T> {}
// SAFETY: `&Slot` exposes only the atomic ops used by the two single-writer ends; see
// the `Send` justification for why crossing payloads only needs `T: Send`.
unsafe impl<T: Send> Sync for Slot<T> {}

/// The two slots shared by both ends.
struct Shared<T> {
    install: Slot<T>,
    retire: Slot<T>,
}

/// The Coordinator's end: fills the install slot, drains the retire slot, and enforces
/// the one-swap-in-flight discipline (ADR-0046 §2).
pub struct CoordinatorMailbox<T: Send> {
    shared: Arc<Shared<T>>,
    in_flight: bool,
}

/// The render side's end: drains the install slot and posts the retiree back. Both
/// operations are pure atomic pointer exchanges — no alloc, no free, no locks.
pub struct RenderMailbox<T: Send> {
    shared: Arc<Shared<T>>,
}

impl<T: Send> CoordinatorMailbox<T> {
    /// Publish the next payload for the render side to install.
    ///
    /// `Release` pairs with the render side's `Acquire` drain, making every write into
    /// the payload (the off-thread Instantiate) visible to the audio thread.
    ///
    /// Refused with [`SwapInFlight`] (payload handed back) while the previous swap is
    /// still in flight — published, drained, or posted but not yet reclaimed. Only a
    /// completed [`try_reclaim`](Self::try_reclaim) / [`reclaim`](Self::reclaim) opens
    /// the next install: that is what keeps the retire slot vacant for the next
    /// retiree, so the render side's post can never collide (ADR-0046 §2).
    pub fn install(&mut self, payload: Box<T>) -> Result<(), SwapInFlight<T>> {
        if self.in_flight {
            return Err(SwapInFlight { rejected: payload });
        }
        let raw = Box::into_raw(payload);
        match self.shared.install.ptr.compare_exchange(
            ptr::null_mut(),
            raw,
            Ordering::Release,
            Ordering::Relaxed,
        ) {
            Ok(_) => {
                self.in_flight = true;
                Ok(())
            }
            // Unreachable through this API (`&mut self` + the in-flight gate keep the
            // slot empty here), kept as defense: hand the payload back, leak nothing.
            Err(_) => Err(SwapInFlight {
                rejected: unsafe { Box::from_raw(raw) },
            }),
        }
    }

    /// Drain the retire slot: take back the retiree the render side displaced, if it
    /// has arrived. Non-blocking.
    ///
    /// `Acquire` pairs with the render side's `Release` post, so the retiree's final
    /// render-thread state is visible before the Coordinator drops it (deferred free,
    /// ADR-0009 reclaim).
    pub fn try_reclaim(&mut self) -> Option<Box<T>> {
        let raw = self
            .shared
            .retire
            .ptr
            .swap(ptr::null_mut(), Ordering::Acquire);
        if raw.is_null() {
            None
        } else {
            self.in_flight = false;
            Some(unsafe { Box::from_raw(raw) })
        }
    }

    /// Drain the retire slot, polling until the retiree returns or the caller's
    /// deadline passes — the error is the actionable "audio isn't running" diagnosis
    /// of ADR-0046 §2 rather than a wedged Coordinator.
    ///
    /// **The caller supplies the clock.** reuben-core is OS-free (no `std::time`, no
    /// sleeping), so the timeout is a `timed_out` predicate consulted after each empty
    /// poll. Embed both the deadline *and* the back-off in it — e.g. a native shell:
    ///
    /// ```ignore
    /// let deadline = Instant::now() + Duration::from_millis(500);
    /// coordinator.reclaim(|| {
    ///     std::thread::sleep(Duration::from_millis(1)); // back off between polls
    ///     Instant::now() >= deadline
    /// })
    /// ```
    ///
    /// A retiree already home wins over an already-expired deadline: the slot is
    /// checked before the clock. Timing out does not corrupt the swap — it stays in
    /// flight, and a later [`reclaim`](Self::reclaim) / [`try_reclaim`](Self::try_reclaim)
    /// completes it normally once the render side wakes up.
    pub fn reclaim(&mut self, mut timed_out: impl FnMut() -> bool) -> Result<Box<T>, SwapTimeout> {
        loop {
            if let Some(retiree) = self.try_reclaim() {
                return Ok(retiree);
            }
            if timed_out() {
                return Err(SwapTimeout);
            }
            // If the caller's predicate doesn't sleep, at least be polite to the core.
            std::hint::spin_loop();
        }
    }
}

impl<T: Send> RenderMailbox<T> {
    /// Drain the install slot (RT-safe: one atomic swap, no alloc/free/lock).
    ///
    /// `Acquire` pairs with the Coordinator's `Release` publish. `Box::from_raw` is a
    /// pointer conversion, not an allocation; the box frees only if the caller drops it
    /// — the render side must hand it back via [`post_retiree`](Self::post_retiree)
    /// (after transplanting into it), never drop it.
    pub fn take_install(&mut self) -> Option<Box<T>> {
        let raw = self
            .shared
            .install
            .ptr
            .swap(ptr::null_mut(), Ordering::Acquire);
        if raw.is_null() {
            None
        } else {
            Some(unsafe { Box::from_raw(raw) })
        }
    }

    /// Post the displaced payload back for off-thread reclaim (RT-safe: one atomic
    /// compare-exchange, no alloc/free/lock).
    ///
    /// `Release` pairs with the Coordinator's `Acquire` reclaim, publishing the render
    /// thread's final writes into the retiree before it is dropped off-thread.
    ///
    /// The one-in-flight discipline guarantees the retire slot is empty here. If a
    /// misbehaving caller posts twice anyway, the slot is left untouched and the
    /// retiree is handed back in `Err` — nothing is dropped or leaked on the render
    /// side; the caller must retry after the Coordinator reclaims.
    pub fn post_retiree(&mut self, retiree: Box<T>) -> Result<(), Box<T>> {
        let raw = Box::into_raw(retiree);
        match self.shared.retire.ptr.compare_exchange(
            ptr::null_mut(),
            raw,
            Ordering::Release,
            Ordering::Relaxed,
        ) {
            Ok(_) => Ok(()),
            Err(_) => Err(unsafe { Box::from_raw(raw) }),
        }
    }
}

/// `install` was refused because the previous swap's retiree has not been reclaimed yet.
pub struct SwapInFlight<T> {
    /// The rejected payload, handed back untouched so the caller can retry it.
    pub rejected: Box<T>,
}

// Manual `Debug`: the payload is opaque (an Engine is not `Debug`), so show only the
// protocol state, without demanding `T: Debug` from every embedder.
impl<T> fmt::Debug for SwapInFlight<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SwapInFlight").finish_non_exhaustive()
    }
}

impl<T> fmt::Display for SwapInFlight<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "previous swap is still in flight; reclaim its retiree before installing the next payload"
        )
    }
}

impl<T> std::error::Error for SwapInFlight<T> {}

/// The render side never consumed the swap within the caller's deadline.
#[derive(Debug, PartialEq, Eq)]
pub struct SwapTimeout;

impl fmt::Display for SwapTimeout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "engine isn't consuming swaps; is audio running?")
    }
}

impl std::error::Error for SwapTimeout {}
