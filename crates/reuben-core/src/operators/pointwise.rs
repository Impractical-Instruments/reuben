//! `PointwiseNum` — the saturating arithmetic the pointwise number family's scalar fns are
//! generic over, so `f32` and `i32` both total every op rather than panicking, wrapping, or
//! relying on the declared port range (which `mul` alone can escape).

/// A number type the pointwise math family can instantiate at, with **total** arithmetic: every
/// operation returns a value of the type for every input, saturating at the type's limits rather
/// than panicking or wrapping.
///
/// Implemented for `f32` (IEEE — the limit is `±inf`) and `i32` (`saturating_*`). A type that
/// cannot answer these five totally does not belong in a `variants:` list.
pub trait PointwiseNum: Copy {
    /// `self + rhs`, saturating at the type's limits.
    fn add(self, rhs: Self) -> Self;
    /// `self - rhs`, saturating at the type's limits.
    fn sub(self, rhs: Self) -> Self;
    /// `self * rhs`, saturating at the type's limits.
    fn mul(self, rhs: Self) -> Self;
    /// `-self`, saturating (`-i32::MIN` is not representable).
    fn neg(self) -> Self;
    /// `|self|`, saturating (`i32::MIN.abs()` is not representable).
    fn abs(self) -> Self;
}

// IEEE arithmetic is already total: overflow saturates to `±inf`, and `f32::MIN` negates and
// absolutes cleanly (the sign is a bit, not a range edge). So these are the bare operators.
impl PointwiseNum for f32 {
    #[inline]
    fn add(self, rhs: Self) -> Self {
        self + rhs
    }
    #[inline]
    fn sub(self, rhs: Self) -> Self {
        self - rhs
    }
    #[inline]
    fn mul(self, rhs: Self) -> Self {
        self * rhs
    }
    #[inline]
    fn neg(self) -> Self {
        -self
    }
    #[inline]
    fn abs(self) -> Self {
        self.abs()
    }
}

// `num_traits`' `SaturatingAdd`/`Sub`/`Mul` cover the binary three but are implemented for
// integers only, so they cannot express the `f32` half of this trait — hence our own.
// `saturating_neg`/`saturating_abs` are inherent on `i32` and have no `num_traits` counterpart at
// all. One trait, both halves, rather than a mixed bound that only reads as total.
impl PointwiseNum for i32 {
    #[inline]
    fn add(self, rhs: Self) -> Self {
        self.saturating_add(rhs)
    }
    #[inline]
    fn sub(self, rhs: Self) -> Self {
        self.saturating_sub(rhs)
    }
    #[inline]
    fn mul(self, rhs: Self) -> Self {
        self.saturating_mul(rhs)
    }
    #[inline]
    fn neg(self) -> Self {
        self.saturating_neg()
    }
    #[inline]
    fn abs(self) -> Self {
        self.saturating_abs()
    }
}

#[cfg(test)]
mod tests {
    use super::PointwiseNum;

    // The whole point: the i32 arithmetic that would panic (debug) or wrap (release) instead
    // pins to the limit. `mul` is the one the port-range clamp cannot bound — two operands at the
    // type-wide ±1e6 sentinel multiply to 1e12, past i32::MAX.
    #[test]
    fn i32_saturates_instead_of_overflowing() {
        assert_eq!(PointwiseNum::mul(1_000_000_i32, 1_000_000), i32::MAX);
        assert_eq!(PointwiseNum::mul(-1_000_000_i32, 1_000_000), i32::MIN);
        assert_eq!(PointwiseNum::add(i32::MAX, 1), i32::MAX);
        assert_eq!(PointwiseNum::sub(i32::MIN, 1), i32::MIN);
        // The two that have no representable answer: `-i32::MIN` and `i32::MIN.abs()`.
        assert_eq!(PointwiseNum::neg(i32::MIN), i32::MAX);
        assert_eq!(PointwiseNum::abs(i32::MIN), i32::MAX);
    }

    // f32 keeps IEEE semantics unchanged — the trait is a pass-through, so no shipping
    // `*_f32_*` operator changes behaviour by moving onto it.
    #[test]
    fn f32_keeps_ieee_semantics() {
        assert_eq!(PointwiseNum::add(1.5_f32, 2.5), 4.0);
        assert_eq!(PointwiseNum::sub(1.5_f32, 2.5), -1.0);
        assert_eq!(PointwiseNum::mul(1.5_f32, 2.0), 3.0);
        assert_eq!(PointwiseNum::neg(1.5_f32), -1.5);
        assert_eq!(PointwiseNum::abs(-1.5_f32), 1.5);
        // Overflow is `inf`, not a panic — f32's own form of saturation.
        assert!(PointwiseNum::mul(f32::MAX, 2.0).is_infinite());
    }
}
