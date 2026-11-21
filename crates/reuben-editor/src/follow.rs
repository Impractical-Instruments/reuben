//! The server's events, followed on a thread of their own so the window never waits on them.
//!
//! The thread holds one subscription. When it ends — the engine stopped, restarted, or was never
//! there — the thread asks for a channel again every [`RETRY_EVERY`] until a subscription opens,
//! and then hands on a [`resubscribed`] event: what changed while nothing was listening is unknown,
//! so the tree is read again.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::Duration;

use reuben_api::engine::{ChangedBy, Channel, Event};

/// How long the thread waits between attempts to subscribe.
const RETRY_EVERY: Duration = Duration::from_secs(1);

/// What the thread hands on each time a subscription opens: a change of unknown documents, with
/// no hash, as a write while nothing plays is.
pub fn resubscribed() -> Event {
    Event::Changed {
        hash: String::new(),
        by: ChangedBy::Outside,
    }
}

/// The thread following the server's events, and where they arrive. Dropping it lets the thread
/// go at its next event or retry; it is not joined, since a subscription waiting on an engine that
/// says nothing would hold the drop.
pub struct Follow {
    events: Receiver<Event>,
    stop: Arc<AtomicBool>,
    /// Dropped with the follow, which wakes a thread waiting to retry.
    _retry: Sender<()>,
}

impl Follow {
    /// Follow the events of whichever engine `channel` answers with when a subscription is to be
    /// opened. `wake` is called from the thread after each event it hands on.
    pub fn start(
        channel: impl Fn() -> Option<Channel> + Send + 'static,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Follow {
        let (sent, events) = mpsc::channel();
        let (retry, retried) = mpsc::channel::<()>();
        let stop = Arc::new(AtomicBool::new(false));
        std::thread::spawn({
            let stop = Arc::clone(&stop);
            move || follow(&channel, &wake, &sent, &retried, &stop)
        });
        Follow {
            events,
            stop,
            _retry: retry,
        }
    }

    /// Every event that arrived since the last call, oldest first. Never waits.
    pub fn drain(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }
}

impl Drop for Follow {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

fn follow(
    channel: &dyn Fn() -> Option<Channel>,
    wake: &dyn Fn(),
    sent: &Sender<Event>,
    retried: &Receiver<()>,
    stop: &AtomicBool,
) {
    while !stop.load(Ordering::Acquire) {
        if let Some(events) = channel().and_then(|channel| channel.subscribe().ok()) {
            if sent.send(resubscribed()).is_err() {
                return;
            }
            wake();
            for event in events {
                if stop.load(Ordering::Acquire) || sent.send(event).is_err() {
                    return;
                }
                wake();
            }
        }
        match retried.recv_timeout(RETRY_EVERY) {
            Err(RecvTimeoutError::Timeout) => {}
            Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}
