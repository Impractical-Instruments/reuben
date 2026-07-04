//! Shared diagnostics counter surface (ADR-0038 §9, P6/#183).
//!
//! reuben's xrun policy is **fixed and observable, not configurable**: an output render
//! deadline miss plays the device's own silence and is counted; nothing about rendering
//! changes because of it. This module is the *one* place those counts live, so that P5's
//! input-ring underrun/overrun counters (#182) land as new fields here rather than a second,
//! parallel counter surface. The ADR asks for "periodic and/or exit" logging; this pass ships
//! periodic logging ([`spawn_periodic_logger`]) since `reuben play` has no clean shutdown path
//! today (it parks the main thread forever; Ctrl-C is an uncaught `SIGINT`) — wiring a
//! process-exit hook is a separate, deliberately out-of-scope concern for a diagnostics-only
//! pass. [`log_snapshot`] is exposed as a free function precisely so an exit hook can call it
//! later without any change to this module. An OSC diagnostic endpoint is explicitly a later
//! step (ADR-0038 §9), not built here.
//!
//! [`Diagnostics`] is designed to be bumped from an RT thread (the audio callback, and later
//! P5's input-ring producer/consumer) and read from an ordinary thread: every field is an
//! [`AtomicU64`], every write is a single `fetch_add`, and reads take a [`Snapshot`] copy so a
//! logger never holds a reference into the live struct.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Atomic counters for the conditions ADR-0038 §9 says reuben must "know and say": an output
/// render that missed its deadline, and (from P5 onward) the input ring's empty-read and
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
    /// Output render callbacks that missed their real-time budget (P6, #183): the callback's
    /// own render + mapping work took longer than the audio time it was producing. The device
    /// still played *something* (its own underrun silence, ADR-0038 §9) — this only counts
    /// that the miss happened.
    pub output_xruns: AtomicU64,
    // P5 (#182) adds input-ring counters here, e.g.:
    //   pub input_ring_underruns: AtomicU64,  // ring empty on a read -> zeros supplied
    //   pub input_ring_overruns: AtomicU64,    // ring full on a write -> oldest frame dropped
    // Add fields, not a second struct — `Snapshot`, `spawn_periodic_logger`, and
    // `log_snapshot` below all need to grow in step (each is a small, mechanical edit).
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

    /// A point-in-time copy of every counter, cheap enough to take on every logging tick.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            output_xruns: self.output_xruns.load(Ordering::Relaxed),
        }
    }
}

/// A point-in-time read of every [`Diagnostics`] counter. `Copy`/`Eq` so a logger can hold the
/// last-logged value and diff against a fresh snapshot without touching the live atomics again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Snapshot {
    pub output_xruns: u64,
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
    eprintln!("diagnostics: output_xruns={}", s.output_xruns);
}

/// Spawn a background thread that logs a [`Diagnostics`] snapshot every `interval`, but only
/// when something counted has changed since the last log — a healthy run stays silent instead
/// of spamming stderr. Not RT: this thread never touches the audio callback's control flow, it
/// only reads the shared atomics on a plain sleep loop.
///
/// The returned `JoinHandle` runs for the life of the process (the loop never exits); callers
/// keep it only to signal intent that the thread is deliberately detached-in-practice, matching
/// `play`'s other background threads (OSC-in/out).
pub fn spawn_periodic_logger(
    diag: Arc<Diagnostics>,
    interval: Duration,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut prior = Snapshot::default();
        loop {
            std::thread::sleep(interval);
            let now = diag.snapshot();
            if now.changed_since(&prior) {
                log_snapshot(&now);
                prior = now;
            }
        }
    })
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
        let b = Snapshot { output_xruns: 1 };
        assert!(b.changed_since(&a));
        assert!(!a.changed_since(&a));
    }

    #[test]
    fn shared_arc_reflects_writes_from_another_handle() {
        // The production shape: one Arc handed to the audio callback (writer) and another to
        // the logger thread (reader) — both must see the same counts.
        let d = Diagnostics::new();
        let writer = Arc::clone(&d);
        writer.record_output_xrun();
        assert_eq!(d.snapshot().output_xruns, 1);
    }
}
