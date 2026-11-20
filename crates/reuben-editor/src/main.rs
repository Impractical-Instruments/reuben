//! `reuben-editor` — the desktop instrument editor.

// A release build on Windows is a GUI-subsystem program, so launching it opens no console window.
// Its stdout and stderr then go nowhere unless the launcher redirects them, which a GUI-subsystem
// program still honours (the README gives the command); a debug build keeps the console.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod audio;
mod canvas;
mod config;
mod controls;
mod edit;
mod engine;
mod feedback;
mod held_swap;
mod inspector;
mod link;
mod meter_strip;
mod pick;
mod sends;
mod session;
mod start;
mod theme;
#[cfg(test)]
mod ui_tests;

use std::path::PathBuf;
use std::time::Instant;

use clap::Parser;
use eframe::egui;
use reuben_native::cli::{EngineFlags, InstrumentRoot};
use reuben_native::engine::{EngineConfig, Instrument};

use crate::audio::AudioSettings;
use crate::canvas::Canvas;
use crate::config::ConfigDir;
use crate::controls::Controls;
use crate::engine::{Engine, Level};
use crate::held_swap::HeldSwaps;
use crate::inspector::Inspector;
use crate::meter_strip::MeterStrip;
use crate::session::{Session, Status, TrailAsk};
use crate::start::{Launch, Start};

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
        conflicts_with_all = ["path", "osc_out", "io_map"]
    )]
    attach: Option<String>,
    #[command(flatten)]
    instrument_root: InstrumentRoot,
    #[command(flatten)]
    engine: EngineFlags,
}

fn main() -> eframe::Result {
    let args = Args::parse();
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
            let root = args.instrument_root.clone().resolve();
            let controls = Controls::open(args.path.as_deref(), root.as_deref());
            let session = args.path.as_deref().map(|path| {
                let ctx = cc.egui_ctx.clone();
                Session::open(path, root.as_deref(), move || ctx.request_repaint())
            });
            let mut canvas = Canvas::open(None, None);
            let mut inspector = Inspector::default();
            if let Some(session) = &session {
                view(&mut canvas, &mut inspector, session);
            }
            let mut start_screen = Start::new(ConfigDir::user(), root.as_deref());
            if let Some(path) = args.path.as_deref().filter(|p| p.is_file()) {
                start_screen.opened(path);
            }
            let held = HeldSwaps::new({
                let ctx = cc.egui_ctx.clone();
                move || ctx.request_repaint()
            });
            if let Some(path) = args.path.as_deref() {
                held.opened(path);
            }
            let launch = Launch {
                root,
                flags: args.engine.clone(),
                swap_hold: Some(held.swap_hold()),
            };
            let mut audio = AudioSettings::load(ConfigDir::user());
            let engine = match args.attach.clone() {
                Some(addr) => Engine::attach(addr, {
                    let ctx = cc.egui_ctx.clone();
                    move || ctx.request_repaint()
                }),
                None => start(args, &mut audio, &launch, Engine::start),
            };
            Ok(Box::new(Editor {
                engine,
                audio,
                controls,
                canvas,
                inspector,
                session,
                start: start_screen,
                launch,
                held,
                meters: MeterStrip::new({
                    let ctx = cc.egui_ctx.clone();
                    move || ctx.request_repaint()
                }),
            }))
        }),
    )
}

/// The engine `args` ask for, on the stored audio choice unless `args` name an `--io-map`.
/// `start_engine` is [`Engine::start`], or a test's stand-in that opens no device.
fn start(
    args: Args,
    audio: &mut AudioSettings,
    launch: &Launch,
    start_engine: impl FnOnce(EngineConfig) -> Engine,
) -> Engine {
    let Some(path) = args.path else {
        return Engine::idle();
    };
    let root = args.instrument_root.resolve();
    match audio.engine_config(&args.engine, Instrument::Path(path), root) {
        Ok(config) => start_engine(EngineConfig {
            swap_hold: launch.swap_hold.clone(),
            ..config
        }),
        Err(e) => Engine::not_started(e),
    }
}

/// Draw the document `session` views. A canvas drawing another document, and the inspector with
/// it, start over on this one: their view, selection and held values belong to the one they drew.
fn view(canvas: &mut Canvas, inspector: &mut Inspector, session: &Session) {
    let viewed = session.viewed();
    let source = std::path::Path::new(&viewed.source);
    if canvas.instrument().is_some_and(|drawn| drawn != source) {
        *canvas = Canvas::open(None, None);
        *inspector = Inspector::default();
    }
    canvas.reload(source, session.root(), &viewed.read);
}

struct Editor {
    engine: Engine,
    audio: AudioSettings,
    controls: Controls,
    canvas: Canvas,
    inspector: Inspector,
    session: Option<Session>,
    start: Start,
    launch: Launch,
    held: HeldSwaps,
    meters: MeterStrip,
}

impl Editor {
    /// Open the instrument at `path` in place of whatever is open. The canvas and the inspector
    /// start over from the new session's read, and its undo history is its own.
    fn open(&mut self, path: &std::path::Path, ctx: &egui::Context) {
        // Before the Swap starts, so a client asking for the file being opened is not held.
        self.held.opened(path);
        let ctx = ctx.clone();
        let session = start::open(
            path,
            &self.launch,
            &mut self.audio,
            &mut self.engine,
            move || ctx.request_repaint(),
            reuben_native::engine::start,
        );
        let panel = start::panel_follows(&session, &self.engine);
        self.show(session, path, panel);
    }

    /// Show `session`, open on `path`, in place of whatever is shown; `panel` is whether the
    /// controls panel is rebuilt for it too.
    fn show(&mut self, session: Session, path: &std::path::Path, panel: bool) {
        self.canvas = Canvas::open(None, None);
        self.inspector = Inspector::default();
        view(&mut self.canvas, &mut self.inspector, &session);
        if panel {
            self.controls = Controls::open(Some(path), self.launch.root.as_deref());
        }
        self.session = Some(session);
        self.start.opened(path);
    }

    /// Follow what the attached engine plays, as it changes: open the file it names, with no Swap,
    /// since it plays it already. Its files are Swapped in where they are not what the engine
    /// installed from them — they changed while it was not answering, or before the editor
    /// attached — or where the session is not sure the engine plays them; a file the engine plays is
    /// opened rather than another Swapped over it.
    fn follow_attached(&mut self, ctx: &egui::Context) {
        self.engine.follow_link();
        // Followed once the Swap lands, whatever it installed.
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.status() == &Status::Swapping)
        {
            return;
        }
        let Some(playing) = self.engine.unfollowed() else {
            return;
        };
        let path = PathBuf::from(&playing.source);
        if self.session.as_ref().is_none_or(|s| s.top() != path) {
            let wake = {
                let ctx = ctx.clone();
                move || ctx.request_repaint()
            };
            let session = Session::open(&path, self.launch.root.as_deref(), wake);
            self.show(session, &path, true);
        }
        let Some(session) = &mut self.session else {
            return;
        };
        let on_disk = session.tree().documents().first().and_then(|d| d.hash());
        if on_disk != Some(playing.content_hash.as_str()) || session.status() != &Status::Current {
            session.reinstall(&|| self.engine.channel());
        }
    }

    /// Take in what the session saw change on disk, and rebuild what is drawn from the files.
    fn follow_files(&mut self, ui: &egui::Ui) {
        self.follow_attached(ui.ctx());
        let Some(session) = &mut self.session else {
            return;
        };
        let poll = session.poll(Instant::now(), &|| self.engine.channel());
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
        if poll.reloaded {
            view(&mut self.canvas, &mut self.inspector, session);
        }
        // The files load and nothing plays them: the engine stopped, or never started, on a
        // tree that did not load.
        if poll.reloaded && session.status() == &Status::NoEngine {
            self.engine.resume();
            session.reinstall(&|| self.engine.channel());
        }
        // A tree that does not load keeps the panel of the instrument still playing.
        if poll.reloaded && !matches!(session.status(), Status::Held(_)) {
            self.controls = Controls::open(Some(session.top()), session.root());
        }
        if let Some(wait) = poll.wait {
            ui.ctx().request_repaint_after(wait);
        }
    }
}

impl eframe::App for Editor {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.follow_files(ui);
        egui::Panel::top("audio").show(ui, |ui| {
            ui.horizontal(|ui| {
                theme::logo(ui, 22.0);
                // Vertical, so the device warnings stack under the device row rather than beside it.
                ui.vertical(|ui| match self.engine.link() {
                    Some(link) => link::show(ui, link),
                    None => self.audio.ui(ui, &mut self.engine),
                });
            });
        });
        self.meters.poll(Instant::now(), self.engine.channel());
        if let Some(session) = &mut self.session {
            let asked = session::shortcut(ui.ctx()).or(session::show(ui, session));
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
                let chosen = self.start.show(ui, self.session.is_some());
                if let Some(path) = chosen {
                    self.open(&path, ui.ctx());
                }
                return Vec::new();
            }
            if ui.button("Start screen").clicked() {
                self.start.show_again();
            }
            if let Some(session) = &mut self.session {
                if let Some(asked) = session::trail(ui, session) {
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
            let rejected = (self.session.as_ref())
                .and_then(|session| {
                    let viewed = &session.viewed().source;
                    session.feedback().map(|feedback| feedback.rejected(viewed))
                })
                .unwrap_or_default();
            self.canvas.show(ui, rejected)
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
        if let Some(path) = held_swap::show(ui.ctx(), &self.held, gestures.response.rect) {
            self.open(&path, ui.ctx());
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

    fn start_nothing(args: Args) -> Engine {
        let temp = TempConfig::new();
        let mut audio = AudioSettings::load_with(temp.dir(), two_devices);
        let launch = Launch {
            root: None,
            flags: args.engine.clone(),
            swap_hold: None,
        };
        start(args, &mut audio, &launch, |_| {
            panic!("these arguments describe no engine to start")
        })
    }

    #[test]
    fn the_engine_started_at_launch_holds_what_the_launch_says() {
        let temp = TempConfig::new();
        let mut audio = AudioSettings::load_with(temp.dir(), two_devices);
        let args = Args::parse_from(["reuben-editor", "instrument.json"]);
        let launch = Launch {
            root: None,
            flags: args.engine.clone(),
            swap_hold: Some(reuben_native::structure::SwapHold::new(|_| true)),
        };
        let mut held = None;
        start(args, &mut audio, &launch, |config| {
            held = config.swap_hold;
            Engine::idle()
        });
        assert!(held.is_some());
    }

    #[test]
    fn no_instrument_starts_no_engine() {
        let engine = start_nothing(Args::parse_from(["reuben-editor"]));
        assert!(engine.report().is_empty());
    }

    #[test]
    fn a_bad_io_map_is_reported_not_started() {
        let engine = start_nothing(Args::parse_from([
            "reuben-editor",
            "--io-map",
            "does-not-exist.json",
            "instrument.json",
        ]));
        assert_eq!(engine.report().len(), 1);
        assert_eq!(engine.report()[0].level, Level::Error);
        assert!(engine.report()[0]
            .text
            .starts_with("io-map does-not-exist.json"));
    }
}
