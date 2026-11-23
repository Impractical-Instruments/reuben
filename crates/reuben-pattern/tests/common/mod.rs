//! Shared helpers for the integration tests.
#![allow(dead_code)]

use proptest::prelude::*;
use reuben_pattern::{Note, Pattern, Rational, Value};
use std::ops::Range;

pub fn r(n: i64, d: i64) -> Rational {
    Rational::new(n, d).unwrap()
}

/// Cycles `a..b`.
pub fn cycles(a: i64, b: i64) -> Range<i64> {
    a..b
}

/// Every note of cycle `c`, sorted, from a buffer large enough that nothing drops.
pub fn query(p: &Pattern, c: i64, seed: u64) -> Vec<Note> {
    let mut buf = vec![Note::EMPTY; 1 << 12];
    let res = p.cycle(c, seed, &mut buf);
    assert_eq!(res.dropped, 0, "test buffer too small");
    assert_eq!(res.overflowed, 0, "unexpected overflow at cycle {c}");
    assert!(!res.exhausted, "cycle {c} ran out of work budget");
    assert!(res.work <= p.max_cycle_work(), "{res:?} over the bound");
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

pub fn sort(notes: &mut [Note]) {
    notes.sort_by_key(|n| (n.onset, n.release, value_key(n.value)));
}

fn show_value(v: Value) -> String {
    match v {
        Value::Number(n) => format!("{n}"),
        Value::Note(m) => format!("n{m}"),
    }
}

/// The notes of cycles `cs`, in absolute cycles, as `"onset-release value"`, sorted: the shape the
/// reference table is written in.
pub fn onsets(src: &str, cs: Range<i64>) -> Vec<String> {
    onsets_seeded(src, cs, 0)
}

pub fn onsets_seeded(src: &str, cs: Range<i64>, seed: u64) -> Vec<String> {
    let p = Pattern::new(src).unwrap_or_else(|e| panic!("{src:?}: {e}"));
    let mut out: Vec<String> = Vec::new();
    for c in cs {
        let at = |x: Rational| Rational::int(c).checked_add(x).unwrap();
        out.extend(
            query(&p, c, seed)
                .into_iter()
                .map(|n| format!("{}-{} {}", at(n.onset), at(n.release), show_value(n.value))),
        );
    }
    out
}

/// Two patterns produce identical notes over cycles `cs`.
pub fn assert_same(a: &str, b: &str, cs: Range<i64>) {
    let pa = Pattern::new(a).unwrap_or_else(|e| panic!("{a:?}: {e}"));
    let pb = Pattern::new(b).unwrap_or_else(|e| panic!("{b:?}: {e}"));
    for c in cs {
        assert_eq!(
            query(&pa, c, 0),
            query(&pb, c, 0),
            "{a:?} vs {b:?} at cycle {c}"
        );
    }
}

/// Compile a generated pattern; `None` if it trips a size limit, which random nesting can reach.
pub fn generated(src: &str) -> Option<Pattern> {
    match Pattern::new(src) {
        Ok(p) => Some(p),
        Err(e)
            if [
                "too far",
                "too deeply",
                "too dense",
                "too large",
                "too many",
            ]
            .iter()
            .any(|m| e.message.contains(m)) =>
        {
            None
        }
        Err(e) => panic!("{src:?}: {e}"),
    }
}

/// A cycle number: small, negative, and near the guaranteed limit.
pub fn any_cycle() -> impl Strategy<Value = i64> {
    prop_oneof![
        -8i64..2000,
        (-reuben_pattern::MAX_CYCLE)..=reuben_pattern::MAX_CYCLE,
        Just(reuben_pattern::MAX_CYCLE),
        Just(-reuben_pattern::MAX_CYCLE),
    ]
}

/// Syntactically valid mini-notation, exercising every v1 construct: broad patterns, and narrow
/// ones nested past `MAX_NESTING` so the depth limits are exercised.
pub fn pattern_src() -> impl Strategy<Value = String> {
    prop_oneof![broad_src(), deep_src()]
}

fn leaf_src() -> impl Strategy<Value = String> {
    prop_oneof![
        (0..12i32).prop_map(|n| n.to_string()),
        Just("-1".to_string()),
        Just("1.5".to_string()),
        Just("c4".to_string()),
        Just("eb3".to_string()),
        Just("~".to_string()),
        Just("-".to_string()),
    ]
}

/// One subpattern wrapped up to 16 times, each wrap a bracket or modifier with a leaf beside it.
fn deep_src() -> impl Strategy<Value = String> {
    let wrap = (0..10u8, leaf_src());
    (leaf_src(), prop::collection::vec(wrap, 1..17)).prop_map(|(mut s, wraps)| {
        for (kind, l) in wraps {
            s = match kind {
                0 => format!("[{s} {l}]"),
                1 => format!("<{s} {l}>"),
                2 => format!("{{{s}, {l}}}"),
                3 => format!("[{s} | {l}]"),
                4 => format!("[{s}]*2"),
                5 => format!("[{s}]/1.5"),
                6 => format!("[{s}]?0.3"),
                7 => format!("[{s}](3,8)"),
                8 => format!("[{s}, {l}]"),
                _ => format!("[{s} . {l}]"),
            };
        }
        s
    })
}

fn broad_src() -> impl Strategy<Value = String> {
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
    leaf.prop_recursive(4, 24, 3, |inner| {
        // Mostly unmodified, as written patterns are: stacked speed-ups compound into patterns too
        // dense to query in real time, which compile refuses and the properties then skip.
        let modifier = prop_oneof![
            Just(""),
            Just(""),
            Just(""),
            Just(""),
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
        prop::collection::vec(term, 1..3).prop_map(|v| v.join(" "))
    })
}
