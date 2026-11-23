//! The retire queue: a fixed-capacity ring the render thread moves replaced heap-owning values
//! into, so their last reference is dropped off the render thread.
//!
//! The per-value counterpart of the retire slot in [`mailbox`](super::mailbox): that one carries a
//! whole retired Engine home, this one carries each latched value the render thread displaces —
//! a held input overwritten by a send or a forward, a seed or authored default overwritten in a
//! Plan's first block, a pipe's dedup baseline, a block's scratch copy. The render side holds a
//! [`RetireQueue`]; the off-thread side holds a [`RetireDrain`] and drops what it drains.
//!
//! Only values that own heap memory ride it ([`OwnsHeap`]); anything else is dropped where it is
//! replaced, because dropping it frees nothing. When the ring is full the value is dropped on the
//! render thread after all, and counted ([`RetireDrain::dropped_full`]).

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::message::Arg;

/// Whether dropping a value can free heap memory — the test for whether it is worth retiring.
pub trait OwnsHeap {
    fn owns_heap(&self) -> bool;
}

/// Create a connected retire queue holding at least `capacity` values, rounded up to a power of
/// two: the render side's end and the off-thread end.
///
/// Allocates the whole ring here, so call it off the render thread, at the same setup time as
/// [`swap_pair`](super::mailbox::swap_pair). Pushing never allocates.
pub fn retire_queue<T: Send>(capacity: usize) -> (RetireQueue<T>, RetireDrain<T>) {
    retire_queue_from(capacity, 0)
}

/// [`retire_queue`] with both counters starting at `start`, so a test can cross their wrap.
fn retire_queue_from<T: Send>(capacity: usize, start: usize) -> (RetireQueue<T>, RetireDrain<T>) {
    let capacity = capacity.max(1).next_power_of_two();
    let slots = (0..capacity)
        .map(|_| UnsafeCell::new(MaybeUninit::uninit()))
        .collect();
    let shared = Arc::new(Ring {
        slots,
        mask: capacity - 1,
        head: AtomicUsize::new(start),
        tail: AtomicUsize::new(start),
        pushing: AtomicBool::new(false),
        draining: AtomicBool::new(false),
        dropped_full: AtomicUsize::new(0),
    });
    (
        RetireQueue {
            ring: Arc::clone(&shared),
        },
        RetireDrain { ring: shared },
    )
}

/// The render side's end. Cloning it is an `Arc` bump: every Engine a Coordinator builds holds one,
/// and only the installed one pushes.
///
/// **RT-safety requirement (drop off-thread).** If this is the last handle, dropping it frees the
/// ring and every value still in it. The Coordinator keeps a [`RetireDrain`], so a handle dropped
/// with a retired Engine never is.
pub struct RetireQueue<T: Send = Arg> {
    ring: Arc<Ring<T>>,
}

impl<T: Send> Clone for RetireQueue<T> {
    fn clone(&self) -> Self {
        Self {
            ring: Arc::clone(&self.ring),
        }
    }
}

impl<T: Send + OwnsHeap> RetireQueue<T> {
    /// Hand `value` off the render thread if dropping it could free; otherwise drop it here.
    /// RT-safe: no allocation, no lock, no wait. A value the full ring refuses is dropped here and
    /// counted.
    #[inline]
    pub fn retire(&self, value: T) {
        if value.owns_heap() {
            self.push(value);
        }
    }
}

/// [`RetireQueue::retire`] through an optional queue: with none attached (an Engine no Coordinator
/// built) the value is dropped here.
#[inline]
pub(crate) fn retire_to<T: Send + OwnsHeap>(queue: Option<&RetireQueue<T>>, value: T) {
    match queue {
        Some(q) => q.retire(value),
        None => drop(value),
    }
}

impl<T: Send> RetireQueue<T> {
    /// Move `value` into the ring, or drop it here and count it when the ring is full.
    ///
    /// One producer at a time: a push that finds another in progress (two handles pushed from two
    /// threads at once, which an installed Engine never does) takes the full-ring path rather than
    /// wait.
    pub fn push(&self, value: T) {
        let ring = &*self.ring;
        if ring.pushing.swap(true, Ordering::Acquire) {
            ring.dropped_full.fetch_add(1, Ordering::Relaxed);
            drop(value);
            return;
        }
        let tail = ring.tail.load(Ordering::Relaxed);
        let head = ring.head.load(Ordering::Acquire);
        let full = tail.wrapping_sub(head) >= ring.slots.len();
        if !full {
            // SAFETY: `pushing` makes this the only writer. The capacity is a power of two dividing
            // 2^usize::BITS, so `tail & mask` names the same slot on both sides of the counters'
            // wrap, and the `capacity` counts before `tail` name every other slot. `tail - head <
            // capacity` then means the drainer has finished with this one (it advanced `head` past
            // it with `Release`, paired with the `Acquire` load above) and will not read it until
            // `tail` is published below.
            unsafe { (*ring.slots[tail & ring.mask].get()).write(value) };
            ring.tail.store(tail.wrapping_add(1), Ordering::Release);
        }
        ring.pushing.store(false, Ordering::Release);
        if full {
            ring.dropped_full.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// The off-thread end: drops what the render side retired.
pub struct RetireDrain<T: Send = Arg> {
    ring: Arc<Ring<T>>,
}

impl<T: Send> Clone for RetireDrain<T> {
    fn clone(&self) -> Self {
        Self {
            ring: Arc::clone(&self.ring),
        }
    }
}

impl<T: Send> core::fmt::Debug for RetireDrain<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RetireDrain")
            .field("len", &self.len())
            .field("capacity", &self.capacity())
            .field("dropped_full", &self.dropped_full())
            .finish()
    }
}

impl<T: Send> RetireDrain<T> {
    /// Drop every value waiting in the ring, here, and return how many. Not RT-safe: each drop may
    /// free. One drainer at a time: a drain that finds another in progress returns 0 at once.
    pub fn drain(&self) -> usize {
        let ring = &*self.ring;
        if ring.draining.swap(true, Ordering::Acquire) {
            return 0;
        }
        let mut drained = 0;
        let mut head = ring.head.load(Ordering::Relaxed);
        loop {
            let tail = ring.tail.load(Ordering::Acquire);
            if head == tail {
                break;
            }
            // SAFETY: `draining` makes this the only reader, and `head != tail` means the producer
            // published this slot (its `Release` store of `tail` pairs with the `Acquire` load
            // above). Reading moves the value out; advancing `head` hands the slot back.
            let value = unsafe { (*ring.slots[head & ring.mask].get()).assume_init_read() };
            head = head.wrapping_add(1);
            ring.head.store(head, Ordering::Release);
            drop(value);
            drained += 1;
        }
        ring.draining.store(false, Ordering::Release);
        drained
    }

    /// Values waiting to be drained.
    pub fn len(&self) -> usize {
        // `head` first: neither counter passes the other, so a `tail` read after it is never behind
        // it, whatever a concurrent drain does in between.
        let head = self.ring.head.load(Ordering::Acquire);
        self.ring.tail.load(Ordering::Acquire).wrapping_sub(head)
    }

    /// Whether nothing is waiting to be drained.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The most values the ring holds between two drains: the requested capacity rounded up to a
    /// power of two.
    pub fn capacity(&self) -> usize {
        self.ring.slots.len()
    }

    /// Values the render thread dropped itself because the ring was full, since the queue was made.
    pub fn dropped_full(&self) -> u64 {
        self.ring.dropped_full.load(Ordering::Relaxed) as u64
    }
}

/// The ring both ends share. `head` and `tail` count every value ever drained and pushed, wrapping,
/// so their difference is the fill and a slot index is a count masked to the capacity.
struct Ring<T> {
    slots: Box<[UnsafeCell<MaybeUninit<T>>]>,
    /// `slots.len() - 1`. A power-of-two length is what keeps the masked index continuous across
    /// the counters' wrap; a modulo by any other length would map two consecutive counts to one
    /// slot there.
    mask: usize,
    /// Advanced only by the drainer.
    head: AtomicUsize,
    /// Advanced only by the pusher.
    tail: AtomicUsize,
    pushing: AtomicBool,
    draining: AtomicBool,
    /// Pointer-width rather than 64-bit: a 32-bit bare-metal target has no 64-bit atomics.
    dropped_full: AtomicUsize,
}

impl<T> Drop for Ring<T> {
    /// Drop whatever was retired and never drained. Runs where the last handle drops.
    fn drop(&mut self) {
        let tail = *self.tail.get_mut();
        let mut head = *self.head.get_mut();
        while head != tail {
            // SAFETY: `&mut self` means no handle is left, and every slot from `head` to `tail` was
            // written by a push and not yet read by a drain.
            unsafe { self.slots[head & self.mask].get_mut().assume_init_drop() };
            head = head.wrapping_add(1);
        }
    }
}

// SAFETY: the ring moves owned `T`s from the pushing thread to the draining thread and never shares
// a `&T` between them, so `T: Send` is the whole requirement. Each slot is touched by one side at a
// time: the `pushing`/`draining` flags admit one writer and one reader, and `head`/`tail` hand each
// slot across with `Release`/`Acquire`.
unsafe impl<T: Send> Send for Ring<T> {}
// SAFETY: as for `Send`; `&Ring` exposes only the atomics and the flag-guarded slot accesses above.
unsafe impl<T: Send> Sync for Ring<T> {}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// Counts its own drops, so a test sees where and how often values were freed.
    struct Tracked(Arc<AtomicUsize>);

    impl Drop for Tracked {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    impl OwnsHeap for Tracked {
        fn owns_heap(&self) -> bool {
            true
        }
    }

    #[test]
    fn pushed_values_drop_at_the_drain_not_the_push() {
        let dropped = Arc::new(AtomicUsize::new(0));
        let (q, d) = retire_queue::<Tracked>(4);
        for _ in 0..3 {
            q.retire(Tracked(dropped.clone()));
        }
        assert_eq!(dropped.load(Ordering::Relaxed), 0);
        assert_eq!(d.len(), 3);
        assert_eq!(d.drain(), 3);
        assert_eq!(dropped.load(Ordering::Relaxed), 3);
        assert!(d.is_empty());
        assert_eq!(d.dropped_full(), 0);
    }

    #[test]
    fn a_full_ring_drops_at_the_push_and_counts_it() {
        let dropped = Arc::new(AtomicUsize::new(0));
        let (q, d) = retire_queue::<Tracked>(2);
        for _ in 0..5 {
            q.push(Tracked(dropped.clone()));
        }
        assert_eq!(
            dropped.load(Ordering::Relaxed),
            3,
            "the overflow, dropped at once"
        );
        assert_eq!(d.dropped_full(), 3);
        assert_eq!(d.drain(), 2);
        q.push(Tracked(dropped.clone()));
        assert_eq!(d.dropped_full(), 3, "a drained ring takes values again");
        assert_eq!(d.drain(), 1);
        assert_eq!(dropped.load(Ordering::Relaxed), 6);
    }

    #[test]
    fn the_ring_wraps_over_many_cycles() {
        let (q, d) = retire_queue::<Vec<u8>>(3);
        let mut drained = 0;
        for i in 0..1000 {
            q.push(alloc::vec![i as u8]);
            if i % 2 == 1 {
                drained += d.drain();
            }
        }
        drained += d.drain();
        assert_eq!(drained, 1000);
        assert_eq!(d.dropped_full(), 0);
    }

    #[test]
    fn the_capacity_rounds_up_to_a_power_of_two() {
        assert_eq!(retire_queue::<u8>(3).1.capacity(), 4);
        assert_eq!(retire_queue::<u8>(0).1.capacity(), 1);
        assert_eq!(retire_queue::<u8>(2048).1.capacity(), 2048);
    }

    /// Counters that cross `usize::MAX` keep every value in its own slot: each is dropped exactly
    /// once, at the drain, and the full check still holds there.
    #[test]
    fn the_counters_wrap_without_reusing_a_slot() {
        let dropped = Arc::new(AtomicUsize::new(0));
        let (q, d) = retire_queue_from::<Tracked>(3, usize::MAX - 5);
        let mut pushed = 0;
        for round in 0..6 {
            for _ in 0..4 {
                q.push(Tracked(dropped.clone()));
                pushed += 1;
            }
            assert_eq!(
                d.len(),
                4,
                "round {round}: the ring holds its full capacity"
            );
            q.push(Tracked(dropped.clone()));
            assert_eq!(
                d.dropped_full(),
                round + 1,
                "round {round}: the fifth is refused"
            );
            assert_eq!(d.drain(), 4);
            assert_eq!(
                dropped.load(Ordering::Relaxed),
                pushed + round as usize + 1,
                "round {round}: every value dropped exactly once"
            );
        }
    }

    #[test]
    fn values_left_in_the_ring_drop_with_the_last_handle() {
        let dropped = Arc::new(AtomicUsize::new(0));
        let (q, d) = retire_queue::<Tracked>(4);
        q.push(Tracked(dropped.clone()));
        q.push(Tracked(dropped.clone()));
        drop(q);
        assert_eq!(dropped.load(Ordering::Relaxed), 0);
        drop(d);
        assert_eq!(dropped.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn only_heap_owning_args_are_queued() {
        let (q, d) = retire_queue::<Arg>(4);
        q.retire(Arg::F32(1.0));
        q.retire(Arg::Enum(2));
        q.retire(Arg::Str("owned".into()));
        assert_eq!(d.drain(), 1);
    }
}
