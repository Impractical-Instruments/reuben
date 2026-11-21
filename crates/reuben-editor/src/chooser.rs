//! Picking one of the names the server offers: a list filtered by what is typed above it, each name
//! shown as a path relative to a directory. Only a name the server listed can be picked.

use std::path::Path;

use eframe::egui;

use crate::inspector::relative_source;

/// How tall the list may grow before it scrolls, in points: about ten names.
const LIST_HEIGHT: f32 = 200.0;

/// Draw the chooser over `names` and return the one picked this frame. Each is shown relative to
/// the directory `relative_to`, or as the server spells it where it cannot be. What is typed into
/// the filter is kept between frames under `id`.
pub fn show(
    ui: &mut egui::Ui,
    id: egui::Id,
    names: &[String],
    relative_to: &str,
) -> Option<String> {
    let mut filter: String = ui.data_mut(|data| data.get_temp(id)).unwrap_or_default();
    ui.add(
        egui::TextEdit::singleline(&mut filter)
            .id(id.with("filter"))
            .hint_text("filter"),
    );
    let shown = entries(names, relative_to, &filter);
    let mut picked = None;
    egui::ScrollArea::vertical()
        .id_salt(id.with("list"))
        .max_height(LIST_HEIGHT)
        .show(ui, |ui| {
            if shown.is_empty() {
                ui.weak("Nothing the server offers matches.");
            }
            for (label, name) in shown {
                if ui
                    .selectable_label(false, label)
                    .on_hover_text(name)
                    .clicked()
                {
                    picked = Some(name.clone());
                }
            }
        });
    ui.data_mut(|data| data.insert_temp(id, filter));
    picked
}

/// Each of `names` as it is shown from `relative_to`, with the name, keeping those whose shown
/// spelling holds `filter`, ignoring case.
fn entries<'a>(names: &'a [String], relative_to: &str, filter: &str) -> Vec<(String, &'a String)> {
    let filter = filter.trim().to_lowercase();
    (names.iter())
        .map(|name| {
            let shown = relative_source(Path::new(relative_to), Path::new(name))
                .unwrap_or_else(|_| name.clone());
            (shown, name)
        })
        .filter(|(shown, _)| shown.to_lowercase().contains(&filter))
        .collect()
}

#[cfg(test)]
mod tests {
    use egui_kittest::kittest::Queryable;
    use egui_kittest::Harness;
    use reuben_api::engine::ListWhat;
    use reuben_native::test_support::within;

    use super::*;
    use crate::session::test_support::{names, Rig};

    const HANG_SECS: u64 = 60;

    #[test]
    fn the_sample_chooser_offers_the_servers_samples_relative_to_the_viewed_document() {
        let (offered, dir, sample) = within(HANG_SECS, || {
            let mut rig = Rig::open("chooser_samples", "granular-cloud.json");
            rig.session.enter("/verb").expect("/verb patches space");
            let viewed = rig.session.viewed().source.clone();
            let dir = Path::new(&viewed).parent().expect("a directory");
            let offered = (rig.channel().list(ListWhat::Samples)).expect("the server lists");
            let sample = rig.lib.join("samples/testvox.wav");
            (offered, dir.display().to_string(), sample)
        });
        let testvox = (offered.iter())
            .find(|name| names(name, &sample))
            .expect("the server offers the library's sample")
            .clone();

        let mut harness = Harness::new_ui_state(
            move |ui, picked: &mut Option<String>| {
                let id = egui::Id::new("samples");
                if let Some(name) = show(ui, id, &offered, &dir) {
                    *picked = Some(name);
                }
            },
            None,
        );
        harness.run();
        harness.get_by_label("../samples/testvox.wav").click();
        harness.run();
        assert_eq!(harness.state().as_ref(), Some(&testvox));
    }

    #[test]
    fn the_filter_keeps_the_names_whose_shown_path_holds_it() {
        let names = [
            "/l/samples/Kick.wav".to_string(),
            "/l/samples/snare.wav".to_string(),
            "/elsewhere/kick.wav".to_string(),
        ];
        let shown = |filter| -> Vec<String> {
            (entries(&names, "/l/patches", filter).into_iter())
                .map(|(shown, _)| shown)
                .collect()
        };
        assert_eq!(
            shown(""),
            [
                "../samples/Kick.wav",
                "../samples/snare.wav",
                "../../elsewhere/kick.wav"
            ]
        );
        assert_eq!(
            shown(" KICK "),
            ["../samples/Kick.wav", "../../elsewhere/kick.wav"]
        );
        assert!(shown("hat").is_empty());
    }
}
