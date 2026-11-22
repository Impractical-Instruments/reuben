//! Measures the query's cost per unit of work, which turns the compile-time work bound into time.
//!
//! `cargo run --release -p reuben-pattern --example query_cost`, pinned to one core on a shared
//! machine (`taskset -c 3 target/release/examples/query_cost`): scheduling noise otherwise poses
//! as a slow unit.
//!
//! Every compiled pattern's query over a span of `len` cycles spends at most
//! `Pattern::work_bound(len)` units, and at most `MAX_WORK` units over `MAX_QUERY_SPAN`. This
//! measures how long a unit takes in the worst cases found (fractional speeds and denominators
//! near `2^43`, which push reduction into 128-bit division), and prints the time bounds that
//! follow. 128-bit division on a Cortex-M is not measured here.

use reuben_pattern::{
    Event, Pattern, QueryResult, Rational, Span, MAX_QUERY_SPAN, MAX_SHORT_WORK, MAX_WORK,
    QUERY_BUDGET,
};
use std::time::Instant;

fn r(n: i64, d: i64) -> Rational {
    Rational::new(n, d).unwrap()
}

fn time(p: &Pattern, s: Span, buf: &mut [Event]) -> (f64, QueryResult) {
    let runs = 100;
    let mut res = QueryResult::default();
    let t = Instant::now();
    for _ in 0..runs {
        res = std::hint::black_box(p.query(std::hint::black_box(s), 1, buf));
    }
    (t.elapsed().as_secs_f64() * 1e6 / f64::from(runs), res)
}

fn main() {
    let patterns = [
        "0*1024",
        "[0 1 2 3]*64",
        "[0 [1 [2 [3 [4 [5 6]]]]]]*16",
        "{0 1 2, 3 4 5 6 7}%128",
        "[[0 1 2]*1.001 [3 4 5 6 7]/1.003]*7",
        "[0 1 2 3 4 5 6]*1.37 [0 1]*1.013",
        "<[0 1 2]*1.003 [3 4 5 6 7]/1.009>*13",
        "[0(5,13,3)*1.007]*9",
        "[0|1|2]*1.01 [3? 4?0.3]*1.03 <5 6 7>*1.07",
        "[[0 1]*1.00001]!8",
    ];
    let dens = [(1i64 << 43) - 1, (1i64 << 40) * 7, 48_000 * 7];
    let mut worst_ns = 0.0f64;
    let mut buf = vec![Event::EMPTY; 8192];
    println!("cost per unit over a {MAX_QUERY_SPAN}-cycle span:");
    for src in patterns {
        let p = match Pattern::new(src) {
            Ok(p) => p,
            Err(e) => {
                println!("  refused: {src}: {e}");
                continue;
            }
        };
        for den in dens {
            for c0 in [0i64, 1 << 16] {
                let start = Rational::int(c0).checked_add(r(den / 3, den)).unwrap();
                let len = MAX_QUERY_SPAN.checked_mul(r(den - 1, den)).unwrap();
                let s = Span::new(start, start.checked_add(len).unwrap());
                // The fastest of five runs: a cold cache or a busy machine does not pose as the
                // worst case, which is a property of the arithmetic.
                let (mut us, res) = time(&p, s, &mut buf);
                for _ in 0..4 {
                    us = us.min(time(&p, s, &mut buf).0);
                }
                assert!(!res.exhausted && u64::from(res.work) <= p.work_bound(len).unwrap());
                let ns = us * 1000.0 / f64::from(res.work.max(1));
                worst_ns = worst_ns.max(ns);
                println!(
                    "  {ns:6.0} ns/unit {us:8.1} us work={:5} den={den:<14} {src}",
                    res.work
                );
            }
        }
    }
    println!("worst cost per unit: {worst_ns:.0} ns");
    println!(
        "  query over {MAX_QUERY_SPAN} cycles: <= {MAX_WORK} units = {:.0} us",
        f64::from(MAX_WORK) * worst_ns / 1000.0
    );
    println!(
        "  query over {} cycles (a 64-frame block at 48 kHz is 0.022): <= {MAX_SHORT_WORK} units = {:.0} us of a 1333 us block",
        reuben_pattern::SHORT_QUERY_SPAN,
        f64::from(MAX_SHORT_WORK) * worst_ns / 1000.0
    );
    println!(
        "  whole budget (out-of-contract spans only): {QUERY_BUDGET} units = {:.0} us",
        f64::from(QUERY_BUDGET) * worst_ns / 1000.0
    );
}
