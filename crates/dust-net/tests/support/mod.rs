//! Shared test scaffolding.
//!
//! Only things that need to be identical across test binaries live here. Each
//! integration test is its own binary, so the allocator below has to be
//! *registered* in each root that wants it; this module only supplies the type.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

/// Bytes currently held by live allocations.
static LIVE: AtomicUsize = AtomicUsize::new(0);
/// The high-water mark of `LIVE` since the last [`reset_peak`].
static PEAK: AtomicUsize = AtomicUsize::new(0);

/// A global allocator that records the high-water mark of live bytes.
///
/// This exists because "never allocate unboundedly" is one of the three
/// invariants the decoder is supposed to hold, and it is the one that cannot
/// be observed from outside. A test that feeds a decompression bomb and checks
/// only the error has not distinguished "refused it cheaply" from "allocated a
/// gigabyte and then refused it" — and the second is still a denial of
/// service, delivered by a frame that this crate correctly rejected.
///
/// **What it does not catch.** It counts what goes through Rust's global
/// allocator, so an allocation made inside a C library with its own allocator
/// would be invisible. That is part of why `flate2` is configured with
/// `rust_backend`; with a vendored C zlib this measurement would be a claim
/// about only half of the decompressor.
///
/// It is also process-wide, so a test that measures a peak must not run
/// concurrently with another test that allocates. Every test using it is
/// marked, and they are gathered into a single `#[test]` for that reason
/// rather than split up for prettier output.
pub struct Counting;

// SAFETY-adjacent note, since this crate denies `unsafe_code` and this is the
// one place that overrides it: the two methods below forward every call
// unchanged to `System` and touch only two atomics of their own. The allocator
// contract is upheld by `System`; this wrapper adds bookkeeping and no
// behaviour. It is confined to test binaries and is not compiled into the
// library.
#[allow(unsafe_code)]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(live, Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(pointer, layout) }
    }
}

/// Start a measurement. Returns the live total the peak is measured from.
pub fn reset_peak() -> usize {
    let live = LIVE.load(Ordering::Relaxed);
    PEAK.store(live, Ordering::Relaxed);
    live
}

/// The most bytes live at once since [`reset_peak`], above the baseline it
/// returned.
pub fn peak_above(baseline: usize) -> usize {
    PEAK.load(Ordering::Relaxed).saturating_sub(baseline)
}

/// The gate every test in an allocator-measuring binary passes through.
///
/// The harness runs a binary's tests on parallel threads, and the peak
/// counters are process-wide, so a measurement taken while any other test is
/// allocating measures the both of them. Holding this lock for the whole body
/// of every test in such a binary — not only the measuring ones — makes the
/// runs sequential and the numbers mean what they say. The cost is nil: these
/// suites run in milliseconds either way.
static SERIAL: Mutex<()> = Mutex::new(());

pub fn serial() -> MutexGuard<'static, ()> {
    match SERIAL.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}
