//! Exact rational time.
//!
//! Every time in this crate is a [`Rational`]: an `i64` numerator over a positive `i64`
//! denominator, always in lowest terms. Arithmetic widens to `i128`, reduces, and narrows back.
//! A result that does not fit in `i64` after reduction is an overflow: the checked operations
//! return `None`, never wrap, and never panic. The query turns that `None` into a counted, skipped
//! sub-query (see [`QueryResult::overflowed`](crate::QueryResult::overflowed)).

use core::cmp::Ordering;
use core::fmt;

/// An exact rational number: a time in cycles, a speed factor, or a numeric pattern value.
///
/// Invariant: `den > 0` and `gcd(|num|, den) == 1`, so two equal values have equal fields and the
/// derived `Eq`/`Hash` are exact.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rational {
    num: i64,
    den: i64,
}

fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

impl Rational {
    /// Zero.
    pub const ZERO: Rational = Rational { num: 0, den: 1 };
    /// One.
    pub const ONE: Rational = Rational { num: 1, den: 1 };

    /// `num / den`, reduced. `None` if `den == 0` or the reduced value does not fit.
    pub fn new(num: i64, den: i64) -> Option<Rational> {
        Self::from_i128(num as i128, den as i128)
    }

    /// The integer `n`.
    pub const fn int(n: i64) -> Rational {
        Rational { num: n, den: 1 }
    }

    /// The numerator, in lowest terms.
    pub const fn numer(self) -> i64 {
        self.num
    }

    /// The denominator, in lowest terms; always positive.
    pub const fn denom(self) -> i64 {
        self.den
    }

    fn from_i128(num: i128, den: i128) -> Option<Rational> {
        if den == 0 {
            return None;
        }
        // Both inputs come from products of two i64s, so neither is i128::MIN and negation is safe.
        let (n, d) = if den < 0 { (-num, -den) } else { (num, den) };
        let g = gcd(n.unsigned_abs(), d as u128) as i128;
        let (n, d) = if g > 1 { (n / g, d / g) } else { (n, d) };
        Some(Rational {
            num: i64::try_from(n).ok()?,
            den: i64::try_from(d).ok()?,
        })
    }

    /// `self + rhs`, or `None` on overflow.
    pub fn checked_add(self, rhs: Rational) -> Option<Rational> {
        if self.den == rhs.den {
            return Self::from_i128(self.num as i128 + rhs.num as i128, self.den as i128);
        }
        Self::from_i128(
            self.num as i128 * rhs.den as i128 + rhs.num as i128 * self.den as i128,
            self.den as i128 * rhs.den as i128,
        )
    }

    /// `self - rhs`, or `None` on overflow.
    pub fn checked_sub(self, rhs: Rational) -> Option<Rational> {
        self.checked_add(rhs.checked_neg()?)
    }

    /// `-self`, or `None` on overflow.
    pub fn checked_neg(self) -> Option<Rational> {
        Some(Rational {
            num: self.num.checked_neg()?,
            den: self.den,
        })
    }

    /// `self * rhs`, or `None` on overflow.
    pub fn checked_mul(self, rhs: Rational) -> Option<Rational> {
        Self::from_i128(
            self.num as i128 * rhs.num as i128,
            self.den as i128 * rhs.den as i128,
        )
    }

    /// `self / rhs`, or `None` on overflow or division by zero.
    pub fn checked_div(self, rhs: Rational) -> Option<Rational> {
        Self::from_i128(
            self.num as i128 * rhs.den as i128,
            self.den as i128 * rhs.num as i128,
        )
    }

    /// The greatest integer `<= self`.
    pub const fn floor(self) -> i64 {
        self.num.div_euclid(self.den)
    }

    /// `true` if the value is strictly positive.
    pub const fn is_positive(self) -> bool {
        self.num > 0
    }

    /// The nearest `f64`. Lossy; for presentation and audio values, never for time.
    pub fn to_f64(self) -> f64 {
        self.num as f64 / self.den as f64
    }
}

impl Ord for Rational {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.num as i128 * other.den as i128).cmp(&(other.num as i128 * self.den as i128))
    }
}

impl PartialOrd for Rational {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Debug for Rational {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for Rational {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.den == 1 {
            write!(f, "{}", self.num)
        } else {
            write!(f, "{}/{}", self.num, self.den)
        }
    }
}

/// A half-open time span `[begin, end)` in cycles.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Span {
    /// Inclusive start.
    pub begin: Rational,
    /// Exclusive end.
    pub end: Rational,
}

impl Span {
    /// `[begin, end)`.
    pub const fn new(begin: Rational, end: Rational) -> Span {
        Span { begin, end }
    }

    /// `true` if the span contains no time.
    pub fn is_empty(&self) -> bool {
        self.begin >= self.end
    }

    /// The overlap of two spans; empty if they do not overlap.
    pub fn intersect(&self, other: &Span) -> Span {
        Span {
            begin: self.begin.max(other.begin),
            end: self.end.min(other.end),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(n: i64, d: i64) -> Rational {
        Rational::new(n, d).unwrap()
    }

    #[test]
    fn reduces_and_normalises_sign() {
        assert_eq!(r(2, 4), r(1, 2));
        assert_eq!(r(1, -2).numer(), -1);
        assert_eq!(r(1, -2).denom(), 2);
        assert_eq!(Rational::new(1, 0), None);
    }

    #[test]
    fn arithmetic_is_exact() {
        assert_eq!(r(1, 3).checked_add(r(1, 6)), Some(r(1, 2)));
        assert_eq!(r(1, 3).checked_sub(r(1, 2)), Some(r(-1, 6)));
        assert_eq!(r(2, 3).checked_mul(r(3, 4)), Some(r(1, 2)));
        assert_eq!(r(2, 3).checked_div(r(4, 3)), Some(r(1, 2)));
        assert_eq!(r(1, 2).checked_div(Rational::ZERO), None);
    }

    #[test]
    fn floor_rounds_toward_negative_infinity() {
        assert_eq!(r(7, 2).floor(), 3);
        assert_eq!(r(-7, 2).floor(), -4);
        assert_eq!(Rational::int(-3).floor(), -3);
    }

    #[test]
    fn overflow_is_none_not_wrap() {
        let big = Rational::int(i64::MAX);
        assert_eq!(big.checked_add(Rational::ONE), None);
        assert_eq!(big.checked_mul(Rational::int(2)), None);
        assert_eq!(Rational::int(i64::MIN).checked_neg(), None);
        assert_eq!(r(1, i64::MAX).checked_mul(r(1, 2)), None);
        // Reduction rescues a product whose unreduced terms do not fit.
        assert_eq!(
            r(i64::MAX, 3).checked_mul(r(3, i64::MAX)),
            Some(Rational::ONE)
        );
    }

    #[test]
    fn ordering_is_exact_across_denominators() {
        assert!(r(1, 3) < r(334, 1000));
        assert!(r(-1, 2) < r(-1, 3));
        assert!(r(i64::MAX - 1, i64::MAX) < Rational::ONE);
    }
}
