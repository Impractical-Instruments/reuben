//! `RoundInto` — the rounding the pointwise number family converts with.
//!
//! Four ways to send a number to a whole one — nearest (`round`), down (`floor`), up (`ceil`),
//! toward zero (`trunc`) — each answering in a caller-chosen output type. One trait rather than
//! four, because the **conversion is the axis**: the four modes differ only in how they break a
//! tie, and each output type wants all four. So the two conversions this family needs are stated
//! once each, and adding a third number type is one `impl`, not four.
//!
//! The generic parameter is what lets `number_operator_contract!` emit both an `f32 -> f32`
//! rounding operator and an `f32 -> i32` converter from **one** scalar fn per mode
//! (`round_fn(x)`), with the output type chosen by the `variants:` entry. That is the same
//! property [`PointwiseNum`](crate::operators::pointwise::PointwiseNum) gives the arithmetic ops, applied to
//! the type *crossing* rather than the type.
//!
//! # Why the `i32` conversion is total
//!
//! `process` runs on the render thread, where a panic is fatal, so a scalar fn must answer for
//! every input its ports can carry. Rust's float-to-int `as` is **saturating**: a value past
//! `i32`'s range pins to `i32::MIN`/`i32::MAX` rather than being UB, and `NaN` — which no
//! rounding mode has an answer for — becomes `0`. Both are the same answers
//! [`Port::coerce`](crate::descriptor::Port::coerce) and `render::held_arg` already give a scalar
//! arriving from **outside** the graph, so the in-graph converter and the boundary quantizer
//! agree bit for bit.
//!
//! The declared port range makes the saturating arm unreachable in practice — a rounding op's
//! input is clamped to the type-wide `±1e6` sentinel, well inside `i32` — but the arithmetic is
//! total on its own terms rather than by a range nothing checks, for the reason spelled out in
//! [`pointwise`](crate::operators::pointwise).
//!
//! # Why the methods are not named `round`/`floor`/`ceil`/`trunc`
//!
//! `f32` has inherent methods by those names. A trait method of the same name on the same type
//! resolves to the inherent one inside the `impl` body, so `fn round(self) -> f32 { self.round() }`
//! would silently mean something different from what it reads as. The `_into` suffix keeps the
//! recursion impossible rather than merely absent.

/// A number type that can be rounded **into** `Out` — the seam the rounding operators' scalar fns
/// are written over, so one fn body serves the same-type operator and the converter alike.
///
/// Implemented for `f32 -> f32` (rounding that stays in floats — quantizing a CV to whole numbers
/// without leaving the signal domain) and `f32 -> i32` (the narrowing converter, the sanctioned
/// path across a boundary the wire check refuses implicitly). A pair that cannot answer all four
/// modes totally does not belong here.
pub trait RoundInto<Out>: Copy {
    /// To the nearest whole number, halfway cases away from zero.
    fn round_into(self) -> Out;
    /// To the greatest whole number `<= self`.
    fn floor_into(self) -> Out;
    /// To the least whole number `>= self`.
    fn ceil_into(self) -> Out;
    /// Toward zero — the fractional part dropped.
    fn trunc_into(self) -> Out;
}

// Rounding that stays in `f32`: the value becomes whole, the type does not change. `±inf` and
// `NaN` pass through unchanged, as they do through every other `f32` operator in the family.
impl RoundInto<f32> for f32 {
    #[inline]
    fn round_into(self) -> f32 {
        self.round()
    }
    #[inline]
    fn floor_into(self) -> f32 {
        self.floor()
    }
    #[inline]
    fn ceil_into(self) -> f32 {
        self.ceil()
    }
    #[inline]
    fn trunc_into(self) -> f32 {
        self.trunc()
    }
}

// The narrowing converter. The `as` is saturating and `NaN`-to-zero (see the module doc), which
// is what makes these total — and is the same conversion the boundary applies to a scalar
// arriving from outside the graph.
impl RoundInto<i32> for f32 {
    #[inline]
    fn round_into(self) -> i32 {
        self.round() as i32
    }
    #[inline]
    fn floor_into(self) -> i32 {
        self.floor() as i32
    }
    #[inline]
    fn ceil_into(self) -> i32 {
        self.ceil() as i32
    }
    #[inline]
    fn trunc_into(self) -> i32 {
        self.trunc() as i32
    }
}

#[cfg(test)]
mod tests {
    use super::RoundInto;

    // The four modes are only distinguishable on a fraction, and only fully distinguishable on a
    // *negative* one: `floor`/`trunc` agree above zero and disagree below it, as do `ceil`/`round`
    // at -0.5. A test that only checks positives would pass with two of the four transposed.
    #[test]
    fn the_four_modes_differ_on_a_negative_fraction() {
        let x = -2.5_f32;
        assert_eq!(RoundInto::<i32>::round_into(x), -3); // halves away from zero
        assert_eq!(RoundInto::<i32>::floor_into(x), -3);
        assert_eq!(RoundInto::<i32>::ceil_into(x), -2);
        assert_eq!(RoundInto::<i32>::trunc_into(x), -2); // toward zero, unlike floor

        let y = 2.5_f32;
        assert_eq!(RoundInto::<i32>::round_into(y), 3);
        assert_eq!(RoundInto::<i32>::floor_into(y), 2);
        assert_eq!(RoundInto::<i32>::ceil_into(y), 3);
        assert_eq!(RoundInto::<i32>::trunc_into(y), 2);
    }

    // The `f32` half rounds the value without changing the type — same tie-breaking, so the two
    // impls cannot drift into disagreeing about which whole number a fraction is nearest.
    #[test]
    fn the_f32_half_agrees_with_the_i32_half_on_which_whole_number() {
        // Both sides spelled out: `f32` implements the trait at two output types, so an inferred
        // `x.round_into()` would be ambiguous rather than defaulting to either.
        for x in [-2.5_f32, -0.5, 0.0, 0.5, 2.5, 7.3, -7.3] {
            assert_eq!(
                RoundInto::<f32>::round_into(x) as i32,
                RoundInto::<i32>::round_into(x)
            );
            assert_eq!(
                RoundInto::<f32>::floor_into(x) as i32,
                RoundInto::<i32>::floor_into(x)
            );
            assert_eq!(
                RoundInto::<f32>::ceil_into(x) as i32,
                RoundInto::<i32>::ceil_into(x)
            );
            assert_eq!(
                RoundInto::<f32>::trunc_into(x) as i32,
                RoundInto::<i32>::trunc_into(x)
            );
        }
    }

    // Totality: the values a scalar fn must not panic on. `process` runs on the render thread,
    // so every one of these has to return an `i32` rather than trap.
    #[test]
    fn the_i32_conversion_is_total() {
        assert_eq!(RoundInto::<i32>::round_into(f32::INFINITY), i32::MAX);
        assert_eq!(RoundInto::<i32>::round_into(f32::NEG_INFINITY), i32::MIN);
        assert_eq!(RoundInto::<i32>::floor_into(f32::MAX), i32::MAX);
        assert_eq!(RoundInto::<i32>::ceil_into(-f32::MAX), i32::MIN);
        // No rounding mode has an answer for NaN; `as` gives zero, matching what a NaN arriving
        // from outside the graph already latches through `render::held_arg`.
        assert_eq!(RoundInto::<i32>::round_into(f32::NAN), 0);
        assert_eq!(RoundInto::<i32>::trunc_into(f32::NAN), 0);
    }
}
