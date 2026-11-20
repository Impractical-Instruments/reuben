//! The fan-out behind `subscribe`: every [`Event`] the server publishes reaches every subscriber
//! still listening, in the order it was published.

use alloc::sync::Arc;
use alloc::vec::Vec;

use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Mutex, MutexGuard};

use super::wire::Event;

/// The subscribers of one server: a sender per subscription, dropped when its [`Subscription`] is,
/// or at the first [`publish`](Self::publish) after a bare receiver is gone.
#[derive(Default)]
pub struct Bus {
    senders: Mutex<Senders>,
}

#[derive(Default)]
struct Senders {
    next: u64,
    live: Vec<(u64, Sender<Event>)>,
    /// Set by [`Bus::close`]: no sender is kept from then on.
    closed: bool,
}

impl Bus {
    /// A receiver of every event published from now on. Its sender stays until a publish finds
    /// the receiver gone; [`subscription`](Self::subscription) is the form that leaves at once.
    pub fn subscribe(&self) -> Receiver<Event> {
        self.add().1
    }

    /// [`subscribe`](Self::subscribe), whose sender is removed the moment it is dropped, so a
    /// subscriber that goes away while nothing is published does not stay counted.
    pub fn subscription(self: &Arc<Self>) -> Subscription {
        let (id, receiver) = self.add();
        Subscription {
            receiver,
            id,
            bus: Arc::clone(self),
        }
    }

    /// Send `event` to each subscriber, dropping those whose receiver is gone. Never blocks: the
    /// channels are unbounded, so a subscriber that stops reading holds its events in memory
    /// rather than stall the publisher, which holds the Coordinator lock.
    pub fn publish(&self, event: Event) {
        self.senders()
            .live
            .retain(|(_, sender)| sender.send(event.clone()).is_ok());
    }

    /// Drop every sender, so each receiver sees the stream disconnect, and keep none added later.
    pub fn close(&self) {
        let mut senders = self.senders();
        senders.closed = true;
        senders.live.clear();
    }

    /// How many subscribers the bus holds a sender for.
    pub fn subscribers(&self) -> usize {
        self.senders().live.len()
    }

    fn add(&self) -> (u64, Receiver<Event>) {
        let (sender, receiver) = channel();
        let mut senders = self.senders();
        let id = senders.next;
        senders.next += 1;
        // After `close` the sender drops here, so the receiver starts out disconnected.
        if !senders.closed {
            senders.live.push((id, sender));
        }
        (id, receiver)
    }

    /// Nothing under this lock can panic part-way through, so a poisoned list is still whole.
    fn senders(&self) -> MutexGuard<'_, Senders> {
        self.senders
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// A receiver of a [`Bus`]'s events that unsubscribes when dropped.
pub struct Subscription {
    receiver: Receiver<Event>,
    id: u64,
    bus: Arc<Bus>,
}

impl core::ops::Deref for Subscription {
    type Target = Receiver<Event>;

    fn deref(&self) -> &Receiver<Event> {
        &self.receiver
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.bus.senders().live.retain(|(id, _)| *id != self.id);
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use std::sync::mpsc::TryRecvError;

    use super::*;
    use crate::engine::wire::ChangedBy;

    fn changed(hash: &str) -> Event {
        Event::Changed {
            hash: hash.to_string(),
            by: ChangedBy::Client,
        }
    }

    #[test]
    fn dropped_subscriber_is_pruned() {
        let bus = Bus::default();
        let live = bus.subscribe();
        drop(bus.subscribe());

        bus.publish(changed("a"));
        bus.publish(changed("b"));

        assert_eq!(live.try_recv().expect("first"), changed("a"));
        assert_eq!(live.try_recv().expect("second"), changed("b"));
        assert_eq!(bus.subscribers(), 1);
    }

    #[test]
    fn a_dropped_subscription_leaves_without_a_publish() {
        let bus = Arc::new(Bus::default());
        let kept = bus.subscription();
        drop(bus.subscription());
        assert_eq!(bus.subscribers(), 1);
        drop(kept);
        assert_eq!(bus.subscribers(), 0);
    }

    #[test]
    fn close_disconnects_every_receiver_and_any_added_after() {
        let bus = Arc::new(Bus::default());
        let receiver = bus.subscribe();
        let subscription = bus.subscription();
        bus.close();
        assert_eq!(receiver.try_recv(), Err(TryRecvError::Disconnected));
        assert_eq!(subscription.try_recv(), Err(TryRecvError::Disconnected));
        assert_eq!(bus.subscribe().try_recv(), Err(TryRecvError::Disconnected));
        assert_eq!(bus.subscribers(), 0);
    }
}
