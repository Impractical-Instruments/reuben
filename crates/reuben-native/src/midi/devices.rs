//! The MIDI input ports the device profile's `midi.inputs` names, opened when `play` starts.
//! Each port's callback hands its bytes to the shared router; a thread notices a port going away
//! (unplugged) and releases the notes it held. Reconnecting is not followed: restart `play`.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use midir::{Ignore, MidiInput, MidiInputConnection};

use super::router::SharedRouter;
use crate::audio::named_index;

/// The client name `play` shows other MIDI programs.
const CLIENT: &str = "reuben play";

/// How often the watch lists the ports: the most a lost device's held notes keep sounding.
const PORT_POLL: Duration = Duration::from_secs(1);

/// Which of `available` a profile's `midi.inputs` entry selects: the port named exactly `wanted`
/// (case-insensitive), else the first whose name contains it, as audio devices are selected.
pub fn select_port(available: &[String], wanted: &str) -> Option<usize> {
    named_index(available.iter().cloned().map(Some), wanted)
}

/// The startup error for a `midi.inputs` entry no port matches, listing every port there is.
pub fn no_match_message(wanted: &str, available: &[String]) -> String {
    if available.is_empty() {
        return format!(
            "no MIDI input port name contains {wanted:?}: there are no MIDI input ports"
        );
    }
    let listed: Vec<String> = available.iter().map(|name| format!("{name:?}")).collect();
    format!(
        "no MIDI input port name contains {wanted:?}; the MIDI input ports are {}",
        listed.join(", ")
    )
}

/// The open MIDI input ports. Dropping it stops the port watch, closes every port, and prints
/// what each port read.
pub(crate) struct MidiInputs {
    // Declared first so it stops before the connections close: a port closing is not a loss.
    _watch: Option<PortWatch>,
    _connections: Vec<MidiInputConnection<()>>,
    /// Each opened port's name and the messages its callback has read.
    read: Vec<(String, Arc<AtomicU64>)>,
}

impl Drop for MidiInputs {
    fn drop(&mut self) {
        // Closed first, so the counts printed are final.
        self._watch = None;
        self._connections.clear();
        for line in received_lines(&self.read) {
            eprintln!("{line}");
        }
    }
}

impl MidiInputs {
    /// Opens the port each of `names` selects. A name that selects no port, or a port that does
    /// not open, is an error naming it. A port two names select is opened once. No names opens
    /// nothing and starts no thread.
    pub(crate) fn open(names: &[String], router: SharedRouter) -> Result<Self, String> {
        if names.is_empty() {
            return Ok(Self {
                _watch: None,
                _connections: Vec::new(),
                read: Vec::new(),
            });
        }
        // Named here because no port can be listed: the system may have no MIDI at all.
        let probe = new_input().map_err(|e| format!("{e}, to read the MIDI inputs {names:?}"))?;
        let ports = probe.ports();
        let available: Vec<String> = ports
            .iter()
            .map(|port| probe.port_name(port).unwrap_or_default())
            .collect();
        let mut chosen: Vec<usize> = Vec::new();
        for wanted in names {
            let index = select_port(&available, wanted)
                .ok_or_else(|| no_match_message(wanted, &available))?;
            if !chosen.contains(&index) {
                chosen.push(index);
            }
        }
        let mut connections = Vec::new();
        let mut opened = Vec::new();
        for index in chosen {
            let (port, name) = (&ports[index], &available[index]);
            let received = Arc::new(AtomicU64::new(0));
            let counted = Arc::clone(&received);
            let router = Arc::clone(&router);
            let connection = new_input()?
                .connect(
                    port,
                    CLIENT,
                    move |_stamp, bytes, _| {
                        counted.fetch_add(1, Ordering::Relaxed);
                        router
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .bytes(bytes);
                    },
                    (),
                )
                .map_err(|e| open_failure(name, &e.to_string()))?;
            eprintln!("midi: reading {name}");
            connections.push(connection);
            opened.push(Opened {
                id: port.id(),
                name: name.clone(),
                received,
                lost: false,
            });
        }
        let read = opened
            .iter()
            .map(|port| (port.name.clone(), Arc::clone(&port.received)))
            .collect();
        let watch = PortWatch::spawn(opened, router)
            .map_err(|e| format!("start the MIDI port watch: {e}"))?;
        Ok(Self {
            _watch: Some(watch),
            _connections: connections,
            read,
        })
    }
}

fn new_input() -> Result<MidiInput, String> {
    let mut input = MidiInput::new(CLIENT).map_err(|e| format!("open MIDI: {e}"))?;
    // Clock, active sensing and SysEx route nowhere; dropping them here spares the router the lock.
    input.ignore(Ignore::All);
    Ok(input)
}

fn open_failure(name: &str, error: &str) -> String {
    if cfg!(windows) {
        format!(
            "open MIDI input {name}: {error} (on Windows a port opens in one program at a time; \
             another program may be holding it)"
        )
    } else {
        format!("open MIDI input {name}: {error}")
    }
}

/// One line per port with what it read, so "is my keyboard arriving?" has an answer per device.
fn received_lines(read: &[(String, Arc<AtomicU64>)]) -> Vec<String> {
    read.iter()
        .map(|(name, received)| {
            format!(
                "midi: {name} read {} messages",
                received.load(Ordering::Relaxed)
            )
        })
        .collect()
}

/// One port `play` opened, as the watch follows it.
struct Opened {
    /// The backend's identity for the port, stable while it stays plugged in, so two devices with
    /// one name are told apart.
    id: String,
    name: String,
    /// Messages its callback has read; clock, time code, active sensing and SysEx never reach it.
    received: Arc<AtomicU64>,
    lost: bool,
}

/// A thread that lists the ports every [`PORT_POLL`] and, for each opened port no longer listed,
/// warns and tells the router, which releases the held notes. Dropping it stops and joins it.
struct PortWatch {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl PortWatch {
    fn spawn(mut opened: Vec<Opened>, router: SharedRouter) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("midi-ports".to_string())
            .spawn(move || {
                let lister = match new_input() {
                    Ok(lister) => lister,
                    Err(e) => {
                        eprintln!("warning: {e}; a MIDI device unplugged now goes unnoticed");
                        return;
                    }
                };
                while !thread_stop.load(Ordering::SeqCst) {
                    std::thread::park_timeout(PORT_POLL);
                    if thread_stop.load(Ordering::SeqCst) {
                        break;
                    }
                    for port in opened.iter_mut().filter(|port| !port.lost) {
                        if lister.find_port_by_id(&port.id).is_none() {
                            port.lost = true;
                            eprintln!(
                                "warning: midi device {} is gone after {} messages; restart \
                                 play to read it again",
                                port.name,
                                port.received.load(Ordering::Relaxed)
                            );
                            router
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .device_lost();
                        }
                    }
                }
            })?;
        Ok(Self {
            stop,
            handle: Some(handle),
        })
    }
}

impl Drop for PortWatch {
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

    fn ports() -> Vec<String> {
        vec!["Arturia KeyStep 32".to_string(), "MPC".to_string()]
    }

    #[test]
    fn each_port_reports_what_it_read() {
        let read = [
            ("KeyStep".to_string(), Arc::new(AtomicU64::new(3))),
            ("MPC".to_string(), Arc::new(AtomicU64::new(0))),
        ];
        assert_eq!(
            received_lines(&read),
            vec![
                "midi: KeyStep read 3 messages".to_string(),
                "midi: MPC read 0 messages".to_string()
            ]
        );
    }

    #[test]
    fn no_inputs_open_nothing_and_start_no_watch() {
        let diagnostics = crate::diagnostics::Diagnostics::new();
        let (producer, _ring) = crate::control::ring_pair(Arc::clone(&diagnostics));
        let router = Arc::new(std::sync::Mutex::new(super::super::router::Router::new(
            producer,
            diagnostics,
            false,
        )));
        let inputs = MidiInputs::open(&[], router).expect("opens nothing");
        assert!(inputs._watch.is_none());
        assert!(inputs._connections.is_empty());
    }

    #[test]
    fn a_port_is_selected_by_part_of_its_name() {
        assert_eq!(select_port(&ports(), "keystep"), Some(0));
        assert_eq!(select_port(&ports(), "nano"), None);
    }

    #[test]
    fn an_exact_name_outranks_an_earlier_containing_one() {
        let listed = vec!["MPC Studio".to_string(), "MPC".to_string()];
        assert_eq!(select_port(&listed, "mpc"), Some(1));
        assert_eq!(select_port(&ports(), "mpc"), Some(1));
    }

    #[test]
    fn the_no_match_message_lists_every_port() {
        let message = no_match_message("nano", &ports());
        assert!(message.contains("\"nano\""), "{message}");
        for port in ports() {
            assert!(message.contains(&port), "{message}");
        }
    }

    #[test]
    fn the_no_match_message_says_when_there_are_no_ports() {
        let message = no_match_message("nano", &[]);
        assert!(message.contains("no MIDI input ports"), "{message}");
    }
}
