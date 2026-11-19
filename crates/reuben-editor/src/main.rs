//! `reuben-editor` — the desktop instrument editor.

mod audio;
mod canvas;
mod config;
mod controls;
mod edit;
mod engine;
mod session;

use std::path::PathBuf;
use std::time::Instant;

use clap::{CommandFactory, Parser};
use eframe::egui;
use reuben_native::cli::{EngineFlags, InstrumentRoot};
use reuben_native::engine::{EngineConfig, Instrument};

use crate::audio::AudioSettings;
use crate::canvas::Canvas;
use crate::config::ConfigDir;
use crate::controls::Controls;
use crate::engine::{Engine, Level};
use crate::session::{Session, Status};

#[derive(Parser)]
#[command(name = "reuben-editor", about = "Edit and play reuben instruments.")]
struct Args {
    /// Instrument JSON to open and play; omit to open the window with no engine running.
    path: Option<PathBuf>,
    #[command(flatten)]
    instrument_root: InstrumentRoot,
    #[command(flatten)]
    engine: EngineFlags,
}

fn main() -> eframe::Result {
    let args = Args::parse();
    if args.path.is_none() && (args.engine.osc_out.is_some() || args.engine.io_map.is_some()) {
        Args::command()
            .error(
                clap::error::ErrorKind::MissingRequiredArgument,
                "--osc-out and --io-map configure the engine, which runs only for an instrument \
                 PATH",
            )
            .exit();
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("reuben-editor")
            .with_app_id("reuben-editor")
            .with_inner_size([1280.0, 800.0]),
        ..Default::default()
    };
    eframe::run_native(
        "reuben-editor",
        options,
        // The engine starts here rather than before `run_native` so a session whose window never
        // opens never takes the audio device or the ports.
        Box::new(|cc| {
            let root = args.instrument_root.clone().resolve();
            let controls = Controls::open(args.path.as_deref(), root.as_deref());
            let canvas = Canvas::open(args.path.as_deref(), root.as_deref());
            let session = args.path.as_deref().map(|path| {
                let ctx = cc.egui_ctx.clone();
                Session::open(path, root.as_deref(), move || ctx.request_repaint())
            });
            let mut audio = AudioSettings::load(ConfigDir::user());
            let engine = start(args, &mut audio, Engine::start);
            Ok(Box::new(Editor::new(
                engine, audio, controls, canvas, session,
            )))
        }),
    )
}

/// The engine `args` ask for, on the stored audio choice unless `args` name an `--io-map`.
/// `start_engine` is [`Engine::start`], or a test's stand-in that opens no device.
fn start(
    args: Args,
    audio: &mut AudioSettings,
    start_engine: impl FnOnce(EngineConfig) -> Engine,
) -> Engine {
    let Some(path) = args.path else {
        return Engine::idle();
    };
    let root = args.instrument_root.resolve();
    match audio.engine_config(&args.engine, Instrument::Path(path), root) {
        Ok(config) => start_engine(config),
        Err(e) => Engine::not_started(e),
    }
}

struct Editor {
    engine: Engine,
    audio: AudioSettings,
    controls: Controls,
    canvas: Canvas,
    session: Option<Session>,
}

impl Editor {
    fn new(
        engine: Engine,
        audio: AudioSettings,
        controls: Controls,
        canvas: Canvas,
        session: Option<Session>,
    ) -> Self {
        Self {
            engine,
            audio,
            controls,
            canvas,
            session,
        }
    }

    /// Take in what the session saw change on disk, and rebuild what is drawn from the files.
    fn follow_files(&mut self, ui: &egui::Ui) {
        let Some(session) = &mut self.session else {
            return;
        };
        let poll = session.poll(Instant::now(), &|| self.engine.channel());
        if poll.reloaded {
            self.canvas.reload(session.top(), session.root());
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
        egui::Panel::top("audio").show(ui, |ui| self.audio.ui(ui, &mut self.engine));
        if let Some(session) = &self.session {
            session::show(ui, session);
        }
        // Before the central panel, which takes whatever space is left.
        self.controls.show(ui, &self.engine);
        // The root `Ui` paints no background of its own, leaving eframe's clear color; the panel
        // frame paints the theme's fill instead.
        let gesture = egui::Frame::central_panel(ui.style()).show(ui, |ui| {
            if self.engine.report().is_empty() {
                ui.label("No instrument open. Run `reuben-editor <instrument.json>` to play one.");
            }
            for line in self.engine.report() {
                let color = match line.level {
                    Level::Info => ui.visuals().text_color(),
                    Level::Warning => ui.visuals().warn_fg_color,
                    Level::Error => ui.visuals().error_fg_color,
                };
                ui.colored_label(color, line.to_string());
            }
            self.canvas.show(ui)
        });
        if let (Some(gesture), Some(session)) = (gesture.inner, &mut self.session) {
            let result = gesture.apply(session, &|| self.engine.channel());
            self.canvas.applied(&gesture, &result);
            // The write is read back already; the next frame redraws from it.
            ui.ctx().request_repaint();
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
        start(args, &mut audio, |_| {
            panic!("these arguments describe no engine to start")
        })
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
