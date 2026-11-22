//! Shared helpers for the integration tests.
#![allow(dead_code)]

use proptest::prelude::*;
use reuben_pattern::{Event, Pattern, Rational, Span, Value};

pub fn r(n: i64, d: i64) -> Rational {
    Rational::new(n, d).unwrap()
}

pub fn span(b: Rational, e: Rational) -> Span {
    Span::new(b, e)
}

pub fn cycles(a: i64, b: i64) -> Span {
    Span::new(Rational::int(a), Rational::int(b))
}

/// Every fragment in `s`, from a buffer large enough that nothing drops.
pub fn query(p: &Pattern, s: Span, seed: u64) -> Vec<Event> {
    let mut buf = vec![Event::EMPTY; 1 << 16];
    let res = p.query(s, seed, &mut buf);
    assert_eq!(res.dropped, 0, "test buffer too small");
    assert_eq!(res.overflowed, 0, "unexpected overflow");
    buf.truncate(res.written);
    sort(&mut buf);
    buf
}

fn value_key(v: Value) -> (u8, i64, i64) {
    match v {
        Value::Number(n) => (0, n.numer(), n.denom()),
        Value::Note(m) => (1, i64::from(m), 1),
    }
}

pub fn sort(evs: &mut [Event]) {
    evs.sort_by_key(|e| {
        (
            e.whole.begin,
            e.whole.end,
            e.part.begin,
            e.part.end,
            value_key(e.value),
        )
    });
}

fn show_value(v: Value) -> String {
    match v {
        Value::Number(n) => format!("{n}"),
        Value::Note(m) => format!("n{m}"),
    }
}

/// The onsets in `s` as `"begin-end value"`, sorted: the shape the reference table is written in.
pub fn onsets(src: &str, s: Span) -> Vec<String> {
    onsets_seeded(src, s, 0)
}

pub fn onsets_seeded(src: &str, s: Span, seed: u64) -> Vec<String> {
    let p = Pattern::new(src).unwrap_or_else(|e| panic!("{src:?}: {e}"));
    query(&p, s, seed)
        .into_iter()
        .filter(Event::has_onset)
        .map(|e| format!("{}-{} {}", e.whole.begin, e.whole.end, show_value(e.value)))
        .collect()
}

/// Compile a generated pattern; `None` if it trips a size limit, which random nesting can reach.
pub fn generated(src: &str) -> Option<Pattern> {
    match Pattern::new(src) {
        Ok(p) => Some(p),
        Err(e) if e.message.contains("too far") || e.message.contains("too deeply") => None,
        Err(e) => panic!("{src:?}: {e}"),
    }
}

/// Two patterns produce identical fragments over `s`.
pub fn assert_same(a: &str, b: &str, s: Span) {
    let pa = Pattern::new(a).unwrap_or_else(|e| panic!("{a:?}: {e}"));
    let pb = Pattern::new(b).unwrap_or_else(|e| panic!("{b:?}: {e}"));
    assert_eq!(query(&pa, s, 0), query(&pb, s, 0), "{a:?} vs {b:?}");
}

/// Syntactically valid mini-notation, exercising every v1 construct.
pub fn pattern_src() -> impl Strategy<Value = String> {
    let leaf = prop_oneof![
        (0..12i32).prop_map(|n| n.to_string()),
        Just("-1".to_string()),
        Just("1.5".to_string()),
        Just("c4".to_string()),
        Just("eb3".to_string()),
        Just("~".to_string()),
        Just("-".to_string()),
        Just("0 .. 3".to_string()),
    ];
    leaf.prop_recursive(4, 48, 5, |inner| {
        let modifier = prop_oneof![
            Just(""),
            Just(""),
            Just("*2"),
            Just("*3"),
            Just("/2"),
            Just("/3"),
            Just("*1.5"),
            Just("/0.75"),
            Just("@2"),
            Just("@1.5"),
            Just("!2"),
            Just("!"),
            Just("?"),
            Just("?0.3"),
            Just("(3,8)"),
            Just("(3,8,2)"),
            Just("(5,8,-1)"),
            Just("(0,3)"),
        ];
        let bracketed = prop_oneof![
            inner.clone().prop_map(|s| format!("[{s}]")),
            inner.clone().prop_map(|s| format!("<{s}>")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("{{{a}, {b}}}")),
            (inner.clone(), 1..9i32).prop_map(|(a, n)| format!("{{{a}}}%{n}")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("[{a} | {b}]")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("[{a}, {b}]")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("[{a} . {b}]")),
        ];
        let modified = (
            bracketed,
            modifier.clone(),
            modifier,
            prop_oneof![Just(""), Just(""), Just(""), Just(" _")],
        )
            .prop_map(|(t, m1, m2, el)| format!("{t}{m1}{m2}{el}"));
        // A bare sub-sequence takes no modifiers: they would bind to its last step only.
        let term = prop_oneof![inner, modified.clone(), modified];
        prop::collection::vec(term, 1..5).prop_map(|v| v.join(" "))
    })
}

/// A query span: start cycle (negatives included), length, and denominator from audio-rate grids.
pub fn query_span() -> impl Strategy<Value = Span> {
    (
        -8i64..2000,
        prop_oneof![
            Just(1i64),
            Just(4),
            Just(7),
            Just(48_000),
            Just(96_000),
            Just(44_100 * 3)
        ],
        0u32..4000,
        1u32..4000,
    )
        .prop_map(|(c, den, off, len)| {
            let b = Rational::int(c)
                .checked_add(r(i64::from(off), den))
                .unwrap();
            let e = b.checked_add(r(i64::from(len), 2000)).unwrap();
            Span::new(b, e)
        })
}
