//! The editor's whole frame driven through `egui_kittest`: widgets found by their AccessKit labels,
//! clicked and typed into, with no window and no audio device.

use std::path::{Path, PathBuf};

use clap::Parser;
use eframe::egui;
use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;
use reuben_api::authoring::{
    add_instrument_interface_input, new_instrument, AddInstrumentInterfaceInput, NewInstrument,
};
use reuben_api::FsResolver;
use serde_json::json;

use super::*;
use crate::audio::tests::two_devices;
use crate::canvas::Item;
use crate::config::test_support::TempConfig;
use crate::feedback::Feedback;
use crate::session::test_support::Scratch;

/// An instrument at `lib/ui.json` with one `f32` interface input, `gain`, at 0.5 in 0..1.
fn instrument(scratch: &Scratch) -> PathBuf {
    let lib = scratch.path("lib");
    std::fs::create_dir_all(&lib).expect("create the library directory");
    let top = lib.join("ui.json");
    let source = top.display().to_string();
    let store = FsResolver::for_document(&source);
    let seeded = new_instrument(
        &NewInstrument {
            source: source.clone(),
            name: "ui".to_string(),
        },
        &store,
    )
    .expect("the seed is written");
    assert!(seeded.output.written, "{:?}", seeded.output.report);
    let added = add_instrument_interface_input(
        &AddInstrumentInterfaceInput {
            source,
            name: "gain".to_string(),
            type_name: "f32".to_string(),
            value: Some(json!(0.5)),
            min: Some(0.0),
            max: Some(1.0),
            ..Default::default()
        },
        &store,
    )
    .expect("the input is added");
    assert!(added.output.written, "{:?}", added.output.report);
    top
}

/// The editor open on `top` with no engine: `main`'s construction, repeated with test stand-ins.
fn editor(top: &Path, config: &TempConfig) -> Editor {
    let args = Args::parse_from(["reuben-editor"]);
    let session = Session::open(top, None, || {});
    let mut canvas = Canvas::open(None, None);
    let mut inspector = Inspector::default();
    view(&mut canvas, &mut inspector, &session);
    let mut start = Start::new(config.dir(), None);
    start.opened(top);
    Editor {
        engine: Engine::idle(),
        audio: AudioSettings::load_with(config.dir(), two_devices),
        controls: Controls::open(Some(top), None),
        canvas,
        inspector,
        session: Some(session),
        start,
        launch: Launch {
            root: None,
            flags: args.engine,
            swap_hold: None,
        },
        held: HeldSwaps::new(|| {}),
        meters: MeterStrip::new(|| {}),
    }
}

fn harness(editor: Editor) -> Harness<'static, Editor> {
    let mut editor = Some(editor);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 800.0))
        .build_eframe(move |cc| {
            theme::install(&cc.egui_ctx);
            editor.take().expect("built once")
        });
    harness.run_steps(4);
    harness
}

/// `gain`'s range as the session reads the document now.
fn range(harness: &Harness<'_, Editor>) -> (Option<f64>, Option<f64>) {
    let session = harness.state().session.as_ref().expect("a session");
    let top = session.viewed().read.as_ref().expect("the document reads");
    let gain = (top.inputs.iter())
        .find(|p| p.port.name == "gain")
        .expect("gain is an input");
    (gain.port.min, gain.port.max)
}

/// Select all of the focused box labelled `label` and type `text` over it.
fn replace_text(harness: &mut Harness<'_, Editor>, label: &str, text: &str) {
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness.run_steps(1);
    harness.get_by_label(label).type_text(text);
    harness.run_steps(2);
}

#[test]
fn typing_min_then_clicking_max_writes_both() {
    let scratch = Scratch::new("ui_min_then_max");
    let config = TempConfig::new();
    let top = instrument(&scratch);
    let mut editor = editor(&top, &config);
    editor.canvas.select(Some(Item::Input("gain".to_string())));
    let mut harness = harness(editor);

    harness.get_by_label("min").focus();
    harness.run_steps(2);
    replace_text(&mut harness, "min", "0.1");
    // Focus leaves min for max in one frame: min is written as max takes focus.
    harness.get_by_label("max").click();
    harness.run_steps(2);
    replace_text(&mut harness, "max", "2");
    harness.key_press(egui::Key::Enter);
    harness.run_steps(4);

    let session = harness.state().session.as_ref().expect("a session");
    assert_eq!(session.feedback(), None);
    assert_eq!(range(&harness), (Some(0.1), Some(2.0)));
}

#[test]
fn a_key_typed_the_frame_after_clicking_from_min_to_max_still_lands() {
    let scratch = Scratch::new("ui_click_then_type");
    let config = TempConfig::new();
    let top = instrument(&scratch);
    let mut editor = editor(&top, &config);
    editor.canvas.select(Some(Item::Input("gain".to_string())));
    let mut harness = harness(editor);

    harness.get_by_label("min").focus();
    harness.run_steps(2);
    replace_text(&mut harness, "min", "0.1");
    // The frame min is written in, max takes focus. The editor draws a frame only on input, so the
    // next frame can be the one carrying the first key.
    harness.get_by_label("max").click();
    harness.run_steps(1);
    harness.get_by_label("max").type_text("0");
    harness.run_steps(1);
    harness.key_press(egui::Key::Enter);
    harness.run_steps(4);

    let session = harness.state().session.as_ref().expect("a session");
    assert_eq!(session.feedback(), None);
    assert_eq!(range(&harness), (Some(0.1), Some(10.0)));
}

#[test]
fn the_edit_banner_leaves_the_inspector_uncovered() {
    let scratch = Scratch::new("ui_banner");
    let config = TempConfig::new();
    let top = instrument(&scratch);
    let mut editor = editor(&top, &config);
    editor.canvas.select(Some(Item::Input("gain".to_string())));
    let session = editor.session.as_mut().expect("a session");
    *session.feedback_mut() = Some(Feedback::Conflict {
        source: top.display().to_string(),
    });
    let mut harness = harness(editor);
    harness.run_steps(2);

    // The inspector's heading, the selected pipe's name, sits at its panel's left margin.
    let inspector = harness.get_by_label("gain").rect().left();
    for part in ["did not land", "Dismiss"] {
        let rect = harness.get_by_label_contains(part).rect();
        assert!(
            rect.right() <= inspector,
            "the banner's {part:?} at {rect:?} reaches the inspector at x = {inspector}"
        );
    }
}

/// Write an instrument at `lib/attached.json`, `level` resting at `level`, whose `level` input
/// comes back out on OSC-out.
fn echoing(scratch: &Scratch, level: f64) -> PathBuf {
    let lib = scratch.path("lib");
    std::fs::create_dir_all(&lib).expect("create the library directory");
    let top = lib.join("attached.json");
    let text = json!({
        "format_version": 3,
        "instrument": "attached",
        "interface": {
            "inputs": {"level": {"type": "f32", "default": level, "min": 0.0, "max": 1.0}},
            "outputs": {"out": {"from": "/osc.audio"}}
        },
        "nodes": [
            {"type": "oscillator", "address": "/osc"},
            {"type": "osc_out", "address": "/level_echo", "inputs": {"in": {"from": "/level"}}}
        ]
    })
    .to_string();
    std::fs::write(&top, text).expect("write the instrument");
    top
}

/// The editor as `main` builds it for `--attach addr`, showing `session` if it is given one.
fn attached(addr: String, config: &TempConfig, session: Option<Session>) -> Editor {
    let args = Args::parse_from(["reuben-editor", "--attach", &addr]);
    Editor {
        engine: Engine::attach(addr, || {}),
        audio: AudioSettings::load_with(config.dir(), two_devices),
        controls: Controls::open(None, None),
        canvas: Canvas::open(None, None),
        inspector: Inspector::default(),
        session,
        start: Start::new(config.dir(), None),
        launch: Launch {
            root: None,
            flags: args.engine,
            swap_hold: None,
        },
        held: HeldSwaps::new(|| {}),
        meters: MeterStrip::new(|| {}),
    }
}

/// The `level` resting value of the document `played` has installed.
fn installed_level(played: &reuben_native::engine::RunningEngine) -> serde_json::Value {
    let installed = played.channel().get_document().unwrap().document;
    installed["interface"]["inputs"]["level"]["default"].clone()
}

/// Step the harness until `done`, sleeping between steps so the link's thread and the session's
/// Swap get to run.
fn step_until(harness: &mut Harness<'_, Editor>, what: &str, done: impl Fn(&Editor) -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !done(harness.state()) {
        assert!(std::time::Instant::now() < deadline, "never {what}");
        harness.run_steps(1);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn an_editor_attached_to_a_running_engine_plays_it_and_swaps_its_saves() {
    use egui::accesskit::Role;
    use reuben_api::render::Arg;
    use reuben_native::engine::EngineConfig;
    use reuben_native::osc;
    use reuben_native::test_support::{start_headless, within};

    let scratch = Scratch::new("ui_attached");
    let config = TempConfig::new();
    let top = echoing(&scratch, 0.2);
    let sink = std::net::UdpSocket::bind("127.0.0.1:0").expect("an OSC-out sink");
    sink.set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .expect("a read timeout");
    let target = sink.local_addr().expect("bound").to_string();

    within(crate::engine::test_support::HANG_SECS, move || {
        // `reuben play`, started by someone else.
        let played = start_headless(EngineConfig {
            osc_out: Some(target),
            ..crate::engine::test_support::config(top.clone())
        })
        .expect("the engine starts");
        let addr = played.structure_addr().expect("bound").to_string();

        let mut harness = harness(attached(addr, &config, None));

        step_until(&mut harness, "opened what the engine plays", |editor| {
            editor.session.is_some()
        });
        assert_eq!(harness.state().session.as_ref().unwrap().top(), top);
        assert_eq!(harness.state().controls.binds(), vec!["level"]);
        harness.get_by_label(&format!(
            "Attached to the engine on {}",
            played.structure_addr().unwrap()
        ));

        // Moving the fader reaches the engine it is attached to.
        harness.get_by_role(Role::Slider).focus();
        harness.run_steps(1);
        harness.key_press(egui::Key::ArrowRight);
        harness.run_steps(2);
        let mut buf = [0u8; 1024];
        loop {
            let n = sink.recv(&mut buf).expect("the level came back");
            let echoed = osc::decode(&buf[..n]).expect("decode");
            if echoed.iter().any(|m| m.args != vec![Arg::F32(0.2)]) {
                break;
            }
        }

        // Saving the file Swaps it in. The engine holds the document as it migrated it, so the
        // saved value is what is compared.
        echoing(&scratch, 0.7);
        step_until(&mut harness, "swapped the save in", |_| {
            installed_level(&played) == json!(0.7)
        });

        harness.state_mut().engine.shutdown();
        played
            .channel()
            .ping()
            .expect("closing the editor stopped nothing");
        reuben_native::diagnostics::log_snapshot(&played.shutdown());
    });
}

/// A proxy in front of the structure channel at `engine` that forwards every exchange, except that
/// it holds each `Send` while `stalled` is set, as a stopped `reuben play` would. Counts the sends
/// that reached it.
struct Stalling {
    addr: String,
    stalled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    sends: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl Stalling {
    fn before(engine: String) -> Self {
        use std::io::{BufRead, BufReader, Write};
        use std::sync::atomic::Ordering;
        use std::sync::Arc;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a proxy port");
        let addr = listener.local_addr().expect("bound").to_string();
        let stalled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let sends = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        std::thread::spawn({
            let (stalled, sends) = (Arc::clone(&stalled), Arc::clone(&sends));
            move || {
                for client in listener.incoming() {
                    let Ok(client) = client else { return };
                    let (stalled, sends, engine) =
                        (Arc::clone(&stalled), Arc::clone(&sends), engine.clone());
                    std::thread::spawn(move || {
                        let mut line = String::new();
                        if BufReader::new(&client).read_line(&mut line).is_err() {
                            return;
                        }
                        let request = serde_json::from_str(&line);
                        if let Ok(reuben_api::engine::Request::Send { .. }) = request {
                            sends.fetch_add(1, Ordering::AcqRel);
                            while stalled.load(Ordering::Acquire) {
                                std::thread::sleep(std::time::Duration::from_millis(1));
                            }
                        }
                        let Ok(upstream) = std::net::TcpStream::connect(&engine) else {
                            return;
                        };
                        let _ = (&upstream).write_all(line.as_bytes());
                        let mut reply = String::new();
                        let _ = BufReader::new(&upstream).read_line(&mut reply);
                        let _ = (&client).write_all(reply.as_bytes());
                    });
                }
            }
        });
        Self {
            addr,
            stalled,
            sends,
        }
    }
}

#[test]
fn a_fader_moved_while_the_attached_engine_stalls_never_holds_the_window() {
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    use egui::accesskit::Role;
    use reuben_api::render::Arg;
    use reuben_native::engine::EngineConfig;
    use reuben_native::osc;
    use reuben_native::test_support::{start_headless, within};

    let scratch = Scratch::new("ui_attached_stall");
    let config = TempConfig::new();
    let top = echoing(&scratch, 0.2);
    let sink = std::net::UdpSocket::bind("127.0.0.1:0").expect("an OSC-out sink");
    sink.set_read_timeout(Some(Duration::from_secs(5)))
        .expect("a read timeout");
    let target = sink.local_addr().expect("bound").to_string();

    within(crate::engine::test_support::HANG_SECS, move || {
        let played = start_headless(EngineConfig {
            osc_out: Some(target),
            ..crate::engine::test_support::config(top.clone())
        })
        .expect("the engine starts");
        let proxy = Stalling::before(played.structure_addr().expect("bound").to_string());
        let mut harness = harness(attached(proxy.addr.clone(), &config, None));
        step_until(&mut harness, "opened what the engine plays", |editor| {
            editor.session.is_some()
        });

        proxy.stalled.store(true, Ordering::Release);
        for press in 0..5 {
            harness.get_by_role(Role::Slider).focus();
            harness.run_steps(1);
            let started = Instant::now();
            harness.key_press(egui::Key::ArrowRight);
            harness.run_steps(1);
            assert!(
                started.elapsed() < Duration::from_millis(500),
                "a frame waited {:?} on the stalled engine",
                started.elapsed()
            );
            if press == 0 {
                // The rest of the drag queues behind the first send, held at the engine.
                let deadline = Instant::now() + Duration::from_secs(5);
                while proxy.sends.load(Ordering::Acquire) == 0 {
                    assert!(Instant::now() < deadline, "the first send never left");
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
        let newest = harness.state().controls.value("level").expect("a value");
        assert!(newest > 0.24, "the fader moved on each press, to {newest}");
        proxy.stalled.store(false, Ordering::Release);

        // The newest value arrives.
        let mut buf = [0u8; 1024];
        loop {
            let n = sink.recv(&mut buf).expect("the newest level came back");
            let echoed = osc::decode(&buf[..n]).expect("decode");
            if echoed
                .iter()
                .any(|m| m.args == vec![Arg::F32(newest as f32)])
            {
                break;
            }
        }

        // One send was held; the four values dragged past behind it went as one more. A slow
        // machine can time the held one out, and a ping then lets one more through.
        let sends = proxy.sends.load(Ordering::Acquire);
        assert!(
            (2..=3).contains(&sends),
            "{sends} sends for a five-value drag"
        );
        harness.state_mut().engine.shutdown();
        reuben_native::diagnostics::log_snapshot(&played.shutdown());
    });
}

#[test]
fn attaching_swaps_in_a_file_changed_since_the_engine_read_it() {
    use reuben_native::test_support::{start_headless, within};

    let scratch = Scratch::new("ui_attached_stale");
    let config = TempConfig::new();
    let top = echoing(&scratch, 0.2);
    within(crate::engine::test_support::HANG_SECS, move || {
        let played = start_headless(crate::engine::test_support::config(top.clone()))
            .expect("the engine starts");
        // Changed on disk, and not Swapped in, before the editor attaches.
        echoing(&scratch, 0.7);
        let addr = played.structure_addr().expect("bound").to_string();
        let mut harness = harness(attached(addr, &config, None));
        step_until(&mut harness, "swapped in what is on disk", |_| {
            installed_level(&played) == json!(0.7)
        });
        harness.state_mut().engine.shutdown();
        reuben_native::diagnostics::log_snapshot(&played.shutdown());
    });
}

#[test]
fn an_engine_playing_another_file_has_it_opened_not_the_shown_one_swapped_over_it() {
    use reuben_native::test_support::{start_headless, within};

    let scratch = Scratch::new("ui_attached_other");
    let config = TempConfig::new();
    let shown = instrument(&scratch);
    let other = echoing(&scratch, 0.2);
    within(crate::engine::test_support::HANG_SECS, move || {
        let played = start_headless(crate::engine::test_support::config(other.clone()))
            .expect("the engine starts");
        let addr = played.structure_addr().expect("bound").to_string();
        // Changes to `shown` the engine never got, as when they were saved while it was down.
        let mut session = Session::open(&shown, None, || {});
        session.reinstall(&|| None);
        assert_eq!(session.status(), &Status::NoEngine);
        let mut harness = harness(attached(addr, &config, Some(session)));
        step_until(&mut harness, "opened what the engine plays", |editor| {
            editor.session.as_ref().is_some_and(|s| s.top() == other)
        });
        harness.run_steps(4);
        let installed = played.channel().get_document().unwrap();
        assert_eq!(installed.source, Some(other.display().to_string()));
        assert_eq!(installed_level(&played), json!(0.2));
        harness.state_mut().engine.shutdown();
        reuben_native::diagnostics::log_snapshot(&played.shutdown());
    });
}

#[test]
fn a_session_saying_nothing_plays_is_current_once_the_engine_answers_with_its_files() {
    use reuben_native::test_support::{start_headless, within};

    let scratch = Scratch::new("ui_attached_back");
    let config = TempConfig::new();
    let top = echoing(&scratch, 0.2);
    within(crate::engine::test_support::HANG_SECS, move || {
        let played = start_headless(crate::engine::test_support::config(top.clone()))
            .expect("the engine starts");
        let addr = played.structure_addr().expect("bound").to_string();
        // Saved while the engine was not answering; it came back on the same files.
        let mut session = Session::open(&top, None, || {});
        session.reinstall(&|| None);
        assert_eq!(session.status(), &Status::NoEngine);
        let mut harness = harness(attached(addr, &config, Some(session)));
        step_until(&mut harness, "said the files play", |editor| {
            editor.session.as_ref().unwrap().status() == &Status::Current
        });
        harness.state_mut().engine.shutdown();
        reuben_native::diagnostics::log_snapshot(&played.shutdown());
    });
}

/// A transport whose one exchange waits until it is let go, then fails: a Swap held in flight.
#[derive(Debug)]
struct Gate(std::sync::Mutex<std::sync::mpsc::Receiver<()>>);

impl reuben_api::engine::Transport for Gate {
    fn round_trip(&self, _: &str, _: std::time::Duration) -> std::io::Result<String> {
        let _ = self.0.lock().unwrap().recv();
        Err(std::io::Error::other("let go"))
    }
    fn endpoint(&self) -> &str {
        "gate"
    }
}

#[test]
fn what_the_engine_plays_while_a_swap_runs_is_followed_once_it_lands() {
    use reuben_api::engine::Channel;
    use reuben_native::test_support::{start_headless, within};

    let scratch = Scratch::new("ui_attached_mid_swap");
    let config = TempConfig::new();
    let shown = instrument(&scratch);
    let other = echoing(&scratch, 0.2);
    within(crate::engine::test_support::HANG_SECS, move || {
        let played = start_headless(crate::engine::test_support::config(other.clone()))
            .expect("the engine starts");
        let addr = played.structure_addr().expect("bound").to_string();
        let (release, gate) = std::sync::mpsc::channel();
        let gate = std::sync::Mutex::new(Some(Channel::new(Gate(std::sync::Mutex::new(gate)))));
        let mut session = Session::open(&shown, None, || {});
        session.reinstall(&|| gate.lock().unwrap().take());
        assert_eq!(session.status(), &Status::Swapping);

        let mut harness = harness(attached(addr, &config, Some(session)));
        // The engine has answered with what it plays, and the Swap still runs.
        std::thread::sleep(std::time::Duration::from_millis(200));
        harness.run_steps(4);
        assert_eq!(harness.state().session.as_ref().unwrap().top(), shown);

        release.send(()).unwrap();
        step_until(&mut harness, "opened what the engine plays", |editor| {
            editor.session.as_ref().is_some_and(|s| s.top() == other)
        });
        harness.state_mut().engine.shutdown();
        reuben_native::diagnostics::log_snapshot(&played.shutdown());
    });
}

#[test]
fn the_meter_strip_reads_the_engine_the_editor_is_attached_to() {
    use reuben_native::test_support::{start_headless, within};

    let scratch = Scratch::new("ui_meters_attached");
    let config = TempConfig::new();
    let top = echoing(&scratch, 0.2);
    within(crate::engine::test_support::HANG_SECS, move || {
        let played =
            start_headless(crate::engine::test_support::config(top)).expect("the engine starts");
        let addr = played.structure_addr().expect("bound").to_string();
        let mut harness = harness(attached(addr, &config, None));
        // A headless engine meters no device, and answers so over the structure channel.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while harness
            .query_by_label("Meters: the engine has no audio device")
            .is_none()
        {
            assert!(
                std::time::Instant::now() < deadline,
                "the strip never read the engine"
            );
            harness.run_steps(1);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        harness.state_mut().engine.shutdown();
        reuben_native::diagnostics::log_snapshot(&played.shutdown());
    });
}

#[test]
fn with_no_engine_the_meter_strip_says_so() {
    let scratch = Scratch::new("ui_meters_idle");
    let config = TempConfig::new();
    let top = instrument(&scratch);
    let harness = harness(editor(&top, &config));
    harness.get_by_label("Meters: no engine to read");
}

#[test]
fn the_meters_ride_the_right_edge_of_the_controls_panel() {
    let scratch = Scratch::new("ui_meters_beside_controls");
    let config = TempConfig::new();
    let top = instrument(&scratch);
    let harness = harness(editor(&top, &config));
    let controls = harness.get_by_label("CONTROLS").rect();
    let meters = harness.get_by_label("Meters: no engine to read").rect();
    assert!(meters.left() > controls.right(), "{meters:?} {controls:?}");
    assert!(
        meters.right() > 1280.0 - 200.0,
        "against the window's right edge: {meters:?}"
    );
    assert!(
        (meters.top() - controls.top()).abs() < 24.0,
        "in the controls panel's band: {meters:?} {controls:?}"
    );
}
