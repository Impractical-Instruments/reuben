//! `reuben-editor` — the desktop instrument editor.

mod engine;

use std::path::PathBuf;

use clap::{CommandFactory, Parser};
use eframe::egui;
use reuben_native::cli::{EngineFlags, InstrumentRoot};
use reuben_native::engine::Instrument;

use crate::engine::{Engine, Level};

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
        Box::new(|_cc| Ok(Box::new(Editor::new(start(args))))),
    )
}

fn start(args: Args) -> Engine {
    let Some(path) = args.path else {
        return Engine::idle();
    };
    let root = args.instrument_root.resolve();
    match args.engine.config(Instrument::Path(path), root) {
        Ok(config) => Engine::start(config),
        Err(e) => Engine::not_started(e),
    }
}

struct Editor {
    engine: Engine,
}

impl Editor {
    fn new(engine: Engine) -> Self {
        Self { engine }
    }
}

impl eframe::App for Editor {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // The root `Ui` paints no background of its own, leaving eframe's clear color; the panel
        // frame paints the theme's fill instead.
        egui::Frame::central_panel(ui.style()).show(ui, |ui| {
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
            ui.take_available_space();
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.engine.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_instrument_starts_no_engine() {
        let engine = start(Args::parse_from(["reuben-editor"]));
        assert!(engine.report().is_empty());
    }

    #[test]
    fn a_bad_io_map_is_reported_not_started() {
        let engine = start(Args::parse_from([
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
