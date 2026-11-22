//! The playing panel: one control per interface input of the open instrument, and an on-screen
//! keyboard for its note inputs.
//!
//! The panel comes from the open instrument's surface doc when the server has one for it, and is
//! generated from the interface inputs otherwise. A surface doc that cannot be used falls back to
//! the generated panel and says why.
//!
//! Controls send live values to the running engine. They never write the instrument document, so
//! what a control is set to lasts until the next Swap.

mod model;
mod surface;
mod view;

use std::path::Path;

use eframe::egui;
use reuben_api::authoring::{Boundary, DescribeBoundary};
use reuben_api::engine::{call_tool, Channel};
use reuben_api::tools::names;

use self::model::{Control, Kind, Panel};
use crate::engine::Engine;
use crate::sends::Replies;

/// The lowest key the on-screen keyboard starts on: C3.
const DEFAULT_LOW_KEY: u8 = 48;

/// A panel and where it came from.
#[derive(Debug)]
struct Loaded {
    panel: Panel,
    /// Whether the panel was resolved from a surface doc; a generated panel was not.
    from_surface: bool,
}

/// Build the panel for the instrument the server names `top`, which it has open, asking the server
/// through `channel` for its boundary and its surface doc.
///
/// `Err` is the instrument itself failing to load: it has no interface to build a panel from.
fn load(channel: &Channel, top: &str) -> Result<Loaded, String> {
    let args = DescribeBoundary {
        source: top.to_string(),
    };
    let answer =
        call_tool(names::DESCRIBE_BOUNDARY, &args, channel).map_err(|refusal| refusal.message)?;
    let boundary: Boundary = serde_json::from_value(answer.output)
        .map_err(|e| format!("the server's boundary could not be read: {e}"))?;

    let unusable = |why: String| {
        let mut panel = Panel::generated(&boundary);
        let file = Path::new(top).file_name().unwrap_or_default();
        panel.warnings.insert(
            0,
            format!(
                "the surface doc of {} {why} — showing the generated panel instead",
                file.to_string_lossy()
            ),
        );
        Ok(Loaded {
            panel,
            from_surface: false,
        })
    };
    match channel.surface() {
        Ok(None) => Ok(Loaded {
            panel: Panel::generated(&boundary),
            from_surface: false,
        }),
        Ok(Some(text)) => match surface::SurfaceDoc::parse(&text) {
            // The server answers for the instrument it has open, which an attached engine can
            // change between the two reads.
            Ok(doc) if doc.instrument != boundary.instrument => unusable(format!(
                "is for instrument `{}`, not `{}`",
                doc.instrument, boundary.instrument
            )),
            Ok(doc) => Ok(Loaded {
                panel: Panel::from_surface(&doc, &boundary),
                from_surface: true,
            }),
            Err(e) => unusable(e.to_string()),
        },
        Err(e) => unusable(format!("could not be read: {e}")),
    }
}

/// What one control is currently set to. Lives in the window only: the engine is told each change
/// and the document is never written.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Setting {
    Value(f64),
    On(bool),
    Choice(usize),
    /// The key held on a keyboard, if any.
    Key(Option<u8>),
}

impl Setting {
    fn at_rest(control: &Control) -> Self {
        match &control.kind {
            Kind::Value { rest, .. } => Self::Value(*rest),
            Kind::Toggle { rest } => Self::On(*rest),
            Kind::NoteToggle { .. } | Kind::ChordButton { .. } => Self::On(false),
            Kind::Choice { rest, .. } => Self::Choice(*rest),
            Kind::Keyboard => Self::Key(None),
        }
    }
}

/// The controls panel of the open instrument, with what each control is set to.
pub struct Controls {
    loaded: Option<Result<Loaded, String>>,
    settings: Vec<Setting>,
    low_key: u8,
    /// Why the last send failed, until one succeeds.
    send_error: Option<String>,
    replies: Replies,
}

impl Controls {
    /// The panel for the open instrument the server names `top`, read through `channel`; no panel
    /// when no instrument is open or no engine is running.
    pub fn open(top: Option<&str>, channel: &dyn Fn() -> Option<Channel>) -> Self {
        let loaded = top.and_then(|top| channel().map(|channel| load(&channel, top)));
        let settings = match &loaded {
            Some(Ok(loaded)) => loaded.panel.controls.iter().map(Setting::at_rest).collect(),
            _ => Vec::new(),
        };
        Self {
            loaded,
            settings,
            low_key: DEFAULT_LOW_KEY,
            send_error: None,
            replies: Replies::default(),
        }
    }

    /// The interface input each control of the panel drives, in the panel's order.
    #[cfg(test)]
    pub fn binds(&self) -> Vec<&str> {
        match &self.loaded {
            Some(Ok(loaded)) => (loaded.panel.controls.iter())
                .map(|c| c.bind.as_str())
                .collect(),
            _ => Vec::new(),
        }
    }

    /// What the control driving `bind` is set to, when it is set to a value.
    #[cfg(test)]
    pub fn value(&self, bind: &str) -> Option<f64> {
        let Some(Ok(loaded)) = &self.loaded else {
            return None;
        };
        let at = (loaded.panel.controls.iter()).position(|c| c.bind == bind)?;
        match self.settings.get(at)? {
            Setting::Value(value) => Some(*value),
            _ => None,
        }
    }

    /// Draw the panel along the bottom of `ui`, with `beside` down its right edge, and send what
    /// the player did to `engine`. Draws nothing when no instrument is open, and returns whether it
    /// drew.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        engine: &Engine,
        beside: impl FnOnce(&mut egui::Ui),
    ) -> bool {
        self.heard();
        let Some(loaded) = &self.loaded else {
            return false;
        };
        let live = engine.live();
        egui::Panel::bottom("controls")
            .resizable(true)
            .default_size(320.0)
            .show(ui, |ui| {
                // First, so the controls take what is left of the panel's width.
                egui::Panel::right("beside_controls")
                    .resizable(false)
                    .show(ui, beside);
                let loaded = match loaded {
                    Ok(loaded) => loaded,
                    Err(e) => {
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            format!("No controls: the instrument did not load. {e}"),
                        );
                        return;
                    }
                };
                view::header(ui, loaded.from_surface, live.is_some());
                if let Some(e) = &self.send_error {
                    ui.colored_label(ui.visuals().error_fg_color, format!("send failed: {e}"));
                }
                let messages = egui::ScrollArea::both()
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        view::warnings(ui, &loaded.panel.warnings);
                        ui.add_enabled_ui(live.is_some(), |ui| {
                            view::panel(ui, &loaded.panel, &mut self.settings, &mut self.low_key)
                        })
                        .inner
                    })
                    .inner;
                if let Some(live) = &live {
                    live.send(messages, &self.replies);
                }
            });
        // A send made now is answered here; one queued for an attached engine on a later frame.
        self.heard();
        true
    }

    fn heard(&mut self) {
        if let Some(result) = self.replies.latest() {
            self.send_error = result.err();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::UdpSocket;
    use std::path::PathBuf;
    use std::time::Duration;

    use reuben_api::authoring::describe_boundary;
    use reuben_api::render::Arg;
    use reuben_api::FsResolver;
    use reuben_native::engine::{EngineConfig, Instrument};
    use reuben_native::osc;
    use reuben_native::test_support::{start_headless, within};

    use super::model::Gesture;
    use super::*;
    use reuben_api::engine::{ControlSendMessage, MAX_SEND_BATCH};

    use crate::sends::send;

    const HANG_SECS: u64 = 20;

    fn repo() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn scratch(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "reuben_editor_controls_{test}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("instruments")).expect("an instruments dir");
        std::fs::create_dir_all(dir.join("surfaces")).expect("a surfaces dir");
        dir
    }

    /// An instrument whose two interface inputs each feed an `osc_out` node, so a live value that
    /// reached the render side comes back out on OSC-out under that node's address.
    const PROBE: &str = r#"{"format_version":3,"instrument":"probe",
        "interface":{"inputs":{"level":{"type":"f32","default":0.2,"min":0.0,"max":1.0},
                               "notes":{"type":"note"}},
                     "outputs":{"out":{"from":"/osc.audio"}}},
        "nodes":[{"type":"oscillator","address":"/osc"},
                 {"type":"osc_out","address":"/level_echo","inputs":{"in":{"from":"/level"}}},
                 {"type":"osc_out","address":"/note_echo","inputs":{"in":{"from":"/notes"}}}]}"#;

    fn headless(instrument: &Path, osc_out: Option<String>) -> Engine {
        Engine::start_with(
            EngineConfig {
                osc_in: None,
                osc_out,
                engine_addr: None,
                ..EngineConfig::new(Instrument::Path(instrument.to_path_buf()))
            },
            start_headless,
        )
    }

    /// The panel `instrument` has on an engine started on it, and that engine.
    fn served(instrument: &Path, osc_out: Option<String>) -> (Result<Loaded, String>, Engine) {
        let engine = headless(instrument, osc_out);
        let channel = (engine.channel())
            .unwrap_or_else(|| panic!("{}: {:?}", instrument.display(), engine.report()));
        (load(&channel, &instrument.display().to_string()), engine)
    }

    fn control<'a>(panel: &'a Panel, bind: &str) -> &'a Control {
        panel
            .controls
            .iter()
            .find(|c| c.bind == bind)
            .unwrap_or_else(|| panic!("a control bound to `{bind}`"))
    }

    /// Wait for each of `expected`, in order, among the OSC-out messages for `address`. Anything
    /// else on that address is passed over: an input echoes the value it rests at when the engine
    /// starts, ahead of anything sent to it.
    fn await_echoes(sink: &UdpSocket, address: &str, expected: &[Vec<Arg>]) {
        let mut expected = expected.iter().peekable();
        let mut buf = [0u8; 1024];
        while expected.peek().is_some() {
            let n = sink
                .recv(&mut buf)
                .unwrap_or_else(|e| panic!("no echo of {:?} on {address}: {e}", expected.peek()));
            for message in osc::decode(&buf[..n]).expect("decode") {
                if message.address == address && expected.peek() == Some(&&message.args) {
                    expected.next();
                }
            }
        }
    }

    #[test]
    fn a_control_move_and_a_keyboard_note_reach_the_render_side() {
        let dir = scratch("round_trip");
        let instrument = dir.join("instruments/probe.json");
        std::fs::write(&instrument, PROBE).expect("the probe instrument");

        within(HANG_SECS, move || {
            let sink = UdpSocket::bind("127.0.0.1:0").expect("an OSC-out sink");
            sink.set_read_timeout(Some(Duration::from_secs(5)))
                .expect("a read timeout");
            let target = sink.local_addr().expect("bound").to_string();
            let (loaded, mut engine) = served(&instrument, Some(target));
            let loaded = loaded.expect("the probe loads");
            assert!(!loaded.from_surface);
            assert_eq!(loaded.panel.warnings, Vec::<String>::new());
            let channel = engine.channel().expect("the engine is running");

            let fader = control(&loaded.panel, "level");
            send(&channel, vec![fader.message(Gesture::Set(0.75)).unwrap()]).expect("sent");
            await_echoes(&sink, "/level_echo", &[vec![Arg::F32(0.75)]]);

            let keyboard = control(&loaded.panel, "notes");
            for down in [true, false] {
                let key = Gesture::Key { midi: 64, down };
                send(&channel, vec![keyboard.message(key).unwrap()]).expect("sent");
            }
            await_echoes(
                &sink,
                "/note_echo",
                &[
                    vec![Arg::F32(64.0), Arg::F32(1.0)],
                    vec![Arg::F32(64.0), Arg::F32(0.0)],
                ],
            );

            // A chord button's pitch is a scale degree, and has to arrive as one.
            let chord = Control {
                kind: Kind::ChordButton { degree: 4 },
                ..keyboard.clone()
            };
            for on in [true, false] {
                let press = Gesture::Switch(on);
                send(&channel, vec![chord.message(press).unwrap()]).expect("sent");
            }
            await_echoes(
                &sink,
                "/note_echo",
                &[
                    vec![Arg::I32(4), Arg::F32(1.0)],
                    vec![Arg::I32(4), Arg::F32(0.0)],
                ],
            );
            engine.shutdown();
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The gesture that sets a control to where it already rests, plus the release of anything it
    /// could be holding: every message a control can send, without changing what the engine plays.
    fn rest_gestures(control: &Control) -> Vec<Gesture> {
        match Setting::at_rest(control) {
            Setting::Value(value) => vec![Gesture::Set(value)],
            Setting::On(on) => vec![Gesture::Switch(on)],
            Setting::Choice(index) => vec![Gesture::Pick(index)],
            Setting::Key(_) => vec![Gesture::Key {
                midi: 60,
                down: false,
            }],
        }
    }

    #[test]
    fn every_library_instrument_gets_a_panel_whose_controls_send() {
        let library = repo().join("instruments");
        let mut instruments: Vec<PathBuf> = std::fs::read_dir(&library)
            .expect("the instrument library")
            .map(|entry| entry.expect("a directory entry").path())
            .filter(|path| path.extension().is_some_and(|e| e == "json"))
            .collect();
        instruments.sort();
        assert!(instruments.len() >= 9, "{instruments:?}");

        for instrument in instruments {
            let name = instrument
                .file_stem()
                .expect("a file name")
                .to_string_lossy()
                .into_owned();
            // Every message the panel can send, sent to the engine it was read from.
            let loaded = within(HANG_SECS, {
                let (instrument, name) = (instrument.clone(), name.clone());
                move || {
                    let (loaded, mut engine) = served(&instrument, None);
                    let messages: Vec<ControlSendMessage> = (loaded.iter())
                        .flat_map(|loaded| &loaded.panel.controls)
                        .flat_map(|control| {
                            rest_gestures(control)
                                .into_iter()
                                .map(|gesture| control.message(gesture).expect("a message"))
                        })
                        .collect();
                    if !messages.is_empty() {
                        let channel = engine.channel().expect("the engine is running");
                        send(&channel, messages).unwrap_or_else(|e| panic!("{name}: {e}"));
                    }
                    engine.shutdown();
                    loaded
                }
            });
            let loaded = loaded.unwrap_or_else(|e| panic!("{name}: {e}"));
            let surface = repo().join("surfaces").join(format!("{name}.json"));
            assert_eq!(
                loaded.from_surface,
                surface.is_file(),
                "{name}: a panel comes from the surface doc exactly when there is one"
            );
            let described = describe_boundary(
                &DescribeBoundary {
                    source: instrument.display().to_string(),
                },
                &FsResolver::for_document(&instrument.display().to_string()).stat_only(),
            )
            .expect("the boundary")
            .output;
            // A surface doc curates, so nothing in it may go unresolved; a generated panel says
            // which inputs it left out, and in the library those are the audio inputs.
            let left_out = described
                .inputs
                .iter()
                .filter(|port| port.channel.is_some() && !loaded.from_surface)
                .count();
            assert_eq!(
                loaded.panel.warnings.len(),
                left_out,
                "{name}: {:?}",
                loaded.panel.warnings
            );
            let playable = described
                .inputs
                .iter()
                .filter(|port| port.channel.is_none())
                .count();
            assert_eq!(
                loaded.panel.controls.is_empty(),
                playable == 0,
                "{name}: controls exactly when there is an input to play"
            );
            for control in &loaded.panel.controls {
                assert!(
                    described.inputs.iter().any(|p| p.name == control.bind),
                    "{name}: `{}` is an interface input",
                    control.bind
                );
            }
            let rows: Vec<usize> = loaded.panel.rows().into_iter().flatten().collect();
            assert_eq!(rows, (0..loaded.panel.controls.len()).collect::<Vec<_>>());
        }
    }

    /// The panel of the library instrument `name`, as the server it is open on has it.
    fn library_panel(name: &str) -> Loaded {
        let instrument = repo().join("instruments").join(format!("{name}.json"));
        within(HANG_SECS, move || {
            let (loaded, mut engine) = served(&instrument, None);
            engine.shutdown();
            loaded.expect("loads")
        })
    }

    #[test]
    fn the_library_surfaces_resolve_to_the_widgets_they_name() {
        let chord = library_panel("chord-player");
        let degrees: Vec<i32> = chord
            .panel
            .controls
            .iter()
            .filter_map(|c| match c.kind {
                Kind::ChordButton { degree } => Some(degree),
                _ => None,
            })
            .collect();
        assert_eq!(degrees, (0..7).collect::<Vec<i32>>());

        let groovebox = library_panel("groovebox");
        let steps = groovebox
            .panel
            .controls
            .iter()
            .filter(|c| matches!(c.kind, Kind::Toggle { .. }))
            .count();
        assert_eq!(steps, 48);
        assert!(groovebox.panel.rows().iter().any(|row| row.len() == 16));

        let generated = library_panel("mic-space");
        assert!(!generated.from_surface);
        assert_eq!(generated.panel.controls, Vec::new());
    }

    #[test]
    fn the_controls_panel_reads_its_surface_from_the_server() {
        let dir = scratch("from_server");
        let instrument = dir.join("instruments/probe.json");
        std::fs::write(&instrument, PROBE).expect("the probe instrument");
        std::fs::write(
            dir.join("surfaces/probe.json"),
            r#"{"surface_version":1,"instrument":"probe",
                "controls":[{"bind":"level","label":"Loudness","widget":"radial"}]}"#,
        )
        .expect("a surface doc");
        within(HANG_SECS, move || {
            let engine = headless(&instrument, None);
            let top = instrument.display().to_string();
            let controls = Controls::open(Some(&top), &|| engine.channel());
            let Some(Ok(loaded)) = &controls.loaded else {
                panic!("no panel: {:?}", controls.loaded)
            };
            assert!(loaded.from_surface);
            assert_eq!(loaded.panel.warnings, Vec::<String>::new());
            assert_eq!(controls.binds(), ["level"]);
            assert_eq!(loaded.panel.controls[0].label, "Loudness");
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_surface_doc_is_reported_and_the_default_panel_shown() {
        let dir = scratch("fallback");
        let instrument = dir.join("instruments/probe.json");
        std::fs::write(&instrument, PROBE).expect("the probe instrument");
        let surface = dir.join("surfaces/probe.json");
        within(HANG_SECS, move || {
            let mut engine = headless(&instrument, None);
            let channel = engine.channel().expect("the engine is running");
            let top = instrument.display().to_string();
            for (text, why) in [
                ("{ not json", "is not a surface doc"),
                (
                    r#"{"surface_version":2,"instrument":"probe","controls":[]}"#,
                    "surface_version 2",
                ),
            ] {
                std::fs::write(&surface, text).expect("a broken surface doc");
                let loaded = load(&channel, &top).expect("the instrument still loads");
                assert!(!loaded.from_surface);
                assert_eq!(loaded.panel.controls.len(), 2, "the generated panel");
                assert_eq!(
                    loaded.panel.warnings.len(),
                    1,
                    "{:?}",
                    loaded.panel.warnings
                );
                let warning = &loaded.panel.warnings[0];
                assert!(warning.contains(why), "{warning}");
                assert!(warning.contains("probe.json"), "{warning}");
                assert!(warning.contains("generated panel"), "{warning}");
            }
            engine.shutdown();
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The server's surface doc is the open instrument's: read for another instrument, it is not
    /// that one's panel.
    #[test]
    fn a_surface_doc_for_another_instrument_is_not_used() {
        let dir = scratch("other_surface");
        let instrument = dir.join("instruments/probe.json");
        std::fs::write(&instrument, PROBE).expect("the probe instrument");
        std::fs::write(
            dir.join("surfaces/probe.json"),
            r#"{"surface_version":1,"instrument":"probe",
                "controls":[{"bind":"level","label":"Loudness"}]}"#,
        )
        .expect("a surface doc");
        let other = dir.join("instruments/other.json");
        std::fs::write(&other, PROBE.replace(r#""probe""#, r#""other""#)).expect("another");
        within(HANG_SECS, move || {
            let mut engine = headless(&instrument, None);
            let channel = engine.channel().expect("the engine is running");
            let loaded = load(&channel, &other.display().to_string()).expect("it loads");
            assert!(!loaded.from_surface);
            assert_eq!(loaded.panel.controls.len(), 2, "the generated panel");
            let warning = &loaded.panel.warnings[0];
            assert!(warning.contains("`probe`, not `other`"), "{warning}");
            assert!(warning.contains("generated panel"), "{warning}");
            engine.shutdown();
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_instrument_that_does_not_load_has_no_panel() {
        let dir = scratch("unloadable");
        let instrument = dir.join("instruments/probe.json");
        std::fs::write(&instrument, PROBE).expect("the probe instrument");
        let broken = dir.join("instruments/broken.json");
        std::fs::write(&broken, r#"{"format_version":3,"nodes":[{"#).expect("a broken doc");
        let missing = dir.join("instruments/missing.json");
        within(HANG_SECS, move || {
            let mut engine = headless(&instrument, None);
            let channel = engine.channel().expect("the engine is running");
            let name = |path: &Path| path.display().to_string();
            assert!(load(&channel, &name(&broken)).is_err());
            assert!(load(&channel, &name(&missing)).is_err());

            let controls = Controls::open(Some(&name(&broken)), &|| engine.channel());
            assert!(matches!(controls.loaded, Some(Err(_))));
            assert!(Controls::open(None, &|| engine.channel()).loaded.is_none());
            let no_engine = Controls::open(Some(&name(&instrument)), &|| None);
            assert!(no_engine.loaded.is_none(), "no engine, no panel");
            engine.shutdown();
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_batch_longer_than_the_engine_takes_at_once_is_sent_in_pieces() {
        let dir = scratch("long_batch");
        let instrument = dir.join("instruments/probe.json");
        std::fs::write(&instrument, PROBE).expect("the probe instrument");
        within(HANG_SECS, move || {
            let (loaded, mut engine) = served(&instrument, None);
            let loaded = loaded.expect("loads");
            let fader = control(&loaded.panel, "level");
            let messages = vec![fader.message(Gesture::Set(0.5)).unwrap(); MAX_SEND_BATCH + 1];
            send(&engine.channel().expect("running"), messages).expect("sent");
            engine.shutdown();
        });
        let _ = std::fs::remove_dir_all(&dir);
    }
}
