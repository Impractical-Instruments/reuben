//! Test scaffolding shared by this crate's unit tests and its integration tests.
//!
//! `#[doc(hidden)] pub`, not `#[cfg(test)]`: an integration test in `tests/` compiles against the
//! crate's *public* surface only, so `cfg(test)` scaffolding is invisible to it. Its stand-in for the
//! cpal render callback is the headless engine's own driver, so tests and `reuben play --headless`
//! drain control and render through one implementation.

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reuben_api::authoring::DocumentHistory;
use reuben_api::engine::ServerStore;
use reuben_api::render::{Message, RenderSide};
use reuben_api::resources::{ResolveError, Resources, SampleBuffer};
use reuben_api::staged::Staged;

use crate::control::{ControlDrain, ControlRing};
use crate::engine::{
    headless_config, start_with, EngineConfig, HeadlessDriver, Pace, Render, RunningEngine,
    StartError,
};
use crate::osc::{ControlBatch, OscIn};

/// A store with nothing in it, for the self-contained documents these tests install: no samples, no
/// nested children, so every source is a miss.
///
/// The seam is not optional even when a document needs nothing from it — a host always answers
/// "what does this source name?", and answering "nothing" is an answer.
pub struct NoResources;

impl Resources for NoResources {
    fn read_samples(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        Err(ResolveError::NotFound(source.to_string()))
    }
}

/// A [`NoResources`] store as a session holds it: hand a clone to `install_initial` and the same
/// one to [`served`], as the host does.
pub fn no_store() -> Staged<Box<dyn Resources + Send + Sync>> {
    Staged::new(Box::new(NoResources))
}

/// The store a `ChannelState` serves `call` through, over `staged`.
pub fn served(staged: Staged<Box<dyn Resources + Send + Sync>>) -> Arc<ServerStore> {
    Arc::new(DocumentHistory::new(staged))
}

/// How long [`FakeCallback::await_queued`] waits for a batch to reach `queue_osc` before giving up.
/// Bounded so a regression fails as a red test rather than hanging CI.
const QUEUED_DEADLINE: Duration = Duration::from_secs(2);

/// How often [`FakeCallback`] renders: faster than real time, so a swap's ramp completes in a few ms
/// of a test's wall time.
const FAKE_PACE: Pace = Pace::Every(Duration::from_millis(1));

/// A background stand-in for the cpal render callback: the headless engine's own driver, plus a log
/// of what it fed.
///
/// It records each control batch it drained, so a `send` test can assert the door delivered
/// `{address, [Arg]}` to the same `queue_osc` call an external OSC datagram reaches, rather than to
/// a hand-written stand-in for it.
pub struct FakeCallback {
    // Held only to be dropped, which stops and joins it.
    _driver: HeadlessDriver,
    queued: Arc<Mutex<Vec<OscIn>>>,
    batches: Arc<Mutex<Vec<usize>>>,
}

impl FakeCallback {
    /// Start driving `side`, draining `control` into the slot's `queue_osc`. The log keeps a copy
    /// of each batch; the batch itself goes back to its producer as on the real callback.
    pub fn spawn(side: RenderSide, control: ControlRing) -> Self {
        let queued = Arc::new(Mutex::new(Vec::new()));
        let batches = Arc::new(Mutex::new(Vec::new()));
        let (queued_log, batch_log) = (Arc::clone(&queued), Arc::clone(&batches));
        let record = move |batch: &ControlBatch| {
            batch_log.lock().expect("batch log").push(batch.len());
            queued_log
                .lock()
                .expect("queued log")
                .extend(batch.iter().cloned());
        };
        let driver = HeadlessDriver::spawn(
            side,
            headless_config(),
            FAKE_PACE,
            ControlDrain::new(vec![control]),
            None,
            record,
        )
        .expect("spawn the fake render callback");
        Self {
            _driver: driver,
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

    /// Stop the loop and join the thread. Dropping it does the same.
    pub fn stop(self) {}
}

/// [`engine::start_headless`](crate::engine::start_headless), the entry point `reuben play
/// --headless` runs.
pub fn start_headless(config: EngineConfig) -> Result<RunningEngine, StartError> {
    crate::engine::start_headless(config)
}

/// [`start_headless`], plus a clone of the render callback's outbound sender held by the caller —
/// the stand-in for a device callback that outlives its stream's drop (cpal on macOS keeps a
/// non-default device's callback alive through its disconnect listener). With it held, the OSC-out
/// thread never sees its channel disconnect, so only the engine's own stop can end it; once that
/// thread has exited, a `send` on the clone fails. `None` when `config.osc_out` is.
pub fn start_headless_holding_outbound(
    config: EngineConfig,
) -> Result<(RunningEngine, Option<Sender<Message>>), StartError> {
    let mut held = None;
    let engine = start_with(
        config,
        Render::Headless {
            config: headless_config(),
            pace: Pace::RealTime,
            outbound: Some(&mut held),
        },
    )?;
    Ok((engine, held))
}

/// How long a whole engine teardown may take. Its own polls add up to well under this (the engine-channel
/// read poll is the longest, at 250 ms), so a teardown that nears it is waiting on a thread that
/// is not being woken.
pub const SHUTDOWN_BOUND: Duration = Duration::from_secs(1);

/// Tear `engine` down — [`shutdown`](RunningEngine::shutdown), or a plain drop when `by_drop` —
/// and fail if that took [`SHUTDOWN_BOUND`] or longer. Pair it with [`within`], which catches the
/// teardown that never returns at all.
pub fn timed_teardown(engine: RunningEngine, by_drop: bool) {
    let started = Instant::now();
    if by_drop {
        drop(engine);
    } else {
        engine.shutdown();
    }
    let took = started.elapsed();
    assert!(
        took < SHUTDOWN_BOUND,
        "teardown took {took:?}, over the {SHUTDOWN_BOUND:?} bound"
    );
}

/// Run `f` on a fresh thread and fail if it has not returned within `secs` — a hang assertion that
/// still reports a panic inside `f` as that panic. A [`RunningEngine`] is not `Send`, so a test
/// starts and stops one inside `f`.
pub fn within<T: Send + 'static>(secs: u64, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    match rx.recv_timeout(Duration::from_secs(secs)) {
        Ok(value) => {
            let _ = handle.join();
            value
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => match handle.join() {
            Err(panic) => std::panic::resume_unwind(panic),
            Ok(()) => unreachable!("the thread sent before returning"),
        },
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            panic!("did not complete within {secs}s — it hung")
        }
    }
}
