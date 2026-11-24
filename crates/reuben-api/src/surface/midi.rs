//! Checking a MIDI-map control against the instrument's interface inputs: the half of reading a
//! MIDI map that does not depend on how a client routes messages.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use reuben_contract::{NUMBER_MAX, NUMBER_MIN};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::authoring::{Boundary, PortInfo};
use crate::surface::pattern::{NumberPattern, NumberSet};
use crate::surface::{clamp, SurfaceControl};

/// The channels a route can name, as printed on hardware.
pub const CHANNELS: core::ops::RangeInclusive<u8> = 1..=16;
/// The controller numbers a `cc` route can name.
pub const CONTROLLERS: core::ops::RangeInclusive<u8> = 0..=127;

/// The message kinds a MIDI map can bind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
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

    /// The kind's name, as a route's `kind` spells it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Note => "note",
            Self::Cc => "cc",
            Self::Bend => "bend",
            Self::Pressure => "pressure",
            Self::Program => "program",
        }
    }

    /// Whether an interface input of `input_kind` can take this kind: a note is a message, a
    /// program number an identity only an int holds, and the rest positions to scale into a
    /// number.
    pub fn fits(self, input_kind: &str) -> bool {
        match self {
            Self::Note => input_kind == "message",
            Self::Program => input_kind == "int",
            Self::Cc | Self::Bend | Self::Pressure => {
                matches!(input_kind, "value" | "int" | "signal")
            }
        }
    }
}

/// A MIDI-map control's `midi`: the messages that drive its input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MidiSource {
    /// The message kind: `note`, `cc`, `bend`, `pressure` or `program`.
    pub kind: MidiKind,
    /// The channel, 1 to 16: an integer, or a pattern string such as `"1-4"` or `"*"`.
    pub channel: NumberPattern,
    /// The controller number, 0 to 127, for kind `cc` only: an integer, or a pattern string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cc: Option<NumberPattern>,
}

impl fmt::Display for MidiSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.kind.name())?;
        if let Some(cc) = &self.cc {
            write!(f, " {cc}")?;
        }
        write!(f, " on channel {}", self.channel)
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

/// One MIDI-map control checked against its interface input: the messages it matches, the
/// address it drives and the scaling that address needs.
#[derive(Debug, Clone)]
pub struct MidiRoute {
    pub kind: MidiKind,
    pub channels: NumberSet,
    /// The controller numbers of a `cc` route; `None` for every other kind.
    pub controllers: Option<NumberSet>,
    /// `/<bind>/in`.
    pub target: String,
    pub scale: Scale,
}

impl MidiRoute {
    /// Whether a message of `kind` on `channel` (and, for a control change, from `controller`)
    /// drives this route.
    pub fn matches(&self, kind: MidiKind, channel: u8, controller: Option<u8>) -> bool {
        kind == self.kind
            && self.channels.contains(channel)
            && match (self.controllers, controller) {
                (Some(controllers), Some(controller)) => controllers.contains(controller),
                (Some(_), None) => false,
                (None, _) => true,
            }
    }
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
    let source = control.midi.as_ref().ok_or("has no midi route")?;
    let kind = source.kind;
    let channels =
        NumberSet::parse(&source.channel, CHANNELS).map_err(|e| format!("channel {e}"))?;
    let controllers = match (kind, &source.cc) {
        (MidiKind::Cc, Some(cc)) => {
            Some(NumberSet::parse(cc, CONTROLLERS).map_err(|e| format!("cc {e}"))?)
        }
        (MidiKind::Cc, None) => return Err("kind cc needs a `cc` controller number".into()),
        (_, Some(_)) => {
            return Err(format!(
                "`cc` is for kind cc only, and this route is kind {}",
                kind.name()
            ))
        }
        (_, None) => None,
    };
    let input = boundary
        .inputs
        .iter()
        .find(|input| input.name == control.bind)
        .ok_or("names no interface input of the instrument")?;
    if !kind.fits(&input.kind) {
        return Err(format!(
            "a {} input cannot take {}",
            input.kind,
            kind.name()
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
        kind,
        channels,
        controllers,
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

    /// `midi` for a control change on channel 1 from controller `cc`.
    fn cc(cc: u8) -> serde_json::Value {
        json!({"kind": "cc", "channel": 1, "cc": cc})
    }

    /// `midi` for `kind` on channel 1.
    fn on_one(kind: &str) -> serde_json::Value {
        json!({"kind": kind, "channel": 1})
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
        refused(json!({"bind": "level"}), "no midi route");
        refused(json!({"bind": "gone", "midi": cc(1)}), "no interface input");
        refused(json!({"bind": "notes", "midi": cc(1)}), "cannot take cc");
        refused(
            json!({"bind": "level", "midi": on_one("note")}),
            "cannot take note",
        );
        refused(
            json!({"bind": "level", "midi": on_one("program")}),
            "cannot take program",
        );
        refused(json!({"bind": "mode", "midi": cc(1)}), "cannot take cc");
        refused(json!({"bind": "sig", "midi": cc(1)}), "no range");
        refused(json!({"bind": "free", "midi": on_one("bend")}), "no range");
    }

    #[test]
    fn kind_cc_needs_a_controller_and_no_other_kind_takes_one() {
        refused(
            json!({"bind": "level", "midi": on_one("cc")}),
            "kind cc needs a `cc` controller number",
        );
        for kind in ["note", "bend", "pressure", "program"] {
            refused(
                json!({"bind": "level", "midi": {"kind": kind, "channel": 1, "cc": 74}}),
                &format!("`cc` is for kind cc only, and this route is kind {kind}"),
            );
        }
    }

    #[test]
    fn a_channel_or_controller_out_of_bounds_is_refused() {
        for (midi, reason) in [
            (
                json!({"kind": "cc", "channel": 0, "cc": 1}),
                "channel 0 is outside 1-16",
            ),
            (
                json!({"kind": "cc", "channel": 17, "cc": 1}),
                "channel 17 is outside 1-16",
            ),
            (
                json!({"kind": "cc", "channel": "1-17", "cc": 1}),
                "channel 17 is outside 1-16",
            ),
            (
                json!({"kind": "cc", "channel": 1, "cc": 128}),
                "cc 128 is outside 0-127",
            ),
            (
                json!({"kind": "cc", "channel": 1, "cc": -1}),
                "cc -1 is outside 0-127",
            ),
            (
                json!({"kind": "cc", "channel": 1, "cc": "100-200"}),
                "cc 200 is outside 0-127",
            ),
        ] {
            refused(json!({"bind": "level", "midi": midi}), reason);
        }
    }

    #[test]
    fn a_malformed_or_reversed_pattern_is_refused() {
        for (midi, reason) in [
            (
                json!({"kind": "note", "channel": "[1-4]"}),
                "channel \"[1-4]\" is not a number pattern",
            ),
            (
                json!({"kind": "note", "channel": "1-"}),
                "channel \"1-\" is not a number pattern",
            ),
            (
                json!({"kind": "note", "channel": ""}),
                "channel \"\" is not a number pattern",
            ),
            (
                json!({"kind": "note", "channel": "8-4"}),
                "channel \"8-4\" is a reversed range",
            ),
            (
                json!({"kind": "cc", "channel": 1, "cc": "7{4,}"}),
                "cc \"7{4,}\" is not a number pattern",
            ),
            (
                json!({"kind": "cc", "channel": 1, "cc": "79-70"}),
                "cc \"79-70\" is a reversed range",
            ),
        ] {
            refused(json!({"bind": "notes", "midi": midi}), reason);
        }
    }

    #[test]
    fn a_route_matches_its_kind_channels_and_controllers() {
        let (route, _) = one(json!({"bind": "level",
                                    "midi": {"kind": "cc", "channel": "1-4,10", "cc": "70-79"}}));
        let route = route.unwrap();
        assert!(route.matches(MidiKind::Cc, 1, Some(74)));
        assert!(route.matches(MidiKind::Cc, 10, Some(70)));
        assert!(!route.matches(MidiKind::Cc, 5, Some(74)), "channel 5");
        assert!(!route.matches(MidiKind::Cc, 1, Some(80)), "cc 80");
        assert!(!route.matches(MidiKind::Cc, 1, None), "no controller");
        assert!(!route.matches(MidiKind::Pressure, 1, None), "another kind");

        let (route, _) = one(json!({"bind": "notes", "midi": {"kind": "note", "channel": "*"}}));
        let route = route.unwrap();
        assert!((1..=16).all(|channel| route.matches(MidiKind::Note, channel, None)));
        assert!(!route.matches(MidiKind::Cc, 1, Some(1)));
    }

    #[test]
    fn a_range_unbounded_on_either_side_is_refused() {
        refused(json!({"bind": "count", "midi": cc(1)}), "no range");
        refused(json!({"bind": "upto", "midi": cc(1)}), "no range");
        refused(json!({"bind": "from", "midi": cc(1)}), "no range");
        refused(json!({"bind": "free", "midi": cc(1), "min": 0}), "no range");
        // No warning, only the refusal, even when the narrowing was also clamped.
        refused(
            json!({"bind": "free", "midi": cc(1), "min": -2e6}),
            "no range",
        );
        let (route, _) = one(json!({"bind": "upto", "midi": cc(1), "min": 0}));
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
        let (route, warnings) = one(json!({"bind": "ranged_sig", "midi": cc(1)}));
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

        let (route, _) = one(json!({"bind": "steps", "midi": cc(2)}));
        assert_eq!(
            route.unwrap().scale,
            Scale::Linear {
                min: 0.0,
                max: 16.0,
                int: true
            }
        );

        let (route, _) = one(json!({"bind": "steps", "midi": on_one("program")}));
        assert_eq!(route.unwrap().scale, Scale::None);

        let (route, _) = one(json!({"bind": "notes", "midi": {"kind": "note", "channel": "4-8"}}));
        let route = route.unwrap();
        assert_eq!(
            (route.kind, route.scale.clone()),
            (MidiKind::Note, Scale::None)
        );
        assert!(route.matches(MidiKind::Note, 6, None));

        let (route, _) = one(json!({"bind": "cutoff", "midi": cc(74)}));
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
            one(json!({"bind": "cutoff", "midi": cc(74), "min": 200, "max": 8000}));
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
            one(json!({"bind": "free", "midi": on_one("bend"), "min": -2, "max": 2}));
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
        let (route, warnings) = one(json!({"bind": "level", "midi": cc(7), "min": -1, "max": 0.5}));
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
    fn each_kind_fits_the_inputs_it_always_has() {
        let takes = |input: &str| -> Vec<&str> {
            MidiKind::ALL
                .into_iter()
                .filter(|kind| kind.fits(input))
                .map(MidiKind::name)
                .collect()
        };
        assert_eq!(takes("message"), ["note"]);
        assert_eq!(takes("int"), ["cc", "bend", "pressure", "program"]);
        assert_eq!(takes("value"), ["cc", "bend", "pressure"]);
        assert_eq!(takes("signal"), ["cc", "bend", "pressure"]);
        assert!(takes("enum").is_empty());
    }

    #[test]
    fn a_source_reads_back_as_written() {
        for midi in [
            json!({"kind": "cc", "channel": 1, "cc": 74}),
            json!({"kind": "note", "channel": "1-4"}),
            json!({"kind": "cc", "channel": "*", "cc": "70-79"}),
        ] {
            let source: MidiSource = serde_json::from_value(midi.clone()).unwrap();
            assert_eq!(serde_json::to_value(&source).unwrap(), midi);
        }
        let source: MidiSource =
            serde_json::from_value(json!({"kind": "cc", "channel": "1-4", "cc": 74})).unwrap();
        assert_eq!(source.to_string(), "cc 74 on channel 1-4");
    }

    #[test]
    fn a_source_with_an_unknown_kind_or_key_does_not_parse() {
        for midi in [
            json!({"kind": "clock", "channel": 1}),
            json!({"kind": "cc", "channel": 1, "controller": 74}),
            json!({"kind": "note"}),
            json!({"channel": 1}),
            json!({"kind": "note", "channel": 1.5}),
        ] {
            assert!(
                serde_json::from_value::<MidiSource>(midi.clone()).is_err(),
                "{midi}"
            );
        }
    }
}
