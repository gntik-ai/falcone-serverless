//! Audited measurement-only allocator boundary (ADR 0014): a process-wide
//! counting allocator for the capability allocation probes.
//! It forwards every request unchanged to `System` and only records sizes, so
//! freed memory that the system allocator reuses is still counted.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

pub struct Counting;

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn grow(bytes: usize) {
    let live = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(live, Ordering::Relaxed);
}

fn shrink(bytes: usize) {
    LIVE.fetch_sub(bytes, Ordering::Relaxed);
}

// SAFETY: each method forwards its arguments unchanged to `System`, which
// upholds the GlobalAlloc contract; the counters never touch the memory.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's layout contract is passed through unchanged.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            grow(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's layout contract is passed through unchanged.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            grow(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: ptr was returned by this allocator, i.e. by System, for layout.
        unsafe { System.dealloc(ptr, layout) };
        shrink(layout.size());
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: ptr was returned by System for layout; new_size is the caller's.
        let new = unsafe { System.realloc(ptr, layout, new_size) };
        if !new.is_null() {
            grow(new_size);
            shrink(layout.size());
        }
        new
    }
}

/// Resets the peak to the current live heap bytes and returns them.
pub fn reset_peak() -> usize {
    let live = LIVE.load(Ordering::SeqCst);
    PEAK.store(live, Ordering::SeqCst);
    live
}

/// Live heap bytes.
pub fn live() -> usize {
    LIVE.load(Ordering::SeqCst)
}

/// Peak live heap bytes since the last reset.
pub fn peak() -> usize {
    PEAK.load(Ordering::SeqCst)
}
