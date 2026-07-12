//! Shared RT-safety allocation-counting harness for the `*_rt_safe` test binaries.
//!
//! A `#[global_allocator]` sees **every** allocation in the process — on any thread, at
//! any time — so a naive counter wrapped around a measured window also tallies stray
//! allocations made by the libtest harness threads (result plumbing, output capture,
//! timing) that happen to interleave with the window. Under a loaded, parallel
//! `cargo test --workspace` run those strays land inside the window often enough to
//! flip a `assert_eq!(allocs, 0)` red on code that never allocated (they are counted on
//! a *different* thread than the one running the ops under test).
//!
//! The fix is to **arm counting per-thread**: the allocator only touches the counters
//! while the *current* thread is armed, and a window is armed on exactly the thread that
//! runs the measured ops, only for the duration of those ops (see [`measure`]). An
//! allocation on any other thread — the harness, a sibling test, setup/teardown — is
//! never counted, so the assertion measures only the ops under test and is immune to
//! cross-thread interleaving under load.
//!
//! The armed flag is a `const`-initialised, `Copy`, destructor-free thread-local, so
//! reading it inside `alloc` neither allocates nor registers a TLS destructor: no
//! re-entrancy into the allocator, no teardown panic.
//!
//! Each test binary is its own crate, so it declares its own `#[global_allocator]`
//! (`static GLOBAL: Counting = Counting;`) and gets a private copy of these counters —
//! nothing is shared across binaries.
#![allow(dead_code)] // not every test binary reads every item (frees, `Counts` fields).

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    /// Number of `alloc`/`realloc` calls counted on this thread while it was armed.
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
    /// Number of `dealloc` calls counted on this thread while it was armed.
    static FREES: Cell<usize> = const { Cell::new(0) };
    /// Whether *this* thread is currently measuring. `const`-initialised and
    /// destructor-free, so `with` inside the allocator is allocation-free (no lazy
    /// init) and cannot panic during thread teardown.
    static ARMED: Cell<bool> = const { Cell::new(false) };
}

#[inline]
fn armed() -> bool {
    ARMED.with(Cell::get)
}

/// System allocator that counts allocations, reallocations, and frees — but only those
/// made on a thread that is currently armed (see [`measure`]). Free-counting matters for
/// the mailbox contract, which forbids the render side to *drop* a payload, not just to
/// allocate one; allocation-only tests simply never inspect [`FREES`].
pub struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if armed() {
            ALLOCS.with(|c| c.set(c.get() + 1));
        }
        System.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if armed() {
            FREES.with(|c| c.set(c.get() + 1));
        }
        System.dealloc(ptr, layout)
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if armed() {
            ALLOCS.with(|c| c.set(c.get() + 1));
        }
        System.realloc(ptr, layout, new_size)
    }
}

/// Allocations and frees observed on the measuring thread during one [`measure`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub allocs: usize,
    pub frees: usize,
}

/// Arms the current thread on construction and restores the previous state on drop, so
/// the window closes even if the measured body panics.
struct Armed(bool);

impl Armed {
    fn new() -> Self {
        Armed(ARMED.with(|c| c.replace(true)))
    }
}

impl Drop for Armed {
    fn drop(&mut self) {
        ARMED.with(|c| c.set(self.0));
    }
}

/// Run `f` on the current thread with allocation counting armed, and return the number
/// of heap allocations and frees that happened **on this thread** while it ran.
///
/// Counting is gated per-thread, so allocations on any other thread (the libtest
/// harness, sibling tests) during the same wall-clock window are never counted — the
/// returned counts reflect only what `f` itself did. Arm exactly the measured ops (put
/// warm-up and setup *outside* the closure); disarming is automatic, panic included.
pub fn measure(f: impl FnOnce()) -> Counts {
    let allocs_before = ALLOCS.with(Cell::get);
    let frees_before = FREES.with(Cell::get);
    {
        let _armed = Armed::new();
        f();
    }
    Counts {
        allocs: ALLOCS.with(Cell::get) - allocs_before,
        frees: FREES.with(Cell::get) - frees_before,
    }
}
