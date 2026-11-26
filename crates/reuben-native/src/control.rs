//! The control ingress into the render callback: one bounded ring pair per producer thread.
//!
//! A producer ([`ControlProducer`]) pushes each [`ControlBatch`] onto its **forward** ring; the
//! render callback ([`ControlDrain`]) pops it, applies it whole, and pushes the spent batch onto
//! that pair's **return** ring, so the batch — its `Vec`, addresses, args and any `Arg::Str` — is
//! dropped or reused on the producer's thread, never the callback's. Both rings of a pair are
//! `rtrb` SPSC rings allocated once at start; the callback never allocates, frees or locks.
//!
//! **The return ring never overflows.** Every batch a producer pushes stays charged to it until it
//! pops that batch back off the return ring, and it pushes only while fewer than
//! [`CONTROL_RING_BATCHES`] are charged. A batch is in the forward ring, in the callback's hand, or
//! in the return ring, so neither ring can ever hold more than that bound. The callback needs no
//! fallback for a full return ring.
//!
//! **When a producer can't push, it drops the batch and counts it** in
//! [`Diagnostics::control_batches_dropped_full`]. Nothing waits: the UDP thread keeps reading and
//! a `send` still answers.

use std::sync::Arc;

use rtrb::{Consumer, Producer, RingBuffer};

use crate::diagnostics::Diagnostics;
use crate::osc::ControlBatch;

/// Batches one producer may have outstanding: pushed and not yet returned to it.
///
/// A batch is one UDP datagram or one engine-channel `send`, so this is gestures per producer
/// the callback has not reached yet. 256 holds a controller stream of ~2700 gestures a second
/// across a 93 ms callback (4096 frames at 44.1 kHz) before anything drops; the ring itself is
/// `256 * size_of::<ControlBatch>()` bytes, twice.
pub const CONTROL_RING_BATCHES: usize = 256;

/// A batch the producer could not push: the pair already had [`CONTROL_RING_BATCHES`] outstanding.
/// It has been counted; the batch itself is handed back for the caller to drop or reuse.
#[derive(Debug)]
pub struct RingFull(pub ControlBatch);

/// A producer thread's end of one ring pair. It is `Send` but not shareable: several threads that
/// produce into one pair share it behind a lock only they take.
pub struct ControlProducer {
    forward: Producer<ControlBatch>,
    back: Consumer<ControlBatch>,
    /// Batches pushed and not yet popped back off `back`. It counts the one the callback is holding
    /// between its pop and its return push, which the two rings' own occupancies cannot see.
    outstanding: usize,
    /// One spent batch kept for [`batch`](Self::batch) to hand out again.
    spare: Option<ControlBatch>,
    /// The addresses of messages the callback marked [`dropped`](crate::osc::OscIn::dropped), from
    /// batches reclaimed and not yet taken by [`take_dropped`](Self::take_dropped).
    dropped: Vec<String>,
    diagnostics: Arc<Diagnostics>,
}

/// The render callback's end of one ring pair.
pub struct ControlRing {
    forward: Consumer<ControlBatch>,
    back: Producer<ControlBatch>,
    /// A batch popped and not applied, because the apply asked to come back to it: the next drain
    /// offers it first. Popped rather than peeked, so whatever the pop made visible (a swap
    /// committed before the batch was pushed) is visible to the apply that decides.
    held: Option<ControlBatch>,
}

/// A new ring pair: the producer's end, counting drops into `diagnostics`, and the callback's.
pub fn ring_pair(diagnostics: Arc<Diagnostics>) -> (ControlProducer, ControlRing) {
    let (forward_tx, forward_rx) = RingBuffer::new(CONTROL_RING_BATCHES);
    let (back_tx, back_rx) = RingBuffer::new(CONTROL_RING_BATCHES);
    (
        ControlProducer {
            forward: forward_tx,
            back: back_rx,
            outstanding: 0,
            spare: None,
            dropped: Vec::new(),
            diagnostics,
        },
        ControlRing {
            forward: forward_rx,
            back: back_tx,
            held: None,
        },
    )
}

impl ControlProducer {
    /// An empty batch to fill: a spent one the callback returned, when there is one, so its buffer
    /// is reused.
    pub fn batch(&mut self) -> ControlBatch {
        self.reclaim();
        self.spare.take().unwrap_or_default()
    }

    /// Push `batch` to the callback, or drop-and-count it when the pair is at
    /// [`CONTROL_RING_BATCHES`] outstanding.
    pub fn push(&mut self, batch: ControlBatch) -> Result<(), RingFull> {
        self.reclaim();
        if self.outstanding >= CONTROL_RING_BATCHES {
            self.diagnostics.record_control_batch_dropped_full();
            return Err(RingFull(batch));
        }
        // Cannot fail: `outstanding` bounds what the forward ring holds.
        self.forward
            .push(batch)
            .expect("an outstanding count under capacity leaves the forward ring room");
        self.outstanding += 1;
        Ok(())
    }

    /// Whether the callback's end is gone, so nothing pushed would ever be applied.
    pub fn is_closed(&self) -> bool {
        self.forward.is_abandoned()
    }

    /// Batches pushed and not yet returned.
    pub fn outstanding(&self) -> usize {
        self.outstanding
    }

    /// The addresses of the messages the callback dropped as compiled for a Plan no longer live,
    /// since the last take, sorted and each once. Reclaims first, so a batch the callback has
    /// returned is counted.
    pub fn take_dropped(&mut self) -> Vec<String> {
        self.reclaim();
        let mut dropped = std::mem::take(&mut self.dropped);
        dropped.sort_unstable();
        dropped.dedup();
        dropped
    }

    /// Pop every returned batch, keeping one cleared for reuse and dropping the rest here.
    fn reclaim(&mut self) {
        while let Ok(mut spent) = self.back.pop() {
            self.outstanding -= 1;
            for m in spent.iter_mut().filter(|m| m.dropped) {
                self.dropped.push(std::mem::take(&mut m.address));
            }
            if self.spare.is_none() {
                spent.clear();
                self.spare = Some(spent);
            }
        }
    }
}

impl ControlRing {
    /// Offer the held batch, then each batch waiting on the forward ring, to `apply`, and hand
    /// back each it took. Takes only what was waiting when it started, so a producer pushing as
    /// fast as it is drained cannot hold the callback here. A batch `apply` declines is held and
    /// the drain stops, so batches keep their order. Answers whether every batch offered was taken.
    fn drain(&mut self, apply: &mut impl FnMut(&mut ControlBatch) -> bool) -> bool {
        if let Some(mut batch) = self.held.take() {
            if !apply(&mut batch) {
                self.held = Some(batch);
                return false;
            }
            self.return_batch(batch);
        }
        for _ in 0..self.forward.slots() {
            let Ok(mut batch) = self.forward.pop() else {
                break;
            };
            if !apply(&mut batch) {
                self.held = Some(batch);
                return false;
            }
            self.return_batch(batch);
        }
        true
    }

    fn return_batch(&mut self, batch: ControlBatch) {
        let returned = self.back.push(batch);
        // Fatal in a debug build and in this crate's unit tests at any opt level; release binaries skip it.
        if cfg!(any(test, debug_assertions)) {
            assert!(
                returned.is_ok(),
                "the return ring overflowed: a producer exceeded CONTROL_RING_BATCHES outstanding"
            );
        }
    }
}

/// Every ring pair the render callback drains, fixed when the session starts.
pub struct ControlDrain {
    rings: Vec<ControlRing>,
    /// The ring the next drain starts from: the first that declined a batch last drain, else 0.
    first: usize,
}

impl ControlDrain {
    pub fn new(rings: Vec<ControlRing>) -> Self {
        Self { rings, first: 0 }
    }

    /// Apply every waiting batch, whole and in its producer's order, then return it to that
    /// producer. Rings are drained in the order they were given, unless one declined last drain.
    /// `apply` answers `false` to decline a batch: that ring stops there and offers the same batch
    /// first on the next drain.
    ///
    /// The next drain then starts at that ring, the rest following in order, so a batch declined
    /// for want of room in the Engine's queue meets the queue first once a block has emptied it,
    /// rather than behind a ring that refills it every callback. RT-safe: atomic loads and stores
    /// and moves of the batches, nothing else.
    pub fn drain(&mut self, mut apply: impl FnMut(&mut ControlBatch) -> bool) {
        let n = self.rings.len();
        let mut declined = None;
        for k in 0..n {
            let i = (self.first + k) % n;
            if !self.rings[i].drain(&mut apply) && declined.is_none() {
                declined = Some(i);
            }
        }
        self.first = declined.unwrap_or(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::osc::OscIn;
    use reuben_api::render::Arg;

    fn one(i: i32) -> ControlBatch {
        vec![OscIn::new("/a/b".to_string(), vec![Arg::I32(i)])]
    }

    /// The batch the callback holds between its pop and its return push is still charged to the
    /// producer. Judged by the two rings' occupancies alone, the producer would see room for one
    /// more here, and the return ring could then be asked to hold `CONTROL_RING_BATCHES + 1`.
    #[test]
    fn a_batch_in_the_callbacks_hand_still_counts_against_the_bound() {
        let (mut producer, mut ring) = ring_pair(Diagnostics::new());
        for i in 0..CONTROL_RING_BATCHES {
            producer.push(one(i as i32)).expect("under the bound");
        }
        let held = ring.forward.pop().expect("the callback pops one");
        assert_eq!(
            ring.forward.slots() + producer.back.slots(),
            CONTROL_RING_BATCHES - 1,
            "the rings alone see one free"
        );
        assert!(
            producer.push(one(-1)).is_err(),
            "but the producer must not push"
        );
        ring.back.push(held).expect("the held batch returns");
        let mut seen = 0;
        ring.drain(&mut |_| {
            seen += 1;
            true
        });
        assert_eq!(seen, CONTROL_RING_BATCHES - 1);
        assert_eq!(producer.back.slots(), CONTROL_RING_BATCHES);
    }

    #[test]
    fn a_full_pair_drops_the_batch_and_counts_it() {
        let diagnostics = Diagnostics::new();
        let (mut producer, mut ring) = ring_pair(Arc::clone(&diagnostics));
        for i in 0..CONTROL_RING_BATCHES {
            producer.push(one(i as i32)).expect("under the bound");
        }
        let RingFull(dropped) = producer.push(one(-1)).expect_err("the pair is full");
        assert_eq!(dropped, one(-1), "the refused batch is handed back");
        assert_eq!(diagnostics.snapshot().control_batches_dropped_full, 1);
        assert_eq!(diagnostics.snapshot().output_xruns, 0);

        let mut applied = Vec::new();
        ring.drain(&mut |b| {
            applied.push(b[0].args[0].clone());
            true
        });
        let expected: Vec<Arg> = (0..CONTROL_RING_BATCHES as i32).map(Arg::I32).collect();
        assert_eq!(
            applied, expected,
            "every pushed batch, in order, and not the dropped one"
        );
        producer
            .push(one(0))
            .expect("returned batches free the bound");
        assert_eq!(diagnostics.snapshot().control_batches_dropped_full, 1);
    }

    /// A producer pushing as fast as it can against a callback that drains a little at a time: the
    /// callback's return push (a `debug_assert!`, so a panic here) never meets a full ring.
    #[test]
    fn the_return_ring_never_overflows_against_a_slow_drain() {
        let diagnostics = Diagnostics::new();
        let (mut producer, ring) = ring_pair(Arc::clone(&diagnostics));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let callback_stop = Arc::clone(&stop);
        let callback = std::thread::spawn(move || {
            let mut drain = ControlDrain::new(vec![ring]);
            let mut applied = 0usize;
            while !callback_stop.load(std::sync::atomic::Ordering::Relaxed) {
                drain.drain(|_| {
                    applied += 1;
                    true
                });
                std::thread::sleep(std::time::Duration::from_micros(200));
            }
            drain.drain(|_| {
                applied += 1;
                true
            });
            applied
        });
        let started = std::time::Instant::now();
        let mut pushed = 0usize;
        while started.elapsed() < std::time::Duration::from_millis(300) {
            let mut batch = producer.batch();
            batch.extend(one(pushed as i32));
            if producer.push(batch).is_ok() {
                pushed += 1;
            }
            assert!(producer.outstanding() <= CONTROL_RING_BATCHES);
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let applied = callback
            .join()
            .expect("the callback never overflowed its return ring");
        assert_eq!(applied, pushed, "every pushed batch was applied");
        assert!(
            diagnostics.snapshot().control_batches_dropped_full > 0,
            "the producer outran the drain, so the bound was reached"
        );
    }

    /// What the callback marked dropped comes back with its batch, and the producer names each
    /// address once, sorted, and only once the batch is back.
    #[test]
    fn the_producer_names_the_messages_the_callback_marked_dropped() {
        let (mut producer, ring) = ring_pair(Diagnostics::new());
        let mut drain = ControlDrain::new(vec![ring]);
        let mut batch = producer.batch();
        batch.extend([
            OscIn::new("/z/in".to_string(), vec![Arg::I32(1)]),
            OscIn::new("/kept/in".to_string(), vec![Arg::I32(2)]),
            OscIn::new("/a/in".to_string(), vec![Arg::I32(3)]),
            OscIn::new("/z/in".to_string(), vec![Arg::I32(4)]),
        ]);
        producer.push(batch).unwrap();
        assert!(producer.take_dropped().is_empty(), "nothing is back yet");
        drain.drain(|batch| {
            for m in batch.iter_mut().filter(|m| m.address != "/kept/in") {
                m.dropped = true;
            }
            true
        });
        assert_eq!(producer.take_dropped(), vec!["/a/in", "/z/in"]);
        assert!(producer.take_dropped().is_empty(), "named once");
        assert!(
            producer.batch().iter().all(|m| !m.dropped),
            "the reused batch comes back cleared"
        );
    }

    /// A declined batch stays put, ahead of what came after it, until a drain takes it; it is
    /// still charged to its producer meanwhile.
    #[test]
    fn a_declined_batch_is_offered_first_on_the_next_drain() {
        let (mut producer, ring) = ring_pair(Diagnostics::new());
        let mut drain = ControlDrain::new(vec![ring]);
        producer.push(one(1)).unwrap();
        producer.push(one(2)).unwrap();
        let mut offered = Vec::new();
        drain.drain(|b| {
            offered.push(b[0].args[0].clone());
            false
        });
        assert_eq!(
            offered,
            vec![Arg::I32(1)],
            "the drain stops at the declined batch"
        );
        assert_eq!(producer.outstanding(), 2);
        let mut applied = Vec::new();
        drain.drain(|b| {
            applied.push(b[0].args[0].clone());
            true
        });
        assert_eq!(applied, vec![Arg::I32(1), Arg::I32(2)], "in order");
        let _ = producer.batch();
        assert_eq!(producer.outstanding(), 0);
    }

    #[test]
    fn a_returned_batch_is_reused_by_the_producer() {
        let (mut producer, ring) = ring_pair(Diagnostics::new());
        let mut drain = ControlDrain::new(vec![ring]);
        let mut batch = producer.batch();
        batch.extend(one(1));
        let buffer = batch.as_ptr();
        producer.push(batch).unwrap();
        drain.drain(|_| true);
        let reused = producer.batch();
        assert!(reused.is_empty());
        assert_eq!(
            reused.as_ptr(),
            buffer,
            "the spent batch's buffer comes back"
        );
        assert_eq!(producer.outstanding(), 0);
    }
}
