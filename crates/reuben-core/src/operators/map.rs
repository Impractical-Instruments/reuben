//! `map` — dense affine remap, the Good Button workhorse (ADR-0017, ADR-0029, ADR-0030).
//!
//! A per-sample `Float`→`Float` pointwise shaper: `brightness [0,1] → cutoff [800,10000]` with a
//! response curve. Reframed from the event-domain remap to a dense shaper (the math-family target
//! shape, ADR-0029): `in` is a materialized `Float` read per-sample, `out` a `Buffer`. The wire's
//! automatic ZOH materialize (ADR-0030) means a sparse control feeding `in` still drives it — the
//! former emit-on-init resting value is now just `in`'s materialized default.
//!
//! - input 0: `in` (`Float`) — the value to remap (per-sample).
//! - inputs 1–4: `in_min`, `in_max`, `out_min`, `out_max` (`Float`, held).
//! - input 5: `curve` (`Enum` [`MapCurve`] {Linear, Exponential}).
//! - output 0: `out` (`Buffer`) — the remapped value.
//!
//! Default inputs are the identity `[0,1] → [0,1]` linear, so an unconfigured `map` is a
//! transparent pass-through (the public face of a Good Button). The affine math is the module-level
//! [`remap`] fn (the pure-fn seam, ADR-0029).

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};
use crate::vocab::MapCurve;

// Single-source contract (ADR-0025/0030). `curve` references the shared `MapCurve` vocab enum.
crate::operator_contract!(Map {
    inputs:  { in:      float { -1_000_000.0..=1_000_000.0, default 0.0, "", lin },
               in_min:  float { -1_000_000.0..=1_000_000.0, default 0.0, "", lin },
               in_max:  float { -1_000_000.0..=1_000_000.0, default 1.0, "", lin },
               out_min: float { -1_000_000.0..=1_000_000.0, default 0.0, "", lin },
               out_max: float { -1_000_000.0..=1_000_000.0, default 1.0, "", lin },
               curve:   enum(MapCurve) },
    outputs: { out: buffer },
});

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

#[derive(Default)]
pub struct Map;

impl Map {
    pub fn new() -> Self {
        Self
    }
}

impl Operator for Map {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let in_min = io.last::<f32>(IN_IN_MIN).unwrap_or(0.0);
        let in_max = io.last::<f32>(IN_IN_MAX).unwrap_or(1.0);
        let out_min = io.last::<f32>(IN_OUT_MIN).unwrap_or(0.0);
        let out_max = io.last::<f32>(IN_OUT_MAX).unwrap_or(1.0);
        let exp = io.last::<MapCurve>(IN_CURVE).unwrap_or_default() == MapCurve::Exponential;

        for i in 0..n {
            let v = io.signal(IN_IN).get(i).copied().unwrap_or(0.0);
            io.signal_mut(OUT_OUT)[i] = remap(v, in_min, in_max, out_min, out_max, exp);
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
    use crate::message::Arg;

    const SR: f32 = 48_000.0;

    /// Run dense `map` over one block: `in` is a per-sample buffer; in_min/in_max/out_min/out_max
    /// are held `Float`s, `curve` the held `MapCurve` — the way the engine latches them.
    #[allow(clippy::too_many_arguments)]
    fn run_map(
        in_min: f32,
        in_max: f32,
        out_min: f32,
        out_max: f32,
        curve: MapCurve,
        values: &[f32],
    ) -> Vec<f32> {
        let n = values.len();
        let latched = [
            Arg::F32(0.0), // `in` is per-sample (buffer), not read via last
            Arg::F32(in_min),
            Arg::F32(in_max),
            Arg::F32(out_min),
            Arg::F32(out_max),
            Arg::MapCurve(curve),
        ];
        let mut out = vec![0.0f32; n];
        {
            let inputs: Vec<Option<&[f32]>> = vec![Some(values), None, None, None, None, None];
            let outs: Vec<&mut [f32]> = vec![&mut out[..]];
            let mut io = Io::new(SR, n, inputs, outs).with_latched(&latched);
            Map::new().process(&mut io);
        }
        out
    }

    #[test]
    fn map_identity_passes_value_through() {
        let out = run_map(0.0, 1.0, 0.0, 1.0, MapCurve::Linear, &[0.42; 4]);
        for &s in &out {
            approx::assert_relative_eq!(s, 0.42);
        }
    }

    #[test]
    fn map_linear_remaps_range_and_clamps() {
        // [0,1] -> [800,10000] linear. 0.5 -> 5400; an over-range 2.0 clamps to 10000.
        let out = run_map(0.0, 1.0, 800.0, 10_000.0, MapCurve::Linear, &[0.5, 2.0]);
        approx::assert_relative_eq!(out[0], 5400.0);
        approx::assert_relative_eq!(out[1], 10_000.0);
    }

    #[test]
    fn map_exponential_curve_is_geometric_midpoint() {
        // [0,1] -> [100,10000] exponential: t=0.5 -> sqrt(100*10000)=1000.
        let out = run_map(0.0, 1.0, 100.0, 10_000.0, MapCurve::Exponential, &[0.5]);
        approx::assert_relative_eq!(out[0], 1000.0, epsilon = 1e-1);
    }

    #[test]
    fn map_is_per_sample() {
        // A rising input ramp produces a rising output ramp (dense, no event gating).
        let out = run_map(
            0.0,
            1.0,
            0.0,
            10.0,
            MapCurve::Linear,
            &[0.0, 0.25, 0.5, 1.0],
        );
        approx::assert_relative_eq!(out[0], 0.0);
        approx::assert_relative_eq!(out[1], 2.5);
        approx::assert_relative_eq!(out[2], 5.0);
        approx::assert_relative_eq!(out[3], 10.0);
    }
}
