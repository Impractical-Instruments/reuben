//! One test per construct, for the behaviour the crate docs commit to beyond the reference docs.

mod common;
use common::*;
use reuben_pattern::{Event, Pattern, Rational, Span, Value};

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
    let mut buf = [Event::EMPTY; 4];
    let res = Pattern::silence().query(cycles(0, 9), 0, &mut buf);
    assert_eq!(res.written, 0);
}

#[test]
fn atom_spans_each_cycle() {
    assert_eq!(
        sorted(onsets("5", cycles(0, 3))),
        v(&["0-1 5", "1-2 5", "2-3 5"])
    );
}

#[test]
fn values_are_exact_numbers_or_notes() {
    let p = Pattern::new("1.25 eb3").unwrap();
    let evs = query(&p, cycles(0, 1), 0);
    assert_eq!(evs[0].value, Value::Number(r(5, 4)));
    assert_eq!(evs[1].value, Value::Note(51));
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
fn degrade_is_keyed_on_onset_not_query() {
    // Same seed, same events however the span is cut; replicated copies decide independently.
    let src = "[0?]!16";
    let whole = onsets_seeded(src, cycles(0, 1), 5);
    let mut halves = onsets_seeded(src, Span::new(Rational::ZERO, r(1, 2)), 5);
    halves.extend(onsets_seeded(src, Span::new(r(1, 2), Rational::ONE), 5));
    assert_eq!(sorted(whole.clone()), sorted(halves));
    assert!(
        !whole.is_empty() && whole.len() < 16,
        "{} of 16 kept",
        whole.len()
    );
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
fn whole_and_part_across_a_query_edge() {
    let p = Pattern::new("0@3 1").unwrap();
    let evs = query(&p, Span::new(r(1, 2), Rational::ONE), 0);
    assert_eq!(evs.len(), 2);
    assert_eq!(evs[0].whole, Span::new(Rational::ZERO, r(3, 4)));
    assert_eq!(evs[0].part, Span::new(r(1, 2), r(3, 4)));
    assert!(!evs[0].has_onset() && evs[0].has_offset());
    assert!(evs[1].has_onset() && evs[1].has_offset());
}

#[test]
fn empty_or_reversed_span_yields_nothing() {
    let p = Pattern::new("0 1 2").unwrap();
    let mut buf = [Event::EMPTY; 8];
    assert_eq!(
        p.query(Span::new(Rational::ONE, Rational::ONE), 0, &mut buf)
            .written,
        0
    );
    assert_eq!(
        p.query(Span::new(Rational::ONE, Rational::ZERO), 0, &mut buf)
            .written,
        0
    );
}

#[test]
fn compile_limits_are_errors_with_spans() {
    let e = Pattern::new("0 [1*1048576]*2").unwrap_err();
    assert!(e.message.contains("too far"), "{e}");
    assert_eq!(e.span, 3..12);
    let deep = "0".to_string() + &"*1".repeat(9);
    assert!(Pattern::new(&deep)
        .unwrap_err()
        .message
        .contains("too many modifiers"));
}
