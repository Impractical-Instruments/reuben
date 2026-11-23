//! The route table: each usable control of a MIDI map, checked against the played instrument's
//! interface inputs and compiled to a pattern, a target address and the scaling the target needs.

use reuben_api::authoring::{Boundary, PortInfo};
use reuben_api::render::Arg;
use reuben_contract::{NUMBER_MAX, NUMBER_MIN};

use super::pattern::OscPattern;
use super::translate::MidiKind;
use crate::surface::{SurfaceControl, SurfaceDoc};

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

#[derive(Debug, Clone)]
pub struct Route {
    pub pattern: OscPattern,
    pub kind: MidiKind,
    /// `/<bind>/in`.
    pub target: String,
    pub scale: Scale,
}

#[derive(Debug, Clone, Default)]
pub struct RouteTable {
    pub routes: Vec<Route>,
}

/// The routes of every usable control in `doc`, and one warning for each control skipped or
/// adjusted, naming it, plus one if `doc` was written for another instrument.
pub fn build_routes(doc: &SurfaceDoc, boundary: &Boundary) -> (RouteTable, Vec<String>) {
    let mut warnings = Vec::new();
    if doc.instrument != boundary.instrument {
        warnings.push(format!(
            "written for instrument \"{}\", used for \"{}\"",
            doc.instrument, boundary.instrument
        ));
    }
    let routes = doc
        .controls
        .iter()
        .filter_map(|control| match route(control, boundary, &mut warnings) {
            Ok(route) => Some(route),
            Err(why) => {
                warnings.push(format!("control \"{}\" skipped: {why}", control.bind));
                None
            }
        })
        .collect();
    (RouteTable { routes }, warnings)
}

fn route(
    control: &SurfaceControl,
    boundary: &Boundary,
    warnings: &mut Vec<String>,
) -> Result<Route, String> {
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
            let (min, max) = range(control, input, warnings)?;
            let int = input.kind == "int";
            if input.curve.as_deref() == Some("exponential") {
                Scale::Exponential { min, max, int }
            } else {
                Scale::Linear { min, max, int }
            }
        }
    };
    Ok(Route {
        pattern,
        kind,
        target: format!("/{}/in", control.bind),
        scale,
    })
}

/// The range to scale into: the control's narrowing, clamped into the input's, else the input's.
/// An input with no declared range has the type-wide one, which a knob's travel cannot usefully
/// span, so the control must narrow it.
fn range(
    control: &SurfaceControl,
    input: &PortInfo,
    warnings: &mut Vec<String>,
) -> Result<(f64, f64), String> {
    let input_min = input.min.unwrap_or(f64::from(NUMBER_MIN));
    let input_max = input.max.unwrap_or(f64::from(NUMBER_MAX));
    let clamp = |v: f64| v.clamp(input_min, input_max);
    let min = control.min.map_or(input_min, clamp);
    let max = control.max.map_or(input_max, clamp);
    if control.min.is_some_and(|v| v != min) || control.max.is_some_and(|v| v != max) {
        warnings.push(format!(
            "control \"{}\": range {}..{} clamped into the input's {input_min}..{input_max}",
            control.bind,
            control.min.map_or("".into(), |v| v.to_string()),
            control.max.map_or("".into(), |v| v.to_string()),
        ));
    }
    if min <= f64::from(NUMBER_MIN) && max >= f64::from(NUMBER_MAX) {
        return Err(
            "the input declares no range and the control gives none (set min and max)".into(),
        );
    }
    Ok((min, max))
}

/// The value `args` (as translated for `kind`) becomes under `scale`.
pub fn scale_value(scale: &Scale, kind: MidiKind, args: &[Arg]) -> Vec<Arg> {
    let (min, max, int, exponential) = match *scale {
        Scale::None => return args.to_vec(),
        Scale::Linear { min, max, int } => (min, max, int, false),
        Scale::Exponential { min, max, int } => (min, max, int, true),
    };
    let Some(Arg::F32(value)) = args.first() else {
        return args.to_vec();
    };
    let value = f64::from(*value);
    let u = match kind {
        MidiKind::Bend => (value + 1.0) / 2.0,
        _ => value,
    }
    .clamp(0.0, 1.0);
    let out = if exponential && min > 0.0 && max > 0.0 {
        min * (max / min).powf(u)
    } else {
        min + u * (max - min)
    };
    if int {
        vec![Arg::I32(out.round() as i32)]
    } else {
        vec![Arg::F32(out as f32)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
                {"name": "sig", "kind": "signal"},
                {"name": "ranged_sig", "kind": "signal", "min": 0.0, "max": 1.0, "curve": "linear"},
                {"name": "notes", "kind": "message"},
                {"name": "mode", "kind": "enum", "variants": ["a", "b"]}
            ],
            "outputs": []
        }))
        .unwrap()
    }

    fn doc(controls: serde_json::Value) -> SurfaceDoc {
        SurfaceDoc::parse(
            &json!({"surface_version": 1, "instrument": "inst", "controls": controls}).to_string(),
        )
        .unwrap()
    }

    /// Builds one control and returns its route (if kept) and the warnings.
    fn one(control: serde_json::Value) -> (Option<Route>, Vec<String>) {
        let (mut table, warnings) = build_routes(&doc(json!([control])), &boundary());
        assert!(table.routes.len() <= 1);
        (table.routes.pop(), warnings)
    }

    fn skipped(control: serde_json::Value, reason: &str) {
        let bind = control["bind"].as_str().unwrap().to_string();
        let (route, warnings) = one(control);
        assert!(route.is_none(), "{bind} kept");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains(&format!("\"{bind}\"")), "{warnings:?}");
        assert!(
            warnings[0].contains(reason),
            "{warnings:?} lacks {reason:?}"
        );
    }

    #[test]
    fn unusable_controls_are_skipped_with_a_warning_naming_them() {
        skipped(json!({"bind": "level"}), "no midi pattern");
        skipped(
            json!({"bind": "level", "midi": "/midi/[1/cc/1"}),
            "not an OSC address pattern",
        );
        skipped(
            json!({"bind": "level", "midi": "/midi/1/*"}),
            "more than one message kind",
        );
        skipped(json!({"bind": "level", "midi": "/fader"}), "/midi/");
        skipped(
            json!({"bind": "gone", "midi": "/midi/1/cc/1"}),
            "no interface input",
        );
        skipped(
            json!({"bind": "notes", "midi": "/midi/1/cc/1"}),
            "cannot take cc",
        );
        skipped(
            json!({"bind": "level", "midi": "/midi/1/note"}),
            "cannot take note",
        );
        skipped(
            json!({"bind": "level", "midi": "/midi/1/program"}),
            "cannot take program",
        );
        skipped(
            json!({"bind": "mode", "midi": "/midi/1/cc/1"}),
            "cannot take cc",
        );
        skipped(json!({"bind": "sig", "midi": "/midi/1/cc/1"}), "no range");
        skipped(json!({"bind": "free", "midi": "/midi/1/bend"}), "no range");
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

    #[test]
    fn another_instruments_map_is_used_with_a_warning() {
        let doc = SurfaceDoc::parse(
            &json!({"surface_version": 1, "instrument": "other",
                    "controls": [{"bind": "level", "midi": "/midi/1/cc/7"}]})
            .to_string(),
        )
        .unwrap();
        let (table, warnings) = build_routes(&doc, &boundary());
        assert_eq!(table.routes.len(), 1);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("other"), "{warnings:?}");
    }

    fn value(args: Vec<Arg>) -> f32 {
        match args.as_slice() {
            [Arg::F32(v)] => *v,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn cc_scales_along_an_exponential_curve() {
        let scale = Scale::Exponential {
            min: 200.0,
            max: 8000.0,
            int: false,
        };
        let at = |u: f32| value(scale_value(&scale, MidiKind::Cc, &[Arg::F32(u)]));
        assert!(
            (at(64.0 / 127.0) - 1283.42).abs() < 0.01,
            "{}",
            at(64.0 / 127.0)
        );
        assert!((at(0.0) - 200.0).abs() < 1e-3);
        assert!((at(1.0) - 8000.0).abs() < 1e-2);
    }

    #[test]
    fn bend_centre_is_the_middle_of_the_range() {
        let scale = Scale::Linear {
            min: -2.0,
            max: 2.0,
            int: false,
        };
        assert_eq!(
            value(scale_value(&scale, MidiKind::Bend, &[Arg::F32(0.0)])),
            0.0
        );
        assert_eq!(
            value(scale_value(&scale, MidiKind::Bend, &[Arg::F32(-1.0)])),
            -2.0
        );
    }

    #[test]
    fn an_int_target_rounds() {
        let scale = Scale::Linear {
            min: 0.0,
            max: 16.0,
            int: true,
        };
        assert_eq!(
            scale_value(&scale, MidiKind::Cc, &[Arg::F32(0.5)]),
            vec![Arg::I32(8)]
        );
    }

    #[test]
    fn exponential_over_a_range_touching_or_crossing_zero_is_linear() {
        for (min, max, want) in [(0.0, 10.0, 5.0), (-10.0, 10.0, 0.0)] {
            let scale = Scale::Exponential {
                min,
                max,
                int: false,
            };
            let got = value(scale_value(&scale, MidiKind::Pressure, &[Arg::F32(0.5)]));
            assert_eq!(got, want, "{min}..{max}");
        }
    }

    #[test]
    fn program_and_note_pass_through() {
        let program = vec![Arg::I32(5)];
        assert_eq!(
            scale_value(&Scale::None, MidiKind::Program, &program),
            program
        );
        let note = vec![Arg::F32(60.0), Arg::F32(0.5)];
        assert_eq!(scale_value(&Scale::None, MidiKind::Note, &note), note);
    }
}
