//! Shared numeric-literal parsing for both contract macros (`operator_contract!` and
//! `number_operator_contract!`).
//!
//! A range endpoint or `default` may be written as a literal *or* as the `min`/`max` sentinel
//! keyword (issue #127), so the `±1e6` type-wide range never appears as a raw literal in an operator
//! contract. In a **range endpoint** the sentinel resolves to the shared
//! [`NUMBER_MIN`]/[`NUMBER_MAX`] type-wide bound; in a **`default`** it resolves to the operand's own
//! declared range extreme (already parsed), so `default max` parks an operand at its ceiling
//! regardless of what that ceiling is.
//!
//! Everything here answers in `f64` — the **type-neutral** form, not the port's type. A literal is
//! parsed once and projected by the caller onto whatever type the port turned out to be: `f32` for
//! `parse_f32_meta`, and per-variant for `number_operator_contract!`, whose one operand declaration
//! serves instantiations at more than one number type (issue #556). `f64` holds every `f32` literal
//! and every `i32` exactly, so the projection is the only place precision is ever at stake.

use reuben_contract::{NUMBER_MAX, NUMBER_MIN};
use syn::parse::ParseStream;
use syn::{Error, Ident, Lit, Token};

/// A numeric literal with an optional leading `-` (bounds and defaults can be negative).
pub(crate) fn parse_signed_float(input: ParseStream) -> syn::Result<f64> {
    let neg = input.peek(Token![-]);
    if neg {
        input.parse::<Token![-]>()?;
    }
    let lit: Lit = input.parse()?;
    let val = match lit {
        Lit::Float(f) => f.base10_parse::<f64>()?,
        Lit::Int(i) => i.base10_parse::<f64>()?,
        other => return Err(Error::new(other.span(), "expected a numeric literal")),
    };
    Ok(if neg { -val } else { val })
}

/// Whether the next token is a `min`/`max` range sentinel (vs a literal, `-`, or the `default`
/// keyword). Used to decide if an optional range is present without consuming tokens.
pub(crate) fn peek_range_sentinel(input: ParseStream) -> bool {
    let fork = input.fork();
    fork.parse::<Ident>()
        .is_ok_and(|id| matches!(id.to_string().as_str(), "min" | "max"))
}

/// A range endpoint: a signed literal, or the `min`/`max` sentinel resolving to the type-wide
/// [`NUMBER_MIN`]/[`NUMBER_MAX`] bound (issue #127).
pub(crate) fn parse_float_or_sentinel(input: ParseStream) -> syn::Result<f64> {
    if input.peek(Ident) {
        let id: Ident = input.parse()?;
        return match id.to_string().as_str() {
            "min" => Ok(f64::from(NUMBER_MIN)),
            "max" => Ok(f64::from(NUMBER_MAX)),
            other => Err(Error::new(
                id.span(),
                format!("expected a number or the `min`/`max` sentinel, got `{other}`"),
            )),
        };
    }
    parse_signed_float(input)
}

/// A `default` value: a signed literal, or `max`/`min` resolving to the operand's **own** range
/// extreme (`hi`/`lo` — the endpoints parsed for this same operand). Parks an operand at its ceiling
/// (`min`'s no-op `b`) or floor (`max`'s) without repeating the bound (issue #127).
pub(crate) fn parse_default_value(input: ParseStream, lo: f64, hi: f64) -> syn::Result<f64> {
    if input.peek(Ident) {
        let id: Ident = input.parse()?;
        return match id.to_string().as_str() {
            "min" => Ok(lo),
            "max" => Ok(hi),
            other => Err(Error::new(
                id.span(),
                format!("expected a number or the `min`/`max` sentinel, got `{other}`"),
            )),
        };
    }
    parse_signed_float(input)
}
