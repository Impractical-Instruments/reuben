//! The fan-out behind `subscribe`: every [`Event`] the server publishes reaches every subscriber
//! still listening, in the order it was published.

use alloc::vec::Vec;

use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Mutex;

use super::wire::Event;

/// The subscribers of one server: a sender per [`subscribe`](Self::subscribe), each dropped at
/// the first [`publish`](Self::publish) after its receiver is gone.
#[derive(Default)]
pub struct Bus {
    senders: Mutex<Vec<Sender<Event>>>,
}

impl Bus {
    /// A receiver of every event published from now on.
    pub fn subscribe(&self) -> Receiver<Event> {
        let (sender, receiver) = channel();
        self.senders().push(sender);
        receiver
    }

    /// Send `event` to each subscriber, dropping those whose receiver is gone. Never blocks: the
    /// channels are unbounded, so a subscriber that stops reading holds its events in memory
    /// rather than stall the publisher, which holds the Coordinator lock.
    pub fn publish(&self, event: Event) {
        self.senders()
            .retain(|sender| sender.send(event.clone()).is_ok());
    }

    /// Nothing under this lock can panic part-way through, so a poisoned list is still whole.
    fn senders(&self) -> std::sync::MutexGuard<'_, Vec<Sender<Event>>> {
        self.senders
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

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
        assert_eq!(bus.senders().len(), 1);
    }
}
