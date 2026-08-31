//! Waiting for a counter the writer task updates on its own schedule.
//!
//! `bytes_out` is incremented inside the writer task *after* `write_all`
//! returns, which is deliberate — the count is what the socket took, not what
//! was handed to it. The consequence is that a peer can read every one of
//! those bytes before the increment happens: the socket takes them, the peer
//! is woken, and the writer task may not be scheduled again until after the
//! test has already read `stats()`.
//!
//! So a test that drains the peer and then asserts on `bytes_out` has arranged
//! no ordering between the two events at all. On an idle machine the writer
//! wins that race every time, which is why five local runs said nothing; on a
//! CI runner scheduling the whole suite in parallel it lost, and
//! `encrypted_egress_is_counted_at_its_wire_size` failed with `left 0, right
//! 8` — the peer's eight bytes against a counter still reading zero.
//!
//! [`dust_net::io::Conn::stats`] documents the property this waits on in as
//! many words: a snapshot "may already be behind a connection that kept
//! moving. Take another to see later." That is what this does — take another
//! until it agrees — so the assertion underneath is about the counter and not
//! about the scheduler.

use std::time::{Duration, Instant};

use dust_net::metrics::StatsSnapshot;

/// How long to keep re-reading before calling the writer wedged.
///
/// A stall guard, not a timing assumption. The loop resolves on the first
/// re-read once the writer task is scheduled at all, which is microseconds
/// even on a loaded box; a run that reaches this deadline has a writer that
/// stopped, and that is a failure worth reporting as itself rather than as a
/// wrong number.
const PATIENCE: Duration = Duration::from_secs(30);

/// Re-read `take` until `want` accepts the snapshot, then return it.
///
/// Panics with the last snapshot seen if [`PATIENCE`] runs out, so a genuinely
/// wedged writer reports what the counters actually said instead of a bare
/// timeout.
pub async fn settled<F, P>(mut take: F, want: P, what: &str) -> StatsSnapshot
where
    F: FnMut() -> StatsSnapshot,
    P: Fn(&StatsSnapshot) -> bool,
{
    let deadline = Instant::now() + PATIENCE;
    loop {
        let snapshot = take();
        if want(&snapshot) {
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "the counters never settled: {what}; last snapshot {snapshot:?}"
        );
        // Yield rather than spin: the thing being waited for is another task
        // getting a turn.
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}
