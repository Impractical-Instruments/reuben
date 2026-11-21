//! `reuben-editor` — the desktop instrument editor.

// A release build on Windows is a GUI-subsystem program, so launching it opens no console window.
// Its stdout and stderr then go nowhere unless the launcher redirects them, which a GUI-subsystem
// program still honours (the README gives the command); a debug build keeps the console.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod audio;
mod canvas;
mod chooser;
mod config;
mod controls;
mod edit;
mod engine;
mod feedback;
mod follow;
mod held_swap;
mod inspector;
mod link;
mod meter_strip;
mod names;
mod pick;
#[cfg(test)]
mod reaches_no_files;
mod sends;
mod session;
mod start;
mod theme;
#[cfg(test)]
mod ui_tests;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use clap::{CommandFactory, Parser};
use eframe::egui;
use reuben_api::engine::{Channel, Event};
use reuben_native::cli::{EngineFlags, InstrumentRoot};
use reuben_native::engine::{EngineConfig, Instrument};
use reuben_native::structure::SwapHold;

use crate::audio::AudioSettings;
use crate::canvas::Canvas;
use crate::config::ConfigDir;
use crate::controls::Controls;
use crate::engine::{Engine, Level};
use crate::follow::Follow;
use crate::held_swap::HeldSwaps;
use crate::inspector::Inspector;
use crate::meter_strip::MeterStrip;
use crate::session::{Session, Status, TrailAsk};
use crate::start::{Choice, Start};

#[derive(Parser)]
#[command(name = "reuben-editor", about = "Edit and play reuben instruments.")]
struct Args {
    /// Instrument JSON to open and play; omit to choose one on the start screen.
    path: Option<PathBuf>,
    /// Attach to a `reuben play` already running instead of starting an engine, at its structure
    /// channel's address: open the instrument it plays and play it live. The audio device and the
    /// engine flags stay what `reuben play` was started with.
    #[arg(
        long,
        value_name = "HOST:PORT",
        conflicts_with_all = ["path", "osc_out", "io_map", "headless", "structure_addr", "osc_in"]
    )]
    attach: Option<String>,
    #[command(flatten)]
    instrument_root: InstrumentRoot,
    #[command(flatten)]
    engine: EngineFlags,
}

impl Args {
    /// `parsed`, refusing the engine flags this binary cannot honour: the editor always plays on
    /// an audio device, so `--headless` belongs to `reuben play`.
    fn checked(parsed: Result<Self, clap::Error>) -> Result<Self, clap::Error> {
        let args = parsed?;
        if args.engine.headless {
            return Err(Args::command().error(
                clap::error::ErrorKind::ArgumentConflict,
                "reuben-editor always plays on an audio device; to run headless, start \
                 `reuben play --headless` and attach to it with --attach",
            ));
        }
        Ok(args)
    }
}

fn main() -> eframe::Result {
    let args = Args::checked(Args::try_parse()).unwrap_or_else(|e| e.exit());
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("reuben-editor")
        .with_app_id("reuben-editor")
        .with_inner_size([1280.0, 800.0]);
    if let Some(icon) = theme::icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "reuben-editor",
        options,
        // The engine starts here rather than before `run_native` so a session whose window never
        // opens never takes the audio device or the ports.
        Box::new(|cc| {
            theme::install(&cc.egui_ctx);
            let wake = {
                let ctx = cc.egui_ctx.clone();
                move || ctx.request_repaint()
            };
            let root = args.instrument_root.clone().resolve_or_default();
            let mut start_screen = Start::new(ConfigDir::user());
            let held = HeldSwaps::new(wake.clone());
            let mut audio = AudioSettings::load(ConfigDir::user());
            let mut engine = match args.attach.clone() {
                Some(addr) => Engine::attach(addr, wake.clone()),
                None => start(
                    &args,
                    root,
                    &mut audio,
                    Some(held.swap_hold()),
                    Engine::start,
                ),
            };
            let session = (args.path.as_deref()).and_then(|path| {
                match launched(path, &mut engine, &held, wake.clone()) {
                    Ok(session) => Some(session),
                    Err(why) => {
                        start_screen.refused(why);
                        None
                    }
                }
            });
            let mut canvas = Canvas::empty();
            let mut inspector = Inspector::default();
            let top = session.as_ref().map(Session::top);
            let controls = Controls::open(top, &|| engine.channel());
            if let Some(session) = &session {
                start_screen.opened(session.top());
                view(&mut canvas, &mut inspector, session);
            }
            let (follow, latest) = follower(engine.channel(), wake.clone());
            Ok(Box::new(Editor {
                engine,
                audio,
                controls,
                canvas,
                inspector,
                session,
                start: start_screen,
                held,
                meters: MeterStrip::new(wake),
                follow,
                latest,
            }))
        }),
    )
}

/// The standalone editor's own engine, which `args` configure, on the stored audio choice unless
/// `args` name an `--io-map`: started with nothing open, its library rooted at `root`, so the
/// start screen lists it and an instrument is opened on it by name. `start_engine` is
/// [`Engine::start`], or a test's stand-in that opens no device.
fn start(
    args: &Args,
    root: Option<PathBuf>,
    audio: &mut AudioSettings,
    swap_hold: Option<SwapHold>,
    start_engine: impl FnOnce(EngineConfig) -> Engine,
) -> Engine {
    match audio.engine_config(&args.engine, Instrument::Silent, root) {
        Ok(config) => start_engine(EngineConfig {
            swap_hold,
            ..config
        }),
        Err(e) => Engine::not_started(e),
    }
}

/// Open the instrument at `path`, named on the command line, on `engine`: its directory offered
/// to the server, and the session opened on the server's name for it.
fn launched(
    path: &std::path::Path,
    engine: &mut Engine,
    held: &HeldSwaps,
    wake: impl Fn() + Send + Sync + 'static,
) -> Result<Session, String> {
    let name = start::offer_file(path, engine);
    held.opened(std::path::Path::new(&name));
    let session = start::open(&name, engine, wake);
    if session.is_err() {
        held.closed();
    }
    session
}

/// Draw the document `session` views. A canvas drawing another document, and the inspector with
/// it, start over on this one: their view, selection and held values belong to the one they drew.
fn view(canvas: &mut Canvas, inspector: &mut Inspector, session: &Session) {
    let viewed = session.viewed();
    if canvas
        .instrument()
        .is_some_and(|drawn| drawn != viewed.source)
    {
        *canvas = Canvas::empty();
        *inspector = Inspector::default();
    }
    canvas.reload(&viewed.source, &viewed.read);
    canvas.tap(session);
}

/// The engine's channel as of the last frame, for [`Follow`]'s thread to subscribe through: the
/// engine itself lives on the UI thread, and is replaced when it restarts.
type Latest = Arc<Mutex<Option<Channel>>>;

/// A [`Follow`] of whichever engine the editor has, starting on `channel`, and the cell each frame
/// puts its channel in. `wake` is called whenever the follow has events.
fn follower(channel: Option<Channel>, wake: impl Fn() + Send + Sync + 'static) -> (Follow, Latest) {
    let latest = Arc::new(Mutex::new(channel));
    let follow = Follow::start(
        {
            let latest = Arc::clone(&latest);
            move || latest.lock().unwrap_or_else(PoisonError::into_inner).take()
        },
        wake,
    );
    (follow, latest)
}

struct Editor {
    engine: Engine,
    audio: AudioSettings,
    controls: Controls,
    canvas: Canvas,
    inspector: Inspector,
    session: Option<Session>,
    start: Start,
    held: HeldSwaps,
    meters: MeterStrip,
    follow: Follow,
    latest: Latest,
}

impl Editor {
    /// Whether the engine plays the open session's top-level document as the session read it, so
    /// a node path the canvas resolves names a node of what is drawn. Another document's Plan can
    /// have the same addresses — before an attached editor opens what the engine plays, or while
    /// the server holds what it read back — and its levels must not show on these boxes.
    fn metered(&mut self) -> bool {
        let (Some(session), Some(plays)) = (&self.session, self.engine.plays()) else {
            return false;
        };
        let read = session.tree().documents().first().and_then(|d| d.hash());
        plays.source == session.top() && read == Some(plays.content_hash.as_str())
    }

    /// Have the server open the instrument it names `name` in place of whatever is open. The
    /// canvas and the inspector start over from the new session's read, and its undo history is
    /// its own. One the server does not open is said on the start screen.
    fn open(&mut self, name: &str, ctx: &egui::Context) {
        // Before the open starts, so a client asking for the instrument being opened is not held.
        self.held.opened(std::path::Path::new(name));
        let ctx = ctx.clone();
        match start::open(name, &self.engine, move || ctx.request_repaint()) {
            Ok(session) => self.show(session),
            Err(why) => {
                // Nothing was opened: what the server has open is still the session's, if any.
                match &self.session {
                    Some(session) => self.held.opened(std::path::Path::new(session.top())),
                    None => self.held.closed(),
                }
                self.start.refused(why);
            }
        }
    }

    /// Show `session`, and the controls panel for it, in place of whatever is shown.
    fn show(&mut self, session: Session) {
        let top = PathBuf::from(session.top());
        self.held.opened(&top);
        self.canvas = Canvas::empty();
        self.inspector = Inspector::default();
        view(&mut self.canvas, &mut self.inspector, &session);
        self.controls = Controls::open(Some(session.top()), &|| self.engine.channel());
        self.start.opened(session.top());
        self.session = Some(session);
    }

    /// Follow what the attached engine plays, as it changes: open the document it names, with no
    /// Swap, since it plays it already.
    fn follow_attached(&mut self, ctx: &egui::Context) {
        self.engine.follow_link();
        let Some(playing) = self.engine.unfollowed() else {
            return;
        };
        if self
            .session
            .as_ref()
            .is_none_or(|s| s.top() != playing.source)
        {
            self.follow_open(&playing.source, ctx);
        }
    }

    /// Show a session on the instrument the server names `name`, which it plays already.
    fn follow_open(&mut self, name: &str, ctx: &egui::Context) {
        let wake = {
            let ctx = ctx.clone();
            move || ctx.request_repaint()
        };
        let session = Session::open(name, &|| self.engine.channel()).with_wake(wake);
        self.show(session);
    }

    /// Take in what the server said changed, and rebuild what is drawn from the tree it reads.
    fn follow_files(&mut self, ui: &egui::Ui) {
        *self.latest.lock().unwrap_or_else(PoisonError::into_inner) = self.engine.channel();
        self.follow_attached(ui.ctx());
        let events = self.follow.drain();
        let Some(session) = &mut self.session else {
            return;
        };
        let poll = session.poll(&events, &|| self.engine.channel());
        // What the server says plays is known the moment it says so, where an attached engine's
        // probe waits on its next round.
        for event in &events {
            match event {
                Event::Changed { hash, .. } if !hash.is_empty() => {
                    self.engine
                        .note_installed(session.top().to_string(), hash.clone());
                }
                Event::Opened { name, hash } if !name.is_empty() => {
                    self.engine.note_installed(name.clone(), hash.clone());
                }
                _ => {}
            }
        }
        // Another instrument opened in this one's place — by another client, or by a held open
        // the user let through — is what the canvas shows from now on.
        if let Some(name) = &poll.opened {
            self.follow_open(name, ui.ctx());
            return;
        }
        start::follow(
            session,
            &mut self.engine,
            poll.reloaded,
            reuben_native::engine::start,
        );
        // A Swap holding the engine's Coordinator keeps what it installed unknown: look again
        // shortly rather than on the next input event.
        if self.engine.busy() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(50));
        }
        if let Some(wait) = poll.wait {
            ui.ctx().request_repaint_after(wait);
        }
        if poll.reloaded {
            view(&mut self.canvas, &mut self.inspector, session);
        }
        // A tree that does not load keeps the panel of the instrument still playing.
        if poll.reloaded && !matches!(session.status(), Status::Held(_)) {
            self.controls = Controls::open(Some(session.top()), &|| self.engine.channel());
        }
    }
}

impl eframe::App for Editor {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.follow_files(ui);
        let mut clicked = None;
        egui::Panel::top("audio").show(ui, |ui| {
            ui.horizontal(|ui| {
                theme::logo(ui, 22.0);
                // Vertical, so the device warnings and the session's rows stack under the device
                // row rather than beside it.
                ui.vertical(|ui| {
                    match self.engine.link() {
                        Some(link) => link::show(ui, link),
                        None => self.audio.ui(ui, &mut self.engine),
                    }
                    if let Some(session) = &self.session {
                        clicked = session::bar(ui, session);
                    }
                });
            });
        });
        let probes = match self.metered() && !self.start.shown() {
            true => self.canvas.probe_paths(),
            false => &[],
        };
        self.meters
            .poll(Instant::now(), self.engine.channel(), probes);
        if let Some(session) = &mut self.session {
            let asked = session::shortcut(ui.ctx()).or(clicked);
            if let Some(direction) = asked {
                edit::restore(session, &|| self.engine.channel(), direction);
                // The reload it made is taken in by the next frame's poll.
                ui.ctx().request_repaint();
            }
        }
        // Before the central panel, which takes whatever space is left. The meters ride the
        // controls' right edge, or have the bottom edge to themselves while no instrument is open.
        let meters = &mut self.meters;
        if !self
            .controls
            .show(ui, &self.engine, |ui| meter_strip::show(ui, meters))
        {
            egui::Panel::bottom("meters").show(ui, |ui| meter_strip::show(ui, meters));
        }
        if let Some(session) = &mut self.session {
            let channel = || self.engine.channel();
            let live = || self.engine.live();
            inspector::show(
                ui,
                &mut self.inspector,
                session,
                &channel,
                &live,
                self.canvas.selection(),
            );
        }
        // The root `Ui` paints no background of its own, leaving eframe's clear color; the panel
        // frame paints the theme's fill instead.
        let gestures = egui::Frame::central_panel(ui.style()).show(ui, |ui| {
            if self.session.is_none() || self.start.shown() {
                let engine = &self.engine;
                let embedded = engine.link().is_none();
                let chosen =
                    (self.start).show(ui, self.session.is_some(), embedded, &|| engine.channel());
                match chosen {
                    Some(Choice::Open(name)) => self.open(&name, ui.ctx()),
                    Some(Choice::OpenFile(path)) => {
                        let name = start::offer_file(&path, &mut self.engine);
                        self.open(&name, ui.ctx());
                    }
                    None => {}
                }
                return Vec::new();
            }
            if ui.button("Start screen").clicked() {
                self.start.show_again();
            }
            if let Some(session) = &mut self.session {
                if let Some(asked) = session::trail(ui, session, &|| self.engine.channel()) {
                    match asked {
                        TrailAsk::Leave(depth) => session.leave(depth),
                        TrailAsk::LocalCopy => {
                            edit::local_copy::make(session, &|| self.engine.channel());
                        }
                    }
                    view(&mut self.canvas, &mut self.inspector, session);
                }
                feedback::show(ui, session.feedback_mut());
            }
            for line in self.engine.report() {
                let color = match line.level {
                    Level::Info => ui.visuals().text_color(),
                    Level::Warning => ui.visuals().warn_fg_color,
                    Level::Error => ui.visuals().error_fg_color,
                };
                ui.colored_label(color, line.to_string());
            }
            let metered = self.metered();
            let rejected = (self.session.as_ref())
                .and_then(|session| {
                    let viewed = &session.viewed().source;
                    session.feedback().map(|feedback| feedback.rejected(viewed))
                })
                .unwrap_or_default();
            let levels = match metered {
                true => self.meters.node_levels(),
                false => &[],
            };
            self.canvas
                .show(ui, rejected, levels, &|| self.engine.channel())
        });
        for gesture in gestures.inner {
            let Some(session) = &mut self.session else {
                break;
            };
            let result = gesture.apply(session, &|| self.engine.channel());
            self.canvas.applied(&gesture, &result);
            // The write is read back already; the next frame redraws from it.
            ui.ctx().request_repaint();
        }
        // After the gestures, which were made on the document drawn this frame.
        if let (Some(address), Some(session)) = (self.canvas.take_entered(), &mut self.session) {
            match session.enter(&address) {
                Ok(()) => view(&mut self.canvas, &mut self.inspector, session),
                Err(why) => self.canvas.refuse(why),
            }
            ui.ctx().request_repaint();
        }
        // Over the canvas's lower corner: the feedback banner sits at the top of the same panel,
        // and the controls and inspector panels stay uncovered.
        if let Some(name) = held_swap::show(ui.ctx(), &self.held, gestures.response.rect) {
            self.open(&name, ui.ctx());
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.engine.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::tests::two_devices;
    use crate::config::test_support::TempConfig;

    /// The config `main` starts its engine with for `args`, or the engine it reports instead.
    fn started(args: &[&str], swap_hold: Option<SwapHold>) -> (Option<EngineConfig>, Engine) {
        let args = Args::parse_from(args);
        let temp = TempConfig::new();
        let mut audio = AudioSettings::load_with(temp.dir(), two_devices);
        let mut config = None;
        let engine = start(&args, None, &mut audio, swap_hold, |asked| {
            config = Some(asked);
            Engine::idle()
        });
        (config, engine)
    }

    #[test]
    fn the_engine_started_at_launch_holds_what_the_launch_says() {
        let hold = reuben_native::structure::SwapHold::new(|_| true);
        let (config, _) = started(&["reuben-editor", "instrument.json"], Some(hold));
        assert!(config.expect("an engine starts").swap_hold.is_some());
    }

    #[test]
    fn a_launch_open_the_server_refuses_holds_nothing() {
        use reuben_api::engine::{open_instrument, OpenInstrument};
        use reuben_mcp::EngineLink;
        use reuben_native::engine::RunningEngine;
        use reuben_native::test_support::{start_headless, within};

        use crate::engine::test_support::{silent, HANG_SECS};
        use crate::session::test_support::Scratch;

        within(HANG_SECS, || {
            let scratch = Scratch::new("main_launch_refused");
            let lib = scratch.library();
            let held = HeldSwaps::new(|| {});
            let mut engine = Engine::start_with(
                EngineConfig {
                    swap_hold: Some(held.swap_hold()),
                    ..silent(&lib)
                },
                start_headless,
            );
            let missing = lib.join("missing.json");
            assert!(launched(&missing, &mut engine, &held, || {}).is_err());

            let addr = (engine.running())
                .and_then(RunningEngine::structure_addr)
                .expect("the structure server bound");
            let link = EngineLink::new(addr.to_string()).expect("a loopback address");
            let opened = open_instrument(
                &OpenInstrument {
                    name: lib.join("groovebox.json").display().to_string(),
                    expect: None,
                },
                link.structure(),
            )
            .expect("the open reached the engine")
            .output;
            assert_eq!(opened.held, None, "nothing is open to hold it for");
            assert_eq!(held.pending(), None);
            engine.shutdown();
        });
    }

    #[test]
    fn headless_is_refused() {
        let error = Args::checked(Args::try_parse_from([
            "reuben-editor",
            "--headless",
            "instrument.json",
        ]))
        .err()
        .expect("--headless is refused");
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
        assert!(
            error.to_string().contains("reuben play --headless"),
            "{error}"
        );
    }

    #[test]
    fn no_instrument_starts_a_silent_engine() {
        for args in [
            &["reuben-editor"][..],
            &["reuben-editor", "instrument.json"],
        ] {
            let (config, engine) = started(args, None);
            let config = config.expect("an engine starts");
            assert_eq!(config.instrument, Instrument::Silent, "{args:?}");
            assert!(engine.report().is_empty());
        }
    }

    #[test]
    fn a_bad_io_map_is_reported_not_started() {
        let args = [
            "reuben-editor",
            "--io-map",
            "does-not-exist.json",
            "instrument.json",
        ];
        let (config, engine) = started(&args, None);
        assert!(config.is_none());
        assert_eq!(engine.report().len(), 1);
        assert_eq!(engine.report()[0].level, Level::Error);
        assert!(engine.report()[0]
            .text
            .starts_with("io-map does-not-exist.json"));
    }
}
