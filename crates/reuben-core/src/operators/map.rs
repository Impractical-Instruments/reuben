//! `map` — Message-domain affine remap, the Good Button workhorse (ADR-0017).
//!
//! One value Message in → one remapped value out, 1:1: `brightness [0,1] → cutoff [800,10000]`
//! with a response curve. Consumes *any* incoming value event (first numeric arg) regardless of
//! address, so it composes whether the value arrives from outside (OSC to the node address) or from
//! an upstream emit.
//!
//! **Staging (ADR-0028/0029):** `map`'s `in`/`out` stay **Message** ports for now. Its reframe into
//! a dense `Float`→`Float` pointwise shaper (the target shape of the math family — ADR-0029) lands
//! **with the instrument migration**, the same staging `m2s` follows: five bundled instruments wire
//! `map` as a Message node today, and the emit-on-init resting value (ADR-0018) folds into the
//! `in` input's materialized default only once those instruments move. Until then it is the
//! event-domain remap, living in its own file (one-op-per-file, ADR-0029) rather than in the
//! deleted `math.rs`. The affine math itself is the module-level [`remap`] fn (the pure-fn seam a
//! future dense/`Note`-field shell reuses — issue #83).

use crate::descriptor::{Curve, Descriptor, EnumMeta, LaneRule, ParamMeta, Port};
use crate::message::Arg;
use crate::operator::{Io, Operator};

/// Message-output ordinal of the `out` port.
pub const MSG_OUT: usize = 0;

/// `map`'s `in` Message input ordinal (value events via [`Io::events`]).
pub const MAP_IN: usize = 0;
/// `map`'s settable `Float`/`Enum` inputs (ADR-0028) — the former params, now read block-rate.
pub const MAP_IN_MIN: usize = 1;
pub const MAP_IN_MAX: usize = 2;
pub const MAP_OUT_MIN: usize = 3;
pub const MAP_OUT_MAX: usize = 4;
pub const MAP_CURVE: usize = 5;
pub const MAP_DEFAULT: usize = 6;

/// `map`'s `curve` variant symbols (index-aligned: 0 = Linear, 1 = Exponential).
const MAP_CURVES: &[&str] = &["Linear", "Exponential"];

/// Affine (optionally exponential) remap of `v` from `[in_min, in_max]` onto `[out_min, out_max]`,
/// clamped to the input range. The op's scalar math, written once (ADR-0029 pure-fn seam).
/// Exponential is used only when both output bounds are positive (it is meaningless across zero),
/// else it falls back to linear.
fn remap(v: f32, in_min: f32, in_max: f32, out_min: f32, out_max: f32, exp: bool) -> f32 {
    let span = in_max - in_min;
    let t = if span.abs() < 1e-12 {
        0.0
    } else {
        ((v - in_min) / span).clamp(0.0, 1.0)
    };
    if exp && out_min > 0.0 && out_max > 0.0 {
        out_min * (out_max / out_min).powf(t)
    } else {
        out_min + t * (out_max - out_min)
    }
}

/// `map` — Message-domain affine remap. One value Message in → one remapped value out.
///
/// - input 0: `in` (Message) — value events; the first numeric arg is remapped.
/// - inputs 1–4, 6: `in_min`, `in_max`, `out_min`, `out_max`, `default` (`Float`, read block-rate).
/// - input 5: `curve` (`Enum` {Linear, Exponential}).
/// - output 0 (Message): `out` — the remapped value at the same frame.
///
/// Default inputs are the identity `[0,1] → [0,1]` linear, so an unconfigured `map` is a
/// transparent pass-through (the public face of a Good Button). Single-Lane (ADR-0014):
/// emission is pre-fan-out.
///
/// **Emit-on-init (ADR-0018):** on its first block, before any message, `map` emits
/// `remap(default)` at frame 0 so a Good Button's whole downstream chain converges to the
/// resting position its control-surface widget shows — sound and UI agree at rest. The seed
/// fires once per instance and re-arms on [`spawn`](Operator::spawn).
#[derive(Default)]
pub struct Map {
    /// Whether the resting `default` has been emitted yet (emit-on-init). Reset on `spawn()`.
    seeded: bool,
}

impl Map {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Operator for Map {
    fn descriptor() -> Descriptor {
        // `in`/`out` stay Message ports (the affine remap is still 1:1 event-domain — the Float
        // shaper reframe waits on the instrument migration); the former params are now `Float`
        // inputs (settable + wire-able) and `curve` is an `Enum` (ADR-0028).
        let range = |name: &'static str, default: f32| {
            Port::float(ParamMeta {
                name,
                min: -1_000_000.0,
                max: 1_000_000.0,
                default,
                unit: "",
                curve: Curve::Linear,
            })
        };
        Descriptor {
            type_name: "map",
            inputs: vec![
                Port::message("in"),
                range("in_min", 0.0),
                range("in_max", 1.0),
                range("out_min", 0.0),
                range("out_max", 1.0),
                Port::enumerated(EnumMeta {
                    name: "curve",
                    variants: MAP_CURVES,
                    default: 0,
                }),
                // Input-domain resting value (ADR-0018), emitted on init so a Good Button's chain
                // converges to the position its widget shows.
                range("default", 0.0),
            ],
            outputs: vec![Port::message("out")],
            params: vec![],
            resources: vec![],
            lanes: LaneRule::Inherit,
        }
    }

    fn process(&mut self, io: &mut Io) {
        let in_min = io.value(MAP_IN_MIN);
        let in_max = io.value(MAP_IN_MAX);
        let out_min = io.value(MAP_OUT_MIN);
        let out_max = io.value(MAP_OUT_MAX);
        let exp = io.enum_index(MAP_CURVE) == 1; // 0 = Linear, 1 = Exponential

        // Emit the resting value once, before any message, so a Good Button's chain converges
        // to the position its widget shows (ADR-0018). A real event at frame 0 lands after this
        // and wins, so live input still overrides the resting default.
        if !self.seeded {
            let out = remap(io.value(MAP_DEFAULT), in_min, in_max, out_min, out_max, exp);
            io.emit(MSG_OUT, "out", [Arg::Float(out)], 0);
            self.seeded = true;
        }

        // Snapshot value events (can't read events while emitting).
        let mut values: smallvec::SmallVec<[(usize, f32); 8]> = smallvec::SmallVec::new();
        for ev in io.events() {
            if let Some(v) = ev.args.first().and_then(Arg::as_f32) {
                values.push((ev.frame, v));
            }
        }
        for (frame, v) in values {
            let out = remap(v, in_min, in_max, out_min, out_max, exp);
            io.emit(MSG_OUT, "out", [Arg::Float(out)], frame);
        }
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(Map);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Emit, Event, Message};

    const SR: f32 = 48_000.0;

    fn val(addr: &str, v: f32, frame: usize) -> Message {
        Message::new(addr, [Arg::Float(v)], frame)
    }

    /// Run `map` over one block, supplying its `Float` inputs (in_min/in_max/out_min/out_max/
    /// default) as constant buffers and `curve` as the held `Enum` index (0 linear / 1 exp) —
    /// the way the engine materializes them (ADR-0028) — plus the value events on `in`.
    #[allow(clippy::too_many_arguments)]
    fn run_map(
        m: &mut dyn Operator,
        in_min: f32,
        in_max: f32,
        out_min: f32,
        out_max: f32,
        curve: usize,
        default: f32,
        values: &[Message],
    ) -> Vec<Emit> {
        let evs: Vec<Event> = values
            .iter()
            .map(|m| Event {
                addr: &m.addr,
                args: &m.args,
                frame: m.frame,
            })
            .collect();
        let n = 256;
        // Buffers in port order for the Float inputs: in_min, in_max, out_min, out_max, default.
        let bufs = [
            vec![in_min; n],
            vec![in_max; n],
            vec![out_min; n],
            vec![out_max; n],
            vec![default; n],
        ];
        let enums = [0usize, 0, 0, 0, 0, curve, 0]; // held index at MAP_CURVE = 5
        let mut emits: Vec<Emit> = Vec::new();
        {
            let outs: Vec<&mut [f32]> = vec![];
            // Port order: in (Message), in_min, in_max, out_min, out_max, curve (Enum), default.
            let inputs: Vec<Option<&[f32]>> = vec![
                None,
                Some(&bufs[0]),
                Some(&bufs[1]),
                Some(&bufs[2]),
                Some(&bufs[3]),
                None,
                Some(&bufs[4]),
            ];
            let mut io = Io::new(SR, n, inputs, outs, &[], &evs)
                .with_emit(&mut emits, 0)
                .with_enums(&enums);
            m.process(&mut io);
        }
        emits
    }

    #[test]
    fn map_identity_passes_value_through() {
        let emits = run_map(
            &mut Map::new(),
            0.0,
            1.0,
            0.0,
            1.0,
            0,
            0.0,
            &[val("in", 0.42, 7)],
        );
        // First block seeds the resting default (0.0) at frame 0 (ADR-0018), then the value.
        assert_eq!(emits.len(), 2);
        assert_eq!(emits[0].frame, 0);
        approx::assert_relative_eq!(emits[0].args[0].as_f32().unwrap(), 0.0);
        assert_eq!(emits[1].addr, "out");
        assert_eq!(emits[1].frame, 7);
        approx::assert_relative_eq!(emits[1].args[0].as_f32().unwrap(), 0.42);
    }

    #[test]
    fn map_linear_remaps_range_and_clamps() {
        // [0,1] -> [800,10000] linear. 0.5 -> 5400; an over-range 2.0 clamps to 10000.
        // emits[0] is the resting seed (default 0.0 -> 800).
        let emits = run_map(
            &mut Map::new(),
            0.0,
            1.0,
            800.0,
            10_000.0,
            0,
            0.0,
            &[val("in", 0.5, 0), val("in", 2.0, 1)],
        );
        approx::assert_relative_eq!(emits[0].args[0].as_f32().unwrap(), 800.0);
        approx::assert_relative_eq!(emits[1].args[0].as_f32().unwrap(), 5400.0);
        approx::assert_relative_eq!(emits[2].args[0].as_f32().unwrap(), 10_000.0);
    }

    #[test]
    fn map_exponential_curve_is_geometric_midpoint() {
        // [0,1] -> [100,10000] exponential: t=0.5 -> sqrt(100*10000)=1000.
        // emits[0] is the resting seed (default 0.0, t=0 -> out_min 100).
        let emits = run_map(
            &mut Map::new(),
            0.0,
            1.0,
            100.0,
            10_000.0,
            1,
            0.0,
            &[val("in", 0.5, 0)],
        );
        approx::assert_relative_eq!(emits[1].args[0].as_f32().unwrap(), 1000.0, epsilon = 1e-1);
    }

    #[test]
    fn map_consumes_events_regardless_of_address() {
        // External OSC to the node address arrives with an empty local address; chained
        // emits arrive as "out". Both must drive the map. emits[0] is the resting seed.
        let from_external = run_map(
            &mut Map::new(),
            0.0,
            1.0,
            0.0,
            10.0,
            0,
            0.0,
            &[val("", 0.5, 0)],
        );
        let from_chain = run_map(
            &mut Map::new(),
            0.0,
            1.0,
            0.0,
            10.0,
            0,
            0.0,
            &[val("out", 0.5, 0)],
        );
        approx::assert_relative_eq!(from_external[1].args[0].as_f32().unwrap(), 5.0);
        approx::assert_relative_eq!(from_chain[1].args[0].as_f32().unwrap(), 5.0);
    }

    #[test]
    fn map_emits_resting_default_once_on_first_block() {
        // default=0.5 over [0,1]->[0,100]: first block emits 50 at frame 0 with no events;
        // a second block with no events emits nothing — the seed fires once per instance.
        let mut m = Map::new();
        let first = run_map(&mut m, 0.0, 1.0, 0.0, 100.0, 0, 0.5, &[]);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].frame, 0);
        approx::assert_relative_eq!(first[0].args[0].as_f32().unwrap(), 50.0);
        let second = run_map(&mut m, 0.0, 1.0, 0.0, 100.0, 0, 0.5, &[]);
        assert!(second.is_empty(), "resting seed fires once per instance");
    }

    #[test]
    fn spawned_map_re_seeds_resting_default() {
        let mut m = Map::new();
        let _ = run_map(&mut m, 0.0, 1.0, 0.0, 100.0, 0, 0.5, &[]);
        let mut m2 = m.spawn();
        let emits = run_map(&mut *m2, 0.0, 1.0, 0.0, 100.0, 0, 0.5, &[]);
        assert_eq!(emits.len(), 1, "spawn re-arms the resting seed");
        approx::assert_relative_eq!(emits[0].args[0].as_f32().unwrap(), 50.0);
    }
}
