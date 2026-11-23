//! Checking a MIDI-map control against the instrument's interface inputs: the half of reading a
//! MIDI map that does not depend on how a client routes messages.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use reuben_contract::{NUMBER_MAX, NUMBER_MIN};

use crate::authoring::{Boundary, PortInfo};
use crate::surface::pattern::OscPattern;
use crate::surface::{clamp, SurfaceControl};

/// The message kinds a MIDI map can bind; the third address segment names one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidiKind {
    Note,
    Cc,
    Bend,
    Pressure,
    Program,
}

impl MidiKind {
    pub const ALL: [MidiKind; 5] = [
        Self::Note,
        Self::Cc,
        Self::Bend,
        Self::Pressure,
        Self::Program,
    ];

    /// The kind's address segment.
    pub fn segment(self) -> &'static str {
        match self {
            Self::Note => "note",
            Self::Cc => "cc",
            Self::Bend => "bend",
            Self::Pressure => "pressure",
            Self::Program => "program",
        }
    }
}

/// How a translated value becomes the target's value.
#[derive(Debug, Clone, PartialEq)]
pub enum Scale {
    /// Sent as translated: a note, or a program number (an identity, not a position).
    None,
    Linear {
        min: f64,
        max: f64,
        int: bool,
    },
    /// Exponential only while both bounds are above zero; otherwise linear, as the `map`
    /// Operator does.
    Exponential {
        min: f64,
        max: f64,
        int: bool,
    },
}

/// One MIDI-map control checked against its interface input: the pattern it matches, the address
/// it drives and the scaling that address needs.
#[derive(Debug, Clone)]
pub struct MidiRoute {
    pub pattern: OscPattern,
    pub kind: MidiKind,
    /// `/<bind>/in`.
    pub target: String,
    pub scale: Scale,
}

/// Checks one control of a MIDI map against `boundary`. `Err` is why the control cannot be used;
/// `Ok` carries the route and a warning for each adjustment made to it. The `Err` names the control.
pub fn check_midi_control(
    control: &SurfaceControl,
    boundary: &Boundary,
) -> Result<(MidiRoute, Vec<String>), String> {
    check(control, boundary).map_err(|why| format!("control \"{}\" skipped: {why}", control.bind))
}

fn check(
    control: &SurfaceControl,
    boundary: &Boundary,
) -> Result<(MidiRoute, Vec<String>), String> {
    let mut warnings = Vec::new();
    let text = control.midi.as_deref().ok_or("has no midi pattern")?;
    let pattern = OscPattern::parse(text).map_err(|e| e.to_string())?;
    let kind = pattern.midi_kind().map_err(|e| format!("{text:?} {e}"))?;
    let input = boundary
        .inputs
        .iter()
        .find(|input| input.name == control.bind)
        .ok_or("names no interface input of the instrument")?;
    let fits = match kind {
        MidiKind::Note => input.kind == "message",
        MidiKind::Program => input.kind == "int",
        MidiKind::Cc | MidiKind::Bend | MidiKind::Pressure => {
            matches!(input.kind.as_str(), "value" | "int" | "signal")
        }
    };
    if !fits {
        return Err(format!(
            "a {} input cannot take {}",
            input.kind,
            kind.segment()
        ));
    }
    let scale = match kind {
        MidiKind::Note | MidiKind::Program => Scale::None,
        MidiKind::Cc | MidiKind::Bend | MidiKind::Pressure => {
            let (min, max) = range(control, input, &mut warnings)?;
            let int = input.kind == "int";
            if input.curve.as_deref() == Some("exponential") {
                Scale::Exponential { min, max, int }
            } else {
                Scale::Linear { min, max, int }
            }
        }
    };
    let route = MidiRoute {
        pattern,
        kind,
        target: format!("/{}/in", control.bind),
        scale,
    };
    Ok((route, warnings))
}

/// The range to scale into: the control's narrowing, clamped into the input's, else the input's.
/// A bound the input leaves undeclared is the type-wide one (`i32`'s own limits for an `i32`), which
/// a knob's travel cannot usefully reach, so both ends must be declared or narrowed.
fn range(
    control: &SurfaceControl,
    input: &PortInfo,
    warnings: &mut Vec<String>,
) -> Result<(f64, f64), String> {
    let input_min = input.min.unwrap_or(f64::from(NUMBER_MIN));
    let input_max = input.max.unwrap_or(f64::from(NUMBER_MAX));
    let held = |v: f64| clamp(v, Some(input_min), Some(input_max));
    let min = control.min.map_or(input_min, held);
    let max = control.max.map_or(input_max, held);
    let unbounded = |v: f64| v <= f64::from(NUMBER_MIN) || v >= f64::from(NUMBER_MAX);
    if unbounded(min) || unbounded(max) {
        return Err(
            "no range to scale into: the input is unbounded on at least one side; set min and max on the control"
                .into(),
        );
    }
    if control.min.is_some_and(|v| v != min) || control.max.is_some_and(|v| v != max) {
        warnings.push(format!(
            "control \"{}\": range {}..{} clamped into the input's {input_min}..{input_max}",
            control.bind,
            control.min.map_or("".into(), |v| v.to_string()),
            control.max.map_or("".into(), |v| v.to_string()),
        ));
    }
    Ok((min, max))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use serde_json::json;

    /// A boundary over every input kind the checks distinguish.
    fn boundary() -> Boundary {
        serde_json::from_value(json!({
            "instrument": "inst",
            "inputs": [
                {"name": "cutoff", "kind": "value", "min": 20.0, "max": 20000.0, "curve": "exponential"},
                {"name": "level", "kind": "value", "min": 0.0, "max": 1.0, "curve": "linear"},
                {"name": "free", "kind": "value", "min": -1_000_000.0, "max": 1_000_000.0, "curve": "linear"},
                {"name": "steps", "kind": "int", "min": 0.0, "max": 16.0},
                {"name": "count", "kind": "int", "min": -2147483648.0, "max": 2147483647.0},
                {"name": "upto", "kind": "int", "min": -2147483648.0, "max": 16.0},
                {"name": "from", "kind": "value", "min": 0.0, "max": 1_000_000.0, "curve": "linear"},
                {"name": "sig", "kind": "signal"},
                {"name": "ranged_sig", "kind": "signal", "min": 0.0, "max": 1.0, "curve": "linear"},
                {"name": "notes", "kind": "message"},
                {"name": "mode", "kind": "enum", "variants": ["a", "b"]}
            ],
            "outputs": []
        }))
        .unwrap()
    }

    /// Checks one control and returns its route (if usable) and the warnings.
    fn one(control: serde_json::Value) -> (Result<MidiRoute, String>, Vec<String>) {
        let control: SurfaceControl = serde_json::from_value(control).unwrap();
        match check_midi_control(&control, &boundary()) {
            Ok((route, warnings)) => (Ok(route), warnings),
            Err(why) => (Err(why), vec![]),
        }
    }

    fn refused(control: serde_json::Value, reason: &str) {
        let bind = control["bind"].as_str().unwrap().to_string();
        let (route, warnings) = one(control);
        let why = route.expect_err(&format!("{bind} kept"));
        assert!(why.contains(reason), "{why:?} lacks {reason:?}");
        assert!(
            why.contains(&format!("\"{bind}\"")),
            "{why:?} lacks the bind"
        );
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn unusable_controls_are_refused_with_the_reason() {
        refused(json!({"bind": "level"}), "no midi pattern");
        refused(
            json!({"bind": "level", "midi": "/midi/[1/cc/1"}),
            "not an OSC address pattern",
        );
        refused(
            json!({"bind": "level", "midi": "/midi/1/*"}),
            "more than one message kind",
        );
        refused(json!({"bind": "level", "midi": "/fader"}), "/midi/");
        refused(
            json!({"bind": "gone", "midi": "/midi/1/cc/1"}),
            "no interface input",
        );
        refused(
            json!({"bind": "notes", "midi": "/midi/1/cc/1"}),
            "cannot take cc",
        );
        refused(
            json!({"bind": "level", "midi": "/midi/1/note"}),
            "cannot take note",
        );
        refused(
            json!({"bind": "level", "midi": "/midi/1/program"}),
            "cannot take program",
        );
        refused(
            json!({"bind": "mode", "midi": "/midi/1/cc/1"}),
            "cannot take cc",
        );
        refused(json!({"bind": "sig", "midi": "/midi/1/cc/1"}), "no range");
        refused(json!({"bind": "free", "midi": "/midi/1/bend"}), "no range");
    }

    #[test]
    fn a_range_unbounded_on_either_side_is_refused() {
        refused(json!({"bind": "count", "midi": "/midi/1/cc/1"}), "no range");
        refused(json!({"bind": "upto", "midi": "/midi/1/cc/1"}), "no range");
        refused(json!({"bind": "from", "midi": "/midi/1/cc/1"}), "no range");
        refused(
            json!({"bind": "free", "midi": "/midi/1/cc/1", "min": 0}),
            "no range",
        );
        // No warning, only the refusal, even when the narrowing was also clamped.
        refused(
            json!({"bind": "free", "midi": "/midi/1/cc/1", "min": -2e6}),
            "no range",
        );
        let (route, _) = one(json!({"bind": "upto", "midi": "/midi/1/cc/1", "min": 0}));
        assert_eq!(
            route.unwrap().scale,
            Scale::Linear {
                min: 0.0,
                max: 16.0,
                int: true
            }
        );
    }

    #[test]
    fn usable_controls_become_routes() {
        let (route, warnings) = one(json!({"bind": "ranged_sig", "midi": "/midi/1/cc/1"}));
        let route = route.unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(route.target, "/ranged_sig/in");
        assert_eq!(route.kind, MidiKind::Cc);
        assert_eq!(
            route.scale,
            Scale::Linear {
                min: 0.0,
                max: 1.0,
                int: false
            }
        );

        let (route, _) = one(json!({"bind": "steps", "midi": "/midi/1/cc/2"}));
        assert_eq!(
            route.unwrap().scale,
            Scale::Linear {
                min: 0.0,
                max: 16.0,
                int: true
            }
        );

        let (route, _) = one(json!({"bind": "steps", "midi": "/midi/1/program"}));
        assert_eq!(route.unwrap().scale, Scale::None);

        let (route, _) = one(json!({"bind": "notes", "midi": "/midi/[4-8]/note"}));
        let route = route.unwrap();
        assert_eq!((route.kind, route.scale), (MidiKind::Note, Scale::None));
        assert!(route.pattern.matches("/midi/6/note"));

        let (route, _) = one(json!({"bind": "cutoff", "midi": "/midi/1/cc/74"}));
        assert_eq!(
            route.unwrap().scale,
            Scale::Exponential {
                min: 20.0,
                max: 20000.0,
                int: false
            }
        );
    }

    #[test]
    fn a_narrowing_range_is_used() {
        let (route, warnings) =
            one(json!({"bind": "cutoff", "midi": "/midi/1/cc/74", "min": 200, "max": 8000}));
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            route.unwrap().scale,
            Scale::Exponential {
                min: 200.0,
                max: 8000.0,
                int: false
            }
        );
        // A narrowing gives an otherwise unbounded input the range it lacks.
        let (route, warnings) =
            one(json!({"bind": "free", "midi": "/midi/1/bend", "min": -2, "max": 2}));
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            route.unwrap().scale,
            Scale::Linear {
                min: -2.0,
                max: 2.0,
                int: false
            }
        );
    }

    #[test]
    fn a_narrowing_outside_the_input_is_clamped_with_a_warning() {
        let (route, warnings) =
            one(json!({"bind": "level", "midi": "/midi/1/cc/7", "min": -1, "max": 0.5}));
        assert_eq!(
            route.unwrap().scale,
            Scale::Linear {
                min: 0.0,
                max: 0.5,
                int: false
            }
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("\"level\""), "{warnings:?}");
    }
}
