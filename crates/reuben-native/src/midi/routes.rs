//! The route table: each usable control of a MIDI map, checked against the played instrument's
//! interface inputs and compiled to a pattern, a target address and the scaling the target needs.

use reuben_api::authoring::Boundary;
use reuben_api::render::Arg;
use reuben_api::surface::{check_midi_control, MidiKind, MidiRoute, SurfaceDoc};

pub use reuben_api::surface::Scale;

pub type Route = MidiRoute;

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
        .filter_map(|control| match check_midi_control(control, boundary) {
            Ok((route, adjusted)) => {
                warnings.extend(adjusted);
                Some(route)
            }
            Err(why) => {
                warnings.push(why);
                None
            }
        })
        .collect();
    (RouteTable { routes }, warnings)
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

    /// A boundary with the one input the tests bind.
    fn boundary() -> Boundary {
        serde_json::from_value(json!({
            "instrument": "inst",
            "inputs": [
                {"name": "level", "kind": "value", "min": 0.0, "max": 1.0, "curve": "linear"}
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

    #[test]
    fn a_skipped_control_warns_naming_it() {
        let (table, warnings) = build_routes(
            &doc(json!([{"bind": "gone", "midi": "/midi/1/cc/1"}])),
            &boundary(),
        );
        assert!(table.routes.is_empty());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("\"gone\""), "{warnings:?}");
        assert!(warnings[0].contains("no interface input"), "{warnings:?}");
    }

    #[test]
    fn an_adjusted_control_keeps_its_route_and_warns() {
        let (table, warnings) = build_routes(
            &doc(json!([{"bind": "level", "midi": "/midi/1/cc/7", "min": -1}])),
            &boundary(),
        );
        assert_eq!(table.routes.len(), 1);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
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
