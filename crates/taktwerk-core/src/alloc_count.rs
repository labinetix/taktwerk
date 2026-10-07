//! A counting allocator for the test binary, so "no allocation on the cycle path" is a test.
//!
//! Each thread counts its own allocations; a test compares two readings across the ticks it
//! drives.

#![allow(
    unsafe_code,
    reason = "a GlobalAlloc impl is unsafe by definition; test-only"
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

/// Counts every allocation on the calling thread and forwards to [`System`].
struct Counting;

// SAFETY: every method forwards its arguments unchanged to `System`, which upholds the
// `GlobalAlloc` contract; the counter is a const-initialised thread-local `Cell<u64>` with no
// destructor, so touching it inside the allocator neither allocates nor re-enters. `try_with`
// covers the thread-teardown window where the local is gone.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
        // SAFETY: `layout` is forwarded unchanged, as `GlobalAlloc` requires.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from `System.alloc` with this `layout`.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

/// Allocations the calling thread has made so far. Compare two readings.
pub fn allocations() -> u64 {
    ALLOCATIONS.try_with(Cell::get).unwrap_or(0)
}
