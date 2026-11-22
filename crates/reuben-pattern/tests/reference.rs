//! The examples of the two reference docs, with the events their prose implies.
//!
//! Sources:
//! - **T** — Tidal, "Mini notation" reference, tidalcycles.org/docs/reference/mini_notation; the
//!   section is named after the symbol.
//! - **S** — Strudel, "Mini-Notation", strudel.cc/learn/mini-notation; the section is named.
//!
//! v1 atoms are numbers and note names, so the docs' sample names are renamed to numbers:
//! bd=0 sd=1 hh=2 cp=3 stab=4 arpy=5 cr=6 superpiano=7 sn=8. Notes are MIDI numbers (`n60` = c4).
//! Notes are written `"onset-release value"` in absolute cycles, sorted.

mod common;
use common::*;

fn v(xs: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = xs.iter().map(|s| s.to_string()).collect();
    out.sort();
    out
}

fn sorted(mut xs: Vec<String>) -> Vec<String> {
    xs.sort();
    xs
}

// ---- Tidal reference ----

#[test]
fn t_rest() {
    // T, "~": `"~ hh"`.
    assert_eq!(onsets("~ 2", cycles(0, 1)), v(&["1/2-1 2"]));
}

#[test]
fn t_grouping() {
    // T, "[ ]": `"[bd sd] hh"`.
    assert_eq!(
        sorted(onsets("[0 1] 2", cycles(0, 1))),
        v(&["0-1/4 0", "1/4-1/2 1", "1/2-1 2"])
    );
}

#[test]
fn t_dot_shorthand() {
    // T, ".": `"bd sd . hh hh hh"` is `"[bd sd] [hh hh hh]"`.
    assert_same("0 1 . 2 2 2", "[0 1] [2 2 2]", cycles(0, 2));
    // T, ".": `"bd*3 . hh*4 cp"` and `"[bd*3 . hh:2*2] . hh*4 cp"` (sample index dropped).
    assert_same("0*3 . 2*4 3", "[0*3] [2*4 3]", cycles(0, 2));
    assert_same("[0*3 . 2*2] . 2*4 3", "[[0*3] [2*2]] [2*4 3]", cycles(0, 2));
}

#[test]
fn t_stack() {
    // T, ",": `"[bd sd, hh hh hh]"`.
    assert_eq!(
        sorted(onsets("[0 1, 2 2 2]", cycles(0, 1))),
        v(&["0-1/2 0", "1/2-1 1", "0-1/3 2", "1/3-2/3 2", "2/3-1 2"])
    );
}

#[test]
fn t_repeat() {
    // T, "*": `"bd*2 sd"` is `"[bd bd] sd"`; `"cp cp cp*2"`.
    assert_same("0*2 1", "[0 0] 1", cycles(0, 2));
    assert_eq!(
        sorted(onsets("3 3 3*2", cycles(0, 1))),
        v(&["0-1/3 3", "1/3-2/3 3", "2/3-5/6 3", "5/6-1 3"])
    );
}

#[test]
fn t_slow() {
    // T, "/": `"bd/2"` is `slow 2 "bd"`: one event two cycles long.
    assert_eq!(onsets("0/2", cycles(0, 1)), v(&["0-2 0"]));
    assert_eq!(onsets("0/2", cycles(1, 2)), v(&[]));
    assert_eq!(onsets("0/2", cycles(2, 3)), v(&["2-4 0"]));
    // T, "/": `"bd cp/2"`, the clap "will only be heard every other cycle". Its event would hold
    // a whole cycle, but a note is cut at the end of its step (see `Note`).
    assert_eq!(onsets("0 3/2", cycles(0, 1)), v(&["0-1/2 0", "1/2-1 3"]));
    assert_eq!(onsets("0 3/2", cycles(1, 2)), v(&["1-3/2 0"]));
    assert_eq!(onsets("0 3/2", cycles(2, 3)), v(&["2-5/2 0", "5/2-3 3"]));
}

#[test]
fn t_random_choice() {
    // T, "|": `"[bd*4|hh*12|cp*2]"`: one branch, chosen per cycle with equal probability.
    let mut seen = [0; 3];
    for c in 0..600 {
        let got = onsets("[0*4|2*12|3*2]", cycles(c, c + 1));
        let idx = match got.len() {
            4 => 0,
            12 => 1,
            2 => 2,
            n => panic!("cycle {c}: {n} events is no single branch"),
        };
        let value = ["0", "2", "3"][idx];
        assert!(got.iter().all(|e| e.ends_with(&format!(" {value}"))));
        seen[idx] += 1;
    }
    for n in seen {
        assert!(
            (150..250).contains(&n),
            "branch counts {seen:?} not near 200 each"
        );
    }
}

#[test]
fn t_alternate() {
    // T, "< >": `"bd <sd hh cp>"` is `slow 3 "bd sd bd hh bd cp"`.
    assert_same("0 <1 2 3>", "[0 1 0 2 0 3]/3", cycles(0, 6));
    // T, "< >": `fast 2 $ n "<c e g>"`, written here as `*2`.
    assert_eq!(
        sorted(onsets("<c e g>*2", cycles(0, 2))),
        v(&["0-1/2 n60", "1/2-1 n64", "1-3/2 n67", "3/2-2 n60"])
    );
    // T, "< >": `"bd <[hh sn] [hh cp]>"`.
    assert_eq!(
        sorted(onsets("0 <[2 8] [2 3]>", cycles(0, 2))),
        v(&[
            "0-1/2 0",
            "1/2-3/4 2",
            "3/4-1 8",
            "1-3/2 0",
            "3/2-7/4 2",
            "7/4-2 3"
        ])
    );
}

#[test]
fn t_replicate() {
    // T, "!": `"bd!3 sd"` is `"bd bd bd sd"`; `"bd!2 cp!2"` has four steps; `"bd!2 cp*2"` three.
    assert_same("0!3 1", "0 0 0 1", cycles(0, 2));
    assert_same("0!2 3!2", "0 0 3 3", cycles(0, 2));
    assert_same("0!2 3*2", "0 0 [3 3]", cycles(0, 2));
}

#[test]
fn t_elongate_underscore() {
    // T, "_": `"bd _ _ ~ sd _"` gives bd at (0>1/2) and sd at (4/6>1).
    assert_eq!(
        sorted(onsets("0 _ _ ~ 1 _", cycles(0, 1))),
        v(&["0-1/2 0", "2/3-1 1"])
    );
    // T, "_": `"bd _ _ hh*4"`.
    assert_same("0 _ _ 2*4", "0@3 2*4", cycles(0, 2));
}

#[test]
fn t_elongate_at() {
    // T, "@": `"superpiano@3 superpiano"` is `"superpiano _ _ superpiano"`.
    assert_same("7@3 7", "7 _ _ 7", cycles(0, 2));
    assert_eq!(
        sorted(onsets("7@3 7", cycles(0, 1))),
        v(&["0-3/4 7", "3/4-1 7"])
    );
}

#[test]
fn t_degrade() {
    // T, "?": `"bd? sd"` and `"bd hh? bd hh?0.8"`: default probability 1/2, or the number given.
    let mut kept = [0; 2];
    for c in 0..1000 {
        let got = onsets("0 2? 0 2?0.8", cycles(c, c + 1));
        assert_eq!(got.iter().filter(|e| e.ends_with(" 0")).count(), 2);
        let at = |k: i64| {
            format!(
                "{}-{} 2",
                common::r(4 * c + k, 4),
                common::r(4 * c + k + 1, 4)
            )
        };
        kept[0] += usize::from(got.contains(&at(1)));
        kept[1] += usize::from(got.contains(&at(3)));
    }
    assert!(
        (430..570).contains(&kept[0]),
        "?    kept {} of 1000",
        kept[0]
    );
    assert!(
        (140..260).contains(&kept[1]),
        "?0.8 kept {} of 1000",
        kept[1]
    );
}

#[test]
fn t_euclid() {
    // T, "( )": the printed grids for (3,8), (3,8,1), (3,8,2), (3,8,3).
    let grid = |src: &str| -> String {
        let got = onsets(src, cycles(0, 1));
        (0..8)
            .map(|i| {
                let at = format!("{}-{} 0", common::r(i, 8), common::r(i + 1, 8));
                if got.contains(&at) {
                    'x'
                } else {
                    '~'
                }
            })
            .collect()
    };
    assert_eq!(grid("0(3,8)"), "x~~x~~x~");
    assert_eq!(grid("0(3,8,1)"), "~~x~~x~x");
    assert_eq!(grid("0(3,8,2)"), "~x~~x~x~");
    assert_eq!(grid("0(3,8,3)"), "x~~x~x~~");
}

#[test]
fn t_polymeter() {
    // T, "{ }": `"{bd bd bd bd, cp cp hh}"`: the first layer sets four steps per cycle.
    assert_eq!(
        sorted(onsets("{0 0 0 0, 3 3 2}", cycles(1, 2))),
        v(&[
            "1-5/4 0",
            "5/4-3/2 0",
            "3/2-7/4 0",
            "7/4-2 0",
            "1-5/4 3",
            "5/4-3/2 2",
            "3/2-7/4 3",
            "7/4-2 3",
        ])
    );
    // T, "{ }": `"{bd sd stab, cp arpy cr arpy}"`: base 3, the four-step layer wraps.
    assert_same(
        "{0 1 4, 3 5 6 5}",
        "[0 1 4, <[3 5 6] [5 3 5] [6 5 3] [5 6 5]>]",
        cycles(0, 4),
    );
    // T, "{ }%": `"{bd cp hh}%8"` is `"bd cp hh bd cp hh bd cp"` (its first cycle).
    assert_same("{0 3 2}%8", "0 3 2 0 3 2 0 3", cycles(0, 1));
    assert_same("{0 3 2}%8", "2 0 3 2 0 3 2 0", cycles(1, 2));
}

#[test]
fn t_ratio() {
    // T, "%": `"bd*4%2"` is `"bd*2"`.
    assert_same("0*4%2", "0*2", cycles(0, 2));
    assert_same("6%3", "2", cycles(0, 1));
}

// ---- Strudel guide ----

#[test]
fn s_sequences() {
    // S, "Sequences": `note("c e g b")`, `note("c d e f g a b")`.
    assert_eq!(
        sorted(onsets("c e g b", cycles(0, 1))),
        v(&["0-1/4 n60", "1/4-1/2 n64", "1/2-3/4 n67", "3/4-1 n71"])
    );
    assert_eq!(onsets("c d e f g a b", cycles(0, 1)).len(), 7);
}

#[test]
fn s_subdivision() {
    // S, "Division": `note("e5 [b4 c5] d5 c5 b4")`.
    assert_eq!(
        sorted(onsets("e5 [b4 c5] d5 c5 b4", cycles(0, 1))),
        v(&[
            "0-1/5 n76",
            "1/5-3/10 n71",
            "3/10-2/5 n72",
            "2/5-3/5 n74",
            "3/5-4/5 n72",
            "4/5-1 n71",
        ])
    );
}

#[test]
fn s_speed() {
    // S, "Multiplication": `[e5 b4 d5 c5]*2`, and decimals such as `*2.75`.
    assert_same("[e5 b4 d5 c5]*2", "e5 b4 d5 c5 e5 b4 d5 c5", cycles(0, 2));
    assert_eq!(onsets("[e5 b4 d5 c5]*2.75", cycles(0, 4)).len(), 44);
    // S, "Division": `[e5 b4 d5 c5]/2` spreads over two cycles.
    assert_eq!(
        sorted(onsets("[e5 b4 d5 c5]/2", cycles(1, 2))),
        v(&["1-3/2 n74", "3/2-2 n72"])
    );
}

#[test]
fn s_alternation() {
    // S, "Angle Brackets": `<e5 b4 d5 c5>`, one per cycle; with `*8`, eight per cycle.
    assert_eq!(onsets("<e5 b4 d5 c5>", cycles(2, 3)), v(&["2-3 n74"]));
    assert_same(
        "<e5 b4 d5 c5 a4 c5>*8",
        "e5 b4 d5 c5 a4 c5 e5 b4",
        cycles(0, 1),
    );
    assert_same(
        "<e5 b4 d5 c5 a4 c5>*8",
        "d5 c5 a4 c5 e5 b4 d5 c5",
        cycles(1, 2),
    );
}

#[test]
fn s_rests() {
    // S, "Rests": `-` means the same as `~`.
    assert_same("c - e", "c ~ e", cycles(0, 1));
}

#[test]
fn s_chords() {
    // S, "Parallel / Chords": `[g3,b3,e4]` and the bare `g3,b3,e4`.
    let chord = v(&["0-1 n55", "0-1 n59", "0-1 n64"]);
    assert_eq!(sorted(onsets("[g3,b3,e4]", cycles(0, 1))), chord);
    assert_eq!(sorted(onsets("g3,b3,e4", cycles(0, 1))), chord);
    // `<[g3,b3,e4] [a3,c3,e4]>*2`: two chords per cycle.
    assert_eq!(
        sorted(onsets("<[g3,b3,e4] [a3,c3,e4]>*2", cycles(0, 1))),
        v(&[
            "0-1/2 n55",
            "0-1/2 n59",
            "0-1/2 n64",
            "1/2-1 n57",
            "1/2-1 n48",
            "1/2-1 n64"
        ])
    );
}

#[test]
fn s_elongation_in_alternation() {
    // S, "Elongation": `<[g3,b3,e4]@2 [a3,c3,e4] [b3,d3,f#4]>*2`: the first chord twice as long.
    let src = "<[g3,b3,e4]@2 [a3,c3,e4] [b3,d3,f#4]>*2";
    assert_eq!(
        sorted(onsets(src, cycles(0, 2))),
        v(&[
            "0-1 n55",
            "0-1 n59",
            "0-1 n64",
            "1-3/2 n57",
            "1-3/2 n48",
            "1-3/2 n64",
            "3/2-2 n59",
            "3/2-2 n50",
            "3/2-2 n66",
        ])
    );
    // S, "Mini-notation review": the `_` form is the same.
    assert_same(src, "<[g3,b3,e4] _ [a3,c3,e4] [b3,d3,f#4]>*2", cycles(0, 4));
}

#[test]
fn s_replication_in_alternation() {
    // S, "Replication": `<[g3,b3,e4]!2 [a3,c3,e4] [b3,d3,f#4]>*2` repeats without speeding up.
    assert_same(
        "<[g3,b3,e4]!2 [a3,c3,e4] [b3,d3,f#4]>*2",
        "<[g3,b3,e4] [g3,b3,e4] [a3,c3,e4] [b3,d3,f#4]>*2",
        cycles(0, 4),
    );
    assert_eq!(
        onsets("<[g3,b3,e4]!2 [a3,c3,e4] [b3,d3,f#4]>*2", cycles(0, 1)).len(),
        6
    );
}

#[test]
fn s_degrade() {
    // S, "Randomness": `[g3,b3,e4]*8?` removes with 50% chance; `?0.1` with 10%.
    let count =
        |src: &str| -> usize { (0..200).map(|c| onsets(src, cycles(c, c + 1)).len()).sum() };
    let half = count("[g3,b3,e4]*8?");
    let tenth = count("[g3,b3,e4]*8?0.1");
    assert!((2160..2640).contains(&half), "?    kept {half} of 4800");
    assert!((4130..4510).contains(&tenth), "?0.1 kept {tenth} of 4800");
}

#[test]
fn s_random_choice_top_level() {
    // S, "Randomness": `[g3,b3,e4] | [a3,c3,e4] | [b3,d3,f#4]`, chosen at random per cycle.
    let chords = [
        v(&["0-1 n55", "0-1 n59", "0-1 n64"]),
        v(&["0-1 n48", "0-1 n57", "0-1 n64"]),
        v(&["0-1 n50", "0-1 n59", "0-1 n66"]),
    ];
    let mut seen = [false; 3];
    for c in 0..60 {
        let got: Vec<String> = sorted(onsets(
            "[g3,b3,e4] | [a3,c3,e4] | [b3,d3,f#4]",
            cycles(c, c + 1),
        ))
        .into_iter()
        .map(|e| e.replacen(&format!("{c}-{}", c + 1), "0-1", 1))
        .collect();
        let idx = chords.iter().position(|ch| *ch == sorted(got.clone()));
        seen[idx.unwrap_or_else(|| panic!("cycle {c}: {got:?} is no single chord"))] = true;
    }
    assert_eq!(seen, [true; 3]);
}

#[test]
fn s_euclid() {
    // S, "Euclidian rhythms": `bd(3,8)` is `bd(3,8,0)`; `bd(3,8,3), hh cp` shifts against `hh cp`.
    assert_same("0(3,8)", "0(3,8,0)", cycles(0, 2));
    assert_eq!(
        sorted(onsets("0(3,8,3), 2 3", cycles(0, 1))),
        v(&["0-1/8 0", "3/8-1/2 0", "5/8-3/4 0", "0-1/2 2", "1/2-1 3"])
    );
}

#[test]
fn s_review_slow_in_alternation() {
    // S, "Mini-notation review": `<[g3,b3,e4]/2 [a3,c3,e4] [b3,d3,f#4]>*2`. The first chord is
    // slowed inside its half-cycle slot, so it sounds on every other turn; its event would hold a
    // full cycle, but the note is cut at the end of its half-cycle step.
    let got = onsets("<g3/2 a3 b3>*2", cycles(0, 6));
    assert_eq!(
        sorted(got),
        v(&[
            "0-1/2 n55",
            "3-7/2 n55",
            "1/2-1 n57",
            "2-5/2 n57",
            "7/2-4 n57",
            "5-11/2 n57",
            "1-3/2 n59",
            "5/2-3 n59",
            "4-9/2 n59",
            "11/2-6 n59",
        ])
    );
}

#[test]
fn s_review_choice_in_alternation() {
    // S, "Mini-notation review": `<[g3|b3|e4] [a3,c3,e4] [b3,d3,f#4]>*2`.
    for c in 0..30 {
        let got = onsets("<[g3|b3|e4] [a3,c3,e4] [b3,d3,f#4]>*2", cycles(c, c + 1));
        let n = got.len();
        assert!(n == 4 || n == 6, "cycle {c}: {n} events");
    }
}

#[test]
fn s_review_alternation_of_five() {
    // S, "Mini-notation review": `<g3 b3 e4 [a3,c3,e4] [b3,d3,f#4]>*2`.
    assert_same(
        "<g3 b3 e4 [a3,c3,e4] [b3,d3,f#4]>*2",
        "[g3 b3 e4 [a3,c3,e4] [b3,d3,f#4]]*2/5",
        cycles(0, 5),
    );
}
