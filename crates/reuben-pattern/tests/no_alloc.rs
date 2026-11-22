//! The query never allocates: a counting global allocator watches every query in this binary.

mod common;
use common::*;
use proptest::prelude::*;
use reuben_pattern::{Event, Pattern, Rational, Span};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct Counting;

thread_local! {
    // `const` init: reading it never allocates, which an allocator hook requires.
    static ARMED: Cell<bool> = const { Cell::new(false) };
    static COUNT: Cell<usize> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ARMED.with(Cell::get) {
            COUNT.with(|c| c.set(c.get() + 1));
        }
        // SAFETY: forwarded unchanged; the caller upholds `alloc`'s contract.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ARMED.with(Cell::get) {
            COUNT.with(|c| c.set(c.get() + 1));
        }
        // SAFETY: `ptr` came from `System.alloc` with this `layout`.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Heap operations performed by `f` on this thread.
fn allocations(f: impl FnOnce()) -> usize {
    COUNT.with(|c| c.set(0));
    ARMED.with(|a| a.set(true));
    f();
    ARMED.with(|a| a.set(false));
    COUNT.with(Cell::get)
}

fn assert_query_allocates_nothing(p: &Pattern, s: Span, buf: &mut [Event]) {
    let n = allocations(|| {
        let _ = std::hint::black_box(p.query(s, 11, buf));
    });
    assert_eq!(n, 0, "query allocated {n} times");
}

#[test]
fn counter_sees_allocations() {
    assert!(allocations(|| drop(std::hint::black_box(vec![1u8; 64]))) > 0);
}

#[test]
fn query_allocates_nothing_including_on_overflow() {
    let p = Pattern::new("[0 1 2]*7 <3 [4 5]/3> {0 1 2, 3 4}%5? c4(5,13,3) [0|1|2]").unwrap();
    let mut roomy = [Event::EMPTY; 1024];
    let mut tight = [Event::EMPTY; 2];
    let mut none: [Event; 0] = [];
    for s in [cycles(0, 1), cycles(-3, 5), Span::new(r(1, 3), r(2, 7))] {
        assert_query_allocates_nothing(&p, s, &mut roomy);
        assert_query_allocates_nothing(&p, s, &mut tight);
        assert_query_allocates_nothing(&p, s, &mut none);
    }
    let dense = Pattern::new("[0*256, 1*256, 2*256, 3*256]?").unwrap();
    let mut res = None;
    assert_eq!(
        allocations(|| res = Some(dense.query(cycles(0, 1000), 0, &mut roomy))),
        0
    );
    assert!(res.unwrap().exhausted);
    let near = Rational::int(i64::MAX / 4);
    assert_query_allocates_nothing(
        &p,
        Span::new(near, near.checked_add(r(1, 3)).unwrap()),
        &mut roomy,
    );
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    #[test]
    fn generated_queries_allocate_nothing(src in pattern_src(), s in query_span(), cap in 0usize..64) {
        let Some(p) = generated(&src) else { return Ok(()) };
        let mut buf = vec![Event::EMPTY; cap];
        assert_query_allocates_nothing(&p, s, &mut buf);
    }
}
