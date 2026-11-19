//! Drawing the panel. Everything decided here is how a control looks and which gesture a pointer
//! made on it; what a gesture sends is [`Control::message`].

use std::path::Path;

use eframe::egui::{self, Rect, Sense, Stroke, StrokeKind, Vec2};
use reuben_api::engine::ControlSendMessage;

use super::model::{self, Control, Gesture, Kind, Panel, Shape, BLACK_DEPTH};
use super::Setting;
use crate::theme;

const CELL_WIDTH: f32 = 72.0;
const SLIDER_LENGTH: f32 = 96.0;
const KNOB_DIAMETER: f32 = 40.0;
const BUTTON_SIZE: Vec2 = Vec2::new(40.0, 28.0);
const WHITE_KEY: Vec2 = Vec2::new(26.0, 96.0);

/// White keys on the on-screen keyboard: two octaves and the C that closes them.
const WHITE_KEYS: usize = 15;

/// The lowest and highest C the keyboard can start on, so its top key stays a MIDI note.
const LOWEST_KEY: u8 = 0;
const HIGHEST_LOW_KEY: u8 = 96;

/// A knob turns through this much of a circle, centred on straight up.
const KNOB_SWEEP: f32 = 1.5 * std::f32::consts::PI;

/// Pointer travel, in points, that takes a knob across its whole range.
const KNOB_TRAVEL: f32 = 160.0;

pub fn header(ui: &mut egui::Ui, surface: Option<&Path>, running: bool) {
    ui.horizontal(|ui| {
        theme::section(ui, "Controls");
        ui.weak(match surface {
            Some(path) => format!("from {}", path.display()),
            None => "generated from the instrument's interface".to_string(),
        });
        if !running {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "the engine is not running, so nothing is sent",
            );
        }
    });
}

pub fn warnings(ui: &mut egui::Ui, warnings: &[String]) {
    for warning in warnings {
        ui.colored_label(ui.visuals().warn_fg_color, format!("warning: {warning}"));
    }
}

/// Draw every control and return what the player's gestures this frame send, in order.
pub fn panel(
    ui: &mut egui::Ui,
    panel: &Panel,
    settings: &mut [Setting],
    low_key: &mut u8,
) -> Vec<ControlSendMessage> {
    if panel.controls.is_empty() {
        ui.weak("This panel has no controls.");
    }
    let mut messages = Vec::new();
    for row in panel.rows() {
        // Only a grouped run is ever longer than `cols`: a lane, which fits as upright faders.
        let upright = row.len() > panel.cols;
        ui.horizontal(|ui| {
            for index in row {
                let control = &panel.controls[index];
                ui.push_id(index, |ui| {
                    let gestures = draw(ui, control, &mut settings[index], upright, low_key);
                    messages.extend(gestures.into_iter().filter_map(|g| control.message(g)));
                });
            }
        });
    }
    messages
}

fn draw(
    ui: &mut egui::Ui,
    control: &Control,
    setting: &mut Setting,
    upright: bool,
    low_key: &mut u8,
) -> Vec<Gesture> {
    match (&control.kind, setting) {
        (kind @ Kind::Value { shape, .. }, Setting::Value(value)) => {
            let before = *value;
            match shape {
                Shape::Fader => fader(ui, &control.label, kind, value, upright),
                Shape::Radial => knob(ui, &control.label, kind, value),
            }
            if *value == before {
                Vec::new()
            } else {
                vec![Gesture::Set(*value)]
            }
        }
        (Kind::Toggle { .. } | Kind::NoteToggle { .. }, Setting::On(on)) => {
            let clicked = ui
                .add(
                    egui::Button::new(&control.label)
                        .selected(*on)
                        .min_size(BUTTON_SIZE),
                )
                .on_hover_text(&control.bind)
                .clicked();
            if clicked {
                *on = !*on;
                vec![Gesture::Switch(*on)]
            } else {
                Vec::new()
            }
        }
        (Kind::ChordButton { .. }, Setting::On(held)) => {
            let down = ui
                .add(
                    egui::Button::new(&control.label)
                        .selected(*held)
                        .sense(Sense::drag())
                        .min_size(BUTTON_SIZE * 1.5),
                )
                .is_pointer_button_down_on();
            if down == *held {
                Vec::new()
            } else {
                *held = down;
                vec![Gesture::Switch(down)]
            }
        }
        (Kind::Choice { variants, .. }, Setting::Choice(chosen)) => {
            let before = *chosen;
            ui.vertical(|ui| {
                ui.label(&control.label);
                egui::ComboBox::from_id_salt("choice")
                    .selected_text(variants.get(*chosen).map_or("", String::as_str))
                    .show_ui(ui, |ui| {
                        for (index, variant) in variants.iter().enumerate() {
                            ui.selectable_value(chosen, index, variant);
                        }
                    });
            });
            if *chosen == before {
                Vec::new()
            } else {
                vec![Gesture::Pick(*chosen)]
            }
        }
        (Kind::Keyboard, Setting::Key(held)) => keyboard(ui, &control.label, held, low_key),
        // `Setting::at_rest` pairs each kind with its own setting, so nothing else occurs.
        _ => Vec::new(),
    }
}

/// The value as the player reads it: whole for an integer control, otherwise to a precision that
/// follows its size, with the input's unit.
fn readout(value: f64, integer: bool, unit: &str) -> String {
    let number = if integer || value.abs() >= 100.0 {
        format!("{value:.0}")
    } else if value.abs() >= 10.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.2}")
    };
    if unit.is_empty() {
        number
    } else {
        format!("{number} {unit}")
    }
}

fn fader(ui: &mut egui::Ui, label: &str, kind: &Kind, value: &mut f64, vertical: bool) {
    let Kind::Value {
        min,
        max,
        integer,
        exponential,
        unit,
        ..
    } = kind
    else {
        return;
    };
    let (Some(min), Some(max)) = (*min, *max) else {
        // No declared range to span, so the value is dragged rather than slid.
        ui.vertical(|ui| {
            ui.label(label);
            let mut drag = egui::DragValue::new(value).speed(0.01);
            if let Some(min) = min {
                drag = drag.range(*min..=f64::INFINITY);
            }
            if let Some(max) = max {
                drag = drag.range(f64::NEG_INFINITY..=*max);
            }
            ui.add(drag);
        });
        return;
    };
    let (exponential, integer) = (*exponential, *integer);
    if vertical {
        ui.vertical(|ui| {
            ui.set_width(BUTTON_SIZE.x);
            ui.spacing_mut().slider_width = SLIDER_LENGTH;
            ui.add(slider(value, min, max, exponential, integer).vertical());
            ui.label(label);
            ui.weak(readout(*value, integer, unit));
        });
    } else {
        ui.vertical(|ui| {
            ui.spacing_mut().slider_width = SLIDER_LENGTH * 1.5;
            ui.label(label);
            ui.horizontal(|ui| {
                ui.add(slider(value, min, max, exponential, integer));
                ui.weak(readout(*value, integer, unit));
            });
        });
    }
}

fn slider(
    value: &mut f64,
    min: f64,
    max: f64,
    exponential: bool,
    integer: bool,
) -> egui::Slider<'_> {
    let slider = egui::Slider::new(value, min..=max)
        // A logarithmic slider cannot start at zero.
        .logarithmic(exponential && min > 0.0)
        .show_value(false);
    if integer {
        slider.integer()
    } else {
        slider
    }
}

fn knob(ui: &mut egui::Ui, label: &str, kind: &Kind, value: &mut f64) {
    let Kind::Value {
        min: Some(min),
        max: Some(max),
        integer,
        exponential,
        unit,
        ..
    } = kind
    else {
        // A knob has no travel without a range; the fader's unranged form serves.
        return fader(ui, label, kind, value, false);
    };
    ui.vertical(|ui| {
        ui.set_width(CELL_WIDTH);
        ui.label(label);
        let (rect, response) =
            ui.allocate_exact_size(Vec2::splat(KNOB_DIAMETER), Sense::click_and_drag());
        let mut unit_value = model::to_unit(*value, *min, *max, *exponential);
        if response.dragged() && response.drag_delta() != Vec2::ZERO {
            // The turn is kept as a continuous position for the length of the drag. Read back
            // from `value` each frame it would lose whatever a whole-numbered knob rounded away,
            // and a drag slower than one step a frame would never leave the step it began on.
            let start = unit_value;
            let delta = response.drag_delta();
            // Up or right turns it up.
            let turn = f64::from(delta.x - delta.y) / f64::from(KNOB_TRAVEL);
            unit_value = ui.data_mut(|data| {
                let position = data.get_temp_mut_or(response.id, start);
                *position = (*position + turn).clamp(0.0, 1.0);
                *position
            });
            let moved = model::from_unit(unit_value, *min, *max, *exponential);
            *value = if *integer { moved.round() } else { moved };
        } else if !response.dragged() {
            ui.data_mut(|data| data.remove_temp::<f64>(response.id));
        }
        if response.double_clicked() {
            if let Kind::Value { rest, .. } = kind {
                *value = *rest;
            }
        }
        let visuals = ui.style().interact(&response);
        let painter = ui.painter();
        let radius = rect.width() / 2.0 - 2.0;
        painter.circle(rect.center(), radius, visuals.bg_fill, visuals.fg_stroke);
        let angle = (unit_value as f32 - 0.5) * KNOB_SWEEP;
        let tip = rect.center() + radius * Vec2::new(angle.sin(), -angle.cos());
        painter.line_segment(
            [rect.center(), tip],
            Stroke::new(2.0, visuals.fg_stroke.color),
        );
        ui.weak(readout(*value, *integer, unit));
    });
}

fn keyboard(
    ui: &mut egui::Ui,
    label: &str,
    held: &mut Option<u8>,
    low_key: &mut u8,
) -> Vec<Gesture> {
    let mut gestures = Vec::new();
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            ui.label(label);
            if ui
                .add_enabled(*low_key > LOWEST_KEY, egui::Button::new("octave −"))
                .clicked()
            {
                *low_key -= 12;
            }
            if ui
                .add_enabled(*low_key < HIGHEST_LOW_KEY, egui::Button::new("octave +"))
                .clicked()
            {
                *low_key += 12;
            }
            // MIDI 60 is C4.
            ui.weak(format!("from C{}", i16::from(*low_key) / 12 - 1));
        });
        let keys = model::keys(*low_key, WHITE_KEYS);
        let size = Vec2::new(WHITE_KEY.x * WHITE_KEYS as f32, WHITE_KEY.y);
        let (rect, response) = ui.allocate_exact_size(size, Sense::drag());

        // Sliding across the keys while the button is down plays each one in turn.
        let under = response
            .is_pointer_button_down_on()
            .then(|| response.interact_pointer_pos())
            .flatten()
            .and_then(|pos| {
                let at = pos - rect.min;
                model::key_at(&keys, at.x / WHITE_KEY.x, at.y / WHITE_KEY.y)
            });
        if under != *held {
            if let Some(midi) = *held {
                gestures.push(Gesture::Key { midi, down: false });
            }
            if let Some(midi) = under {
                gestures.push(Gesture::Key { midi, down: true });
            }
            *held = under;
        }

        let painter = ui.painter();
        let held_fill = ui.visuals().selection.bg_fill;
        // White keys first, so the black keys paint over the boundaries they straddle.
        for black in [false, true] {
            for key in keys.iter().filter(|k| k.black == black) {
                let depth = if black { BLACK_DEPTH } else { 1.0 };
                let key_rect = Rect::from_min_max(
                    rect.min + Vec2::new(key.left * WHITE_KEY.x, 0.0),
                    rect.min + Vec2::new(key.right * WHITE_KEY.x, depth * WHITE_KEY.y),
                );
                let fill = if *held == Some(key.midi) {
                    held_fill
                } else if black {
                    theme::KEY_BLACK
                } else {
                    theme::KEY_WHITE
                };
                painter.rect(
                    key_rect,
                    2.0,
                    fill,
                    Stroke::new(1.0, theme::KEY_EDGE),
                    StrokeKind::Inside,
                );
            }
        }
    });
    gestures
}

#[cfg(test)]
mod tests {
    use eframe::egui::{Event, Modifiers, PointerButton, Pos2, RawInput};

    use super::*;

    /// One control drawn at the top-left of a headless context, fed pointer events a frame at a
    /// time.
    struct Stage {
        ctx: egui::Context,
        control: Control,
        setting: Setting,
        low_key: u8,
    }

    impl Stage {
        fn new(kind: Kind) -> Self {
            let control = Control {
                bind: "x".to_string(),
                label: "X".to_string(),
                group: None,
                kind,
            };
            let mut stage = Self {
                ctx: egui::Context::default(),
                setting: Setting::at_rest(&control),
                control,
                low_key: 48,
            };
            // Widgets are hit-tested against where the previous frame put them.
            assert_eq!(stage.frames(Vec::new()), Vec::new());
            stage
        }

        /// Feed `events`, then let one more frame pass, and return every gesture made.
        fn frames(&mut self, events: Vec<Event>) -> Vec<Gesture> {
            let mut gestures = Vec::new();
            for events in [events, Vec::new()] {
                let input = RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 400.0))),
                    events,
                    ..Default::default()
                };
                let output = self.ctx.run_ui(input, |ui| {
                    gestures.extend(draw(
                        ui,
                        &self.control,
                        &mut self.setting,
                        false,
                        &mut self.low_key,
                    ));
                });
                // Nothing here paints, and the font atlas it would upload refuses a silent drop.
                output.drop_without_applying_deltas();
            }
            gestures
        }

        fn button(&mut self, pos: Pos2, pressed: bool) -> Vec<Gesture> {
            self.frames(vec![
                Event::PointerMoved(pos),
                Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed,
                    modifiers: Modifiers::NONE,
                },
            ])
        }
    }

    #[test]
    fn a_toggle_latches_on_each_click() {
        let mut stage = Stage::new(Kind::Toggle { rest: false });
        let at = Pos2::new(20.0, 16.0);
        assert_eq!(stage.button(at, true), Vec::new());
        assert_eq!(stage.button(at, false), vec![Gesture::Switch(true)]);
        assert_eq!(stage.button(at, true), Vec::new());
        assert_eq!(stage.button(at, false), vec![Gesture::Switch(false)]);
    }

    #[test]
    fn a_chord_button_plays_for_as_long_as_it_is_held() {
        let mut stage = Stage::new(Kind::ChordButton { degree: 0 });
        let at = Pos2::new(20.0, 16.0);
        assert_eq!(stage.button(at, true), vec![Gesture::Switch(true)]);
        assert_eq!(stage.frames(Vec::new()), Vec::new());
        assert_eq!(stage.button(at, false), vec![Gesture::Switch(false)]);
    }

    #[test]
    fn the_keyboard_plays_the_key_under_the_pointer_and_releases_it() {
        let mut stage = Stage::new(Kind::Keyboard);
        // Low on the keys, where only white keys reach: the keys sit under a row of buttons.
        let y = 110.0;
        let white = |n: f32| Pos2::new(8.0 + WHITE_KEY.x * (n + 0.5), y);
        let key = |midi, down| Gesture::Key { midi, down };

        assert_eq!(stage.button(white(0.0), true), vec![key(48, true)]);
        assert_eq!(
            stage.frames(vec![Event::PointerMoved(white(2.0))]),
            vec![key(48, false), key(52, true)]
        );
        assert_eq!(stage.button(white(2.0), false), vec![key(52, false)]);
        assert_eq!(stage.setting, Setting::Key(None));
    }

    fn value_kind(shape: Shape, min: f64, max: f64, rest: f64, integer: bool) -> Kind {
        Kind::Value {
            shape,
            min: Some(min),
            max: Some(max),
            rest,
            integer,
            exponential: false,
            unit: String::new(),
        }
    }

    /// Press at `from` and move right `step` pixels a frame for `frames` frames, then release.
    fn drag_right(stage: &mut Stage, from: Pos2, step: f32, frames: usize) -> Vec<Gesture> {
        let mut gestures = stage.button(from, true);
        let mut at = from;
        for _ in 0..frames {
            at.x += step;
            gestures.extend(stage.frames(vec![Event::PointerMoved(at)]));
        }
        gestures.extend(stage.button(at, false));
        gestures
    }

    /// A step of an integer knob is many pixels of travel, so a drag slower than a step a frame
    /// has to add up across frames.
    #[test]
    fn a_whole_numbered_knob_turns_under_a_slow_drag() {
        let mut stage = Stage::new(value_kind(Shape::Radial, 1.0, 16.0, 4.0, true));
        let on_knob = Pos2::new(KNOB_DIAMETER / 2.0, 18.0 + KNOB_DIAMETER / 2.0);

        let gestures = drag_right(&mut stage, on_knob, 1.0, KNOB_TRAVEL as usize + 20);

        assert_eq!(stage.setting, Setting::Value(16.0), "{gestures:?}");
        assert!(gestures.len() >= 12, "{gestures:?}");
        assert!(
            gestures
                .iter()
                .all(|g| matches!(g, Gesture::Set(v) if v.fract() == 0.0)),
            "{gestures:?}"
        );
    }

    #[test]
    fn a_knob_returns_to_its_rest_value_on_a_double_click() {
        let mut stage = Stage::new(value_kind(Shape::Radial, 0.0, 1.0, 0.25, false));
        let on_knob = Pos2::new(KNOB_DIAMETER / 2.0, 18.0 + KNOB_DIAMETER / 2.0);
        drag_right(&mut stage, on_knob, 4.0, 10);
        assert_ne!(stage.setting, Setting::Value(0.25));

        stage.button(on_knob, true);
        stage.button(on_knob, false);
        stage.button(on_knob, true);
        let gestures = stage.button(on_knob, false);

        assert_eq!(stage.setting, Setting::Value(0.25));
        assert_eq!(gestures, vec![Gesture::Set(0.25)]);
    }

    #[test]
    fn dragging_a_fader_sets_its_value() {
        let mut stage = Stage::new(value_kind(Shape::Fader, 0.0, 1.0, 0.0, false));

        let gestures = drag_right(&mut stage, Pos2::new(12.0, 34.0), 4.0, 20);

        assert!(
            matches!(gestures.last(), Some(Gesture::Set(v)) if *v > 0.2),
            "{gestures:?}"
        );
    }
}
