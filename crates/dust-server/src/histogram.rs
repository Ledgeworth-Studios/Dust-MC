//! Per-tick timing, kept honestly: a sliding window with min/avg/max/p99.
//!
//! Two design decisions worth writing down, because both are load-bearing for
//! how the numbers get read later:
//!
//! **A window, not a lifetime total.** A server that has run for a week does
//! not care what its very first tick cost — that tick warmed caches nobody
//! will ever warm again. What operations wants is *recent* behaviour, so
//! samples older than [`TIMING_WINDOW`] records fall off the end. The count of
//! every sample ever recorded travels alongside (`total_recorded`) because
//! "the window holds 1024 ticks" and "we ran 40 ticks" are different facts.
//!
//! **p99 by nearest rank.** Percentile definitions multiply quietly —
//! interpolation, lower bound, nearest rank all give different answers on the
//! same data. Nearest rank (`ceil(p × n)`, 1-based) is used here because it is
//! the one an operator can recompute by sorting a column in a spreadsheet, and
//! because it always returns an actual observed sample rather than a value
//! between two samples that never happened.

/// How many most-recent samples the statistics cover.
///
/// At 20 ticks per second this is a little over fifty seconds of history:
/// long enough to smooth out one slow chunk generation, short enough that a
/// problem five minutes ago no longer hides in today's numbers.
pub const TIMING_WINDOW: usize = 1024;

/// Sliding-window statistics over tick durations, in nanoseconds.
#[derive(Debug)]
pub struct TimingHistogram {
    window: Vec<u64>,
    /// Next slot to write; the ring position of the *next* sample.
    next: usize,
    filled: usize,
    total_recorded: u64,
}

impl Default for TimingHistogram {
    fn default() -> Self {
        Self::new()
    }
}

impl TimingHistogram {
    /// An empty histogram with the standard window size.
    pub fn new() -> Self {
        Self::with_window(TIMING_WINDOW)
    }

    /// An empty histogram with a specific window size, for tests that want a
    /// window small enough to watch samples fall off the end.
    pub fn with_window(size: usize) -> Self {
        assert!(
            size > 0,
            "a zero-length timing window can never hold a sample"
        );
        Self {
            window: vec![0; size],
            next: 0,
            filled: 0,
            total_recorded: 0,
        }
    }

    /// Record one duration.
    pub fn record(&mut self, duration_ns: u64) {
        self.window[self.next] = duration_ns;
        self.next = (self.next + 1) % self.window.len();
        self.filled = (self.filled + 1).min(self.window.len());
        self.total_recorded += 1;
    }

    /// Statistics over the current window. All four summary fields are `None`
    /// when nothing has been recorded: a report that printed zeros would be
    /// indistinguishable from one whose ticks were genuinely free, and those
    /// are opposite situations.
    pub fn snapshot(&self) -> TimingStats {
        let samples = &self.window[..self.filled];
        if samples.is_empty() {
            return TimingStats {
                window_samples: 0,
                total_recorded: self.total_recorded,
                ..TimingStats::EMPTY
            };
        }
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();
        let sum: u64 = sorted.iter().sum();
        TimingStats {
            window_samples: self.filled,
            total_recorded: self.total_recorded,
            min: Some(sorted[0]),
            avg: Some(sum / sorted.len() as u64),
            max: Some(sorted[sorted.len() - 1]),
            p99: Some(sorted[percentile_rank(sorted.len(), 0.99)]),
        }
    }
}

/// One row of timing output.
///
/// Durations are integer nanoseconds throughout. The average floors to whole
/// nanoseconds on purpose: a fraction of a nanosecond is not a measurement
/// this crate ever made, and printing one implies precision nobody has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TimingStats {
    /// Samples currently inside the window.
    pub window_samples: usize,
    /// Samples ever recorded, including ones already evicted from the window.
    pub total_recorded: u64,
    pub min: Option<u64>,
    pub avg: Option<u64>,
    pub max: Option<u64>,
    /// Nearest-rank 99th percentile over the current window.
    pub p99: Option<u64>,
}

const EMPTY: TimingStats = TimingStats {
    window_samples: 0,
    total_recorded: 0,
    min: None,
    avg: None,
    max: None,
    p99: None,
};

impl TimingStats {
    const EMPTY: Self = EMPTY;

    /// Whether anything has ever been recorded.
    pub fn is_empty(&self) -> bool {
        self.min.is_none()
    }
}

/// Nearest-rank index into an ascending slice: the `ceil(p × n)`-th value.
fn percentile_rank(len: usize, p: f64) -> usize {
    // Rank is 1-based, hence the -1 for the index. At p=0.99 the smallest
    // window this can be asked about is length 1, where ceil(0.99) == 1 == the
    // only element, so no clamping is needed below.
    ((p * len as f64).ceil() as usize)
        .saturating_sub(1)
        .min(len - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_window_reports_nothing_rather_than_zeros() {
        let stats = TimingHistogram::new().snapshot();
        assert!(stats.is_empty());
        assert_eq!(stats.window_samples, 0);
        assert_eq!(stats.total_recorded, 0);
        assert_eq!(stats.min, None);
        assert_eq!(stats.p99, None);
    }

    #[test]
    fn a_single_sample_is_min_avg_max_and_p99_at_once() {
        let mut hist = TimingHistogram::new();
        hist.record(7);
        let stats = hist.snapshot();
        assert_eq!(stats.min, Some(7));
        assert_eq!(stats.avg, Some(7));
        assert_eq!(stats.max, Some(7));
        assert_eq!(stats.p99, Some(7));
    }

    #[test]
    fn percentiles_follow_nearest_rank_over_the_sorted_window() {
        let mut hist = TimingHistogram::new();
        // 100 samples, values 1..=100: p99 must be the 99th smallest, which
        // here is literally the value 99 — an interpolation scheme would have
        // answered 99.0-something or 100 depending on convention.
        for v in 1..=100u64 {
            hist.record(v);
        }
        let stats = hist.snapshot();
        assert_eq!(stats.p99, Some(99));
        assert_eq!(stats.min, Some(1));
        assert_eq!(stats.max, Some(100));
        assert_eq!(stats.avg, Some(50), "mean of 1..=100 floors to 50");
    }

    #[test]
    fn the_window_keeps_only_the_most_recent_samples() {
        let mut hist = TimingHistogram::with_window(4);
        for v in [10u64, 20, 30, 40, 999] {
            hist.record(v);
        }
        let stats = hist.snapshot();
        assert_eq!(stats.window_samples, 4);
        assert_eq!(stats.total_recorded, 5, "eviction is not forgetting");
        assert_eq!(stats.max, Some(999), "the newest sample is still in");
        assert_eq!(
            stats.min,
            Some(20),
            "10 fell off the front when 999 arrived"
        );
    }

    #[test]
    fn the_average_floors_instead_of_lying_about_precision() {
        let mut hist = TimingHistogram::new();
        hist.record(1);
        hist.record(2);
        assert_eq!(hist.snapshot().avg, Some(1), "(1+2)/2 = 1.5 floors to 1");
    }
}
