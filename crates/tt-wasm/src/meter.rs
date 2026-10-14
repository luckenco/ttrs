//! Requested live allocation bytes (not linear-memory pages or allocator overhead).
use std::{
    alloc,
    sync::atomic::{AtomicUsize, Ordering},
};

struct Meter;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn add(bytes: usize) {
    let live = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(live, Ordering::Relaxed);
}

// SAFETY: every operation forwards the original pointer/layout to System; only
// successful allocations affect counters. Atomics do not allocate or recurse.
unsafe impl alloc::GlobalAlloc for Meter {
    unsafe fn alloc(&self, layout: alloc::Layout) -> *mut u8 {
        let ptr = unsafe { alloc::System.alloc(layout) };
        if !ptr.is_null() {
            add(layout.size());
        }
        ptr
    }
    unsafe fn alloc_zeroed(&self, layout: alloc::Layout) -> *mut u8 {
        let ptr = unsafe { alloc::System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            add(layout.size());
        }
        ptr
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: alloc::Layout) {
        unsafe { alloc::System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: alloc::Layout, size: usize) -> *mut u8 {
        let new = unsafe { alloc::System.realloc(ptr, layout, size) };
        if !new.is_null() {
            if size >= layout.size() {
                add(size - layout.size());
            } else {
                LIVE.fetch_sub(layout.size() - size, Ordering::Relaxed);
            }
        }
        new
    }
}

#[global_allocator]
static ALLOCATOR: Meter = Meter;

/// Live requested Rust heap bytes, excluding allocator overhead and Wasm pages.
#[unsafe(no_mangle)]
pub extern "C" fn live_bytes() -> u32 {
    u32::try_from(LIVE.load(Ordering::Relaxed)).expect("live bytes fit u32")
}

/// Peak requested Rust heap bytes since reset (or instance creation).
#[unsafe(no_mangle)]
pub extern "C" fn peak_bytes() -> u32 {
    u32::try_from(PEAK.load(Ordering::Relaxed)).expect("peak bytes fit u32")
}

/// Reset peak to current live bytes. Call only between synchronous exports.
#[unsafe(no_mangle)]
pub extern "C" fn reset_peak() {
    PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
}
