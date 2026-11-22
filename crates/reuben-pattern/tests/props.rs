//! Property tests: span-edge exactness, determinism, buffer truncation, and hostile input.
//!
//! The hostile-input property doubles as the in-CI fuzz target; `fuzz/` runs the same body under
//! libFuzzer for longer campaigns.

mod common;
use common::*;
use proptest::prelude::*;
use reuben_pattern::{parse, Event, Pattern, Rational, Span};

/// Split `s` at `cuts` (fractions of its length, any order) into adjacent spans.
fn pieces(s: Span, cuts: &[u32], den: i64) -> Vec<Span> {
    let len = s.end.checked_sub(s.begin).unwrap();
    let mut points: Vec<Rational> = cuts
        .iter()
        .map(|c| {
            let f = r(i64::from(*c) % (den + 1), den);
            s.begin.checked_add(len.checked_mul(f).unwrap()).unwrap()
        })
        .collect();
    points.push(s.begin);
    points.push(s.end);
    points.sort();
    points.dedup();
    points.windows(2).map(|w| Span::new(w[0], w[1])).collect()
}

/// What the split queries must return: every whole-span fragment cut at the piece edges.
fn cut(whole: &[Event], parts: &[Span]) -> Vec<Event> {
    let mut out = Vec::new();
    for e in whole {
        for p in parts {
            let part = e.part.intersect(p);
            if !part.is_empty() {
                out.push(Event { part, ..*e });
            }
        }
    }
    sort(&mut out);
    out
}

fn check_split(p: &Pattern, s: Span, parts: &[Span], seed: u64) {
    let whole = query(p, s, seed);
    let mut split: Vec<Event> = parts
        .iter()
        .flat_map(|piece| query(p, *piece, seed))
        .collect();
    sort(&mut split);
    assert_eq!(split, cut(&whole, parts));
    // An onset is identified by its whole and value; its part differs once the span is cut.
    let onsets = |evs: &[Event]| -> Vec<Event> {
        let mut o: Vec<Event> = evs
            .iter()
            .filter(|e| e.has_onset())
            .map(|e| Event {
                part: e.whole,
                ..*e
            })
            .collect();
        sort(&mut o);
        o
    };
    let split_onsets = onsets(&split);
    assert_eq!(
        split_onsets,
        onsets(&whole),
        "an onset was dropped or doubled at a span edge"
    );
}

const BLOCK_PATTERNS: &[&str] = &[
    "0 [3 5] <7 ~> 2*2",
    "c4(3,8,2) eb3?0.4 [0|1|2]",
    "{0 1 2, 3 4}%5 <0 _ 1@2 2!2>",
    "[0 1]/1.5 2*2.75 . 0 .. 3",
    "<0 <1 2>>*3 [0, 1 2, 3 4 5]?",
];

#[test]
fn audio_blocks_partition_exactly() {
    // A cycle of 2 s at 48 kHz, cut into the irregular block sizes a host may deliver.
    let frames_per_cycle = 96_000;
    let blocks = [64, 128, 480, 512, 37, 1, 1024, 257];
    for src in BLOCK_PATTERNS {
        let p = Pattern::new(src).unwrap();
        let start = 1_000_000 * frames_per_cycle;
        let total = 4 * frames_per_cycle;
        let mut edges = vec![start];
        let mut i = 0;
        while *edges.last().unwrap() < start + total {
            let next = (edges.last().unwrap() + blocks[i % blocks.len()]).min(start + total);
            edges.push(next);
            i += 1;
        }
        let at = |f: i64| r(f, frames_per_cycle);
        let parts: Vec<Span> = edges
            .windows(2)
            .map(|w| Span::new(at(w[0]), at(w[1])))
            .collect();
        check_split(&p, Span::new(at(start), at(start + total)), &parts, 7);
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn adjacent_spans_equal_one_span(
        src in pattern_src(),
        s in query_span(),
        cuts in prop::collection::vec(any::<u32>(), 0..12),
        den in prop_oneof![Just(7i64), Just(64), Just(48_000), Just(1 << 20)],
        seed in any::<u64>(),
    ) {
        let Some(p) = generated(&src) else { return Ok(()) };
        check_split(&p, s, &pieces(s, &cuts, den), seed);
    }

    #[test]
    fn query_is_deterministic(src in pattern_src(), s in query_span(), seed in any::<u64>()) {
        let Some(a) = generated(&src) else { return Ok(()) };
        let b = Pattern::new(&src).unwrap();
        let mut x = vec![Event::EMPTY; 4096];
        let mut y = vec![Event::EMPTY; 4096];
        let rx = a.query(s, seed, &mut x);
        let ry = b.query(s, seed, &mut y);
        prop_assert_eq!(rx, ry);
        prop_assert_eq!(&x[..rx.written], &y[..ry.written]);
    }

    #[test]
    fn small_buffer_truncates_to_a_prefix(src in pattern_src(), s in query_span(), cap in 0usize..8) {
        let Some(p) = generated(&src) else { return Ok(()) };
        let mut big = vec![Event::EMPTY; 1 << 16];
        let full = p.query(s, 3, &mut big);
        prop_assert_eq!(full.dropped, 0);
        let mut small = vec![Event::EMPTY; cap];
        let res = p.query(s, 3, &mut small);
        prop_assert_eq!(res.written, full.written.min(cap));
        prop_assert_eq!(res.written + res.dropped, full.written);
        prop_assert_eq!(&small[..res.written], &big[..res.written]);
    }

    #[test]
    fn events_lie_inside_the_query(src in pattern_src(), s in query_span()) {
        let Some(p) = generated(&src) else { return Ok(()) };
        for e in query(&p, s, 1) {
            prop_assert!(!e.part.is_empty());
            prop_assert!(s.begin <= e.part.begin && e.part.end <= s.end);
            prop_assert!(e.whole.begin <= e.part.begin && e.part.end <= e.whole.end);
        }
    }

    /// The in-CI fuzz target: arbitrary text never panics the parser, and whatever compiles
    /// never panics a query or writes past the buffer.
    #[test]
    fn hostile_input_never_panics(
        src in "[0-9a-g~\\-_.,|!@?*/%:()<>\\[\\]{} #sbx\"é]{0,64}",
        s in query_span(),
        cap in 0usize..64,
    ) {
        fuzz_body(&src, s, cap);
    }

    #[test]
    fn arbitrary_bytes_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..96)) {
        if let Ok(src) = std::str::from_utf8(&bytes) {
            fuzz_body(src, Span::new(Rational::ZERO, r(7, 3)), 16);
        }
    }
}

fn fuzz_body(src: &str, s: Span, cap: usize) {
    match parse(src) {
        Err(e) => {
            assert!(
                e.span.start <= e.span.end && e.span.end <= src.len(),
                "{src:?}: {e:?}"
            );
            assert!(src.is_char_boundary(e.span.start) && src.is_char_boundary(e.span.end));
            assert!(!e.message.is_empty());
        }
        Ok(ast) => {
            if let Ok(p) = reuben_pattern::compile(&ast) {
                let mut buf = vec![Event::EMPTY; cap];
                let res = p.query(s, 0, &mut buf);
                assert!(res.written <= cap);
            }
        }
    }
}

#[test]
fn overflow_is_counted_not_panicked() {
    let p = Pattern::new("[0 1 2]*7 <3 4>/5").unwrap();
    let near = Rational::int(i64::MAX / 4);
    let s = Span::new(near, near.checked_add(r(1, 3)).unwrap());
    let mut buf = [Event::EMPTY; 32];
    let res = p.query(s, 0, &mut buf);
    assert!(res.overflowed > 0, "{res:?}");
}

#[test]
fn long_sets_at_audio_rate_do_not_overflow() {
    // Up to a year of cycles, frame-aligned at audio rates, through patterns at the scale cap.
    let patterns = [
        "[[0 1 2]*7 <3 [4 5]/3>]*1.37 {0 1 2, 3 4}%5?0.2 6(5,13,3)",
        "[0*1000]*1000",
        "0*1048576",
        "0/1048576",
        "<0 1 2 3 4 5 6 7 8 9>/100*1.001",
    ];
    for src in patterns {
        let p = Pattern::new(src).unwrap();
        for hours in [1i64, 24, 24 * 365] {
            for den in [48_000i64, 96_000, 44_100 * 4] {
                let start = r(hours * 3600 * den + 17, den);
                let s = Span::new(start, start.checked_add(r(512, den)).unwrap());
                let mut buf = vec![Event::EMPTY; 4096];
                let res = p.query(s, 0, &mut buf);
                assert_eq!(res.overflowed, 0, "{src} at {hours} h, 1/{den}");
            }
        }
    }
}

#[test]
fn negative_cycles_work() {
    assert_same("<0 1 2>", "<0 1 2>", cycles(-5, 0));
    assert_eq!(onsets("<0 1 2>", cycles(-1, 0)), vec!["-1-0 2".to_string()]);
}

#[test]
fn seed_changes_random_outcomes_only() {
    let src = "[0*16?, 1*4]";
    let a = onsets_seeded(src, cycles(0, 4), 1);
    let b = onsets_seeded(src, cycles(0, 4), 2);
    assert_ne!(a, b);
    let ones = |v: &[String]| v.iter().filter(|e| e.ends_with(" 1")).count();
    assert_eq!(ones(&a), 16);
    assert_eq!(ones(&b), 16);
}

#[test]
fn generator_mostly_compiles() {
    // The properties skip generated patterns that trip a size limit; that must stay the exception.
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let mut runner = TestRunner::deterministic();
    let strategy = pattern_src();
    let total = 500;
    let ok = (0..total)
        .filter(|_| generated(&strategy.new_tree(&mut runner).unwrap().current()).is_some())
        .count();
    assert!(
        ok * 10 >= total * 9,
        "only {ok} of {total} generated patterns compiled"
    );
}
