//! Shared diagnostics counter surface — the one place every counted degradation lands, output
//! and input alike.
//!
//! [`Diagnostics`] is designed to be bumped from an RT thread and read from an ordinary one: every
//! field is an [`AtomicU64`], every write a single `fetch_add`, and reads take a [`Snapshot`]
//! copy so a logger never holds a reference into the live struct.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use reuben_api::engine::RetireDrain;
use reuben_api::render::OperatorCounts;
use std::time::{Duration, Instant};

/// Atomic counters for the conditions reuben must "know and say": an output
/// render that missed its deadline, and the input ring's empty-read and
/// full-write events. Shared via `Arc` between whichever RT thread(s) bump a counter and
/// whatever logs it.
///
/// Every counter but `stale_sends_dropped` uses `Ordering::Relaxed`: each is an independent running
/// total with no ordering relationship to any other memory a reader might inspect — a logger only
/// ever wants "how many so far," never a happens-before guarantee against another field or another
/// thread's writes.
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
    /// Control batches a producer dropped, counted in **batches**: its ring pair into the render
    /// callback already held [`CONTROL_RING_BATCHES`](crate::control::CONTROL_RING_BATCHES) it had
    /// not had back. One batch is one UDP datagram or one engine-channel `send`, lost whole.
    /// Diagnosis: control is arriving faster than the callback runs, or the callback has stopped.
    pub control_batches_dropped_full: AtomicU64,
    /// Inbound control messages the engine dropped because its per-block queue was full, counted
    /// in **messages**. The callback's drain waits for room before it queues a batch, so only a
    /// single batch larger than the queue (a UDP bundle of hundreds of messages) reaches this.
    /// Diagnosis: split the bundle.
    pub control_messages_dropped_full: AtomicU64,
    /// Inbound control messages the engine dropped, counted in **messages**: each addressed an
    /// input a wire or an interface pipe feeds, whose value is its feeder's. Diagnosis: a
    /// controller writes a node input it should reach through the interface input feeding it.
    pub wired_writes_dropped: AtomicU64,
    /// Strings the engine dropped at a `str` input that compiles its text, counted in
    /// **messages**: raw text, or a value another preparer built. The engine channel's `send`
    /// compiles a string before it is queued; OSC over UDP carries raw text, so a string sent that
    /// way counts here. Diagnosis: send it through the engine channel instead.
    pub unprepared_writes_dropped: AtomicU64,
    /// Inbound control messages the engine dropped, counted in **messages**, because their
    /// address reaches no input of the playing Plan. Diagnosis: a mistyped address over OSC, or an
    /// instrument that lacks it; over the engine channel, a `send` that raced a swap removing it.
    pub unaddressed_messages_dropped: AtomicU64,
    /// Inbound control messages the engine dropped, counted in **messages**, because their args do
    /// not fit the input they address. Diagnosis: a controller sends the wrong kind of value.
    pub unfit_args_dropped: AtomicU64,
    /// Messages of `send` batches whose strings were compiled for one Plan, dropped whole by the
    /// callback because a swap had installed another before it drained them, counted in
    /// **messages**. Their addresses are named to the engine channel's subscribers, so a client
    /// can resend. Diagnosis: a send raced an edit or an open; resend it.
    ///
    /// Written with `Release` once the callback has handed back every batch it drained, and read
    /// with `Acquire` by whatever names the drops, so a reader that sees the count move finds
    /// those batches back at their producer.
    pub stale_sends_dropped: AtomicU64,
    /// `pattern` cycle queries that returned only part of their cycle's notes, counted in
    /// **queries**. Diagnosis: a pattern plays more notes per cycle than the Operator holds; thin
    /// it, or split it across more cycles.
    pub pattern_queries_truncated: AtomicU64,
    /// Pattern atoms a `pattern` Operator's `mode` cannot read, skipped, counted in **atoms**.
    /// Diagnosis: a note name sent to a pattern outside `Midi` mode, or a fractional degree.
    pub pattern_atoms_skipped: AtomicU64,
    /// Sounding pattern notes released early to make room for a new one, counted in **notes**.
    /// Diagnosis: a pattern holds more overlapping notes than the Operator tracks.
    pub pattern_notes_evicted: AtomicU64,
    /// MIDI messages read from a device or injected, realtime and system messages excepted.
    pub midi_received: AtomicU64,
    /// Received MIDI messages of a kind no MIDI map can bind (polyphonic aftertouch) or not
    /// well-formed.
    pub midi_unsupported: AtomicU64,
    /// Received MIDI messages that matched at least one control of the MIDI map, including a
    /// note-off sent nowhere because the note it would stop was already released.
    pub midi_routed: AtomicU64,
    /// Received MIDI messages no control matched. Diagnosis: the played instrument has no MIDI
    /// map, or the map's patterns name other channels or controllers than the device sends.
    pub midi_unmatched: AtomicU64,
    /// MIDI input ports that went away while `play` ran (unplugged). Each one's held notes were
    /// released; restart `play` to read it again.
    pub midi_devices_lost: AtomicU64,
    /// The Coordinator's retire queue, once [`watch_retire_queue`](Self::watch_retire_queue) is
    /// given it: its overflow count is the engine's, read here so the log line carries it too.
    retired: OnceLock<RetireDrain>,
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

    /// Count one control batch a producer dropped against a full ring pair. A single atomic add.
    pub fn record_control_batch_dropped_full(&self) {
        self.control_batches_dropped_full
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Count `n` control messages the engine dropped against its full queue. A single atomic add,
    /// skipped for zero.
    pub fn record_control_messages_dropped_full(&self, n: u64) {
        if n > 0 {
            self.control_messages_dropped_full
                .fetch_add(n, Ordering::Relaxed);
        }
    }

    /// Count `n` control messages the engine dropped for addressing a fed input. A single atomic
    /// add, skipped for zero.
    pub fn record_wired_writes_dropped(&self, n: u64) {
        if n > 0 {
            self.wired_writes_dropped.fetch_add(n, Ordering::Relaxed);
        }
    }

    /// Count `n` control messages the engine dropped for addressing no input. A single atomic
    /// add, skipped for zero.
    pub fn record_unaddressed_messages_dropped(&self, n: u64) {
        if n > 0 {
            self.unaddressed_messages_dropped
                .fetch_add(n, Ordering::Relaxed);
        }
    }

    /// Count `n` control messages the engine dropped for args that do not fit their input. A
    /// single atomic add, skipped for zero.
    pub fn record_unfit_args_dropped(&self, n: u64) {
        if n > 0 {
            self.unfit_args_dropped.fetch_add(n, Ordering::Relaxed);
        }
    }

    /// Count `n` strings the engine dropped at a `str` input for not being prepared for it. A
    /// single atomic add, skipped for zero.
    pub fn record_unprepared_writes_dropped(&self, n: u64) {
        if n > 0 {
            self.unprepared_writes_dropped
                .fetch_add(n, Ordering::Relaxed);
        }
    }

    /// Count `n` messages the callback dropped in stale batches, after handing back every batch it
    /// drained. A single atomic add, skipped for zero.
    pub fn record_stale_sends_dropped(&self, n: u64) {
        if n > 0 {
            self.stale_sends_dropped.fetch_add(n, Ordering::Release);
        }
    }

    /// Add what the render thread's Operators counted. One atomic add per non-zero count.
    pub fn record_operator_counts(&self, counts: OperatorCounts) {
        let OperatorCounts {
            pattern_queries_truncated,
            pattern_atoms_skipped,
            pattern_notes_evicted,
        } = counts;
        for (counter, n) in [
            (&self.pattern_queries_truncated, pattern_queries_truncated),
            (&self.pattern_atoms_skipped, pattern_atoms_skipped),
            (&self.pattern_notes_evicted, pattern_notes_evicted),
        ] {
            if n > 0 {
                counter.fetch_add(n, Ordering::Relaxed);
            }
        }
    }

    /// Count one under `counter`, one of the `midi_*` fields. Off the render thread.
    pub fn record_midi(&self, counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// The running count of stale sends dropped, read with `Acquire`; see
    /// [`stale_sends_dropped`](Self::stale_sends_dropped).
    pub fn load_stale_sends_dropped(&self) -> u64 {
        self.stale_sends_dropped.load(Ordering::Acquire)
    }

    /// Read the retire overflow from `drain` from now on. Only the first call takes effect: a
    /// session has one Coordinator.
    pub fn watch_retire_queue(&self, drain: RetireDrain) {
        let _ = self.retired.set(drain);
    }

    /// A point-in-time copy of every counter, cheap enough to take on every logging tick.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            output_xruns: self.output_xruns.load(Ordering::Relaxed),
            input_ring_underruns: self.input_ring_underruns.load(Ordering::Relaxed),
            input_ring_overruns: self.input_ring_overruns.load(Ordering::Relaxed),
            input_ring_producer_drops: self.input_ring_producer_drops.load(Ordering::Relaxed),
            control_batches_dropped_full: self.control_batches_dropped_full.load(Ordering::Relaxed),
            control_messages_dropped_full: self
                .control_messages_dropped_full
                .load(Ordering::Relaxed),
            wired_writes_dropped: self.wired_writes_dropped.load(Ordering::Relaxed),
            retire_queue_dropped_full: self.retired.get().map_or(0, RetireDrain::dropped_full),
            unprepared_writes_dropped: self.unprepared_writes_dropped.load(Ordering::Relaxed),
            unaddressed_messages_dropped: self.unaddressed_messages_dropped.load(Ordering::Relaxed),
            unfit_args_dropped: self.unfit_args_dropped.load(Ordering::Relaxed),
            stale_sends_dropped: self.stale_sends_dropped.load(Ordering::Relaxed),
            pattern_queries_truncated: self.pattern_queries_truncated.load(Ordering::Relaxed),
            pattern_atoms_skipped: self.pattern_atoms_skipped.load(Ordering::Relaxed),
            pattern_notes_evicted: self.pattern_notes_evicted.load(Ordering::Relaxed),
            midi_received: self.midi_received.load(Ordering::Relaxed),
            midi_unsupported: self.midi_unsupported.load(Ordering::Relaxed),
            midi_routed: self.midi_routed.load(Ordering::Relaxed),
            midi_unmatched: self.midi_unmatched.load(Ordering::Relaxed),
            midi_devices_lost: self.midi_devices_lost.load(Ordering::Relaxed),
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
    pub control_batches_dropped_full: u64,
    pub control_messages_dropped_full: u64,
    pub wired_writes_dropped: u64,
    /// Replaced heap-owning values the render thread dropped itself because the retire queue was
    /// full (values). Diagnosis: control that bypasses the engine channel's `send` (OSC over UDP)
    /// is not drained between batches.
    pub retire_queue_dropped_full: u64,
    pub unprepared_writes_dropped: u64,
    pub unaddressed_messages_dropped: u64,
    pub unfit_args_dropped: u64,
    pub stale_sends_dropped: u64,
    pub pattern_queries_truncated: u64,
    pub pattern_atoms_skipped: u64,
    pub pattern_notes_evicted: u64,
    pub midi_received: u64,
    pub midi_unsupported: u64,
    pub midi_routed: u64,
    pub midi_unmatched: u64,
    pub midi_devices_lost: u64,
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
         input_ring_producer_drops={} control_batches_dropped_full={} \
         control_messages_dropped_full={} wired_writes_dropped={} retire_queue_dropped_full={} \
         unprepared_writes_dropped={} unaddressed_messages_dropped={} unfit_args_dropped={} \
         stale_sends_dropped={} \
         pattern_queries_truncated={} pattern_atoms_skipped={} pattern_notes_evicted={} \
         midi_received={} midi_unsupported={} midi_routed={} midi_unmatched={} midi_devices_lost={}",
        s.output_xruns,
        s.input_ring_underruns,
        s.input_ring_overruns,
        s.input_ring_producer_drops,
        s.control_batches_dropped_full,
        s.control_messages_dropped_full,
        s.wired_writes_dropped,
        s.retire_queue_dropped_full,
        s.unprepared_writes_dropped,
        s.unaddressed_messages_dropped,
        s.unfit_args_dropped,
        s.stale_sends_dropped,
        s.pattern_queries_truncated,
        s.pattern_atoms_skipped,
        s.pattern_notes_evicted,
        s.midi_received,
        s.midi_unsupported,
        s.midi_routed,
        s.midi_unmatched,
        s.midi_devices_lost
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
        let f = Snapshot {
            control_batches_dropped_full: 1,
            ..Snapshot::default()
        };
        assert!(c.changed_since(&a));
        assert!(d.changed_since(&a));
        assert!(e.changed_since(&a));
        assert!(f.changed_since(&a));
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
