//! Shared diagnostics counter surface — the one place every counted degradation lands, output
//! and input alike.
//!
//! [`Diagnostics`] is designed to be bumped from an RT thread and read from an ordinary one: every
//! field is an [`AtomicU64`], every write a single `fetch_add`, and reads take a [`Snapshot`]
//! copy so a logger never holds a reference into the live struct.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Atomic counters for the conditions reuben must "know and say": an output
/// render that missed its deadline, and the input ring's empty-read and
/// full-write events. Shared via `Arc` between whichever RT thread(s) bump a counter and
/// whatever logs it.
///
/// All counters use `Ordering::Relaxed`: each is an independent running total with no ordering
/// relationship to any other memory a reader might inspect — a logger only ever wants "how many
/// so far," never a happens-before guarantee against another field or another thread's writes.
/// That is exactly what `Relaxed` guarantees and it is the cheapest ordering available, which
/// matters because [`Diagnostics::record_output_xrun`] can be called from the render callback.
#[derive(Debug, Default)]
pub struct Diagnostics {
    /// Output render callbacks that missed their real-time budget: the callback's
    /// own render + mapping work took longer than the audio time it was producing. The device
    /// still played *something* (its own underrun silence) — this only counts
    /// that the miss happened.
    pub output_xruns: AtomicU64,
    /// Input-ring underruns, counted in **frames**: the ring ran empty while the
    /// output callback was pulling logical input, and this many input frames were read as
    /// zeros instead (the empty→zeros policy). Silence delivered while *re*-priming
    /// after a dry spell is counted too (per callback, as its input-frame demand) — a
    /// glitching device's delivered silence is fully accounted for. Only the *initial*
    /// startup prefill is uncounted: silence before the ring has ever flowed is expected,
    /// not an underrun.
    pub input_ring_underruns: AtomicU64,
    /// Input-ring overruns, counted in **frames**: the *consumer-side* drop-oldest
    /// trim discarded this many of the oldest queued frames because ring fill crossed the
    /// high-water mark (the full→drop-oldest policy). Diagnosis: input is arriving
    /// faster than the drift servo's ±0.5% authority can absorb (a real rate mismatch), or an
    /// output stall left a backlog the trim re-anchored to the floor. The opposite failure —
    /// the output callback stalled outright — counts in
    /// [`Self::input_ring_producer_drops`] instead.
    pub input_ring_overruns: AtomicU64,
    /// Producer-side backstop drops, counted in **frames**: the ring was
    /// completely full when the input callback tried to commit, so the *incoming* (newest)
    /// frame was dropped — the only move a producer has in an SPSC ring. Diagnosis: the
    /// consumer (the output callback) has stalled outright and nothing is draining; distinct
    /// from [`Self::input_ring_overruns`], whose fix is rate/servo-side. Kept as a separate
    /// counter precisely so the two opposite diagnoses stay distinguishable.
    pub input_ring_producer_drops: AtomicU64,
}

impl Diagnostics {
    /// A fresh counter set, ready to share.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Count one output render deadline miss. RT-safe: a single atomic add, no allocation, no
    /// syscall, no lock.
    pub fn record_output_xrun(&self) {
        self.output_xruns.fetch_add(1, Ordering::Relaxed);
    }

    /// Count `frames` input frames read as zeros because the ring was empty.
    /// RT-safe: a single atomic add, no allocation, no syscall, no lock.
    pub fn record_input_ring_underrun_frames(&self, frames: u64) {
        self.input_ring_underruns
            .fetch_add(frames, Ordering::Relaxed);
    }

    /// Count `frames` *oldest* input frames dropped by the consumer-side high-water trim.
    /// RT-safe: a single atomic add, no allocation, no syscall, no lock.
    pub fn record_input_ring_overrun_frames(&self, frames: u64) {
        self.input_ring_overruns
            .fetch_add(frames, Ordering::Relaxed);
    }

    /// Count `frames` *incoming* input frames dropped by the producer because the ring was
    /// completely full (the stalled-consumer backstop). RT-safe: a single atomic add, no
    /// allocation, no syscall, no lock.
    pub fn record_input_ring_producer_drop_frames(&self, frames: u64) {
        self.input_ring_producer_drops
            .fetch_add(frames, Ordering::Relaxed);
    }

    /// A point-in-time copy of every counter, cheap enough to take on every logging tick.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            output_xruns: self.output_xruns.load(Ordering::Relaxed),
            input_ring_underruns: self.input_ring_underruns.load(Ordering::Relaxed),
            input_ring_overruns: self.input_ring_overruns.load(Ordering::Relaxed),
            input_ring_producer_drops: self.input_ring_producer_drops.load(Ordering::Relaxed),
        }
    }
}

/// A point-in-time read of every [`Diagnostics`] counter. `Copy`/`Eq` so a logger can hold the
/// last-logged value and diff against a fresh snapshot without touching the live atomics again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Snapshot {
    pub output_xruns: u64,
    pub input_ring_underruns: u64,
    pub input_ring_overruns: u64,
    pub input_ring_producer_drops: u64,
}

impl Snapshot {
    /// Whether any counter moved since `prior` — the gate periodic logging uses to stay quiet
    /// while everything is healthy.
    pub fn changed_since(&self, prior: &Snapshot) -> bool {
        self != prior
    }
}

/// Emit one snapshot to stderr. Shared wording for periodic and exit logging so both read the
/// same line format.
pub fn log_snapshot(s: &Snapshot) {
    eprintln!(
        "diagnostics: output_xruns={} input_ring_underruns={} input_ring_overruns={} \
         input_ring_producer_drops={}",
        s.output_xruns, s.input_ring_underruns, s.input_ring_overruns, s.input_ring_producer_drops
    );
}

/// Spawn a background thread that logs a [`Diagnostics`] snapshot every `interval`, but only
/// when something counted has changed since the last log — a healthy run stays silent instead
/// of spamming stderr. Not RT: this thread never touches the audio callback's control flow, it
/// only reads the shared atomics on a plain timed wait.
///
/// The thread runs until the returned [`PeriodicLogger`] is dropped, which wakes it at once rather
/// than after the rest of the current `interval`.
pub fn spawn_periodic_logger(diag: Arc<Diagnostics>, interval: Duration) -> PeriodicLogger {
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let handle = std::thread::Builder::new()
        .name("diagnostics-log".to_string())
        .spawn(move || {
            let mut prior = Snapshot::default();
            let mut next = Instant::now() + interval;
            while !thread_stop.load(Ordering::SeqCst) {
                // `park_timeout` may wake spuriously, so the cadence is kept by the deadline, not
                // by the park returning.
                let now = Instant::now();
                if now < next {
                    std::thread::park_timeout(next - now);
                    continue;
                }
                next = now + interval;
                let snapshot = diag.snapshot();
                if snapshot.changed_since(&prior) {
                    log_snapshot(&snapshot);
                    prior = snapshot;
                }
            }
        })
        .expect("spawn diagnostics-log thread");
    PeriodicLogger {
        stop,
        handle: Some(handle),
    }
}

/// The running [`spawn_periodic_logger`] thread. Dropping it stops and joins the thread.
#[must_use = "dropping the logger stops it"]
pub struct PeriodicLogger {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for PeriodicLogger {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            handle.thread().unpark();
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_diagnostics_are_all_zero() {
        let d = Diagnostics::new();
        assert_eq!(d.snapshot(), Snapshot::default());
    }

    #[test]
    fn record_output_xrun_increments_the_counter() {
        let d = Diagnostics::new();
        d.record_output_xrun();
        d.record_output_xrun();
        assert_eq!(d.snapshot().output_xruns, 2);
    }

    #[test]
    fn snapshot_is_a_copy_not_a_live_view() {
        let d = Diagnostics::new();
        let before = d.snapshot();
        d.record_output_xrun();
        assert_eq!(before.output_xruns, 0, "snapshot must not see later writes");
        assert_eq!(d.snapshot().output_xruns, 1);
    }

    #[test]
    fn changed_since_detects_a_moved_counter() {
        let a = Snapshot::default();
        let b = Snapshot {
            output_xruns: 1,
            ..Snapshot::default()
        };
        assert!(b.changed_since(&a));
        assert!(!a.changed_since(&a));
        let c = Snapshot {
            input_ring_underruns: 1,
            ..Snapshot::default()
        };
        let d = Snapshot {
            input_ring_overruns: 1,
            ..Snapshot::default()
        };
        let e = Snapshot {
            input_ring_producer_drops: 1,
            ..Snapshot::default()
        };
        assert!(c.changed_since(&a));
        assert!(d.changed_since(&a));
        assert!(e.changed_since(&a));
    }

    #[test]
    fn input_ring_counters_accumulate_frame_counts() {
        let d = Diagnostics::new();
        d.record_input_ring_underrun_frames(3);
        d.record_input_ring_underrun_frames(2);
        d.record_input_ring_overrun_frames(7);
        d.record_input_ring_producer_drop_frames(4);
        let s = d.snapshot();
        assert_eq!(s.input_ring_underruns, 5);
        assert_eq!(s.input_ring_overruns, 7);
        assert_eq!(s.input_ring_producer_drops, 4);
        assert_eq!(s.output_xruns, 0);
    }

    #[test]
    fn overruns_and_producer_drops_are_separate_diagnoses() {
        let d = Diagnostics::new();
        d.record_input_ring_overrun_frames(3);
        let s = d.snapshot();
        assert_eq!(s.input_ring_overruns, 3);
        assert_eq!(s.input_ring_producer_drops, 0);
    }
}
