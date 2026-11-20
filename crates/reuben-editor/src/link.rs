//! The link to an engine the editor attached to rather than started: a `reuben play` already
//! running, reached over its loopback structure channel.
//!
//! The editor owns nothing behind the link — no device, no port, no process — so losing it stops
//! nothing. A thread pings the engine every [`PROBE_EVERY`] and, while it answers, asks what it
//! plays. It says when that changes: when the engine stops answering, when it answers again, and
//! when another client, or a restart between two probes, has it play something else.
//!
//! Live control values to it go through a [`Sends`] worker, so a stopped engine never holds the
//! window.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::Duration;

use eframe::egui;
use reuben_api::engine::{Channel, TcpTransport, DEFAULT_CONNECT_TIMEOUT};

use crate::sends::{Live, Sends};

/// How often the link is probed: a lost engine is noticed, and a change to what it plays is seen,
/// within this.
const PROBE_EVERY: Duration = Duration::from_secs(1);

/// The read budget for a send. The engine answers one as soon as it has queued it, before it is
/// applied, so a send that takes longer than this is to an engine that is not running.
const SEND_READ_TIMEOUT: Duration = Duration::from_secs(1);

const UNKNOWN: u8 = 0;
const UP: u8 = 1;
const DOWN: u8 = 2;

/// What a probe found changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// The engine answers with what it plays, on the first probe, after it stopped answering, or
    /// because what it plays changed. `source` is `None` for a document installed without a file.
    Up {
        source: Option<String>,
        content_hash: String,
    },
    /// The engine stopped answering, and why.
    Down(String),
}

/// An attached engine's address, and what the probe thread last found there.
pub struct Link {
    addr: String,
    transport: TcpTransport,
    sends: Sends,
    state: Arc<AtomicU8>,
    changes: Receiver<Change>,
    /// Dropped with the link, which ends the probe thread at its next wait.
    _stop: Sender<()>,
}

impl Link {
    /// Start probing the engine at `addr`. `wake` is called from another thread on each
    /// [`Change`], and each time a send is answered.
    pub fn dial(addr: impl Into<String>, wake: impl Fn() + Send + Sync + 'static) -> Self {
        let addr = addr.into();
        // A name that does not resolve now is looked up again on each exchange, which fails as
        // the engine not answering until it does.
        let transport = TcpTransport::resolved(&addr, DEFAULT_CONNECT_TIMEOUT)
            .unwrap_or_else(|_| TcpTransport::new(&addr, DEFAULT_CONNECT_TIMEOUT));
        let wake = Arc::new(wake);
        let sends = Sends::start(
            Channel::with_read_timeout(transport.clone(), SEND_READ_TIMEOUT),
            {
                let wake = Arc::clone(&wake);
                move || wake()
            },
        );
        let state = Arc::new(AtomicU8::new(UNKNOWN));
        let (changed, changes) = mpsc::channel();
        let (stop, stopped) = mpsc::channel();
        // Not joined on drop: a probe waiting on a wedged engine would hold the window's close for
        // up to a read budget, and the thread holds nothing that outlives it.
        std::thread::spawn({
            let channel = Channel::new(transport.clone());
            let state = Arc::clone(&state);
            move || probe(&channel, &state, &changed, &stopped, move || wake())
        });
        Self {
            addr,
            transport,
            sends,
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
        Channel::new(self.transport.clone())
    }

    /// Where live control values to the engine go: queued for the link's send worker.
    pub fn live(&self) -> Live {
        self.sends.live()
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

fn probe(
    channel: &Channel,
    state: &AtomicU8,
    changed: &Sender<Change>,
    stopped: &Receiver<()>,
    wake: impl Fn(),
) {
    let mut last = None;
    loop {
        let change = match channel.ping() {
            Err(e) => {
                last = None;
                let was = state.swap(DOWN, Ordering::AcqRel);
                (was != DOWN).then(|| Change::Down(e.to_string()))
            }
            Ok(()) => {
                state.store(UP, Ordering::Release);
                // Waits on the Coordinator, which a Swap holds while it builds: a read that fails
                // is asked again at the next probe, and says nothing about whether the engine runs.
                match channel.get_document() {
                    Ok(document) => {
                        let seen = Change::Up {
                            source: document.source,
                            content_hash: document.content_hash,
                        };
                        (last.as_ref() != Some(&seen)).then_some(seen)
                    }
                    Err(_) => None,
                }
            }
        };
        if let Some(change) = change {
            last = Some(change.clone());
            if changed.send(change).is_err() {
                return;
            }
            wake();
        }
        match stopped.recv_timeout(PROBE_EVERY) {
            Err(RecvTimeoutError::Timeout) => {}
            Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}
