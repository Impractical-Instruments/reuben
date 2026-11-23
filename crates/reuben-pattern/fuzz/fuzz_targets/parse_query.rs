//! `cargo +nightly fuzz run parse_query` from `crates/reuben-pattern`.
//!
//! Arbitrary text must never panic the parser; whatever compiles must never panic a cycle query,
//! write past the caller's buffer, or exceed its compile-time work bound, for any cycle the input
//! also chooses.
#![no_main]

use libfuzzer_sys::fuzz_target;
use reuben_pattern::{compile, parse, Note};

fuzz_target!(|data: &[u8]| {
    // The first 10 bytes choose the cycle and buffer size; the rest is the pattern text.
    if data.len() < 10 {
        return;
    }
    let (head, text) = data.split_at(10);
    let Ok(src) = core::str::from_utf8(text) else {
        return;
    };
    let cycle = i64::from_le_bytes(head[..8].try_into().unwrap());
    let cap = usize::from(head[8]);
    match parse(src) {
        Err(e) => {
            assert!(e.span.start <= e.span.end && e.span.end <= src.len());
            assert!(src.is_char_boundary(e.span.start) && src.is_char_boundary(e.span.end));
        }
        Ok(ast) => {
            if let Ok(p) = compile(&ast) {
                let mut buf = vec![Note::EMPTY; cap];
                let res = p.cycle(cycle, u64::from(head[9]), &mut buf);
                assert!(res.written <= cap);
                assert!(!res.exhausted && res.work <= p.max_cycle_work());
            }
        }
    }
});
