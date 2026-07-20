//! Single-slot atomic mailbox pair — the lock-free RT crossing a Swap rides.
//!
//! Two hand-rolled single-slot mailboxes on [`AtomicPtr`]: an **install slot** the
//! Coordinator fills and the render side drains, and a **retire slot** the render side
//! fills and the Coordinator drains. The payload is generic/opaque — the install bundle
//! (Engine + output map) is a later ticket; this is just the channel primitive.
//!
//! see rules: execution-runtime

use std::fmt;
use std::marker::PhantomData;
use std::ops::Deref;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Arc;

/// Create a connected mailbox pair: the Coordinator's end and the render side's end.
///
/// Allocates (two empty slots behind an [`Arc`]) — call it at Swap-machinery setup time,
/// off the audio thread, like all Instantiate-phase work.
pub fn swap_pair<T: Send>() -> (CoordinatorMailbox<T>, RenderMailbox<T>) {
    let shared = Arc::new(Shared {
        install: CacheLine(Slot::empty()),
        retire: CacheLine(Slot::empty()),
    });
    (
        CoordinatorMailbox {
            shared: Arc::clone(&shared),
            in_flight: false,
        },
        RenderMailbox { shared },
    )
}

/// Cache-line pad (CachePadded-style): raise the wrapped value's alignment to a full
/// 64-byte line and round its size up to match, so each padded field lands on its own
/// line.
///
/// The two slots and the `Arc` header's refcounts would otherwise share one line: a hot
/// `reclaim`/`try_reclaim` poll issuing a `swap` on the retire slot would then bounce
/// exclusive ownership of that line against the render callback's own atomics (the
/// install slot, the `Arc` strong count). Padding keeps the install slot, the retire
/// slot, and the refcount header each on a private line — the RT boundary.
#[repr(align(64))]
struct CacheLine<T>(T);

impl<T> Deref for CacheLine<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
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

    /// The fill half of the channel: publish `payload` into an empty slot.
    ///
    /// `Ok` when the slot was empty and now owns the payload; `Err` (handing the box
    /// back untouched) when the slot was already occupied — the caller's protocol gate
    /// is what normally keeps that from happening. `Release` on success publishes every
    /// write into the payload to whichever end later drains it; the failure path stores
    /// nothing and only needs `Relaxed`.
    #[inline]
    fn fill(&self, payload: Box<T>) -> Result<(), Box<T>> {
        let raw = Box::into_raw(payload);
        match self
            .ptr
            .compare_exchange(ptr::null_mut(), raw, Ordering::Release, Ordering::Relaxed)
        {
            Ok(_) => Ok(()),
            // SAFETY: the compare-exchange failed, so `raw` was never stored in the slot
            // — no other thread can observe or free it, and this end still solely owns
            // it. Reconstitute the very `Box` we destructured with `Box::into_raw` just
            // above (same pointer, same `T`, same allocator) and hand it back, so a
            // refused fill leaks nothing.
            Err(_) => Err(unsafe { Box::from_raw(raw) }),
        }
    }

    /// The drain half of the channel: take the payload if one is present, leaving the
    /// slot empty. `Acquire` pairs with the filling end's `Release`.
    #[inline]
    fn drain(&self) -> Option<Box<T>> {
        let raw = self.ptr.swap(ptr::null_mut(), Ordering::Acquire);
        if raw.is_null() {
            None
        } else {
            // SAFETY: the swap returned non-null, so the slot held a payload that this
            // end has now atomically taken (storing null hands the slot back empty, so
            // no other end can also take it). The pointer came from `Box::into_raw` in a
            // prior `fill` on the same `T`, so rebuilding the `Box` is sound; as the sole
            // owner, moving or dropping it cannot double-free.
            Some(unsafe { Box::from_raw(raw) })
        }
    }

    /// Whether the slot currently holds a payload — a plain `Acquire` load, no RMW.
    ///
    /// A polling drain peeks with this first so an *empty* poll never issues the
    /// exclusive-ownership `swap` that would steal the slot's cache line from the
    /// filling thread on every miss.
    #[inline]
    fn is_occupied(&self) -> bool {
        !self.ptr.load(Ordering::Acquire).is_null()
    }
}

impl<T> Drop for Slot<T> {
    /// Free a payload stranded in the slot at teardown (an install nobody drained, a
    /// retiree nobody reclaimed). Runs where the *last* endpoint drops.
    fn drop(&mut self) {
        // `&mut self`: both endpoints are gone, no atomics needed.
        let raw = *self.ptr.get_mut();
        if !raw.is_null() {
            // SAFETY: we hold `&mut self`, so both endpoints have been dropped and no
            // other thread can reach this slot; a non-null pointer is a payload from a
            // `fill` that was never drained. It came from `Box::into_raw` on this `T`, so
            // reconstituting and dropping the `Box` frees it exactly once.
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

/// The two slots shared by both ends, each padded onto its own cache line.
struct Shared<T> {
    install: CacheLine<Slot<T>>,
    retire: CacheLine<Slot<T>>,
}

/// The Coordinator's end: fills the install slot, drains the retire slot, and enforces
/// the one-swap-in-flight discipline.
pub struct CoordinatorMailbox<T: Send> {
    shared: Arc<Shared<T>>,
    in_flight: bool,
}

/// The render side's end: drains the install slot and posts the retiree back. Both
/// operations are pure atomic pointer exchanges — no alloc, no free, no locks.
///
/// **RT-safety requirement (drop off-thread).** Dropping a `RenderMailbox` runs the
/// slot destructors, and a slot still holding a payload *frees* it — a heap free, which
/// the audio thread may never do. A `RenderMailbox` must therefore not be
/// dropped on the render thread. In practice this is moot: the callback holds it for the
/// life of the stream and it is torn down only after the stream stops, off-thread, where
/// any stranded free is a Coordinator-side, non-RT act (deferred free).
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
    /// retiree, so the render side's post can never collide.
    pub fn install(&mut self, payload: Box<T>) -> Result<(), SwapInFlight<T>> {
        if self.in_flight {
            return Err(SwapInFlight { rejected: payload });
        }
        match self.shared.install.fill(payload) {
            Ok(()) => {
                self.in_flight = true;
                Ok(())
            }
            // Unreachable through this API (`&mut self` + the in-flight gate keep the
            // slot empty here), kept as defense: hand the payload back, leak nothing.
            Err(rejected) => Err(SwapInFlight { rejected }),
        }
    }

    /// Drain the retire slot: take back the retiree the render side displaced, if it
    /// has arrived. Non-blocking.
    ///
    /// `Acquire` pairs with the render side's `Release` post, so the retiree's final
    /// render-thread state is visible before the Coordinator drops it (deferred free).
    /// Peeks with an `Acquire` load first and only issues the `swap`
    /// when a retiree is actually present, so a hot poll loop does not ping-pong the
    /// retire slot's cache line against the render thread on every empty poll.
    pub fn try_reclaim(&mut self) -> Option<Box<T>> {
        if !self.shared.retire.is_occupied() {
            return None;
        }
        let retiree = self.shared.retire.drain();
        if retiree.is_some() {
            self.in_flight = false;
        }
        retiree
    }

    /// Drain the retire slot, polling until the retiree returns or the caller's
    /// deadline passes — the error is the actionable "audio isn't running" diagnosis
    /// rather than a wedged Coordinator.
    ///
    /// **The caller supplies the clock.** reuben-core is OS-free (no `std::time`, no
    /// sleeping), so the timeout is a `timed_out` predicate consulted after each empty
    /// poll. Embed both the deadline *and* the back-off in it — e.g. a native shell:
    ///
    /// ```no_run
    /// use std::time::{Duration, Instant};
    /// use reuben_core::coordinator::swap_pair;
    ///
    /// let (mut coordinator, _render) = swap_pair::<u32>();
    /// coordinator.install(Box::new(1)).expect("install");
    ///
    /// let deadline = Instant::now() + Duration::from_millis(500);
    /// let _retiree = coordinator.reclaim(|| {
    ///     std::thread::sleep(Duration::from_millis(1)); // back off between polls
    ///     Instant::now() >= deadline
    /// });
    /// ```
    ///
    /// Called with no swap in flight it returns [`ReclaimError::NotInFlight`] at once (a
    /// caller protocol bug — nothing can ever come back, so it must not spin the whole
    /// deadline and then report a timeout it did not have). A retiree already home wins
    /// over an already-expired deadline: the slot is checked before the clock. Timing
    /// out does not corrupt the swap — it stays in flight, and a later
    /// [`reclaim`](Self::reclaim) / [`try_reclaim`](Self::try_reclaim) completes it
    /// normally once the render side wakes up.
    pub fn reclaim(&mut self, mut timed_out: impl FnMut() -> bool) -> Result<Box<T>, ReclaimError> {
        debug_assert!(
            self.in_flight,
            "reclaim called with no swap in flight; an install must precede each reclaim"
        );
        if !self.in_flight {
            // Nothing is in flight, so no retiree can ever arrive: refuse immediately
            // with a distinct error instead of spinning the deadline and mislabelling a
            // protocol bug as "the engine isn't consuming swaps".
            return Err(ReclaimError::NotInFlight);
        }
        loop {
            if let Some(retiree) = self.try_reclaim() {
                return Ok(retiree);
            }
            if timed_out() {
                return Err(ReclaimError::TimedOut(SwapTimeout));
            }
            // If the caller's predicate doesn't sleep, at least be polite to the core.
            std::hint::spin_loop();
        }
    }
}

impl<T: Send> RenderMailbox<T> {
    /// Whether an install is waiting to be drained — a plain `Acquire` load, no RMW, no
    /// alloc/free/lock (RT-safe).
    ///
    /// The install slot ramp "sees the pending Engine in the install slot
    /// but does not consume it immediately": the render side peeks with this to *begin*
    /// the master-gain down-ramp, then drains with [`take_install`](Self::take_install)
    /// only when the ramp reaches zero. Peeking (a load) rather than draining (a `swap`)
    /// on the steady-state miss keeps the empty callback from stealing the install slot's
    /// cache line from the Coordinator on every poll — the same reason
    /// [`try_reclaim`](CoordinatorMailbox::try_reclaim) peeks the retire slot first.
    pub fn has_install(&self) -> bool {
        self.shared.install.is_occupied()
    }

    /// Drain the install slot (RT-safe: one atomic swap, no alloc/free/lock).
    ///
    /// `Acquire` pairs with the Coordinator's `Release` publish. `Box::from_raw` is a
    /// pointer conversion, not an allocation; the box frees only if the caller drops it
    /// — the render side must hand it back via [`post_retiree`](Self::post_retiree)
    /// (after transplanting into it), never drop it.
    pub fn take_install(&mut self) -> Option<Box<T>> {
        self.shared.install.drain()
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
        self.shared.retire.fill(retiree)
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

/// Why a blocking [`reclaim`](CoordinatorMailbox::reclaim) returned no retiree.
#[derive(Debug, PartialEq, Eq)]
pub enum ReclaimError {
    /// `reclaim` was called with no swap in flight — a caller protocol bug (an
    /// [`install`](CoordinatorMailbox::install) must precede each reclaim). Refused
    /// immediately, and kept distinct from [`TimedOut`](Self::TimedOut) so a protocol
    /// misuse can never wear the timeout's "is audio running?" message.
    NotInFlight,
    /// The render side never consumed the swap within the caller's deadline.
    TimedOut(SwapTimeout),
}

impl fmt::Display for ReclaimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReclaimError::NotInFlight => write!(
                f,
                "reclaim called with no swap in flight; install a payload before reclaiming"
            ),
            ReclaimError::TimedOut(timeout) => timeout.fmt(f),
        }
    }
}

impl std::error::Error for ReclaimError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ReclaimError::NotInFlight => None,
            ReclaimError::TimedOut(timeout) => Some(timeout),
        }
    }
}

/// The render side never consumed the swap within the caller's deadline.
#[derive(Debug, PartialEq, Eq)]
pub struct SwapTimeout;

impl fmt::Display for SwapTimeout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "engine isn't consuming swaps; is audio running?")
    }
}

impl std::error::Error for SwapTimeout {}
