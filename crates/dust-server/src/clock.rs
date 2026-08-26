//! Time, as the server sees it.
//!
//! Every delay, deadline and measurement in this crate reads time through the
//! [`Clock`] trait rather than through `std::time` directly. That is not
//! ceremony; it is what makes the lifecycle testable at all:
//!
//! * A fixed-timestep loop driven by a real clock takes real time to test.
//!   Driven by a [`ManualClock`], a thousand ticks cost microseconds and the
//!   tick count is exact, so tests can assert on it.
//! * A watchdog that fires after a deadline would need a ten-second sleep to
//!   test against a real clock. Against a manual one, the test moves time
//!   forward itself and the watchdog cannot tell the difference.
//!
//! The rule the rest of the crate follows is: **no code under test may reach
//! for the wall clock on its own.** Anything that needs "now" is given it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// A source of monotonic time, in nanoseconds since an arbitrary origin.
///
/// The origin is arbitrary because every use here compares one reading against
/// another — deadlines, elapsed time, durations — never an absolute date. The
/// logging layer is the exception, and it keeps its own wall-clock conversion
/// rather than dragging calendar semantics into this trait.
///
/// Implementations must be monotonic within themselves: `now_ns` never goes
/// backwards between two calls on the same clock. Both provided
/// implementations hold that guarantee; a custom one that does not will get
/// exactly the negative-duration chaos it deserves.
pub trait Clock: Send + Sync {
    /// The current reading, in nanoseconds from this clock's origin.
    fn now_ns(&self) -> u64;
}

/// The production clock: nanoseconds since the process started.
///
/// `Instant` is the platform's monotonic source, so this is as honest as time
/// gets in-process. It exists as a named type mostly so call sites can spell
/// out that they want the real thing, and so there is one place to change if
/// a platform ever needs special treatment.
#[derive(Debug)]
pub struct MonotonicClock {
    start: Instant,
}

impl Default for MonotonicClock {
    fn default() -> Self {
        Self::new()
    }
}

impl MonotonicClock {
    /// Start measuring from now.
    pub fn new() -> Self {
        Self { start: Instant::now() }
    }
}

impl Clock for MonotonicClock {
    fn now_ns(&self) -> u64 {
        // A u64 of nanoseconds covers about 584 years of uptime. If a Dust
        // process ever reaches that, the wrap is the least of its problems.
        u64::try_from(self.start.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
}

/// A clock that only moves when something moves it.
///
/// This is the virtual-time workhorse behind most of the crate's tests: the
/// tick loop, the parkers and the watchdog all read it through [`Clock`], and
/// the test advances it by hand, so a full boot-and-shutdown cycle costs no
/// real time at all and produces exactly the ticks the test asked for.
///
/// The reading lives in an atomic because several threads legitimately share
/// one manual clock — a stepper parker advancing it while the engine reads it,
/// say — and the whole point of virtual time is that those threads agree on
/// what "now" is.
#[derive(Debug, Default)]
pub struct ManualClock {
    now_ns: AtomicU64,
}

impl ManualClock {
    /// A clock frozen at its origin.
    pub fn new() -> Self {
        Self::default()
    }

    /// Move time forward by `delta_ns` and return the new reading.
    ///
    /// Advancing backwards is deliberately impossible through this method;
    /// [`set_ns`](Self::set_ns) exists for the rare case a test truly wants
    /// to reposition, and its name says so.
    pub fn advance_ns(&self, delta_ns: u64) -> u64 {
        self.now_ns.fetch_add(delta_ns, Ordering::SeqCst) + delta_ns
    }

    /// Reposition the clock outright. See [`advance_ns`](Self::advance_ns)
    /// for why this is not the normal way to move time.
    pub fn set_ns(&self, value_ns: u64) {
        self.now_ns.store(value_ns, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ns(&self) -> u64 {
        self.now_ns.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_manual_clock_starts_at_zero_and_stays_there_until_moved() {
        let clock = ManualClock::new();
        assert_eq!(clock.now_ns(), 0);
        assert_eq!(clock.now_ns(), 0, "reading twice must not advance it");
    }

    #[test]
    fn advancing_returns_the_new_reading_and_advances_exactly_once() {
        let clock = ManualClock::new();
        assert_eq!(clock.advance_ns(50_000_000), 50_000_000);
        assert_eq!(clock.advance_ns(25), 50_000_025);
        assert_eq!(clock.now_ns(), 50_000_025);
    }

    #[test]
    fn the_monotonic_clock_only_moves_forward() {
        let clock = MonotonicClock::new();
        let first = clock.now_ns();
        // No sleep, by house rule: two consecutive readings are enough to
        // assert the property that matters, which is direction, not pace.
        let second = clock.now_ns();
        assert!(second >= first, "{second} went backwards from {first}");
    }
}
