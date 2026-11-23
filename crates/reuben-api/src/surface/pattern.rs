//! OSC 1.0 address patterns, matched one `/`-separated segment at a time: `?` is one character,
//! `*` any run of characters, `[a-z]` / `[!a-z]` one character in (or not in) a set, and `{a,b}`
//! one of several literal strings. No token matches a `/`.

use std::format;
use std::string::{String, ToString};
use std::vec::Vec;

use super::midi::MidiKind;

/// A compiled address pattern.
#[derive(Debug, Clone, PartialEq)]
pub struct OscPattern {
    segments: Vec<Vec<Token>>,
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Literal(char),
    One,
    Any,
    Class {
        negated: bool,
        ranges: Vec<(char, char)>,
    },
    Choice(Vec<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum PatternError {
    Malformed(String),
    /// The pattern's first segment is not the literal `midi`.
    NotMidi,
    /// The pattern can match more than one message kind.
    MixedKinds,
    /// The pattern can match no translated MIDI address.
    NoKind,
}

impl std::fmt::Display for PatternError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(why) => write!(f, "is not an OSC address pattern: {why}"),
            Self::NotMidi => f.write_str("does not start with /midi/"),
            Self::MixedKinds => f.write_str(
                "can match more than one message kind (note, cc, bend, pressure, program)",
            ),
            Self::NoKind => f.write_str("can match no MIDI message"),
        }
    }
}

impl OscPattern {
    pub fn parse(pattern: &str) -> Result<Self, PatternError> {
        let malformed = |why: &str| PatternError::Malformed(format!("{pattern:?} {why}"));
        let rest = pattern
            .strip_prefix('/')
            .ok_or_else(|| malformed("does not start with '/'"))?;
        let segments = rest
            .split('/')
            .map(|segment| {
                if segment.is_empty() {
                    return Err(malformed("has an empty segment"));
                }
                parse_segment(segment).map_err(&malformed)
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { segments })
    }

    pub fn matches(&self, address: &str) -> bool {
        let Some(rest) = address.strip_prefix('/') else {
            return false;
        };
        let parts: Vec<&str> = rest.split('/').collect();
        parts.len() == self.segments.len()
            && self
                .segments
                .iter()
                .zip(parts)
                .all(|(tokens, part)| matches_segment(tokens, &part.chars().collect::<Vec<_>>()))
    }

    /// The one kind of translated message this pattern can match, read from its third segment
    /// tested against each kind's name. A segment that could name two kinds is refused even where
    /// the segment count would rule one out (`/midi/1/{note,cc}`), so a pattern names its kind
    /// plainly; a pattern whose segment count fits no address of its kind matches nothing.
    pub fn midi_kind(&self) -> Result<MidiKind, PatternError> {
        let first_is_midi = self.segments.first().is_some_and(|tokens| {
            tokens
                .iter()
                .map(|t| match t {
                    Token::Literal(c) => Some(*c),
                    _ => None,
                })
                .collect::<Option<String>>()
                .as_deref()
                == Some("midi")
        });
        if !first_is_midi {
            return Err(PatternError::NotMidi);
        }
        let Some(kind_tokens) = self.segments.get(2) else {
            return Err(PatternError::NoKind);
        };
        let mut kinds = MidiKind::ALL.into_iter().filter(|kind| {
            matches_segment(kind_tokens, &kind.segment().chars().collect::<Vec<_>>())
        });
        let kind = match (kinds.next(), kinds.next()) {
            (Some(kind), None) => kind,
            (Some(_), Some(_)) => return Err(PatternError::MixedKinds),
            (None, _) => return Err(PatternError::NoKind),
        };
        let segment_count = if kind == MidiKind::Cc { 4 } else { 3 };
        if self.segments.len() != segment_count {
            return Err(PatternError::NoKind);
        }
        Ok(kind)
    }
}

fn parse_segment(segment: &str) -> Result<Vec<Token>, &'static str> {
    let mut tokens = Vec::new();
    let mut chars = segment.chars().peekable();
    while let Some(c) = chars.next() {
        tokens.push(match c {
            '?' => Token::One,
            '*' => Token::Any,
            '[' => {
                let negated = chars.next_if_eq(&'!').is_some();
                let mut members = Vec::new();
                loop {
                    match chars.next() {
                        None => return Err("has an unclosed '['"),
                        Some(']') => break,
                        Some(c) => members.push(c),
                    }
                }
                if members.is_empty() {
                    return Err("has an empty '[]'");
                }
                // A '-' between two characters makes a range; at either end it is itself.
                let mut ranges = Vec::new();
                let mut i = 0;
                while i < members.len() {
                    if i + 2 < members.len() && members[i + 1] == '-' {
                        if members[i] > members[i + 2] {
                            return Err("has a reversed range, which matches nothing");
                        }
                        ranges.push((members[i], members[i + 2]));
                        i += 3;
                    } else {
                        ranges.push((members[i], members[i]));
                        i += 1;
                    }
                }
                Token::Class { negated, ranges }
            }
            '{' => {
                let mut body = String::new();
                loop {
                    match chars.next() {
                        None => return Err("has an unclosed '{'"),
                        Some('}') => break,
                        Some('{') => return Err("nests '{'"),
                        Some(c) => body.push(c),
                    }
                }
                if body.is_empty() {
                    return Err("has an empty '{}', which matches nothing");
                }
                Token::Choice(body.split(',').map(str::to_string).collect())
            }
            ']' => return Err("has a ']' with no '['"),
            '}' => return Err("has a '}' with no '{'"),
            c => Token::Literal(c),
        });
    }
    Ok(tokens)
}

/// Backtracking match of one segment; segments are a few characters long, so the worst case of
/// several `*` stays small.
fn matches_segment(tokens: &[Token], text: &[char]) -> bool {
    let Some((token, rest)) = tokens.split_first() else {
        return text.is_empty();
    };
    match token {
        Token::Literal(c) => text.first() == Some(c) && matches_segment(rest, &text[1..]),
        Token::One => !text.is_empty() && matches_segment(rest, &text[1..]),
        Token::Any => (0..=text.len()).any(|skip| matches_segment(rest, &text[skip..])),
        Token::Class { negated, ranges } => {
            text.first().is_some_and(|c| {
                ranges.iter().any(|(low, high)| (low..=high).contains(&c)) != *negated
            }) && matches_segment(rest, &text[1..])
        }
        Token::Choice(options) => options.iter().any(|option| {
            let option: Vec<char> = option.chars().collect();
            text.starts_with(&option) && matches_segment(rest, &text[option.len()..])
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(text: &str) -> OscPattern {
        OscPattern::parse(text).unwrap_or_else(|e| panic!("{text}: {e:?}"))
    }

    #[test]
    fn alternatives_pick_channels() {
        let p = pattern("/midi/{1,2,5,9}/cc/*");
        assert!(p.matches("/midi/5/cc/74"));
        assert!(!p.matches("/midi/3/cc/74"));
    }

    #[test]
    fn a_range_is_one_character() {
        let p = pattern("/midi/[4-8]/note");
        assert!(p.matches("/midi/6/note"));
        assert!(!p.matches("/midi/9/note"));
        assert!(!p.matches("/midi/16/note"));
    }

    #[test]
    fn question_mark_is_one_character() {
        let p = pattern("/midi/1?/note");
        assert!(p.matches("/midi/12/note"));
        assert!(!p.matches("/midi/1/note"));
    }

    #[test]
    fn a_class_can_be_negated() {
        let p = pattern("/midi/[!1]/note");
        assert!(p.matches("/midi/2/note"));
        assert!(!p.matches("/midi/1/note"));
    }

    #[test]
    fn star_never_crosses_a_slash() {
        let p = pattern("/midi/*/note");
        assert!(p.matches("/midi/12/note"));
        assert!(!p.matches("/midi/1/cc/note"));
        assert!(pattern("/midi/1/cc/*").matches("/midi/1/cc/7"));
        assert!(!pattern("/midi/1/*").matches("/midi/1/cc/7"));
    }

    #[test]
    fn literals_match_only_themselves() {
        let p = pattern("/midi/1/cc/74");
        assert!(p.matches("/midi/1/cc/74"));
        assert!(!p.matches("/midi/1/cc/7"));
        assert!(!p.matches("/midi/1/cc/740"));
    }

    #[test]
    fn midi_kind_is_the_one_kind_a_pattern_can_match() {
        for (text, kind) in [
            ("/midi/1/cc/74", Ok(MidiKind::Cc)),
            ("/midi/[4-8]/note", Ok(MidiKind::Note)),
            ("/midi/1/no*", Ok(MidiKind::Note)),
            ("/midi/*/bend", Ok(MidiKind::Bend)),
            ("/midi/1/*/74", Err(PatternError::MixedKinds)),
            ("/midi/1/*", Err(PatternError::MixedKinds)),
            ("/midi/1/{note,cc}", Err(PatternError::MixedKinds)),
            ("/fader1", Err(PatternError::NotMidi)),
            ("/m*/1/note", Err(PatternError::NotMidi)),
            ("/midi/1/clock", Err(PatternError::NoKind)),
            ("/midi/1/note/2", Err(PatternError::NoKind)),
            ("/midi/1/cc", Err(PatternError::NoKind)),
        ] {
            assert_eq!(pattern(text).midi_kind(), kind, "{text}");
        }
    }

    #[test]
    fn a_pattern_that_can_match_nothing_is_malformed() {
        for text in ["/midi/[8-4]/note", "/midi/1/{}", "/midi/[]/note"] {
            assert!(
                matches!(OscPattern::parse(text), Err(PatternError::Malformed(_))),
                "{text}"
            );
        }
        // An empty alternative beside others matches the empty string, and is kept.
        assert!(pattern("/midi/1/cc/7{4,}").matches("/midi/1/cc/7"));
    }

    #[test]
    fn malformed_patterns_are_refused() {
        for text in [
            "/midi/[1-/note",
            "midi/1/note",
            "/midi/{1,2/note",
            "/midi//note",
            "/midi/1/note/",
            "",
        ] {
            assert!(
                matches!(OscPattern::parse(text), Err(PatternError::Malformed(_))),
                "{text}"
            );
        }
    }
}
