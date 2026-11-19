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

/// An instrument at `lib/ui.json` with one `f32` interface input, `gain`, and no range on it.
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
            ..Default::default()
        },
        &store,
    )
    .expect("the input is added");
    assert!(added.output.written, "{:?}", added.output.report);
    top
}

/// The editor open on `top` with no engine, as `main` builds it.
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

    let dismiss = harness.get_by_label("Dismiss").rect();
    let inspector = harness.get_by_label("min").rect();
    assert!(
        dismiss.right() <= inspector.left(),
        "the banner's Dismiss at {dismiss:?} reaches the inspector at {inspector:?}"
    );
}
