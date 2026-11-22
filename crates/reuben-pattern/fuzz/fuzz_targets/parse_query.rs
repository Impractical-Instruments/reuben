//! `cargo +nightly fuzz run parse_query` from `crates/reuben-pattern`.
//!
//! Arbitrary text must never panic the parser; whatever compiles must never panic a query or
//! write past the caller's buffer, for any span the input also chooses.
#![no_main]

use libfuzzer_sys::fuzz_target;
use reuben_pattern::{compile, parse, Event, Rational, Span};

fuzz_target!(|data: &[u8]| {
    // The first 16 bytes choose the span and buffer size; the rest is the pattern text.
    if data.len() < 16 {
        return;
    }
    let (head, text) = data.split_at(16);
    let Ok(src) = core::str::from_utf8(text) else {
        return;
    };
    let word = |i: usize| i32::from_le_bytes([head[i], head[i + 1], head[i + 2], head[i + 3]]);
    let den = i64::from(word(4).unsigned_abs().max(1));
    let Some(begin) = Rational::new(i64::from(word(0)), den) else {
        return;
    };
    let len = Rational::new(i64::from(word(8).unsigned_abs() % 8192), 1024).unwrap();
    let Some(end) = begin.checked_add(len) else {
        return;
    };
    let cap = usize::from(head[12]);
    match parse(src) {
        Err(e) => {
            assert!(e.span.start <= e.span.end && e.span.end <= src.len());
            assert!(src.is_char_boundary(e.span.start) && src.is_char_boundary(e.span.end));
        }
        Ok(ast) => {
            if let Ok(p) = compile(&ast) {
                let mut buf = vec![Event::EMPTY; cap];
                let res = p.query(Span::new(begin, end), u64::from(head[13]), &mut buf);
                assert!(res.written <= cap);
            }
        }
    }
});
