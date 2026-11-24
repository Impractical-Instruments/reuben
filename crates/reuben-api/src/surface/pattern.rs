//! The numeric pattern a MIDI route's `channel` and `cc` take: `*` is every number, `a-b` an
//! inclusive range, and `a,b,c` a list of either, so `1-4,10` is five numbers. Not OSC's address
//! pattern grammar, whose `[1-16]` is a class of the characters `1` and `6`.

use alloc::borrow::Cow;
use alloc::string::{String, ToString};
use core::fmt;
use core::ops::RangeInclusive;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A MIDI route's `channel` or `cc` as written: a JSON integer for one number, a string for a
/// pattern over numbers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NumberPattern {
    Number(i64),
    Pattern(String),
}

impl NumberPattern {
    /// What a text field holding `text` writes: a number when it is one, else the pattern.
    pub fn from_text(text: &str) -> Self {
        let text = text.trim();
        let digits = !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
        match text.parse() {
            Ok(number) if digits => Self::Number(number),
            _ => Self::Pattern(text.to_string()),
        }
    }
}

impl fmt::Display for NumberPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(n) => write!(f, "{n}"),
            Self::Pattern(text) => f.write_str(text),
        }
    }
}

impl Serialize for NumberPattern {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Number(n) => serializer.serialize_i64(*n),
            Self::Pattern(text) => serializer.serialize_str(text),
        }
    }
}

impl<'de> Deserialize<'de> for NumberPattern {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;

        impl serde::de::Visitor<'_> for Visitor {
            type Value = NumberPattern;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an integer, or a pattern string such as \"1-4\" or \"*\"")
            }

            fn visit_i64<E: serde::de::Error>(self, n: i64) -> Result<Self::Value, E> {
                Ok(NumberPattern::Number(n))
            }

            // An integer past `i64` is kept as its digits, so the check skips the one control
            // as out of bounds rather than the whole map failing to parse.
            fn visit_u64<E: serde::de::Error>(self, n: u64) -> Result<Self::Value, E> {
                Ok(i64::try_from(n).map_or_else(
                    |_| NumberPattern::Pattern(n.to_string()),
                    NumberPattern::Number,
                ))
            }

            fn visit_str<E: serde::de::Error>(self, text: &str) -> Result<Self::Value, E> {
                Ok(NumberPattern::Pattern(text.to_string()))
            }
        }

        deserializer.deserialize_any(Visitor)
    }
}

impl JsonSchema for NumberPattern {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        "NumberPattern".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        // The fields that hold one describe the grammar; repeating it here would put it in the
        // advertised schema once per field.
        schemars::json_schema!({"anyOf": [{"type": "integer"}, {"type": "string"}]})
    }
}

/// The numbers a [`NumberPattern`] matches, all within 0–127.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NumberSet(u128);

/// Why a [`NumberPattern`] matches no number, or one outside its bounds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatternError {
    /// The pattern string, and what is wrong with it.
    Malformed(String, &'static str),
    /// A number outside `bounds`, as it was written.
    OutOfBounds {
        number: String,
        bounds: RangeInclusive<u8>,
    },
    /// A range whose low end is above its high end, which matches nothing.
    Reversed(u8, u8),
}

impl fmt::Display for PatternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(text, why) => write!(
                f,
                "{text:?} is not a number pattern: {why}; write a number, `*`, a range `a-b` or a \
                 list `a,b`"
            ),
            Self::OutOfBounds { number, bounds } => {
                write!(f, "{number} is outside {}-{}", bounds.start(), bounds.end())
            }
            Self::Reversed(low, high) => {
                write!(
                    f,
                    "\"{low}-{high}\" is a reversed range, which matches nothing"
                )
            }
        }
    }
}

impl NumberSet {
    /// The numbers `pattern` matches, each of which must lie in `bounds`. `bounds` ends at 127 at
    /// most, the widest a MIDI data byte is.
    pub fn parse(
        pattern: &NumberPattern,
        bounds: RangeInclusive<u8>,
    ) -> Result<Self, PatternError> {
        debug_assert!(*bounds.end() <= 127);
        // `written` is the number as the author spelled it, so an error quotes it even where it
        // does not fit an `i64`.
        let within = |number: Option<i64>, written: &dyn Fn() -> String| {
            number
                .and_then(|n| u8::try_from(n).ok())
                .filter(|n| bounds.contains(n))
                .ok_or_else(|| PatternError::OutOfBounds {
                    number: written(),
                    bounds: bounds.clone(),
                })
        };
        let text = match pattern {
            NumberPattern::Number(number) => {
                let n = within(Some(*number), &|| number.to_string())?;
                return Ok(Self::span(n, n));
            }
            NumberPattern::Pattern(text) => text,
        };
        let malformed = |why| PatternError::Malformed(text.clone(), why);
        let number = |part: &str| -> Result<u8, PatternError> {
            let part = part.trim();
            if part.is_empty() {
                return Err(malformed("a range needs a number at each end"));
            }
            if part.contains('*') {
                return Err(malformed(
                    "`*` stands alone, never as part of a number or a range",
                ));
            }
            if part.contains('-') {
                return Err(malformed("a range has one `-`, between two numbers"));
            }
            if !part.bytes().all(|b| b.is_ascii_digit()) {
                return Err(malformed(
                    "it holds something other than digits, `*`, `-` and `,`",
                ));
            }
            within(part.parse().ok(), &|| part.to_string())
        };
        let mut set = Self::default();
        for item in text.split(',') {
            let item = item.trim();
            let (low, high) = match item.split_once('-') {
                _ if item.is_empty() => return Err(malformed("it has an empty list item")),
                _ if item == "*" => (*bounds.start(), *bounds.end()),
                Some((low, high)) => (number(low)?, number(high)?),
                None => {
                    let n = number(item)?;
                    (n, n)
                }
            };
            if low > high {
                return Err(PatternError::Reversed(low, high));
            }
            set.0 |= Self::span(low, high).0;
        }
        Ok(set)
    }

    pub fn contains(self, number: u8) -> bool {
        number < 128 && self.0 & (1 << number) != 0
    }

    /// `low..=high`, both at most 127.
    fn span(low: u8, high: u8) -> Self {
        Self((u128::MAX >> (127 - high)) & (u128::MAX << low))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::vec::Vec;

    const CHANNEL: RangeInclusive<u8> = 1..=16;
    const CC: RangeInclusive<u8> = 0..=127;

    fn pattern(text: &str) -> NumberPattern {
        NumberPattern::Pattern(text.to_string())
    }

    fn members(set: NumberSet) -> Vec<u8> {
        (0..=127).filter(|n| set.contains(*n)).collect()
    }

    fn parsed(text: &str, bounds: RangeInclusive<u8>) -> Vec<u8> {
        members(NumberSet::parse(&pattern(text), bounds).unwrap_or_else(|e| panic!("{text}: {e}")))
    }

    #[test]
    fn a_number_is_itself() {
        let set = NumberSet::parse(&NumberPattern::Number(74), CC).unwrap();
        assert_eq!(members(set), [74]);
        assert_eq!(parsed("10", CHANNEL), [10]);
    }

    #[test]
    fn star_is_every_number_in_bounds() {
        assert_eq!(parsed("*", CHANNEL), (1..=16).collect::<Vec<_>>());
        assert_eq!(parsed("*", CC), (0..=127).collect::<Vec<_>>());
    }

    #[test]
    fn a_range_is_inclusive_and_more_than_one_digit_wide() {
        assert_eq!(parsed("1-4", CHANNEL), [1, 2, 3, 4]);
        assert_eq!(parsed("1-16", CHANNEL), (1..=16).collect::<Vec<_>>());
        assert_eq!(parsed("70-79", CC), (70..=79).collect::<Vec<_>>());
        assert_eq!(parsed("5-5", CHANNEL), [5]);
    }

    #[test]
    fn lists_and_ranges_combine() {
        assert_eq!(parsed("1,3,5", CHANNEL), [1, 3, 5]);
        assert_eq!(parsed("1-4,10", CHANNEL), [1, 2, 3, 4, 10]);
        assert_eq!(parsed(" 1 - 2 , 9 ", CHANNEL), [1, 2, 9]);
        assert_eq!(parsed("3,1-4", CHANNEL), [1, 2, 3, 4]);
    }

    #[test]
    fn a_number_outside_the_bounds_is_refused() {
        for (field, bounds, number) in [
            (NumberPattern::Number(0), CHANNEL, 0),
            (NumberPattern::Number(17), CHANNEL, 17),
            (NumberPattern::Number(-1), CC, -1),
            (NumberPattern::Number(128), CC, 128),
            (pattern("0"), CHANNEL, 0),
            (pattern("1-17"), CHANNEL, 17),
            (pattern("120-200"), CC, 200),
        ] {
            let error = NumberSet::parse(&field, bounds.clone()).expect_err("out of bounds");
            assert_eq!(
                error,
                PatternError::OutOfBounds {
                    number: number.to_string(),
                    bounds: bounds.clone()
                },
                "{field}"
            );
            assert_eq!(
                error.to_string(),
                format!("{number} is outside {}-{}", bounds.start(), bounds.end())
            );
        }
    }

    #[test]
    fn a_number_too_large_for_any_integer_is_quoted_as_written() {
        let digits = "99999999999999999999999";
        for text in [digits.to_string(), format!("1-{digits}")] {
            let error = NumberSet::parse(&pattern(&text), CHANNEL).expect_err(&text);
            assert_eq!(error.to_string(), format!("{digits} is outside 1-16"));
        }
        let json: NumberPattern = serde_json::from_str("18446744073709551615").unwrap();
        let error = NumberSet::parse(&json, CC).expect_err("past i64");
        assert_eq!(error.to_string(), "18446744073709551615 is outside 0-127");
    }

    #[test]
    fn a_malformed_pattern_says_what_is_wrong_with_it() {
        for (text, why) in [
            ("*-4", "`*` stands alone"),
            ("1-*", "`*` stands alone"),
            ("*1", "`*` stands alone"),
            ("1--2", "a range has one `-`"),
            ("1-2-3", "a range has one `-`"),
            ("1-", "a range needs a number at each end"),
            ("-4", "a range needs a number at each end"),
            ("1,,2", "an empty list item"),
            ("x", "something other than digits"),
            ("1.5", "something other than digits"),
        ] {
            let error = NumberSet::parse(&pattern(text), CHANNEL).expect_err(text);
            assert!(error.to_string().contains(why), "{text}: {error}");
        }
    }

    #[test]
    fn a_reversed_range_is_refused() {
        let error = NumberSet::parse(&pattern("8-4"), CHANNEL).expect_err("reversed");
        assert_eq!(error, PatternError::Reversed(8, 4));
        assert!(error.to_string().contains("reversed range"), "{error}");
    }

    #[test]
    fn a_malformed_pattern_is_refused() {
        for text in [
            "", " ", "1-", "-4", "1,", ",1", "1,,2", "[1-4]", "{1,2}", "1?", "x", "1-2-3", "+1",
            "*1", "1.5",
        ] {
            let error = NumberSet::parse(&pattern(text), CHANNEL).expect_err(text);
            assert!(
                matches!(error, PatternError::Malformed(..)),
                "{text}: {error:?}"
            );
            assert!(
                error.to_string().contains("is not a number pattern"),
                "{text}: {error}"
            );
        }
    }

    #[test]
    fn a_field_reads_a_json_integer_or_string_and_writes_it_back() {
        for (json, field) in [
            (serde_json::json!(1), NumberPattern::Number(1)),
            (serde_json::json!("1-4"), pattern("1-4")),
            (serde_json::json!("*"), pattern("*")),
        ] {
            let read: NumberPattern = serde_json::from_value(json.clone()).unwrap();
            assert_eq!(read, field);
            assert_eq!(serde_json::to_value(&read).unwrap(), json);
        }
        for json in [
            serde_json::json!(1.5),
            serde_json::json!(true),
            serde_json::json!([1]),
        ] {
            let error = serde_json::from_value::<NumberPattern>(json.clone()).expect_err("refused");
            assert!(
                error.to_string().contains("an integer, or a pattern"),
                "{json}: {error}"
            );
        }
    }

    #[test]
    fn from_text_is_a_number_only_when_the_text_is_one() {
        assert_eq!(NumberPattern::from_text(" 7 "), NumberPattern::Number(7));
        assert_eq!(NumberPattern::from_text("1-4"), pattern("1-4"));
        assert_eq!(NumberPattern::from_text("-1"), pattern("-1"));
        assert_eq!(NumberPattern::from_text(""), pattern(""));
    }
}
