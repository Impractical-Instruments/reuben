//! Measures how long a unit of work takes, which turns the compile-time per-cycle work bound into
//! time.
//!
//! `cargo run --release -p reuben-pattern --example query_cost`, pinned to one core on a shared
//! machine (`taskset -c 3 target/release/examples/query_cost`): scheduling noise otherwise poses
//! as a slow unit.
//!
//! Every compiled pattern's cycle query spends at most `Pattern::max_cycle_work()` units, never more
//! than `MAX_WORK`. This times the patterns that make units slowest (deep `?` chains, which hash
//! once per filter, and fractional speeds, whose reduction is the costliest arithmetic) at cycles
//! across the guaranteed range, and prints the time bounds that follow. 128-bit division on a
//! Cortex-M is not measured here.

use reuben_pattern::{CycleResult, Note, Pattern, MAX_CYCLE, MAX_WORK};
use std::time::Instant;

fn time(p: &Pattern, cycle: i64, buf: &mut [Note]) -> (f64, CycleResult) {
    let runs = 200;
    let mut res = CycleResult::default();
    let t = Instant::now();
    for _ in 0..runs {
        res = std::hint::black_box(p.cycle(std::hint::black_box(cycle), 1, buf));
    }
    (t.elapsed().as_secs_f64() * 1e6 / f64::from(runs), res)
}

fn main() {
    // Each near the cap (`max_cycle_work` is printed beside it), and each a kind of slow unit.
    let patterns = [
        "0*150",
        "[0 1 2 3]*9",
        "[0 [1 [2 [3 [4 [5 6]]]]]]",
        "{0 1 2, 3 4 5 6 7}%16",
        "[[0 1 2]*1.001 [3 4 5 6 7]/1.003]*2",
        "[0 1 2 3 4 5 6]*1.37 [0 1]*1.013",
        "<[0 1 2]*1.003 [3 4 5 6 7]/1.009>*3",
        "[0(5,13,3)*1.007]*2",
        "[0|1|2]*1.01 [3? 4?0.3]*1.03 <5 6 7>*1.07",
        "[[0 1]*1.00001]!4",
        "0*12?0?0?0?0?0?0?0",
        "[[0*2?0?0?0?0?0?0?0]?0?0?0?0?0?0?0?0]?0?0?0?0?0?0?0?0",
        "[[[0*1.7?0?0?0?0?0?0?0]?0?0?0?0?0?0?0?0]?0?0?0?0?0?0?0?0]*1.0001",
        "[[0*2?0?0?0?0?0?0?0]?0?0?0?0?0?0?0?0]*1.0001",
    ];

    let cycles = [0i64, 1 << 20, -MAX_CYCLE, MAX_CYCLE];
    let (mut worst_ns, mut worst_us) = (0.0f64, 0.0f64);
    let mut buf = vec![Note::EMPTY; 4096];
    for src in patterns {
        let p = match Pattern::new(src) {
            Ok(p) => p,
            Err(e) => {
                println!("  refused: {src}: {e}");
                continue;
            }
        };
        for c in cycles {
            // The fastest of five runs: a cold cache or a busy machine does not pose as the worst
            // case, which is a property of the arithmetic.
            let (mut us, res) = time(&p, c, &mut buf);
            for _ in 0..4 {
                us = us.min(time(&p, c, &mut buf).0);
            }
            assert!(!res.exhausted && res.overflowed == 0 && res.work <= p.max_cycle_work());
            let ns = us * 1000.0 / f64::from(res.work.max(1));
            worst_ns = worst_ns.max(ns);
            worst_us = worst_us.max(us);
            println!(
                "  {ns:5.0} ns/unit {us:7.1} us work={:4} of {:4} cycle={c:<12} {src}",
                res.work,
                p.max_cycle_work()
            );
        }
    }
    let cap = f64::from(MAX_WORK) * worst_ns / 1000.0;
    println!("worst cost per unit: {worst_ns:.0} ns; slowest query measured: {worst_us:.0} us");
    println!("  one cycle query at the cap ({MAX_WORK} units): <= {cap:.0} us");
    println!(
        "  four queries (one 4096-frame block at 22.05 kHz, 999 BPM, 1 beat/cycle): <= {:.0} us",
        4.0 * cap
    );
    println!(
        "  as a share of a 64-frame block at 48 kHz (1333 us): one {:.1}%, four {:.1}%",
        cap / 13.33,
        4.0 * cap / 13.33
    );
}
