//! Live control values on their way to the engine, without the window waiting for them.
//!
//! An embedded engine takes a send without waiting, so it is sent where it is made. To an attached
//! engine a send is a round trip over its structure channel, which a stopped `reuben play` holds
//! for the channel's read budget; those are queued for a worker that owns the link's channel, and
//! the panel that made them hears how they went on a later frame.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, PoisonError};

use reuben_api::engine::{
    send_live_controls, Channel, ControlSendMessage, SendLiveControls, MAX_SEND_BATCH,
};

/// Where live control values go now.
pub enum Live {
    /// Sent where they are made.
    Now(Channel),
    /// Queued for an attached engine's worker.
    Queued(Arc<Shared>),
}

impl Live {
    /// Send `messages`, in order, and tell `replies` how it went: before this returns when they
    /// are sent now, and once the worker has sent them when queued.
    pub fn send(&self, messages: Vec<ControlSendMessage>, replies: &Replies) {
        if messages.is_empty() {
            return;
        }
        match self {
            Self::Now(channel) => {
                let _ = replies.tx.send(send(channel, messages));
            }
            Self::Queued(shared) => shared.queue(messages, replies),
        }
    }
}

/// What one panel hears about the sends it made.
#[derive(Debug)]
pub struct Replies {
    id: u64,
    tx: Sender<Result<(), String>>,
    rx: Receiver<Result<(), String>>,
}

impl Default for Replies {
    fn default() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let (tx, rx) = mpsc::channel();
        Self {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            tx,
            rx,
        }
    }
}

impl Replies {
    /// How the newest send heard of since the last call went; `None` when none was. Never waits.
    pub fn latest(&self) -> Option<Result<(), String>> {
        self.rx.try_iter().last()
    }
}

/// The worker for an attached engine's sends, which it ends when dropped.
pub struct Sends {
    shared: Arc<Shared>,
}

impl Sends {
    /// Start a worker sending over `channel`. `wake` is called from it each time a panel has a
    /// reply to hear.
    pub fn start(channel: Channel, wake: impl Fn() + Send + 'static) -> Self {
        let shared = Arc::new(Shared::default());
        // Not joined on drop: a send waiting on a wedged engine would hold the window's close for
        // up to a read budget, and the thread holds nothing that outlives it.
        std::thread::spawn({
            let shared = Arc::clone(&shared);
            move || work(&channel, &shared, wake)
        });
        Self { shared }
    }

    pub fn live(&self) -> Live {
        Live::Queued(Arc::clone(&self.shared))
    }
}

impl Drop for Sends {
    fn drop(&mut self) {
        self.shared.lock().closed = true;
        self.shared.ready.notify_one();
    }
}

#[derive(Debug, Default)]
pub struct Shared {
    queue: Mutex<Queue>,
    ready: Condvar,
}

#[derive(Debug, Default)]
struct Queue {
    pending: Vec<Pending>,
    closed: bool,
}

#[derive(Debug)]
struct Pending {
    message: ControlSendMessage,
    replies: u64,
    tx: Sender<Result<(), String>>,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Queue> {
        // Held only to move messages in or out, which cannot leave the queue half-changed.
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn queue(&self, messages: Vec<ControlSendMessage>, replies: &Replies) {
        let mut queue = self.lock();
        for message in messages {
            if setting(&message) {
                // Moved to the back rather than replaced in place, so the newest value still lands
                // after whatever was asked for between the two.
                queue
                    .pending
                    .retain(|p| !(setting(&p.message) && p.message.address == message.address));
            }
            queue.pending.push(Pending {
                message,
                replies: replies.id,
                tx: replies.tx.clone(),
            });
        }
        drop(queue);
        self.ready.notify_one();
    }
}

/// Whether only the newest of `message` and a later one to the same address matters: a value the
/// control is set to, as a fader or a toggle sends. A note (`[pitch, velocity]`) shares its address
/// with every other note, so dropping one could drop a note-off.
fn setting(message: &ControlSendMessage) -> bool {
    message.args.len() == 1
}

fn work(channel: &Channel, shared: &Shared, wake: impl Fn()) {
    loop {
        let pending = {
            let mut queue = shared.lock();
            while queue.pending.is_empty() && !queue.closed {
                queue = (shared.ready.wait(queue)).unwrap_or_else(PoisonError::into_inner);
            }
            if queue.closed {
                return;
            }
            std::mem::take(&mut queue.pending)
        };
        // Each panel's run of messages is its own exchange, so one panel's refusal is not
        // reported to another.
        let mut pending = pending.into_iter().peekable();
        while let Some(first) = pending.next() {
            let mut messages = vec![first.message];
            while let Some(next) = pending.next_if(|p| p.replies == first.replies) {
                messages.push(next.message);
            }
            let _ = first.tx.send(send(channel, messages));
        }
        wake();
    }
}

/// Send `messages` to the engine as live control values, in order.
pub fn send(channel: &Channel, messages: Vec<ControlSendMessage>) -> Result<(), String> {
    for batch in messages.chunks(MAX_SEND_BATCH) {
        send_live_controls(
            &SendLiveControls {
                messages: batch.to_vec(),
            },
            channel,
        )
        .map_err(|refusal| refusal.message)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};

    use reuben_api::engine::{Request, Response, Transport};
    use serde_json::{json, Value};

    use super::*;

    /// An engine that holds each exchange until let go, and keeps every control message it took.
    #[derive(Debug, Default)]
    struct Stalling {
        held: AtomicBool,
        taken: Mutex<Vec<(String, Vec<Value>)>>,
        exchanges: AtomicU64,
    }

    #[derive(Debug)]
    struct Handle(Arc<Stalling>);

    impl Transport for Handle {
        fn round_trip(&self, line: &str, _: Duration) -> std::io::Result<String> {
            while self.0.held.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(1));
            }
            let Ok(Request::Send { messages }) = serde_json::from_str(line) else {
                panic!("not a send: {line}");
            };
            let mut taken = self.0.taken.lock().unwrap();
            for m in messages {
                let args = serde_json::to_value(&m.args).expect("args");
                taken.push((m.address, args.as_array().expect("a list").clone()));
            }
            self.0.exchanges.fetch_add(1, Ordering::AcqRel);
            Ok(Response::Sent.to_ndjson())
        }

        fn endpoint(&self) -> &str {
            "stalling"
        }
    }

    fn set(address: &str, value: f64) -> ControlSendMessage {
        ControlSendMessage {
            address: address.into(),
            args: vec![json!(value)],
        }
    }

    fn note(address: &str, pitch: f64, velocity: f64) -> ControlSendMessage {
        ControlSendMessage {
            address: address.into(),
            args: vec![json!(pitch), json!(velocity)],
        }
    }

    fn wait_for(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn stalled() -> (Arc<Stalling>, Sends) {
        let engine = Arc::new(Stalling::default());
        engine.held.store(true, Ordering::Release);
        let sends = Sends::start(Channel::new(Handle(Arc::clone(&engine))), || {});
        (engine, sends)
    }

    #[test]
    fn a_send_to_a_stalled_engine_returns_at_once_and_its_newest_value_still_arrives() {
        let (engine, sends) = stalled();
        let replies = Replies::default();
        let live = sends.live();

        let started = Instant::now();
        live.send(vec![set("/cutoff/in", 0.125)], &replies);
        // The first is in flight and held; the drag goes on behind it.
        wait_for("the first send to be taken", || {
            sends.shared.lock().pending.is_empty()
        });
        for value in [0.25, 0.375, 0.625] {
            live.send(vec![set("/cutoff/in", value)], &replies);
        }
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "a send waited"
        );
        assert_eq!(replies.latest(), None, "nothing has been answered yet");

        engine.held.store(false, Ordering::Release);
        wait_for("both exchanges", || {
            engine.exchanges.load(Ordering::Acquire) == 2
        });
        let taken = engine.taken.lock().unwrap().clone();
        assert_eq!(
            taken,
            vec![
                ("/cutoff/in".to_string(), vec![json!(0.125)]),
                ("/cutoff/in".to_string(), vec![json!(0.625)]),
            ],
            "the stale values in between were dropped"
        );
        wait_for("the reply", || replies.latest() == Some(Ok(())));
    }

    #[test]
    fn notes_queued_behind_a_stall_all_arrive_in_order() {
        let (engine, sends) = stalled();
        let replies = Replies::default();
        let live = sends.live();
        live.send(vec![set("/level/in", 0.5)], &replies);
        wait_for("the first send to be taken", || {
            sends.shared.lock().pending.is_empty()
        });
        live.send(vec![note("/keys/in", 60.0, 1.0)], &replies);
        live.send(vec![set("/level/in", 0.75)], &replies);
        live.send(vec![note("/keys/in", 60.0, 0.0)], &replies);
        live.send(vec![set("/level/in", 0.875)], &replies);

        engine.held.store(false, Ordering::Release);
        wait_for("both exchanges", || {
            engine.exchanges.load(Ordering::Acquire) == 2
        });
        let taken = engine.taken.lock().unwrap().clone();
        assert_eq!(
            taken[1..],
            [
                ("/keys/in".to_string(), vec![json!(60.0), json!(1.0)]),
                ("/keys/in".to_string(), vec![json!(60.0), json!(0.0)]),
                ("/level/in".to_string(), vec![json!(0.875)]),
            ],
        );
    }

    #[test]
    fn each_panel_hears_only_of_its_own_sends() {
        let (engine, sends) = stalled();
        let (controls, inspector) = (Replies::default(), Replies::default());
        let live = sends.live();
        live.send(vec![set("/a/in", 0.5)], &controls);
        wait_for("the first send to be taken", || {
            sends.shared.lock().pending.is_empty()
        });
        // Not a number: the verb refuses it before it reaches the engine.
        let refused = ControlSendMessage {
            address: "/b/in".into(),
            args: vec![json!({"not": "a value"})],
        };
        live.send(vec![refused], &inspector);
        live.send(vec![set("/c/in", 0.5)], &controls);

        engine.held.store(false, Ordering::Release);
        wait_for("the inspector's reply", || {
            matches!(inspector.latest(), Some(Err(_)))
        });
        wait_for("the controls' reply", || controls.latest() == Some(Ok(())));
    }

    #[test]
    fn a_send_now_is_answered_before_it_returns() {
        let engine = Arc::new(Stalling::default());
        let live = Live::Now(Channel::new(Handle(Arc::clone(&engine))));
        let replies = Replies::default();
        live.send(vec![set("/a/in", 0.5)], &replies);
        assert_eq!(replies.latest(), Some(Ok(())));
    }
}
