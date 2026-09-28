//! `reuben-editor` — the desktop instrument editor.

use eframe::egui;

fn main() -> eframe::Result {
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
        Box::new(|_cc| Ok(Box::new(Editor))),
    )
}

struct Editor;

impl eframe::App for Editor {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // The root `Ui` paints no background of its own; the panel frame fills the window with
        // the theme's fill so an empty editor is not a transparent or undefined surface.
        egui::Frame::central_panel(ui.style()).show(ui, |ui| {
            ui.take_available_space();
        });
    }
}
