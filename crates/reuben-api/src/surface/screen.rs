//! Checking a screen-surface control against the instrument's interface inputs: the half of
//! reading a screen doc that does not depend on how a client draws.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::authoring::{Boundary, PortInfo};
use crate::surface::SurfaceControl;

/// Every widget kind the surface schema names, drawn or reserved.
const WIDGETS: [&str; 9] = [
    "fader",
    "radial",
    "param-toggle",
    "note-toggle",
    "chord-button",
    "xy-pad",
    "grid",
    "visualizer",
    "keyboard",
];

/// One screen control checked against its interface input, with the defaults filled in and the
/// narrowing clamped into the input's range.
#[derive(Debug, Clone, PartialEq)]
pub struct ScreenControl {
    pub bind: String,
    pub label: String,
    pub widget: String,
    /// The narrowed range of a `fader` or `radial`, falling back to the input's own; `None` for
    /// every other widget.
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub note: Option<i64>,
    pub velocity: f64,
    pub degree: Option<i32>,
    pub group: Option<String>,
}

/// The default label for an input: underscores become spaces and each word's first letter is
/// uppercased (`kick_step1` -> `Kick Step1`). Every surface-doc reader derives it the same way, so
/// the labels match across surfaces.
pub fn default_label(name: &str) -> String {
    name.replace('_', " ")
        .split(' ')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        })
        .collect::<Vec<String>>()
        .join(" ")
}

/// `value` held inside whichever of `min`/`max` are set.
pub fn clamp(value: f64, min: Option<f64>, max: Option<f64>) -> f64 {
    let value = min.map_or(value, |min| value.max(min));
    max.map_or(value, |max| value.min(max))
}

/// Whether a value control can drive this input: a held scalar, an integer, or a signal with a
/// declared range. A signal with none is audio.
pub fn takes_a_value(port: &PortInfo) -> bool {
    match port.kind.as_str() {
        "value" | "int" => true,
        "signal" => port.min.is_some() && port.max.is_some(),
        _ => false,
    }
}

/// Whether a `widget` from [`WIDGETS`] can drive `port`. A value widget sends a number and a note
/// widget sends a note, so each needs the input kind that takes one. A `grid` is a field of cells
/// that are steps or pads, so it takes either; a `visualizer` shows its input and sends nothing.
fn fits(widget: &str, port: &PortInfo) -> bool {
    match widget {
        "fader" | "radial" | "param-toggle" | "xy-pad" => takes_a_value(port),
        "note-toggle" | "chord-button" | "keyboard" => port.kind == "message",
        "grid" => takes_a_value(port) || port.kind == "message",
        _ => true,
    }
}

/// Checks one control of a screen doc against `boundary`. `Err` is why the control cannot be used;
/// `Ok` carries the resolved control and a warning for each adjustment made to it.
pub fn check_screen_control(
    control: &SurfaceControl,
    boundary: &Boundary,
) -> Result<(ScreenControl, Vec<String>), String> {
    let bind = &control.bind;
    let Some(port) = boundary.inputs.iter().find(|p| &p.name == bind) else {
        return Err(format!(
            "surface control binds `{bind}`, which is not an interface input — skipped"
        ));
    };
    let label = control.label.clone().unwrap_or_else(|| default_label(bind));
    let widget = match control.widget.as_deref() {
        Some(widget) => widget,
        None if port.channel.is_none() && takes_a_value(port) => "fader",
        None => {
            return Err(format!(
                "surface control `{label}` binds `{bind}` ({}) with no `widget`, and that input \
                 has no default widget — skipped",
                port.kind
            ))
        }
    };
    if !WIDGETS.contains(&widget) {
        return Err(format!(
            "surface control `{label}` (input `{bind}`) names an unknown `{widget}` widget — skipped"
        ));
    }
    if !fits(widget, port) {
        return Err(format!(
            "surface control `{label}` (input `{bind}`) is a `{widget}`, which cannot drive a {} \
             input — skipped",
            port.kind
        ));
    }
    let mut adjusted = Vec::new();
    let (mut min, mut max) = (None, None);
    let mut note = None;
    let mut degree = None;
    match widget {
        "fader" | "radial" => {
            let mut narrow = |name: &str, given: Option<f64>, declared: Option<f64>| {
                let Some(given) = given else { return declared };
                let inside = clamp(given, port.min, port.max);
                if inside != given {
                    adjusted.push(format!(
                        "surface control `{label}`: {name} {given} is outside the input's range \
                         — using {inside}"
                    ));
                }
                Some(inside)
            };
            let low = narrow("min", control.min, port.min);
            let high = narrow("max", control.max, port.max);
            // A range with nothing in it clamps every send to one value while the widget still
            // shows travel; the input's own range is the honest fallback.
            (min, max) = match (low, high) {
                (Some(low), Some(high)) if low > high => {
                    adjusted.push(format!(
                        "surface control `{label}`: min {low} is above max {high} — using the \
                         input's own range"
                    ));
                    (port.min, port.max)
                }
                range => range,
            };
        }
        "note-toggle" => {
            note = Some(control.note.ok_or_else(|| {
                format!("note-toggle `{label}` (input `{bind}`) has no `note` — skipped")
            })?);
        }
        "chord-button" => {
            degree = Some(
                control
                    .degree
                    .and_then(|degree| i32::try_from(degree).ok())
                    .ok_or_else(|| {
                        format!("chord-button `{label}` (input `{bind}`) has no `degree` — skipped")
                    })?,
            );
        }
        _ => {}
    }
    Ok((
        ScreenControl {
            bind: bind.clone(),
            label,
            widget: widget.to_string(),
            min,
            max,
            note,
            velocity: control.velocity.unwrap_or(1.0),
            degree,
            group: control.group.clone(),
        },
        adjusted,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authoring::{Boundary, PortInfo};
    use crate::surface::SurfaceControl;

    #[test]
    fn default_labels_uppercase_each_word_and_nothing_else() {
        assert_eq!(default_label("kick_step1"), "Kick Step1");
        assert_eq!(default_label("mix2out"), "Mix2out");
        assert_eq!(default_label("tempo"), "Tempo");
    }

    fn value_port(name: &str) -> PortInfo {
        serde_json::from_value(serde_json::json!({
            "name": name, "kind": "value", "min": 0.0, "max": 1.0, "default": 0.5
        }))
        .expect("a port")
    }

    fn boundary() -> Boundary {
        serde_json::from_value(serde_json::json!({
            "instrument": "toy",
            "inputs": [
                value_port("level"),
                {"name": "notes", "kind": "message"},
                {"name": "mode", "kind": "enum", "variants": ["a", "b"]},
                {"name": "audio", "kind": "signal"},
            ],
            "outputs": [],
        }))
        .expect("a boundary")
    }

    fn control(json: &str) -> SurfaceControl {
        serde_json::from_str(json).expect("a control")
    }

    #[test]
    fn screen_accepts_reserved_widgets() {
        for (bind, widget) in [
            ("level", "xy-pad"),
            ("level", "grid"),
            ("notes", "grid"),
            ("level", "visualizer"),
            ("notes", "keyboard"),
        ] {
            let c = control(&format!(r#"{{"bind":"{bind}","widget":"{widget}"}}"#));
            let (screen, _) = check_screen_control(&c, &boundary()).expect(widget);
            assert_eq!(screen.widget, widget);
        }
    }

    #[test]
    fn screen_refuses_a_widget_that_does_not_fit_its_inputs_kind() {
        for (bind, widget, kind) in [
            ("notes", "fader", "message"),
            ("notes", "radial", "message"),
            ("notes", "param-toggle", "message"),
            ("notes", "xy-pad", "message"),
            ("mode", "fader", "enum"),
            ("audio", "fader", "signal"),
            ("level", "keyboard", "value"),
            ("level", "note-toggle", "value"),
            ("level", "chord-button", "value"),
            ("mode", "grid", "enum"),
        ] {
            let c = control(&format!(
                r#"{{"bind":"{bind}","widget":"{widget}","note":60,"degree":1}}"#
            ));
            let error = check_screen_control(&c, &boundary()).expect_err(widget);
            assert!(
                error.contains(&format!("`{widget}`, which cannot drive a {kind} input")),
                "{bind} {widget}: {error}"
            );
        }
    }

    #[test]
    fn screen_accepts_each_shipped_widget_on_the_kind_it_fits() {
        for json in [
            r#"{"bind":"level","widget":"fader"}"#,
            r#"{"bind":"level","widget":"radial"}"#,
            r#"{"bind":"level","widget":"param-toggle"}"#,
            r#"{"bind":"notes","widget":"note-toggle","note":60}"#,
            r#"{"bind":"notes","widget":"chord-button","degree":2}"#,
        ] {
            check_screen_control(&control(json), &boundary()).expect(json);
        }
    }

    #[test]
    fn screen_refuses_unknown_bind_and_missing_payloads() {
        for json in [
            r#"{"bind":"nope"}"#,
            r#"{"bind":"notes","widget":"note-toggle"}"#,
            r#"{"bind":"notes","widget":"chord-button"}"#,
            r#"{"bind":"level","widget":"sparkle"}"#,
        ] {
            let error = check_screen_control(&control(json), &boundary()).expect_err(json);
            assert!(
                error.contains("nope") || error.contains("level") || error.contains("notes"),
                "{json}: {error}"
            );
        }
    }

    #[test]
    fn screen_clamps_narrowing_with_a_warning() {
        let c = control(r#"{"bind":"level","min":-5}"#);
        let (screen, warnings) = check_screen_control(&c, &boundary()).expect("ok");
        assert_eq!(screen.min, Some(0.0));
        assert_eq!(screen.widget, "fader");
        assert_eq!(screen.label, "Level");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
    }
}
