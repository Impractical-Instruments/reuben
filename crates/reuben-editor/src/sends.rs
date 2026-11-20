//! Live control values on their way to the engine, without the window waiting for them.
//!
//! An embedded engine takes a send without waiting, so it is sent where it is made. To an attached
//! engine a send is a round trip over its structure channel, which a stopped `reuben play` holds
//! for the channel's read budget; those are queued for a worker that owns the link's channel, and
//! the panel that made them hears how they went on a later frame.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

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
        self.shared.ready.notify_all();
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
        queue
            .pending
            .extend(messages.into_iter().map(|message| Pending {
                message,
                replies: replies.id,
                tx: replies.tx.clone(),
            }));
        coalesce(&mut queue.pending);
        drop(queue);
        self.ready.notify_one();
    }

    /// Put `unsent` back in front of what was queued since it was taken.
    fn requeue(&self, mut unsent: Vec<Pending>) {
        let mut queue = self.lock();
        unsent.append(&mut queue.pending);
        coalesce(&mut unsent);
        queue.pending = unsent;
    }

    /// Wait out `pause`, or less if the worker is closed. Returns whether it was.
    fn closed_within(&self, pause: Duration) -> bool {
        let queue = self.lock();
        let (queue, _) = (self.ready.wait_timeout_while(queue, pause, |q| !q.closed))
            .unwrap_or_else(PoisonError::into_inner);
        queue.closed
    }
}

/// Whether only the newest of `message` and a later one to the same address matters: a value the
/// control is set to, as a fader or a toggle sends. A note (`[pitch, velocity]`) shares its address
/// with every other note, so dropping one could drop a note-off.
fn setting(message: &ControlSendMessage) -> bool {
    message.args.len() == 1
}

/// Drop every setting a later one to the same address supersedes. The newest keeps its own place,
/// so it still lands after whatever was asked for before it.
fn coalesce(pending: &mut Vec<Pending>) {
    let mut seen = HashSet::new();
    let mut kept: Vec<Pending> = Vec::with_capacity(pending.len());
    for p in pending.drain(..).rev() {
        if !setting(&p.message) || seen.insert(p.message.address.clone()) {
            kept.push(p);
        }
    }
    kept.reverse();
    *pending = kept;
}

/// How long the worker waits between pings to an engine that did not answer a send.
const RETRY_EVERY: Duration = Duration::from_millis(250);

fn work(channel: &Channel, shared: &Shared, wake: impl Fn()) {
    let mut failed = false;
    loop {
        // A send that timed out is still in the engine's socket buffer, and is applied when the
        // engine runs again. Its connection is handled on its own thread, so another send made
        // before then could land first: nothing more is sent until the engine answers.
        while failed && channel.ping().is_err() {
            if shared.closed_within(RETRY_EVERY) {
                return;
            }
        }
        failed = false;
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
            let mut run = vec![first];
            while let Some(next) = pending.next_if(|p| p.replies == run[0].replies) {
                run.push(next);
            }
            let tx = run[0].tx.clone();
            let mut run = run.into_iter();
            let mut sent = Ok(());
            loop {
                let batch: Vec<_> = run.by_ref().take(MAX_SEND_BATCH).collect();
                if batch.is_empty() {
                    break;
                }
                let messages = batch.into_iter().map(|p| p.message).collect();
                sent = send(channel, messages);
                if sent.is_err() {
                    break;
                }
            }
            failed = sent.is_err();
            let _ = tx.send(sent);
            wake();
            if failed {
                // The batch that failed may yet be applied, so it is not sent again; what was
                // behind it waits for the engine to answer.
                shared.requeue(run.chain(pending).collect());
                break;
            }
        }
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
    /// While `down`, every exchange fails as a stopped engine's does; `fail` more sends fail first.
    #[derive(Debug, Default)]
    struct Stalling {
        held: AtomicBool,
        down: AtomicBool,
        fail: AtomicU64,
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
            if self.0.down.load(Ordering::Acquire) {
                return Err(std::io::ErrorKind::TimedOut.into());
            }
            let messages = match serde_json::from_str(line) {
                Ok(Request::Send { messages }) => messages,
                Ok(Request::Ping) => return Ok(Response::Pong.to_ndjson()),
                _ => panic!("not a send: {line}"),
            };
            let failing =
                (self.0.fail).try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1));
            if failing.is_ok() {
                return Err(std::io::ErrorKind::TimedOut.into());
            }
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

    #[test]
    fn after_a_send_fails_nothing_more_is_sent_until_the_engine_answers() {
        let engine = Arc::new(Stalling::default());
        engine.down.store(true, Ordering::Release);
        let sends = Sends::start(Channel::new(Handle(Arc::clone(&engine))), || {});
        let replies = Replies::default();
        let live = sends.live();
        live.send(vec![note("/keys/in", 60.0, 1.0)], &replies);
        wait_for("the failure", || matches!(replies.latest(), Some(Err(_))));
        live.send(vec![note("/keys/in", 60.0, 0.0)], &replies);
        live.send(vec![set("/level/in", 0.5)], &replies);
        std::thread::sleep(RETRY_EVERY * 2);
        assert_eq!(engine.exchanges.load(Ordering::Acquire), 0);

        engine.down.store(false, Ordering::Release);
        wait_for("the rest", || engine.exchanges.load(Ordering::Acquire) == 1);
        let taken = engine.taken.lock().unwrap().clone();
        assert_eq!(
            taken,
            [
                ("/keys/in".to_string(), vec![json!(60.0), json!(0.0)]),
                ("/level/in".to_string(), vec![json!(0.5)]),
            ],
            "the failed send is not made again, and what was behind it arrives in order"
        );
    }

    #[test]
    fn what_was_behind_a_failed_batch_is_still_sent() {
        let engine = Arc::new(Stalling::default());
        engine.fail.store(1, Ordering::Release);
        engine.held.store(true, Ordering::Release);
        let sends = Sends::start(Channel::new(Handle(Arc::clone(&engine))), || {});
        let replies = Replies::default();
        let notes: Vec<_> = (0..MAX_SEND_BATCH + 4)
            .map(|i| note("/keys/in", 60.0, if i % 2 == 0 { 1.0 } else { 0.0 }))
            .collect();
        sends.live().send(notes, &replies);
        engine.held.store(false, Ordering::Release);

        wait_for("the batch behind", || {
            engine.exchanges.load(Ordering::Acquire) == 1
        });
        let taken = engine.taken.lock().unwrap().clone();
        assert_eq!(
            taken.len(),
            4,
            "the first batch failed; the four behind it arrive"
        );
        assert_eq!(
            taken[3],
            ("/keys/in".to_string(), vec![json!(60.0), json!(0.0)])
        );
    }
}
