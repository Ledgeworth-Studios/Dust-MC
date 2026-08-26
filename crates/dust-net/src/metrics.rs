//! What one connection did, counted where it happened.
//!
//! # Why counters and not logs
//!
//! A server that wants to know how its connections are behaving has two
//! ways to find out: read what the code says while it runs, or ask it later.
//! Logs answer richly but expire — nobody greps a rotated log to learn that
//! pre-authentication timeouts doubled after Tuesday's deploy. Counters are
//! the other half: coarse, permanent, and cheap enough to keep for every
//! connection, including the thousands that misbehave in boring ways.
//!
//! This module is deliberately the second thing only. It emits no lines,
//! because it has nothing to say that a number does not say better, and it
//! pulls in no logging framework: `tracing` appears nowhere in this
//! workspace's dependency graph, and adding it so that a struct of six
//! integers could occasionally print itself would widen every build's audit
//! for output the caller can format anyway. [`Conn::stats`] hands back a
//! snapshot; rendering it is the layer above's business.
//!
//! # What is counted, and where the count happens
//!
//! The counts follow the driver's own delivery semantics rather than an
//! idealised model of them:
//!
//! * `frames_out` counts frames **accepted** by
//!   [`Conn::send`](crate::io::Conn::send), because that is the moment the
//!   driver takes responsibility for them; `bytes_out` counts wire bytes
//!   actually **written** by the writer task, because that is the moment the
//!   socket takes over. Under a stalled peer the two diverge, on purpose —
//!   the divergence *is* the backlog.
//! * `frames_in` counts complete frames handed up; `bytes_in` counts raw
//!   socket bytes pulled off the wire, before decryption shrank them and
//!   before decompression expanded them. The wire is what the peer chose to
//!   spend.
//! * Errors are bucketed by kind at the single place each failure becomes a
//!   [`ConnError`], so two code paths cannot disagree about what went wrong.
//!   A connection that ends reports its ending once; the redundant
//!   `Closed` errors every later call returns are not events and add
//!   nothing.
//!
//! All fields live in atomics because the reader half and the writer task
//! update different counters concurrently, and a snapshot torn across their
//! updates is still a lower bound on activity — the one direction a
//! under-count is honest.

use std::sync::atomic::{AtomicU64, Ordering};

/// The running counters for one connection.
///
/// Held internally by the driver behind an `Arc`, shared with the writer
/// task, and read through [`Conn::stats`](crate::io::Conn::stats). Not
/// constructed by callers; see [`StatsSnapshot`] for the readable form.
#[derive(Debug, Default)]
pub(crate) struct ConnCounters {
    pub(super) frames_in: AtomicU64,
    pub(super) frames_out: AtomicU64,
    pub(super) bytes_in: AtomicU64,
    pub(super) bytes_out: AtomicU64,
    pub(super) protocol_errors: AtomicU64,
    pub(super) io_errors: AtomicU64,
    pub(super) truncated_frames: AtomicU64,
    pub(super) idle_timeouts: AtomicU64,
    pub(super) pre_auth_deadlines: AtomicU64,
}

impl ConnCounters {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Record which bucket `error` belongs to, once per distinct event.
    ///
    /// Called exactly where a failure is first produced. [`ConnError::
    /// Closed`] is deliberately absent from the match: it is not something
    /// that happened to the connection but the absence of anything further,
    /// and counting it would grow with how many times a caller retried
    /// after the end.
    pub(crate) fn note_error(&self, error: &crate::io::ConnError) {
        use crate::io::ConnError as E;
        let slot = match error {
            E::Protocol(_) => &self.protocol_errors,
            E::Io(_) => &self.io_errors,
            E::TruncatedFrame { .. } => &self.truncated_frames,
            E::IdleTimeout { .. } => &self.idle_timeouts,
            E::PreAuthDeadline { .. } => &self.pre_auth_deadlines,
            // Caller mistakes and already-dead connections say more about
            // the caller than the connection; they surface as return values
            // and are not tallied here.
            E::Illegal(_) | E::Handshake(_) | E::Closed => return,
        };
        slot.fetch_add(1, Ordering::Relaxed);
    }
}

/// One moment's view of a connection's counters, in plain numbers.
///
/// A copy, not a window: the connection keeps moving underneath whatever a
/// caller does with this, which is why the fields are `u64` rather than
/// something shared. Take another snapshot to see later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StatsSnapshot {
    /// Complete frames received from the peer.
    pub frames_in: u64,
    /// Frames accepted for sending. See the module docs for why this and
    /// `bytes_out` can legitimately disagree about progress.
    pub frames_out: u64,
    /// Raw wire bytes pulled off the socket, before decryption and
    /// decompression changed their size.
    pub bytes_in: u64,
    /// Wire bytes the writer task handed to the socket, after encryption
    /// changed their size.
    pub bytes_out: u64,
    /// Frames the codec refused — malformed prefixes, threshold violations,
    /// decompression bombs. Each one ended the connection.
    pub protocol_errors: u64,
    /// Failures of the socket itself, inbound or outbound.
    pub io_errors: u64,
    /// Hangups with part of a frame still missing.
    pub truncated_frames: u64,
    /// Reads abandoned because the peer sent nothing in time.
    pub idle_timeouts: u64,
    /// Connections cut by the wall-clock budget for unauthenticated life.
    pub pre_auth_deadlines: u64,
}

impl StatsSnapshot {
    /// Every terminal error the connection reported, across all kinds.
    ///
    /// This is "how many ways did this connection fail", not "did it fail" —
    /// a connection fails once, terminally, and the interesting question is
    /// usually which kind dominated across many connections.
    pub fn total_errors(&self) -> u64 {
        self.protocol_errors + self.io_errors + self.truncated_frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::FrameError;
    use crate::io::ConnError;
    use std::time::Duration;

    /// Run one failure through classification and sum every error bucket,
    /// so a misrouted count (or a double count) shows up as any number but
    /// one.
    fn counted(error: &ConnError) -> u64 {
        let counters = ConnCounters::new();
        counters.note_error(error);
        [
            &counters.protocol_errors,
            &counters.io_errors,
            &counters.truncated_frames,
            &counters.idle_timeouts,
            &counters.pre_auth_deadlines,
        ]
        .into_iter()
        .map(|slot| slot.load(Ordering::Relaxed))
        .sum()
    }

    #[test]
    fn every_terminal_failure_lands_in_exactly_one_bucket() {
        assert_eq!(counted(&ConnError::Protocol(FrameError::Empty)), 1);
        assert_eq!(counted(&ConnError::TruncatedFrame { pending: 3 }), 1);
        assert_eq!(
            counted(&ConnError::IdleTimeout {
                limit: Duration::from_secs(1)
            }),
            1
        );
        assert_eq!(
            counted(&ConnError::PreAuthDeadline {
                budget: Duration::from_secs(1)
            }),
            1
        );
    }

    #[test]
    fn closed_is_an_absence_and_not_an_event() {
        // Every operation after the end returns Closed; a connection that
        // was polled ten more times would show ten errors if this counted.
        assert_eq!(counted(&ConnError::Closed), 0);
    }

    #[test]
    fn snapshots_are_plain_numbers_and_total_their_errors() {
        let snapshot = StatsSnapshot {
            protocol_errors: 2,
            truncated_frames: 1,
            ..StatsSnapshot::default()
        };
        assert_eq!(snapshot.total_errors(), 3);
        assert_eq!(StatsSnapshot::default().total_errors(), 0);
    }
}
