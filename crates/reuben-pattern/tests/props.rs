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
        prop_assume!(fits_budget(&p, s));
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
        prop_assume!(fits_budget(&p, s));
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

/// Patterns the reviewer found overflowing under clock-derived spans before time became
/// cycle-plus-fraction.
const CLOCK_PATTERNS: &[&str] = &[
    "0*1001",
    "0*7*11*13",
    "<0 1 2 3 4 5 6>/100*1.001",
    "0*1023",
    "0*1048573%1048571",
    "[0 1 2 3 4 5 6]*1.37",
    "[[0 1 2]*7 <3 [4 5]/3>]*1.37 {0 1 2, 3 4}%5?0.2 6(5,13,3)",
    "c4 [e4 g4]/3 <b4 d5>?",
];

#[test]
fn clock_positions_never_overflow_over_long_sets() {
    // Frames just after a beat, where the f32 phase is smallest and its denominator largest.
    for src in CLOCK_PATTERNS {
        let p = Pattern::new(src).unwrap();
        for (bpm, hours) in [(120.0f64, 12.0f64), (999.0, 80.0), (30.0, 80.0)] {
            let beat = (hours * 60.0 * bpm) as i64;
            let inc = bpm / 60.0 / 48_000.0;
            for bpc in [1, 4, 7] {
                for k in 1..64 {
                    let a = clock_pos(beat, (k as f64 * inc) as f32, bpc);
                    let b = clock_pos(beat, ((k + 1) as f64 * inc) as f32, bpc);
                    let mut buf = [Event::EMPTY; 64];
                    let res = p.query(Span::new(a, b), 0, &mut buf);
                    assert_eq!(
                        res.overflowed, 0,
                        "{src} at {hours} h, {bpm} BPM, frame {k}"
                    );
                }
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// Clock-derived spans: more than 72 h at 999 BPM, any frame, several cycle lengths.
    #[test]
    fn clock_spans_never_overflow(
        src in pattern_src(),
        bpm in prop_oneof![Just(30.0f64), Just(120.0), Just(999.0)],
        hours in 0.0f64..80.0,
        frame in 0u32..96_000,
        frames in 1u32..512,
        bpc in prop_oneof![Just(1i64), Just(3), Just(4), Just(7)],
    ) {
        let Some(p) = generated(&src) else { return Ok(()) };
        let inc = bpm / 60.0 / 48_000.0;
        let beat = (hours * 60.0 * bpm) as i64;
        let frames_per_beat = (1.0 / inc) as u32;
        let k = frame % frames_per_beat;
        let end = (k + frames).min(frames_per_beat - 1);
        prop_assume!(end > k);
        let a = clock_pos(beat, (f64::from(k) * inc) as f32, bpc);
        let b = clock_pos(beat, (f64::from(end) * inc) as f32, bpc);
        prop_assume!(a < b);
        let mut buf = vec![Event::EMPTY; 256];
        prop_assert_eq!(p.query(Span::new(a, b), 0, &mut buf).overflowed, 0);
    }

    /// Spans on a `2^40` grid, up to `2^22` cycles in (beyond 72 h at 999 BPM, one beat per cycle).
    #[test]
    fn two_to_the_forty_denominators_never_overflow(
        src in pattern_src(),
        cycle in 0i64..(1 << 22),
        offset in 0i64..(1 << 40),
        len in 1i64..(1 << 36),
        odd in prop_oneof![Just(1i64), Just(3), Just(7)],
    ) {
        let Some(p) = generated(&src) else { return Ok(()) };
        let den = (1i64 << 40) * odd;
        // A `Rational` endpoint holds `cycle * den` in an i64, which caps the cycle at 2^23 / odd.
        let cycle = cycle / odd;
        let b = Rational::int(cycle).checked_add(r(offset, den)).unwrap();
        let e = b.checked_add(r(len, den)).unwrap();
        let mut buf = vec![Event::EMPTY; 256];
        prop_assert_eq!(p.query(Span::new(b, e), 0, &mut buf).overflowed, 0);
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
fn generator_mostly_compiles_and_fits_the_budget() {
    // The properties skip generated patterns that trip a limit, and queries that exhaust the work
    // budget; both must stay the exception or the properties test little.
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let mut runner = TestRunner::deterministic();
    let (patterns, spans) = (pattern_src(), query_span());
    let total = 500;
    let mut compiled = 0;
    let mut fit = 0;
    for _ in 0..total {
        let src = patterns.new_tree(&mut runner).unwrap().current();
        let s = spans.new_tree(&mut runner).unwrap().current();
        if let Some(p) = generated(&src) {
            compiled += 1;
            fit += usize::from(fits_budget(&p, s));
        }
    }
    assert!(
        compiled * 10 >= total * 9,
        "only {compiled} of {total} generated patterns compiled"
    );
    assert!(
        fit * 10 >= compiled * 9,
        "only {fit} of {compiled} queries fit the budget"
    );
}

fn arb_rational() -> impl Strategy<Value = Rational> {
    prop_oneof![
        (-3i64..6, 1i64..5).prop_map(|(n, d)| r(n, d)),
        Just(Rational::ZERO),
        Just(Rational::int(i64::MAX)),
        Just(Rational::int(i64::MIN)),
        Just(r(1, i64::MAX)),
        Just(r(1_048_573, 1_048_571)),
    ]
}

fn arb_expr() -> impl Strategy<Value = reuben_pattern::Expr> {
    use reuben_pattern::{Expr, ExprKind, Step};
    let leaf = prop_oneof![
        arb_rational().prop_map(ExprKind::Number),
        any::<u8>().prop_map(ExprKind::Note),
        Just(ExprKind::Rest),
    ]
    .prop_map(|kind| Expr { kind, span: 0..0 });
    leaf.prop_recursive(5, 64, 4, |inner| {
        let step = (
            inner.clone(),
            arb_rational(),
            prop_oneof![Just(0u32), 1u32..4, Just(u32::MAX)],
        )
            .prop_map(|(expr, weight, copies)| Step {
                expr,
                weight,
                copies,
            });
        let steps = prop::collection::vec(step, 0..4).boxed();
        let exprs = prop::collection::vec(inner.clone(), 0..3);
        prop_oneof![
            steps.clone().prop_map(ExprKind::Seq),
            steps.clone().prop_map(ExprKind::Alt),
            exprs.clone().prop_map(ExprKind::Stack),
            exprs.prop_map(ExprKind::Choice),
            (
                prop::collection::vec(steps, 0..3),
                prop::option::of(arb_rational())
            )
                .prop_map(|(layers, steps)| ExprKind::Poly { layers, steps }),
            (inner.clone(), arb_rational()).prop_map(|(e, f)| ExprKind::Fast(Box::new(e), f)),
            (inner.clone(), arb_rational()).prop_map(|(e, f)| ExprKind::Slow(Box::new(e), f)),
            (inner.clone(), arb_rational()).prop_map(|(e, p)| ExprKind::Degrade(Box::new(e), p)),
            (inner, 0u32..12, 0u32..12, any::<i64>()).prop_map(|(e, pulses, steps, rotation)| {
                ExprKind::Euclid {
                    expr: Box::new(e),
                    pulses,
                    steps,
                    rotation,
                }
            }),
        ]
        .prop_map(|kind| Expr { kind, span: 0..0 })
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    /// `compile` takes a public `Ast`, so it must reject anything malformed with an `Error`, and
    /// whatever it accepts must never panic a query.
    #[test]
    fn hand_built_asts_never_panic(root in arb_expr(), s in query_span(), cap in 0usize..32) {
        if let Ok(p) = reuben_pattern::compile(&reuben_pattern::Ast { root }) {
            let mut buf = vec![Event::EMPTY; cap];
            let res = p.query(s, 0, &mut buf);
            prop_assert!(res.written <= cap);
        }
    }
}

#[test]
fn malformed_asts_are_errors() {
    use reuben_pattern::{compile, Ast, Expr, ExprKind, Step};
    let one = || Expr {
        kind: ExprKind::Number(Rational::ONE),
        span: 0..1,
    };
    let step = |weight, copies| Step {
        expr: one(),
        weight,
        copies,
    };
    let cases = [
        ExprKind::Choice(vec![]),
        ExprKind::Stack(vec![]),
        ExprKind::Seq(vec![]),
        ExprKind::Alt(vec![]),
        ExprKind::Poly {
            layers: vec![],
            steps: None,
        },
        ExprKind::Poly {
            layers: vec![vec![]],
            steps: None,
        },
        ExprKind::Poly {
            layers: vec![vec![step(Rational::ONE, 1)]],
            steps: Some(Rational::ZERO),
        },
        ExprKind::Slow(Box::new(one()), Rational::ZERO),
        ExprKind::Fast(Box::new(one()), r(-1, 2)),
        ExprKind::Degrade(Box::new(one()), r(3, 2)),
        ExprKind::Euclid {
            expr: Box::new(one()),
            pulses: 1,
            steps: 0,
            rotation: 0,
        },
        ExprKind::Euclid {
            expr: Box::new(one()),
            pulses: 5,
            steps: 4,
            rotation: 0,
        },
        ExprKind::Seq(vec![step(Rational::ZERO, 1), step(Rational::ONE, 1)]),
        ExprKind::Seq(vec![step(Rational::ONE, 0)]),
        ExprKind::Seq(vec![step(Rational::ONE, u32::MAX)]),
        ExprKind::Note(200),
    ];
    for kind in cases {
        let ast = Ast {
            root: Expr {
                kind: kind.clone(),
                span: 0..9,
            },
        };
        assert!(compile(&ast).is_err(), "{kind:?} compiled");
    }
}

/// Cut points at every event edge plus an audio-rate grid, across cycle offsets up to 10^12.
#[test]
fn adversarial_splits_partition_exactly() {
    let pats = [
        "[0 [1 [2 3 4 5 6 7 8]] 9 10 11 12 13 14 15 16 17]",
        "[0 1 2 3 4 5 6]*1.1 . [1 2 3 4 5 6 7 8 9 10 11]/1.3",
        "<0 1@3 [2 3]/7 4!2 _>/1.5",
        "{0 1 2 3 4 5 6, 1 2 3 4 5 6 7 8 9 10, 1 2 3 4 5 6 7 8 9 10 11 12 13}%11",
        "0(5,13,3)@2.5 1(7,11,-4)/3 2?0.37*13",
        "<0 <1 2 <3 4 5>>>*7%3 [0|1|2|3]*5?",
        "[0@1.1 1@2.3 2@0.7]*1.7",
        "<[0 1]/3 [2 3 4]*7>*11%13",
        "[0 1]/1.5 2",
    ];
    for src in pats {
        let p = Pattern::new(src).unwrap();
        for c in [-7i64, 0, 1, 999_999_937, 1_000_000_000_000] {
            for den in [44_100i64, 48_000, 7 * 11 * 13] {
                let b = Rational::int(c);
                let s = Span::new(b, b.checked_add(Rational::int(3)).unwrap());
                let whole = query(&p, s, 1);
                let mut pts: Vec<Rational> = whole
                    .iter()
                    .flat_map(|e| [e.whole.begin, e.whole.end])
                    .collect();
                let mut k = 0;
                loop {
                    let x = b.checked_add(r(k * 4099 + 1, den)).unwrap();
                    if x >= s.end {
                        break;
                    }
                    pts.push(x);
                    k += 1;
                }
                pts.retain(|x| *x > s.begin && *x < s.end);
                pts.extend([s.begin, s.end]);
                pts.sort();
                pts.dedup();
                let parts: Vec<Span> = pts.windows(2).map(|w| Span::new(w[0], w[1])).collect();
                check_split(&p, s, &parts, 1);
            }
        }
    }
}

#[test]
fn extreme_spans_never_panic() {
    let (m, n) = (i64::MAX, i64::MIN);
    let spans = [
        Span::new(Rational::int(n), Rational::int(n + 1)),
        Span::new(Rational::int(m - 1), Rational::int(m)),
        Span::new(r(n + 1, m), r(n + 2, m)),
        Span::new(r(m - 1, m), Rational::ONE),
        Span::new(r(-1, m), r(1, m)),
        Span::new(r(m - 2, 3), r(m, 3)),
        Span::new(r(n, 7), r(n + 7, 7)),
    ];
    for src in [
        "0",
        "0 1",
        "<0 1>",
        "[0|1]",
        "0?",
        "0(3,8,1)",
        "{0 1 2}%4",
        "0/3",
        "0*3",
        "0@2 1",
    ] {
        let p = Pattern::new(src).unwrap();
        for s in spans {
            let mut buf = [Event::EMPTY; 16];
            let res = p.query(s, 0, &mut buf);
            assert!(res.written <= 16);
        }
    }
}
