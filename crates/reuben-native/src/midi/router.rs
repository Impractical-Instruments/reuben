//! The router: matches each translated MIDI message against the route table, scales it, and
//! pushes the resulting `/<bind>/in` sends onto its own control ring. It runs on the MIDI callback
//! threads and the engine channel's thread, behind one lock ([`SharedRouter`]), never on the
//! render thread.

use std::collections::BTreeSet;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use reuben_api::engine::MAX_SEND_BATCH;
use reuben_api::render::Arg;

use super::routes::{scale_value, RouteTable};
use super::translate::{translate, Decoded, MidiKind, Translated};
use crate::control::ControlProducer;
use crate::diagnostics::Diagnostics;
use crate::osc::OscIn;

/// The router every MIDI source shares: the devices' callbacks, the `send_midi` verb, and the
/// thread that installs rebuilt tables. A `ControlProducer` has one owner, so they take turns.
pub type SharedRouter = Arc<Mutex<Router>>;

pub struct Router {
    producer: ControlProducer,
    table: RouteTable,
    /// Notes this router has sent a note-on for and no note-off yet, keyed by the target it sent
    /// them to, channel and pitch, so a rebuilt table or a lost device can release each one where
    /// it is sounding.
    held: BTreeSet<(String, u8, u8)>,
    diagnostics: Arc<Diagnostics>,
    /// Print each translated message and where it went to stderr.
    log: bool,
}

impl Router {
    /// A router with an empty table: every message counts as unmatched until one is installed.
    pub fn new(producer: ControlProducer, diagnostics: Arc<Diagnostics>, log: bool) -> Self {
        Self {
            producer,
            table: RouteTable::default(),
            held: BTreeSet::new(),
            diagnostics,
            log,
        }
    }

    /// One MIDI message from a device.
    pub fn bytes(&mut self, bytes: &[u8]) {
        match translate(bytes) {
            Decoded::Message(t) => self.translated(&t),
            Decoded::Unsupported => {
                self.count(|d| &d.midi_received);
                self.count(|d| &d.midi_unsupported);
            }
            Decoded::Ignored => {}
        }
    }

    /// One translated message, from a device or injected: every matching route gets the value,
    /// in one batch so the sends land in the same block.
    pub fn translated(&mut self, t: &Translated) {
        self.count(|d| &d.midi_received);
        let mut sends = Vec::new();
        let mut matched = false;
        for route in &self.table.routes {
            if !route.pattern.matches(&t.address) {
                continue;
            }
            matched = true;
            if t.kind == MidiKind::Note {
                // Two routes to one target would send it the note twice, and the one held entry
                // could release only one of them.
                if sends.iter().any(|m: &OscIn| m.address == route.target) {
                    continue;
                }
                let Some((pitch, on)) = note(&t.args) else {
                    continue;
                };
                let key = (route.target.clone(), t.channel, pitch);
                // A note-off for a note not held was released already (its route changed or its
                // device was lost), or was never sent; either way its target has nothing to stop.
                if on {
                    self.held.insert(key);
                } else if !self.held.remove(&key) {
                    continue;
                }
            }
            sends.push(OscIn::new(
                route.target.clone(),
                scale_value(&route.scale, t.kind, &t.args),
            ));
        }
        if self.log {
            let targets: Vec<&str> = sends.iter().map(|m| m.address.as_str()).collect();
            eprintln!("midi: {} {:?} -> {targets:?}", t.address, t.args);
        }
        self.count(|d| {
            if matched {
                &d.midi_routed
            } else {
                &d.midi_unmatched
            }
        });
        self.push(sends);
    }

    /// Replaces the route table, first releasing each held note `table` could no longer stop: one
    /// no note route of `table` both sends to its target and matches its channel. Its note-off
    /// would otherwise go nowhere, or go unmatched, and the note would sound until `play` stops.
    pub fn install(&mut self, table: RouteTable) {
        let reachable = |(target, channel, _): &(String, u8, u8)| {
            let address = format!("/midi/{channel}/note");
            table.routes.iter().any(|route| {
                route.kind == MidiKind::Note
                    && route.target == *target
                    && route.pattern.matches(&address)
            })
        };
        let (keep, release): (BTreeSet<_>, BTreeSet<_>) = std::mem::take(&mut self.held)
            .into_iter()
            .partition(reachable);
        self.held = keep;
        self.push(release.into_iter().map(note_off).collect());
        self.table = table;
    }

    /// A device went away, and its note-offs with it: counts the loss and releases every held
    /// note. Held notes are not kept per device, so a note held on another device is released too.
    pub fn device_lost(&mut self) {
        self.count(|d| &d.midi_devices_lost);
        let release = std::mem::take(&mut self.held);
        self.push(release.into_iter().map(note_off).collect());
    }

    fn count(&self, counter: impl Fn(&Diagnostics) -> &AtomicU64) {
        self.diagnostics.record_midi(counter(&self.diagnostics));
    }

    /// Pushes `sends` in batches of at most [`MAX_SEND_BATCH`], the bound a batch's cost in one
    /// render callback is held to. A full ring drops and counts the batch.
    fn push(&mut self, sends: Vec<OscIn>) {
        for chunk in sends.chunks(MAX_SEND_BATCH) {
            let mut batch = self.producer.batch();
            batch.extend_from_slice(chunk);
            let _ = self.producer.push(batch);
        }
    }
}

/// A translated note's pitch, and whether it is a note-on.
fn note(args: &[Arg]) -> Option<(u8, bool)> {
    match args {
        [Arg::F32(pitch), Arg::F32(velocity)] => Some((*pitch as u8, *velocity > 0.0)),
        _ => None,
    }
}

fn note_off((target, _, pitch): (String, u8, u8)) -> OscIn {
    OscIn::new(target, vec![Arg::F32(f32::from(pitch)), Arg::F32(0.0)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::{ring_pair, ControlDrain};
    use crate::diagnostics::Snapshot;
    use crate::midi::pattern::OscPattern;
    use crate::midi::routes::{Route, RouteTable, Scale};

    fn route(pattern: &str, kind: MidiKind, bind: &str, scale: Scale) -> Route {
        Route {
            pattern: OscPattern::parse(pattern).unwrap(),
            kind,
            target: format!("/{bind}/in"),
            scale,
        }
    }

    fn note_route(pattern: &str, bind: &str) -> Route {
        route(pattern, MidiKind::Note, bind, Scale::None)
    }

    fn table(routes: Vec<Route>) -> RouteTable {
        RouteTable { routes }
    }

    struct Rig {
        router: Router,
        drain: ControlDrain,
        diagnostics: Arc<Diagnostics>,
    }

    impl Rig {
        fn new(routes: Vec<Route>) -> Self {
            let diagnostics = Diagnostics::new();
            let (producer, ring) = ring_pair(Arc::clone(&diagnostics));
            let mut router = Router::new(producer, Arc::clone(&diagnostics), false);
            router.install(table(routes));
            Self {
                router,
                drain: ControlDrain::new(vec![ring]),
                diagnostics,
            }
        }

        /// Every batch pushed since the last call, each as its (address, args) messages.
        fn batches(&mut self) -> Vec<Vec<(String, Vec<Arg>)>> {
            let mut out = Vec::new();
            self.drain.drain(|batch| {
                out.push(
                    batch
                        .iter()
                        .map(|m| (m.address.clone(), m.args.clone()))
                        .collect(),
                );
                true
            });
            out
        }

        fn counters(&self) -> (u64, u64, u64, u64) {
            let Snapshot {
                midi_received,
                midi_unsupported,
                midi_routed,
                midi_unmatched,
                ..
            } = self.diagnostics.snapshot();
            (midi_received, midi_unsupported, midi_routed, midi_unmatched)
        }
    }

    fn off(target: &str, pitch: f32) -> (String, Vec<Arg>) {
        (target.to_string(), vec![Arg::F32(pitch), Arg::F32(0.0)])
    }

    #[test]
    fn routes_a_cc_to_every_matching_input() {
        let mut rig = Rig::new(vec![
            route(
                "/midi/1/cc/74",
                MidiKind::Cc,
                "a",
                Scale::Linear {
                    min: 0.0,
                    max: 10.0,
                    int: false,
                },
            ),
            route(
                "/midi/[1-2]/cc/74",
                MidiKind::Cc,
                "b",
                Scale::Linear {
                    min: 0.0,
                    max: 16.0,
                    int: true,
                },
            ),
            route(
                "/midi/3/cc/74",
                MidiKind::Cc,
                "c",
                Scale::Linear {
                    min: 0.0,
                    max: 1.0,
                    int: false,
                },
            ),
        ]);
        rig.router.bytes(&[0xB0, 74, 127]);
        assert_eq!(
            rig.batches(),
            vec![vec![
                ("/a/in".to_string(), vec![Arg::F32(10.0)]),
                ("/b/in".to_string(), vec![Arg::I32(16)]),
            ]]
        );
        assert_eq!(rig.counters(), (1, 0, 1, 0));
    }

    #[test]
    fn unmatched_is_counted_and_sends_nothing() {
        let mut rig = Rig::new(vec![]);
        rig.router.bytes(&[0x90, 60, 100]);
        rig.router.bytes(&[0xB0, 1, 1]);
        assert!(rig.batches().is_empty());
        assert_eq!(rig.counters(), (2, 0, 0, 2));
    }

    #[test]
    fn unsupported_and_ignored_bytes_are_told_apart() {
        let mut rig = Rig::new(vec![]);
        rig.router.bytes(&[0xA0, 60, 10]);
        rig.router.bytes(&[0xF8]);
        rig.router.bytes(&[0xFE]);
        assert!(rig.batches().is_empty());
        assert_eq!(rig.counters(), (1, 1, 0, 0));
    }

    #[test]
    fn install_releases_notes_whose_route_is_gone() {
        let mut rig = Rig::new(vec![note_route("/midi/1/note", "keys")]);
        rig.router.bytes(&[0x90, 60, 100]);
        rig.router.bytes(&[0x90, 64, 100]);
        rig.batches();
        rig.router
            .install(table(vec![note_route("/midi/1/note", "other")]));
        assert_eq!(
            rig.batches(),
            vec![vec![off("/keys/in", 60.0), off("/keys/in", 64.0)]]
        );
        // The device's note-off now finds nothing held for the new target, so nothing is sent.
        rig.router.bytes(&[0x80, 60, 0]);
        assert!(rig.batches().is_empty());
    }

    #[test]
    fn install_keeps_notes_whose_route_survives() {
        let mut rig = Rig::new(vec![note_route("/midi/1/note", "keys")]);
        rig.router.bytes(&[0x90, 60, 100]);
        rig.batches();
        rig.router
            .install(table(vec![note_route("/midi/[1-4]/note", "keys")]));
        assert!(rig.batches().is_empty());
        rig.router.bytes(&[0x80, 60, 0]);
        assert_eq!(rig.batches(), vec![vec![off("/keys/in", 60.0)]]);
    }

    #[test]
    fn install_releases_notes_whose_channel_no_longer_reaches_their_target() {
        let mut rig = Rig::new(vec![note_route("/midi/1/note", "keys")]);
        rig.router.bytes(&[0x90, 60, 100]);
        rig.batches();
        rig.router
            .install(table(vec![note_route("/midi/2/note", "keys")]));
        assert_eq!(rig.batches(), vec![vec![off("/keys/in", 60.0)]]);
    }

    #[test]
    fn two_routes_to_one_target_send_a_note_once() {
        let mut rig = Rig::new(vec![
            note_route("/midi/1/note", "keys"),
            note_route("/midi/[1-2]/note", "keys"),
        ]);
        rig.router.bytes(&[0x90, 60, 100]);
        rig.router.bytes(&[0x80, 60, 0]);
        assert_eq!(
            rig.batches(),
            vec![
                vec![(
                    "/keys/in".to_string(),
                    vec![Arg::F32(60.0), Arg::F32(100.0 / 127.0)]
                )],
                vec![off("/keys/in", 60.0)],
            ]
        );
    }

    #[test]
    fn retriggered_note_is_held_once() {
        let mut rig = Rig::new(vec![note_route("/midi/1/note", "keys")]);
        rig.router.bytes(&[0x90, 60, 100]);
        rig.router.bytes(&[0x90, 60, 90]);
        rig.router.bytes(&[0x80, 60, 0]);
        assert_eq!(rig.batches().len(), 3);
        rig.router.install(RouteTable::default());
        assert!(rig.batches().is_empty());
    }

    #[test]
    fn a_note_on_one_channel_is_not_released_by_another() {
        let mut rig = Rig::new(vec![note_route("/midi/*/note", "keys")]);
        rig.router.bytes(&[0x90, 60, 100]);
        rig.router.bytes(&[0x81, 60, 0]);
        assert_eq!(rig.batches().len(), 1);
        rig.router.device_lost();
        assert_eq!(rig.batches(), vec![vec![off("/keys/in", 60.0)]]);
    }

    #[test]
    fn a_lost_device_releases_each_held_note() {
        let mut rig = Rig::new(vec![
            note_route("/midi/1/note", "keys"),
            note_route("/midi/2/note", "bass"),
        ]);
        rig.router.bytes(&[0x90, 60, 100]);
        rig.router.bytes(&[0x91, 40, 100]);
        rig.batches();
        rig.router.device_lost();
        assert_eq!(
            rig.batches(),
            vec![vec![off("/bass/in", 40.0), off("/keys/in", 60.0)]]
        );
        rig.router.device_lost();
        assert!(rig.batches().is_empty());
    }

    #[test]
    fn a_lost_device_is_counted() {
        let mut rig = Rig::new(vec![]);
        rig.router.device_lost();
        rig.router.device_lost();
        assert_eq!(rig.diagnostics.snapshot().midi_devices_lost, 2);
    }

    #[test]
    fn injected_messages_route_like_device_ones() {
        let mut rig = Rig::new(vec![note_route("/midi/1/note", "keys")]);
        let Decoded::Message(t) = crate::midi::translate::translate(&[0x90, 60, 127]) else {
            unreachable!()
        };
        rig.router.translated(&t);
        assert_eq!(
            rig.batches(),
            vec![vec![(
                "/keys/in".to_string(),
                vec![Arg::F32(60.0), Arg::F32(1.0)]
            )]]
        );
        assert_eq!(rig.counters(), (1, 0, 1, 0));
    }
}
