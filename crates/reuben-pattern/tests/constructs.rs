//! One test per construct, for the behaviour the crate docs commit to beyond the reference docs.

mod common;
use common::*;
use reuben_pattern::{Note, Pattern, Rational, Value, MAX_CYCLE, MAX_WORK};

fn sorted(mut xs: Vec<String>) -> Vec<String> {
    xs.sort();
    xs
}

fn v(xs: &[&str]) -> Vec<String> {
    sorted(xs.iter().map(|s| s.to_string()).collect())
}

#[test]
fn empty_and_silence() {
    assert_eq!(onsets("", cycles(0, 4)), v(&[]));
    assert_eq!(onsets("~ - ~", cycles(0, 4)), v(&[]));
    let mut buf = [Note::EMPTY; 4];
    let res = Pattern::silence().cycle(9, 0, &mut buf);
    assert_eq!(res.written, 0);
}

#[test]
fn values_are_exact_numbers_or_notes() {
    let p = Pattern::new("1.25 eb3").unwrap();
    let notes = query(&p, 0, 0);
    assert_eq!(notes[0].value, Value::Number(r(5, 4)));
    assert_eq!(notes[1].value, Value::Note(51));
}

#[test]
fn onsets_are_relative_to_their_cycle() {
    let p = Pattern::new("0 [1 2]").unwrap();
    for c in [-5, 0, 7, MAX_CYCLE, -MAX_CYCLE] {
        let notes = query(&p, c, 0);
        let got: Vec<(Rational, Rational)> = notes.iter().map(|n| (n.onset, n.release)).collect();
        assert_eq!(
            got,
            vec![
                (Rational::ZERO, r(1, 2)),
                (r(1, 2), r(3, 4)),
                (r(3, 4), Rational::ONE)
            ],
            "cycle {c}"
        );
    }
}

#[test]
fn slowed_notes_belong_to_their_onset_cycle() {
    // `0/3` is one note three cycles long: cycle 0 reports it, cycles 1 and 2 do not.
    let p = Pattern::new("0/3").unwrap();
    assert_eq!(
        query(&p, 0, 0),
        vec![Note {
            onset: Rational::ZERO,
            release: Rational::int(3),
            value: Value::Number(Rational::ZERO)
        }]
    );
    assert!(query(&p, 1, 0).is_empty() && query(&p, 2, 0).is_empty());
    assert_eq!(query(&p, 3, 0).len(), 1);
    // `[0 1 2]/1.5`: notes of 1/2 cycle starting at 0, 1/2, 1, 3/2, ... each reported once.
    let p = Pattern::new("[0 1 2]/1.5").unwrap();
    let starts: Vec<String> = (0..3)
        .flat_map(|c| {
            query(&p, c, 0)
                .into_iter()
                .map(move |n| format!("{}", Rational::int(c).checked_add(n.onset).unwrap()))
        })
        .collect();
    assert_eq!(starts, ["0", "1/2", "1", "3/2", "2", "5/2"]);
}

#[test]
fn release_is_cut_to_the_enclosing_step() {
    // `[0 1]/1.5 2`: the slowed `1` starts at 3/8 and would hold to 3/4, but its step ends at 1/2,
    // where `2` starts.
    let p = Pattern::new("[0 1]/1.5 2").unwrap();
    let notes = query(&p, 0, 0);
    let one = notes
        .iter()
        .find(|n| n.value == Value::Number(r(1, 1)))
        .unwrap();
    assert_eq!((one.onset, one.release), (r(3, 8), r(1, 2)));
    let two = notes
        .iter()
        .find(|n| n.value == Value::Number(r(2, 1)))
        .unwrap();
    assert_eq!((two.onset, two.release), (r(1, 2), Rational::ONE));
    // Cycle 1: the `1` whose event started in cycle 0 is not repeated; `0` starts at 5/4 - 1.
    let next = query(&p, 1, 0);
    assert_eq!(
        next.iter()
            .filter(|n| n.value == Value::Number(r(1, 1)))
            .count(),
        0
    );
    // `<0 1>/1.5 9`: every alternation note is cut where `9` starts, half way through its cycle.
    let p = Pattern::new("<0 1>/1.5 9").unwrap();
    for c in 0..12 {
        for n in query(&p, c, 0) {
            let step_end = if n.value == Value::Number(r(9, 1)) {
                Rational::ONE
            } else {
                r(1, 2)
            };
            assert!(
                n.onset < n.release && n.release <= step_end,
                "cycle {c}: {n:?}"
            );
        }
    }
    let zero = query(&p, 0, 0)
        .into_iter()
        .find(|n| n.value == Value::Number(Rational::ZERO))
        .unwrap();
    assert_eq!((zero.onset, zero.release), (Rational::ZERO, r(1, 2)));
}

#[test]
fn alternation_and_choice_are_per_cycle() {
    assert_eq!(
        onsets("<0 1 2>", cycles(0, 6)),
        v(&["0-1 0", "1-2 1", "2-3 2", "3-4 0", "4-5 1", "5-6 2"])
    );
    // `|` decides once per cycle: a choice between two sequences never mixes them, and both
    // branches turn up.
    let p = Pattern::new("[0 0 0] | [1 1 1]").unwrap();
    let mut seen = [false; 2];
    for c in 0..200 {
        let vals: Vec<Value> = query(&p, c, 4).into_iter().map(|n| n.value).collect();
        assert_eq!(vals.len(), 3);
        assert!(vals.iter().all(|x| *x == vals[0]), "cycle {c}: {vals:?}");
        seen[usize::from(vals[0] == Value::Number(Rational::ONE))] = true;
    }
    assert_eq!(seen, [true; 2]);
}

#[test]
fn degrade_is_deterministic_per_note() {
    let src = "[0?]!16";
    let a = onsets_seeded(src, cycles(0, 1), 5);
    let b = onsets_seeded(src, cycles(0, 1), 5);
    assert_eq!(a, b);
    assert!(!a.is_empty() && a.len() < 16, "{} of 16 kept", a.len());
}

#[test]
fn far_cycles_are_exact() {
    // `<0 1 2>` at cycle n plays n mod 3, for any cycle within the guaranteed range.
    let p = Pattern::new("<0 1 2> [3 4]*1.5").unwrap();
    for c in [
        MAX_CYCLE,
        MAX_CYCLE - 1,
        -MAX_CYCLE,
        1 << 30,
        -(1 << 30) - 7,
    ] {
        let notes = query(&p, c, 0);
        let alt = notes.iter().find(|n| n.onset == Rational::ZERO).unwrap();
        assert_eq!(alt.value, Value::Number(Rational::int(c.rem_euclid(3))));
    }
    // Even at the extremes of i64 nothing panics; out-of-range parts are counted, not wrapped.
    let mut buf = [Note::EMPTY; 16];
    for c in [i64::MIN, i64::MAX, i64::MAX - 1] {
        let res = p.cycle(c, 0, &mut buf);
        assert!(res.written <= 16);
    }
}

fn values_per_cycle(src: &str, c: i64, seed: u64) -> Vec<Value> {
    let p = Pattern::new(src).unwrap();
    query(&p, c, seed).into_iter().map(|n| n.value).collect()
}

#[test]
fn copies_and_pulses_draw_independently() {
    // Replicated, written out, and euclid pulses of one `|`: each step is its own coin flip.
    for src in ["[0|1]!2", "[0|1] [0|1]", "[0|1](2,2)"] {
        let same = (0..1000)
            .filter(|c| {
                let v = values_per_cycle(src, *c, 5);
                v[0] == v[1]
            })
            .count();
        assert!(
            (430..570).contains(&same),
            "{src}: same value in {same} of 1000 cycles"
        );
    }
    // Stack layers under one `?`: each layer is its own draw, as polymeter layers already were.
    for src in ["[0, 1]?", "[0(3,8), 1(3,8)]?"] {
        let p = Pattern::new(src).unwrap();
        let mut kept = [0usize; 3];
        for c in 0..2000 {
            let notes = query(&p, c, 7);
            let mut onsets: Vec<Rational> = notes.iter().map(|n| n.onset).collect();
            onsets.sort();
            onsets.dedup();
            for t in onsets {
                kept[notes.iter().filter(|n| n.onset == t).count()] += 1;
            }
        }
        // Of times with any onset kept, about a third keep both layers and two thirds one.
        let any = kept[1] + kept[2];
        assert!(
            kept[2] * 4 < any * 2 && kept[2] * 4 > any,
            "{src}: {kept:?}"
        );
    }
    // `?` inside copies: both kept about a quarter of the time.
    let both = (0..1000)
        .filter(|c| values_per_cycle("[0?]!2", *c, 5).len() == 2)
        .count();
    assert!(
        (190..310).contains(&both),
        "both copies kept in {both} of 1000 cycles"
    );
}

#[test]
fn atom_spans_each_cycle() {
    assert_eq!(
        sorted(onsets("5", cycles(0, 3))),
        v(&["0-1 5", "1-2 5", "2-3 5"])
    );
}

#[test]
fn nested_alternation_counts_its_own_turns() {
    assert_eq!(
        sorted(onsets("<0 <1 2>>", cycles(0, 4))),
        v(&["0-1 0", "1-2 1", "2-3 0", "3-4 2"])
    );
}

#[test]
fn weights_in_alternation_hold_for_cycles() {
    assert_same("<0@2 1>", "[0@2 1]/3", cycles(0, 6));
    assert_eq!(
        sorted(onsets("<0@2 1>", cycles(0, 3))),
        v(&["0-2 0", "2-3 1"])
    );
}

#[test]
fn bare_bang_adds_one_copy() {
    assert_same("0!", "0 0", cycles(0, 1));
    assert_same("0 !", "0 0", cycles(0, 1));
    assert_same("0!2", "0 0", cycles(0, 1));
    assert_same("0!3!", "0 0 0 0", cycles(0, 1));
    assert_same("0 ! ! 1", "0 0 0 1", cycles(0, 1));
}

#[test]
fn elongate_accumulates() {
    assert_same("0@2 _ 1", "0@3 1", cycles(0, 1));
}

#[test]
fn modifiers_wrap_in_written_order() {
    // `0*2(3,8)`: the sped-up step plays on each pulse. `0(3,8)*2`: the whole rhythm twice.
    assert_eq!(onsets("0*2(3,8)", cycles(0, 1)).len(), 6);
    assert_eq!(onsets("0(3,8)*2", cycles(0, 1)).len(), 6);
    assert_ne!(
        onsets("0*2(3,8)", cycles(0, 1)),
        onsets("0(3,8)*2", cycles(0, 1))
    );
    // `@` and `!` apply wherever they appear among a step's modifiers.
    assert_same("0@2*2 1", "0*2@2 1", cycles(0, 2));
    assert_same("0!2*2", "0*2!2", cycles(0, 2));
}

#[test]
fn euclid_negative_rotation_rotates_right() {
    assert_same("0(3,8,-1)", "0(3,8,7)", cycles(0, 2));
    assert_same("0(0,8)", "~", cycles(0, 2));
    assert_same("0(4,4)", "0*4", cycles(0, 2));
}

#[test]
fn polymeter_without_count_uses_first_layer() {
    assert_same("{0 1 2}", "0 1 2", cycles(0, 3));
    assert_same("{0 1, 2 3 4}", "[0 1, <[2 3] [4 2] [3 4]>]", cycles(0, 3));
}

#[test]
fn range_is_inclusive_and_either_direction() {
    assert_same("0 .. 3", "0 1 2 3", cycles(0, 1));
    assert_same("3..0", "3 2 1 0", cycles(0, 1));
    assert_same("-1 .. 1", "-1 0 1", cycles(0, 1));
    assert_same("[0 .. 2]*2", "[0 1 2]*2", cycles(0, 1));
}

#[test]
fn degrade_extremes() {
    assert_same("0*8?0", "0*8", cycles(0, 4));
    assert_same("0*8?1", "~", cycles(0, 4));
}

#[test]
fn choice_is_per_cycle_and_seeded() {
    let pick = |seed| -> Vec<String> {
        (0..32)
            .map(|c| onsets_seeded("0 | 1", cycles(c, c + 1), seed)[0].clone())
            .collect()
    };
    assert_eq!(pick(1), pick(1));
    assert_ne!(pick(1), pick(2));
}

#[test]
fn compile_limits_are_errors_with_spans() {
    let e = Pattern::new("0 [1*1048576]*2").unwrap_err();
    assert!(e.message.contains("too far"), "{e}");
    // The span of the `*1048576` that made the slot too fine.
    assert_eq!(e.span, 4..12);
    let deep = "0".to_string() + &"*1".repeat(9);
    assert!(Pattern::new(&deep)
        .unwrap_err()
        .message
        .contains("too many modifiers"));
}

#[test]
fn seed_high_bits_matter() {
    let draws = |seed: u64| -> Vec<Vec<Value>> {
        (0..64)
            .map(|c| values_per_cycle("[0|1|2] 3?", c, seed))
            .collect()
    };
    assert_ne!(draws(9), draws(9 ^ (1 << 32)));
    assert_ne!(draws(9), draws(9 ^ (1 << 63)));
}

#[test]
fn too_fine_errors_point_at_their_cause() {
    // A slot that is only too fine because of the speed above it: the error is the `/factor`.
    for (src, at) in [("[0 1]/1.000003", 5..14), ("[0 [1 2]]/1.000001", 9..18)] {
        let e = Pattern::new(src).unwrap_err();
        assert!(e.message.contains("too far for exact time"), "{src}: {e}");
        assert_eq!(e.span, at, "{src}");
    }
    // A weight whose denominator is too fine is not "too large", and the error is that step.
    let e = Pattern::new("[0@1.000001 1]").unwrap_err();
    assert_eq!(e.message, "step weights too finely divided for exact time");
    assert_eq!(e.span, 1..2);
    let e = Pattern::new("0@1048577").unwrap_err();
    assert!(e.message.contains("too large"), "{e}");
}

#[test]
fn compiled_size_cap() {
    // 448 bytes of text asking for 65,536 steps, inside every time and density limit.
    let src = format!("[{}]/64", "0!1024 ".repeat(64));
    let e = Pattern::new(&src).unwrap_err();
    assert_eq!(e.message, "pattern too large to compile");
    assert_eq!(e.span.start, 0);
    assert!(e.span.end < src.len());
    // The reviewer's 7 KB, 2^20-step text is refused before it allocates the steps.
    let layer = format!("[{}]", "0!1024 ".repeat(1024));
    assert!(Pattern::new(&layer).is_err());
    let ranges = "[0 .. 1023]/1024 ".repeat(80);
    assert!(Pattern::new(&ranges)
        .unwrap_err()
        .message
        .contains("too many steps"));
}

#[test]
fn offsets_from_fractional_weights_are_bounded_at_compile() {
    let src = "0@1%1048574 1@1%1048538 2@524286%1048574 3@524268%1048538";
    assert!(Pattern::new(src).is_err());
}

/// Patterns whose cycle query would do more work than a real-time budget allows.
const OVER_BUDGET: &[&str] = &[
    "[0 0]!512",
    "<0*1048576 ~!1023>",
    "[0*1048576 ~!1023]/1024",
    "{0*4096 ~ ~ ~}%1",
    "[~ ~]*262144 0",
    "[[~ ~]*524288, 0]",
    "[0*1*1*1*1*1*1*1*1]!1024",
    "0*200",
    "[0 1 2 3]*64",
    "[0*20?0?0?0?0?0?0?0]?0?0?0?0?0?0?0?0",
];

#[test]
fn work_cap_refuses_dense_patterns() {
    for src in OVER_BUDGET {
        let e = Pattern::new(src).unwrap_err();
        assert!(
            e.message.contains("too dense") || e.message.contains("too far"),
            "{src}: {e}"
        );
    }
    for src in [
        "0*128",
        "[0 1 2 3 4 5 6 7]*4",
        "<0 .. 1023>",
        "{0 1 2, 3 4 5 6 7}%16",
        "[0 [1 [2 [3 [4 [5 [6 [7 [8 [9 10]]]]]]]]]]",
        "<[e5 [b4 c5] d5 [c5 b4]] [a4 [a4 c5] e5 [d5 c5]]>, [[e2 e3]*4]",
    ] {
        let p = Pattern::new(src).unwrap_or_else(|e| panic!("{src}: {e}"));
        assert!(p.max_cycle_work() <= MAX_WORK, "{src}");
        for c in -3..40 {
            query(&p, c, 1);
        }
    }
}

#[test]
fn filter_chains_are_charged() {
    // Each `?` above an onset costs a unit per candidate, so deep chains count against the cap.
    let shallow = Pattern::new("0*16?0").unwrap().max_cycle_work();
    let deep = Pattern::new("0*16?0?0?0?0?0?0?0").unwrap().max_cycle_work();
    assert!(deep >= shallow + 6 * 16, "{shallow} vs {deep}");
    let p = Pattern::new("0*16?0?0?0?0?0?0?0").unwrap();
    let mut buf = [Note::EMPTY; 32];
    let res = p.cycle(0, 0, &mut buf);
    assert!(
        res.work <= p.max_cycle_work() && res.work > 16 * 7,
        "{res:?}"
    );
}

#[test]
fn clock_protocol_facts() {
    // A release can lie two cycles past its cycle's start: `0/1.5` in cycle 1 starts at 1/2 and
    // releases at 2.
    let p = Pattern::new("0/1.5").unwrap();
    let n = query(&p, 1, 0);
    assert_eq!((n[0].onset, n[0].release), (r(1, 2), Rational::int(2)));
    // Adjacent steps: each release equals the next onset, so a consumer releases before firing.
    for src in ["0 0", "0*4"] {
        let n = query(&Pattern::new(src).unwrap(), 0, 0);
        for w in n.windows(2) {
            assert_eq!(w[0].release, w[1].onset, "{src}");
        }
    }
    // The `MAX_WORK` examples, as documented.
    for (src, work) in [
        ("0*128", 130),
        ("[0,4,7,11]*16", 70),
        ("[0 1 2 3 4 5 6 7]*4", 106),
        ("[[0 1 2 3]*4]?0.3, [4 5]*8", 142),
    ] {
        assert_eq!(Pattern::new(src).unwrap().max_cycle_work(), work, "{src}");
    }
}
