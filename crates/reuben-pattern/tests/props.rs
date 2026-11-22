//! Property tests: determinism, notes reported exactly once across cycles, the work bound,
//! overflow freedom across the guaranteed cycle range, releases, and hostile input.
//!
//! The hostile-input properties double as the in-CI fuzz target; `fuzz/` runs the same body under
//! libFuzzer for longer campaigns.

mod common;
use common::*;
use proptest::prelude::*;
use reuben_pattern::{parse, Note, Pattern, Rational, MAX_WORK};

/// The notes of `p` over cycles `cs` as `(absolute onset, absolute release, value)`, scaled by `k`.
fn absolute(
    p: &Pattern,
    cs: std::ops::Range<i64>,
    k: Rational,
) -> Vec<(Rational, Rational, reuben_pattern::Value)> {
    let mut out = Vec::new();
    for c in cs {
        for n in query(p, c, 3) {
            let at = |x: Rational| {
                Rational::int(c)
                    .checked_add(x)
                    .unwrap()
                    .checked_mul(k)
                    .unwrap()
            };
            out.push((at(n.onset), at(n.release), n.value));
        }
    }
    out.sort_by_key(|(o, r, v)| (*o, *r, format!("{v:?}")));
    out
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn cycle_query_is_deterministic(src in pattern_src(), c in any_cycle(), seed in any::<u64>()) {
        let Some(a) = generated(&src) else { return Ok(()) };
        let b = Pattern::new(&src).unwrap();
        let mut x = vec![Note::EMPTY; 4096];
        let mut y = vec![Note::EMPTY; 4096];
        let rx = a.cycle(c, seed, &mut x);
        let ry = b.cycle(c, seed, &mut y);
        prop_assert_eq!(rx, ry);
        prop_assert_eq!(&x[..rx.written], &y[..ry.written]);
    }

    /// Every compiled pattern stays within its work bound, never exhausts the budget, and never
    /// overflows, at any cycle within `MAX_CYCLE`.
    #[test]
    fn work_and_overflow_contract(src in pattern_src(), c in any_cycle(), seed in any::<u64>()) {
        let Some(p) = generated(&src) else { return Ok(()) };
        prop_assert!(p.max_cycle_work() <= MAX_WORK);
        let mut buf = vec![Note::EMPTY; 64];
        let res = p.cycle(c, seed, &mut buf);
        prop_assert!(!res.exhausted, "{src}: {res:?}");
        prop_assert_eq!(res.overflowed, 0, "{} at cycle {}", src, c);
        prop_assert!(res.work <= p.max_cycle_work(), "{src}: {res:?}");
    }

    /// Notes lie in their cycle, release after they start, and a note never sounds past its step:
    /// in `[A] [B]`, a note starting in the first half releases by the half cycle.
    #[test]
    fn notes_lie_in_their_cycle_and_step(a in pattern_src(), b in pattern_src(), c in any_cycle()) {
        let src = format!("[{a}] [{b}]");
        let Some(p) = generated(&src) else { return Ok(()) };
        let half = r(1, 2);
        for n in query(&p, c, 0) {
            prop_assert!(Rational::ZERO <= n.onset && n.onset < Rational::ONE, "{n:?}");
            prop_assert!(n.onset < n.release, "{n:?}");
            let step_end = if n.onset < half { half } else { Rational::ONE };
            prop_assert!(n.release <= step_end, "{src}: {n:?}");
        }
    }

    /// Each note is reported exactly once, by the cycle its onset lies in, however it straddles
    /// cycle boundaries: `[P]/2` over cycles `2m` and `2m + 1` is `P` over cycle `m` stretched,
    /// and `[P]*2` over cycle `m` is `P` over cycles `2m` and `2m + 1` squeezed. (`?` is stripped:
    /// it draws on the note's absolute onset, which stretching moves.)
    #[test]
    fn notes_are_reported_once_across_cycles(src in pattern_src(), m in -1000i64..1000) {
        let src = src.replace("?0.3", "").replace('?', "");
        let Some(p) = generated(&src) else { return Ok(()) };
        if let Some(slow) = generated(&format!("[{src}]/2")) {
            prop_assert_eq!(absolute(&slow, 2 * m..2 * m + 2, Rational::ONE), absolute(&p, m..m + 1, Rational::int(2)));
        }
        if let Some(fast) = generated(&format!("[{src}]*2")) {
            prop_assert_eq!(absolute(&fast, m..m + 1, Rational::int(2)), absolute(&p, 2 * m..2 * m + 2, Rational::ONE));
        }
    }

    #[test]
    fn small_buffer_truncates_to_a_prefix(src in pattern_src(), c in any_cycle(), cap in 0usize..8) {
        let Some(p) = generated(&src) else { return Ok(()) };
        let mut big = vec![Note::EMPTY; 4096];
        let full = p.cycle(c, 3, &mut big);
        prop_assert_eq!(full.dropped, 0);
        let mut small = vec![Note::EMPTY; cap];
        let res = p.cycle(c, 3, &mut small);
        prop_assert_eq!(res.written, full.written.min(cap));
        prop_assert_eq!(res.written + res.dropped, full.written);
        prop_assert_eq!(&small[..res.written], &big[..res.written]);
    }

    /// The in-CI fuzz target: arbitrary text never panics the parser, and whatever compiles
    /// never panics a query or writes past the buffer.
    #[test]
    fn hostile_input_never_panics(
        src in "[0-9a-g~\\-_.,|!@?*/%:()<>\\[\\]{} #sbx\"é]{0,64}",
        c in any::<i64>(),
        cap in 0usize..64,
    ) {
        fuzz_body(&src, c, cap);
    }

    #[test]
    fn arbitrary_bytes_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..96), c in any::<i64>()) {
        if let Ok(src) = std::str::from_utf8(&bytes) {
            fuzz_body(src, c, 16);
        }
    }
}

fn fuzz_body(src: &str, c: i64, cap: usize) {
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
                let mut buf = vec![Note::EMPTY; cap];
                let res = p.cycle(c, 0, &mut buf);
                assert!(res.written <= cap);
                assert!(!res.exhausted && res.work <= p.max_cycle_work());
            }
        }
    }
}

#[test]
fn generator_reaches_deep_nesting_and_mostly_compiles() {
    // The properties skip generated patterns that trip a limit; that must stay the exception, and
    // the generator must reach far enough to exercise the depth limits.
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let mut runner = TestRunner::deterministic();
    let patterns = pattern_src();
    let total = 500;
    let (mut compiled, mut deepest) = (0, 0);
    for _ in 0..total {
        let src = patterns.new_tree(&mut runner).unwrap().current();
        let (mut depth, mut max) = (0i32, 0);
        for ch in src.chars() {
            match ch {
                '[' | '<' | '{' => {
                    depth += 1;
                    max = max.max(depth);
                }
                ']' | '>' | '}' => depth -= 1,
                _ => {}
            }
        }
        if generated(&src).is_some() {
            compiled += 1;
            deepest = deepest.max(max);
        }
    }
    assert!(
        compiled * 10 >= total * 6,
        "only {compiled} of {total} generated patterns compiled"
    );
    assert!(
        deepest >= 8,
        "compiled patterns only reached nesting {deepest}"
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
    fn hand_built_asts_never_panic(root in arb_expr(), c in any::<i64>(), cap in 0usize..32) {
        if let Ok(p) = reuben_pattern::compile(&reuben_pattern::Ast { root }) {
            let mut buf = vec![Note::EMPTY; cap];
            let res = p.cycle(c, 0, &mut buf);
            prop_assert!(res.written <= cap);
            prop_assert!(!res.exhausted);
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

#[test]
fn doc_strings_parse() {
    // Every example string on the two reference pages that v1 accepts, sample names as numbers,
    // including Strudel's Tetris theme.
    let tetris = "<\n[e5 [b4 c5] d5 [c5 b4]]\n[a4 [a4 c5] e5 [d5 c5]]\n[b4 [~ c5] d5 e5]\n[c5 a4 a4 ~]\n[[~ d5] [~ f5] a5 [g5 f5]]\n[e5 [~ c5] e5 [d5 c5]]\n[b4 [b4 c5] d5 e5]\n[c5 a4 a4 ~]\n,\n[[e2 e3]*4]\n[[a2 a3]*4]\n[[g#2 g#3]*2 [e2 e3]*2]\n[a2 a3 a2 a3 a2 a3 b1 c2]\n[[d2 d3]*4]\n[[c2 c3]*4]\n[[b1 b2]*2 [e2 e3]*2]\n[[a1 a2]*4]\n>";
    for s in [
        "[e5 [b4 c5] d5 [c5 b4]]*2",
        "[e5 b4 d5 c5]/2.75",
        "[b4 [~ c5] d5 e5]",
        "g3,b3,e4",
        "<[g3,b3,e4] [a3,c3,e4] [b3,d3,f#4] [b3,e4,g4]>*2",
        "<[g3,b3,e4]@2 [a3,c3,e4] [b3,d3,f#4]>*2",
        "<[g3,b3,e4]!2 [a3,c3,e4] [b3,d3,f#4]>*2",
        "[g3,b3,e4]*8?",
        "[g3,b3,e4]*8?0.1",
        "[g3,b3,e4] | [a3,c3,e4] | [b3,d3,f#4]",
        "<[g3,b3,e4]/2 [a3,c3,e4] [b3,d3,f#4]>*2",
        "<[g3,b3,e4] _ [a3,c3,e4] [b3,d3,f#4]>*2",
        "<[g3,b3,e4]? [a3,c3,e4] [b3,d3,f#4]>*2",
        "<[g3|b3|e4] [a3,c3,e4] [b3,d3,f#4]>*2",
        "c(3,8,0)",
        "e5(2,8) b4(3,8) d5(2,8) c5(3,8)",
        "c(3,8,5), d e",
        tetris,
        "[[1*3],[1 2 3 4*3]]",
        "[1*2,2*3,[~ 3]*2, 4]",
        "[1*3 . 2*2] . 2*4 3",
        "[1 [2 [3 4] 2]] 1 1 1",
        "[1(5,8), 2(1,8)?, 3(7,8), 4(8,16)]",
        "{1*2, 2*4, 1 2 3}",
        "[1*4%2]",
        "1 2? 1 2?0.8",
        "[1*4|2*12|3*2]",
        "[1|2|3]",
        "{1 1 1 1, 2 2 3}",
        "1*4%2",
        "{1 2 3}%8",
    ] {
        let p = Pattern::new(s).unwrap_or_else(|e| panic!("{s:?}: {e}"));
        for c in -2..20 {
            query(&p, c, 0);
        }
    }
}
