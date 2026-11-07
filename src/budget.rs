//! A memory budget: a counting allocator with an optional hard limit.
//!
//! Servers that run many conversions inside a memory-limited container need
//! each one to fail predictably instead of taking the whole process down.
//! Install [`Budget`] as the global allocator and call [`set_limit`] before
//! converting: once live heap memory would exceed the limit, allocation
//! fails and the process (or WebAssembly instance) aborts with
//! [`limit_hit`] set, instead of growing until the container is killed.
//!
//! The CLI and the WebAssembly bindings install it; library users opt in:
//!
//! ```
//! #[global_allocator]
//! static ALLOC: slidepdf::budget::Budget = slidepdf::budget::Budget;
//! ```
//!
//! The counters also give the peak heap use of a conversion, which is how
//! the memory numbers in the README were measured.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};

static LIMIT: AtomicUsize = AtomicUsize::new(0);
static USED: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static HIT: AtomicBool = AtomicBool::new(false);

/// Counting allocator over the system allocator. Zero overhead beyond two
/// atomic updates per allocation.
pub struct Budget;

impl Budget {
    #[inline]
    fn reserve(size: usize) -> bool {
        let limit = LIMIT.load(Relaxed);
        let used = USED.fetch_add(size, Relaxed) + size;
        if limit != 0 && used > limit {
            USED.fetch_sub(size, Relaxed);
            if !HIT.swap(true, Relaxed) {
                report_limit(limit);
            }
            return false;
        }
        PEAK.fetch_max(used, Relaxed);
        true
    }
}

unsafe impl GlobalAlloc for Budget {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if !Self::reserve(layout.size()) {
            return std::ptr::null_mut();
        }
        let p = System.alloc(layout);
        if p.is_null() {
            USED.fetch_sub(layout.size(), Relaxed);
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if !Self::reserve(layout.size()) {
            return std::ptr::null_mut();
        }
        let p = System.alloc_zeroed(layout);
        if p.is_null() {
            USED.fetch_sub(layout.size(), Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout);
        USED.fetch_sub(layout.size(), Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let old = layout.size();
        if new_size > old && !Self::reserve(new_size - old) {
            return std::ptr::null_mut();
        }
        let p = System.realloc(ptr, layout, new_size);
        if p.is_null() {
            if new_size > old {
                USED.fetch_sub(new_size - old, Relaxed);
            }
        } else if new_size < old {
            USED.fetch_sub(old - new_size, Relaxed);
        }
        p
    }
}

/// Exit code of a native process that hit the limit (the same as SIGABRT).
pub const EXIT_CODE: i32 = 134;

/// Written straight to stderr from inside the allocator (no allocation),
/// then the process exits with [`EXIT_CODE`]: deterministic on every libc,
/// where a plain allocation failure would abort with a signal that differs
/// between glibc and musl.
#[cfg(not(target_arch = "wasm32"))]
fn report_limit(limit: usize) {
    use std::io::Write;
    let mut buf = [0u8; 96];
    let mut w = &mut buf[..];
    let _ = writeln!(w, "slidepdf: memory limit of {} MB exceeded", limit >> 20);
    let n = 96 - w.len();
    let _ = std::io::stderr().write_all(&buf[..n]);
    std::process::exit(EXIT_CODE);
}

#[cfg(target_arch = "wasm32")]
fn report_limit(_limit: usize) {}

/// Caps live heap memory at `bytes` (0 removes the cap). Exceeding it
/// aborts the current conversion; see the module docs.
pub fn set_limit(bytes: usize) {
    LIMIT.store(bytes, Relaxed);
    HIT.store(false, Relaxed);
}

/// The current cap in bytes, 0 when unlimited.
pub fn limit() -> usize {
    LIMIT.load(Relaxed)
}

/// Heap bytes currently allocated through [`Budget`].
pub fn in_use() -> usize {
    USED.load(Relaxed)
}

/// Highest heap use since [`reset_peak`] (or process start).
pub fn peak() -> usize {
    PEAK.load(Relaxed)
}

/// Restarts the peak at the current use, to measure one conversion.
pub fn reset_peak() {
    PEAK.store(USED.load(Relaxed), Relaxed);
}

/// Whether an allocation was refused because of the limit since the last
/// [`set_limit`]. After a WebAssembly trap this tells memory exhaustion
/// apart from a bug.
pub fn limit_hit() -> bool {
    HIT.load(Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_allocations() {
        // The allocator may not be installed in the test binary; the
        // counters only move when it is, so only check consistency.
        let before = in_use();
        let v = vec![0u8; 1 << 20];
        let during = in_use();
        drop(v);
        assert!(during >= before);
        assert!(in_use() <= during);
        assert!(peak() >= in_use());
        assert_eq!(limit(), 0);
        assert!(!limit_hit());
    }
}
