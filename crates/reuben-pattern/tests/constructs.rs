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
    assert!(!evs[0].has_onset());
    assert!(evs[1].has_onset());
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

fn values_per_cycle(src: &str, c: i64, seed: u64) -> Vec<Value> {
    let p = Pattern::new(src).unwrap();
    let mut evs: Vec<Event> = query(&p, cycles(c, c + 1), seed)
        .into_iter()
        .filter(Event::has_onset)
        .collect();
    evs.sort_by_key(|e| e.whole.begin);
    evs.into_iter().map(|e| e.value).collect()
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
fn density_cap() {
    assert!(Pattern::new("0*1024").is_ok());
    assert!(Pattern::new("[0*512, 1*512]").is_ok());
    // Slowing pays back density: 2048 events stretched over two cycles.
    assert!(Pattern::new("[0*2048]/2").is_ok());
    for src in [
        "0*1025",
        "[0*512, 1*513]",
        "[0 1 2 3]*257",
        "{0 1 2, 3 4}%1025",
    ] {
        let e = Pattern::new(src).unwrap_err();
        assert!(e.message.contains("events per cycle"), "{src}: {e}");
    }
}

#[test]
fn compiled_size_cap() {
    // 448 bytes of text asking for 65,536 steps, inside every time and density limit.
    let src = format!("[{}]/64", "0!1024 ".repeat(64));
    let e = Pattern::new(&src).unwrap_err();
    assert_eq!(e.message, "pattern too large to compile");
    assert_eq!(e.span.start, 1);
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
fn work_budget_stops_long_queries() {
    let p = Pattern::new("[0*256, 1*256, 2*256, 3*256]").unwrap();
    let mut buf = [Event::EMPTY; 16];
    let res = p.query(cycles(0, 1000), 0, &mut buf);
    assert!(res.exhausted, "{res:?}");
    assert!(res.written <= 16);
    // A 4096-frame block at 999 BPM, 4 beats per cycle, at the density cap does not exhaust.
    for src in [
        "0*1024",
        "[0 [1 [2 [3 [4 [5 6]]]]]]*146",
        "{0 1 2, 3 4 5 6 7}%512",
    ] {
        let p = Pattern::new(src).unwrap();
        let start = r(15_000 * 3 + 1, 3);
        let s = Span::new(
            start,
            start.checked_add(r(999 * 4096, 60 * 4 * 48_000)).unwrap(),
        );
        let mut buf = vec![Event::EMPTY; 1024];
        let res = p.query(s, 0, &mut buf);
        assert!(!res.exhausted, "{src}: {res:?}");
    }
}

#[test]
fn offsets_from_fractional_weights_are_bounded_at_compile() {
    let src = "0@1%1048574 1@1%1048538 2@524286%1048574 3@524268%1048538";
    assert!(Pattern::new(src).is_err());
}

#[test]
fn release_is_known_at_onset() {
    // Play block by block the way the Operator will: note-on at each onset fragment, note-off at
    // its `whole.end`, ignoring every other fragment. That yields the same notes as one query.
    for src in [
        "[0 1]/1.5 2",
        "0 3/2",
        "<0 1>/2 2",
        "<0@3 [1 2]/5>*2 0(3,8)/3",
    ] {
        let p = Pattern::new(src).unwrap();
        let notes = |evs: Vec<Event>| -> Vec<(Rational, Rational, Value)> {
            let mut n: Vec<_> = evs
                .into_iter()
                .filter(Event::has_onset)
                .map(|e| (e.whole.begin, e.whole.end, e.value))
                .collect();
            n.sort_by_key(|(b, e, _)| (*b, *e));
            n
        };
        let whole = notes(query(&p, cycles(0, 6), 0));
        let mut blocks = Vec::new();
        for k in 0..6 * 97 {
            blocks.extend(query(&p, Span::new(r(k, 97), r(k + 1, 97)), 0));
        }
        let played = notes(blocks);
        assert_eq!(played, whole, "{src}");
        assert!(played.iter().all(|(b, e, _)| b < e));
    }
}
