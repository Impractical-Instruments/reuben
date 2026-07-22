//! Test scaffolding shared by this crate's unit tests and its integration tests.
//!
//! `#[doc(hidden)] pub` rather than `#[cfg(test)]`: an integration test in `tests/` compiles against
//! the crate's *public* surface, so `cfg(test)` scaffolding is invisible to it. Without this module
//! the only way to drive a `StructureServer` from both levels is to write the harness twice — which
//! is what used to happen, and it put two copies of the render-callback mirror in the tree. A change
//! to how `audio.rs` drains control then has to land in both, or one copy quietly stops mirroring
//! production while still claiming to.
//!
//! Nothing here is part of the crate's API. It exists so there is exactly **one** stand-in for the
//! cpal callback.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use reuben_core::coordinator::{RenderSide, RenderSlot};

use crate::osc::{ControlBatch, OscIn};

/// How long [`FakeCallback::await_queued`] waits for a batch to reach `queue_osc` before giving up.
/// Bounded so a regression fails as a red test rather than hanging CI.
const QUEUED_DEADLINE: Duration = Duration::from_secs(2);

/// The block size the fake renders, matching the `AudioConfig` the tests build.
const BLOCK: usize = 128;

/// A background stand-in for the cpal render callback.
///
/// It owns the [`RenderSlot`] the real callback would and drives it in a loop, so the machinery a
/// device would exercise runs with no device: the install mailbox drains, the master-gain ramp runs,
/// survivors are box-transplanted, and retirees are posted back for the Coordinator's off-thread
/// `reclaim`. That is what makes a swap real end-to-end in a test. Rendering the *logical* master
/// directly (no device output map — that half stays a scripted human ritual) is enough.
///
/// It also drains the control ingress exactly as `audio.rs` does — **batch by batch, each batch
/// whole** — and records what it fed, so a `send` test can assert the door delivered
/// `{address, [Arg]}` to the same `queue_osc` call an external OSC datagram reaches, rather than to
/// a hand-written stand-in for it.
pub struct FakeCallback {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    queued: Arc<Mutex<Vec<OscIn>>>,
    batches: Arc<Mutex<Vec<usize>>>,
}

impl FakeCallback {
    /// Start driving `side`, draining `control_rx` into the slot's `queue_osc`.
    pub fn spawn(side: RenderSide, control_rx: Receiver<ControlBatch>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let queued = Arc::new(Mutex::new(Vec::new()));
        let queued_thread = Arc::clone(&queued);
        let batches = Arc::new(Mutex::new(Vec::new()));
        let batches_thread = Arc::clone(&batches);
        let handle = std::thread::spawn(move || {
            let mut slot = RenderSlot::new(side);
            let mut buf = vec![0.0f32; BLOCK * slot.channels().max(1)];
            while !stop_thread.load(Ordering::SeqCst) {
                // The real callback's control drain, mirrored: each item is a whole batch and is
                // applied without splitting, so a gesture reaches one block. Flat args in, typed at
                // the slot's Engine where the destination port's type is known.
                while let Ok(batch) = control_rx.try_recv() {
                    batches_thread.lock().expect("batch log").push(batch.len());
                    for m in &batch {
                        slot.queue_osc(&m.address, &m.args);
                    }
                    queued_thread.lock().expect("queued log").extend(batch);
                }
                let ch = slot.channels().max(1);
                if buf.len() != BLOCK * ch {
                    buf.resize(BLOCK * ch, 0.0);
                }
                slot.fill(&mut buf);
                // Pace the loop like a device would: fast enough that a swap's ramp completes in a
                // few ms, slow enough not to spin a core.
                std::thread::sleep(Duration::from_millis(1));
            }
            // The slot (and its Engine + mailbox) drops here, off any RT thread.
        });
        Self {
            stop,
            handle: Some(handle),
            queued,
            batches,
        }
    }

    /// Poll until `n` messages have reached `queue_osc`, then return them in order.
    pub fn await_queued(&self, n: usize) -> Vec<OscIn> {
        let deadline = Instant::now() + QUEUED_DEADLINE;
        loop {
            let queued = self.queued.lock().expect("queued log").clone();
            if queued.len() >= n || Instant::now() >= deadline {
                return queued;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// The size of each batch the callback drained, in order — the proof that a gesture arrived as
    /// **one** unit rather than as N single-message pushes it could have interleaved with.
    pub fn drained_batch_sizes(&self) -> Vec<usize> {
        self.batches.lock().expect("batch log").clone()
    }

    /// Stop the loop and join the thread.
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
