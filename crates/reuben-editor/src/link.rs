//! The link to an engine the editor attached to rather than started: a `reuben play` already
//! running, reached over its loopback structure channel.
//!
//! The editor owns nothing behind the link — no device, no port, no process — so losing it stops
//! nothing. A thread probes the engine every [`PROBE_EVERY`] and says when it stops answering and
//! when it answers again.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::Duration;

use eframe::egui;
use reuben_api::engine::{Channel, TcpTransport, DEFAULT_CONNECT_TIMEOUT};

/// How often the link is probed: a lost engine is noticed, and one that came back is reached
/// again, within this. A ping over loopback costs well under a millisecond.
const PROBE_EVERY: Duration = Duration::from_secs(1);

const UNKNOWN: u8 = 0;
const UP: u8 = 1;
const DOWN: u8 = 2;

/// What a probe found changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// The engine answers, on the first probe or after it stopped. `playing` is the file it has
    /// installed: `Ok(None)` when it was installed without one, `Err` when it would not say.
    Up {
        playing: Result<Option<String>, String>,
    },
    /// The engine stopped answering, and why.
    Down(String),
}

/// An attached engine's address, and what the probe thread last found there.
pub struct Link {
    addr: String,
    state: Arc<AtomicU8>,
    changes: Receiver<Change>,
    /// Dropped with the link, which ends the probe thread at its next wait.
    _stop: Sender<()>,
}

impl Link {
    /// Start probing the engine at `addr`. `wake` is called from the probe thread on each
    /// [`Change`].
    pub fn dial(addr: impl Into<String>, wake: impl Fn() + Send + 'static) -> Self {
        let addr = addr.into();
        let state = Arc::new(AtomicU8::new(UNKNOWN));
        let (changed, changes) = mpsc::channel();
        let (stop, stopped) = mpsc::channel();
        // Not joined on drop: a probe waiting on a wedged engine would hold the window's close for
        // up to a read budget, and the thread holds nothing that outlives it.
        std::thread::spawn({
            let channel = channel(&addr);
            let state = Arc::clone(&state);
            move || probe(&channel, &state, &changed, &stopped, wake)
        });
        Self {
            addr,
            state,
            changes,
            _stop: stop,
        }
    }

    pub fn addr(&self) -> &str {
        &self.addr
    }

    /// Whether the engine answered the last probe; `None` until the first one is done.
    pub fn up(&self) -> Option<bool> {
        match self.state.load(Ordering::Acquire) {
            UP => Some(true),
            DOWN => Some(false),
            _ => None,
        }
    }

    /// A channel to the engine. Every exchange dials it afresh, so the channel outlives the
    /// engine restarting.
    pub fn channel(&self) -> Channel {
        channel(&self.addr)
    }

    /// What the probe found changed since the last call, oldest first. Never waits.
    pub fn changes(&self) -> Vec<Change> {
        self.changes.try_iter().collect()
    }
}

/// The bar in place of the audio settings while attached: the editor chooses no device for an
/// engine it did not start. Says when the engine stops answering, which ends nothing.
pub fn show(ui: &mut egui::Ui, link: &Link) {
    let addr = link.addr();
    match link.up() {
        Some(true) => {
            ui.label(format!("Attached to the engine on {addr}"));
        }
        None => {
            ui.label(format!("Reaching the engine on {addr}…"));
        }
        Some(false) => {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("The engine on {addr} is not answering; trying again"),
            );
        }
    }
}

fn channel(addr: &str) -> Channel {
    Channel::new(TcpTransport::new(addr, DEFAULT_CONNECT_TIMEOUT))
}

fn probe(
    channel: &Channel,
    state: &AtomicU8,
    changed: &Sender<Change>,
    stopped: &Receiver<()>,
    wake: impl Fn(),
) {
    let mut was = UNKNOWN;
    loop {
        let (now, change) = match channel.ping() {
            Ok(()) => (UP, None),
            Err(e) => (DOWN, Some(Change::Down(e.to_string()))),
        };
        if now != was {
            let change = change.unwrap_or_else(|| Change::Up {
                playing: (channel.get_document())
                    .map(|document| document.source)
                    .map_err(|e| e.to_string()),
            });
            // Before the change is sent, so a reader taking it sees the state it describes.
            state.store(now, Ordering::Release);
            if changed.send(change).is_err() {
                return;
            }
            wake();
            was = now;
        }
        match stopped.recv_timeout(PROBE_EVERY) {
            Err(RecvTimeoutError::Timeout) => {}
            Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}
