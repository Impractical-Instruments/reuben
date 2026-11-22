//! Measures the query's worst cases, which `MAX_DENSITY` and `QUERY_BUDGET` are set from.
//!
//! `cargo run --release -p reuben-pattern --example query_cost`
//!
//! Two figures: the time of one block-sized query against patterns at the density cap, and the
//! time of a query that spends the whole work budget, which bounds every query whatever the
//! pattern or span.

use reuben_pattern::{Event, Pattern, Rational, Span, QUERY_BUDGET};
use std::time::Instant;

fn time(p: &Pattern, s: Span, buf: &mut [Event]) -> (f64, reuben_pattern::QueryResult) {
    let runs = 200;
    let mut res = Default::default();
    let t = Instant::now();
    for _ in 0..runs {
        res = std::hint::black_box(p.query(std::hint::black_box(s), 1, buf));
    }
    (t.elapsed().as_secs_f64() * 1e6 / f64::from(runs), res)
}

fn main() {
    let r = |n: i64, d: i64| Rational::new(n, d).unwrap();
    // A 4096-frame block at 48 kHz, 999 BPM, 4 beats per cycle: 0.355 cycles, an hour in.
    let cycles_per_frame = r(999, 60 * 4 * 48_000);
    let start = Rational::int(15_000).checked_add(r(1, 3)).unwrap();
    let block = Span::new(
        start,
        start
            .checked_add(cycles_per_frame.checked_mul(Rational::int(4096)).unwrap())
            .unwrap(),
    );
    let mut buf = vec![Event::EMPTY; 2048];
    println!("block of 4096 frames at 999 BPM (0.355 cycles), buffer 2048:");
    for src in [
        "0*1024",
        "[0*256, 1*256, 2*256, 3*256]",
        "[0 1 2 3]*256",
        "[0 [1 [2 [3 [4 [5 6]]]]]]*146",
        "[0 [1 [2 [3 [4 [5 6]]]]]]*146?0.999",
        "<[0 1]*512 [2 3]*512>",
        "{0 1 2, 3 4 5 6 7}%512",
        "[0(5,8)*16]!12",
    ] {
        let p = Pattern::new(src).unwrap();
        let (us, res) = time(&p, block, &mut buf);
        println!("  {us:8.1} us  {src:40} {res:?}");
    }
    println!("whole-budget queries ({QUERY_BUDGET} units):");
    for src in [
        "0*1024",
        "[0*256, 1*256, 2*256, 3*256]?0.999",
        "[0 [1 [2 [3 [4 [5 6]]]]]]*146",
    ] {
        let p = Pattern::new(src).unwrap();
        let long = Span::new(start, start.checked_add(Rational::int(1000)).unwrap());
        let (us, res) = time(&p, long, &mut buf);
        println!("  {us:8.1} us  {src:40} {res:?}");
    }
}
