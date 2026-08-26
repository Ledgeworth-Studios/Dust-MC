//! Per-connection resource policy: pacing what a peer may send, and
//! counting who may connect at all.
//!
//! # Why this exists, and why it sits before decompression
//!
//! Every byte an unauthenticated stranger sends is processed at least
//! three times on its way in: read off the socket, decrypted, and — once
//! compression is on, which happens during login, before anyone has proved
//! who they are — run through zlib. The first two are linear and cheap.
//! The third is where amplification lives, and [`crate::frame`] already
//! bounds it per frame. What no per-frame check can bound is *rate*: a peer
//! that streams maximal legal frames back to back keeps one core busy
//! decompressing forever while staying inside every size limit in the
//! codec. The defences upstream bound how bad one frame can be; this
//! module bounds how many of them arrive per second.
//!
//! Hence the placement: the token bucket is charged against raw socket
//! bytes **before** the decoder runs, let alone the decompressor. A paced
//! connection cannot spend more CPU than its policy allows, whatever it
//! sends, because the expensive work never sees the bytes until the bucket
//! has paid for them.
//!
//! # Blocking, not dropping
//!
//! A framed stream cannot skip bytes, so "over quota" has exactly two
//! honest meanings here: make the sender wait, or end the connection.
//! Waiting is chosen, for two reasons. A legitimate client is bursty —
//! chunk updates, keepalives and responses arriving together after a lag
//! spike — and a burst-sized bucket absorbs exactly that shape; ending the
//! connection would punish ordinary jitter. And an attacker gains nothing
//! from the patience: their bytes trickle through at the sustained rate,
//! so their cost to us stays bounded while our TCP receive window quietly
//! pushes back on them for free. The idle timeout still applies
//! underneath — pacing delays work within the liveness budget, it never
//! extends it.
//!
//! # What pacing does not do
//!
//! It is per-connection arithmetic. One bucket per connection cannot see
//! ten thousand connections arriving together, which is why
//! [`AdmissionGate`] is a type rather than a policy: it holds the permits,
//! and whoever builds the server decides how many there are. Neither piece
//! decides anything about *which* connections deserve resources — that is
//! authentication's job, and it has not happened yet.

use std::sync::Arc;
use std::time::{Duration, Instant};

/// The inbound pacing policy for one connection.
///
/// `burst_bytes` is what may arrive at once — the bucket's capacity, sized
/// to absorb an honest client's burst without waiting. `bytes_per_second`
/// is what may arrive sustained — the refill rate, which is the number
/// that actually bounds a flood.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InboundRate {
    /// How many wire bytes may be consumed back to back before pacing
    /// starts asking the peer to slow down.
    pub burst_bytes: usize,
    /// How many wire bytes per second the connection may sustain, forever,
    /// regardless of what the peer keeps sending.
    pub bytes_per_second: usize,
}

impl InboundRate {
    /// The default policy, sized for a public server speaking vanilla's
    /// protocol: enough burst for several maximal chunks at once, and a
    /// sustained rate far above what an honest player produces.
    ///
    /// The largest frame vanilla will ever send fits inside the burst with
    /// room to spare, so a well-behaved client never waits; a peer streaming
    /// such frames back to back settles at the sustained rate within a
    /// fraction of a second.
    pub const fn generous() -> Self {
        Self {
            burst_bytes: 128 * 1024,
            bytes_per_second: 4 * 1024 * 1024,
        }
    }
}

/// A token bucket over wire bytes, refilled continuously.
///
/// Pure arithmetic on caller-supplied [`Instant`]s: nothing here reads the
/// clock or sleeps, which is what makes the property tests deterministic —
/// a schedule of takes at synthetic times exercises exactly the rule the
/// driver will run, without a single real millisecond passing.
///
/// The accounting is integer arithmetic in byte-nanoseconds, so a schedule
/// admits exactly the same bytes on every platform and every run; a float
/// time-credit bucket drifts by rounding, and drift in the thing that
/// bounds an attacker is a bug nobody can see from the outside.
#[derive(Debug, Clone)]
pub struct TokenBucket {
    capacity: u64,
    per_second: u64,
    tokens: u64,
    /// Sub-byte refill credit, carried between calls in units of
    /// byte-nanoseconds so no refill is lost to truncation.
    fractional: u128,
    last: Instant,
}

const NANOSECOND: u128 = 1_000_000_000;

impl TokenBucket {
    /// A bucket that starts full: a fresh connection owes nothing and may
    /// burst immediately, which is the shape of every honest login.
    pub fn new(rate: InboundRate, now: Instant) -> Self {
        // A degenerate config degrades to "one byte bursts, trickles in",
        // never to division by zero or an always-empty bucket.
        let capacity = rate.burst_bytes.max(1) as u64;
        Self {
            capacity,
            per_second: rate.bytes_per_second.max(1) as u64,
            tokens: capacity,
            fractional: 0,
            last: now,
        }
    }

    /// Credit elapsed time and report how many bytes are affordable now.
    ///
    /// Asking is what advances the clock; [`take`](Self::take) credits
    /// again internally, so interleaving the two loses nothing.
    pub fn available(&mut self, now: Instant) -> u64 {
        self.refill(now);
        self.tokens
    }

    /// Take up to `wanted` bytes, returning how many were granted.
    ///
    /// Never more than asked, never more than available, never negative.
    /// A partial grant is normal: a paced read is smaller, not wrong.
    pub fn take(&mut self, wanted: usize, now: Instant) -> usize {
        self.refill(now);
        let granted = (wanted as u64).min(self.tokens);
        self.tokens -= granted;
        granted as usize
    }

    /// How long until at least one byte becomes affordable if nothing is
    /// taken meanwhile. Zero when the bucket can pay now.
    ///
    /// Credits elapsed time first, exactly as [`available`](Self::available)
    /// does, so either question can be asked first without the answer
    /// depending on which came second.
    pub fn wait_for_one(&mut self, now: Instant) -> Duration {
        self.refill(now);
        if self.tokens >= 1 {
            return Duration::ZERO;
        }
        // One byte needs `NANOSECOND - fractional` more byte-nanoseconds of
        // credit, arriving at `per_second` per second of wall time. When
        // the bucket is empty the fraction is below a whole byte by
        // construction, so the deficit is well inside one second.
        let deficit = NANOSECOND - self.fractional.min(NANOSECOND - 1);
        Duration::from_nanos(deficit.div_ceil(self.per_second as u128) as u64)
    }

    /// Credit the time since the last look, capped at capacity.
    fn refill(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.last);
        self.last = now;
        if elapsed.is_zero() {
            return;
        }
        // Bytes × nanoseconds in a 128-bit lane: any sane rate over any
        // sane idle period fits without overflow, and the floor division
        // below discards only sub-byte credit, which `fractional` keeps.
        self.fractional += elapsed.as_nanos() * self.per_second as u128;
        let gained = (self.fractional / NANOSECOND) as u64;
        self.fractional %= NANOSECOND;
        self.tokens = self.capacity.min(self.tokens.saturating_add(gained));
    }
}

/// The semaphore behind a server-wide cap on live connections.
///
/// A type, not a policy: this holds however many permits the operator
/// decided on and hands them out one at a time. Nothing here chooses the
/// number, exempts localhost, prioritises logins over transfers, or any of
/// the hundred things a finished server will want to decide differently
/// per deployment — those decisions need context this type deliberately
/// does not have.
///
/// Cloning shares one pool of permits, so the accept loop and every worker
/// can hold the same gate without plumbing.
#[derive(Debug, Clone)]
pub struct AdmissionGate {
    permits: Arc<tokio::sync::Semaphore>,
    ceiling: usize,
}

impl AdmissionGate {
    /// A gate admitting at most `max_connections` holders at once.
    pub fn new(max_connections: usize) -> Self {
        let ceiling = max_connections.max(1);
        Self {
            permits: Arc::new(tokio::sync::Semaphore::new(ceiling)),
            ceiling,
        }
    }

    /// Wait for a permit, holding it until the returned guard drops.
    ///
    /// Cancellation is safe: a dropped future acquires nothing, so a login
    /// abandoned mid-queue leaves the gate exactly as it found it.
    pub async fn admit(&self) -> AdmissionPermit {
        AdmissionPermit {
            _permit: self
                .permits
                .clone()
                .acquire_owned()
                .await
                .expect("the gate's semaphore is never closed"),
        }
    }

    /// Take a permit only if one is free, for callers that would rather
    /// refuse a connection now than queue it.
    pub fn try_admit(&self) -> Option<AdmissionPermit> {
        self.permits
            .clone()
            .try_acquire_owned()
            .ok()
            .map(|permit| AdmissionPermit { _permit: permit })
    }

    /// How many more connections could be admitted right now.
    pub fn available(&self) -> usize {
        self.permits.available_permits()
    }

    /// The configured ceiling: `available` never exceeds it, and total
    /// outstanding permits plus `available` equals it.
    pub fn capacity(&self) -> usize {
        self.ceiling
    }
}

/// One admitted connection's hold on the gate.
///
/// Dropping it frees the slot, wherever the drop happens — including out
/// of a panicking handler. There is no way to hold a slot longer than the
/// guard lives, which is the point: capacity leaks are how servers die
/// slowly.
#[derive(Debug)]
pub struct AdmissionPermit {
    _permit: tokio::sync::OwnedSemaphorePermit,
}
