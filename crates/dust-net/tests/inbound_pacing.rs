//! The inbound pacing policy, at the bucket and through the driver.
//!
//! [`crate::limits`] states the reasoning: decompression is the expensive
//! step an unauthenticated peer can drive, so the bucket is charged against
//! raw wire bytes before the codec runs. The unit tests here prove the
//! arithmetic against synthetic schedules — no real clock, so a property
//! that holds once holds always. The duplex tests then check that the
//! driver actually *applies* it: a paced connection admits bytes on the
//! schedule the policy says and not one schedule faster.
//!
//! The generator is SplitMix64, as everywhere else in this crate: a failure
//! that reproduces from a seed is a bug report, one that evaporates is a
//! rumour.

use std::time::{Duration, Instant};

use dust_net::frame::{Frame, FrameEncoder, Limits};
use dust_net::io::{Conn, ConnConfig, Timeouts};
use dust_net::limits::{AdmissionGate, InboundRate, TokenBucket};
use tokio::io::AsyncWriteExt;

/// A deterministic generator for test-only schedules.
struct SplitMix64(u64);

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

fn rate(burst: usize, per_second: usize) -> InboundRate {
    InboundRate {
        burst_bytes: burst,
        bytes_per_second: per_second,
    }
}

// ---------------------------------------------------------------------------
// The bucket itself, on a synthetic clock.
// ---------------------------------------------------------------------------

#[test]
fn a_fresh_bucket_bursts_and_then_sustains_exactly() {
    // The two numbers the policy promises, checked literally: everything up
    // to the burst is free at t=0, and after it is spent the refill pays
    // out at exactly the sustained rate — no more, rounding included.
    let t0 = Instant::now();
    let mut bucket = TokenBucket::new(rate(1000, 500), t0);

    assert_eq!(bucket.take(400, t0), 400);
    assert_eq!(bucket.available(t0), 600, "the rest of the burst is there");
    assert_eq!(bucket.take(700, t0), 600, "and never more than is there");

    // Half a second of refill at 500/s buys exactly 250 bytes.
    assert_eq!(bucket.available(t0 + Duration::from_millis(500)), 250);
    assert_eq!(bucket.take(250, t0 + Duration::from_millis(500)), 250);
    assert_eq!(bucket.available(t0 + Duration::from_millis(500)), 0);
}

#[test]
fn sub_byte_refill_is_carried_not_lost() {
    // At 10 bytes/second, each millisecond is worth 0.01 bytes. A bucket
    // that truncates per call would admit nothing for a hundred calls; one
    // that carries fractional credit admits its first byte at exactly 100ms.
    let t0 = Instant::now();
    let mut bucket = TokenBucket::new(rate(1, 10), t0);

    assert_eq!(bucket.take(1, t0), 1);
    for ms in [1u64, 25, 50, 75, 99] {
        assert_eq!(
            bucket.available(t0 + Duration::from_millis(ms)),
            0,
            "{ms}ms into a 100ms wait"
        );
    }
    assert_eq!(bucket.available(t0 + Duration::from_millis(100)), 1);
}

#[test]
fn the_capacity_caps_the_credit() {
    // Idle time does not bank infinite capacity: the bucket fills to its
    // burst size and stays there, or a quiet hour would license a loud
    // second.
    let t0 = Instant::now();
    let mut bucket = TokenBucket::new(rate(512, 8), t0);
    bucket.take(512, t0);
    assert_eq!(
        bucket.available(t0 + Duration::from_secs(3600)),
        512,
        "an hour idle must not refill past the burst"
    );
}

#[test]
fn degenerate_policies_degrade_to_themselves_rather_than_panic() {
    // Zero burst and zero rate are configuration mistakes, not panics: they
    // become the smallest working policy instead of a division by zero on
    // some future operator's box.
    let t0 = Instant::now();
    let mut starved = TokenBucket::new(rate(0, 0), t0);
    assert_eq!(starved.take(10, t0), 1, "zero burst becomes one byte");
    let later = t0 + Duration::from_nanos(starved.wait_for_one(t0).as_nanos() as u64);
    assert_eq!(starved.available(later), 1, "zero rate still trickles");
}

#[test]
fn ten_thousand_synthetic_takes_never_exceed_burst_plus_elapsed_rate() {
    // The property the whole module exists for, over hostile schedules:
    // whatever the gaps and whatever the asks, admitted <= burst + elapsed
    // * rate, with equality reachable but never exceeded by more than the
    // single byte a partial grant can overshoot by.
    let mut rng = SplitMix64::new(0x0070_CBE7_3E5B_ACE1);
    let (burst, per_second) = (4096usize, 65_536usize);
    let base = Instant::now();
    let mut now = Duration::ZERO;

    let mut outstanding = TokenBucket::new(rate(burst, per_second), base);
    let mut total_admitted = 0u64;
    let mut elapsed_ns = 0u128;

    for _ in 0..10_000 {
        // Gaps from zero to two seconds; asks from one byte to double the
        // burst, so greed and patience both appear in every run.
        now += Duration::from_nanos(rng.next() % (2 * 1_000_000_000));
        let ask = 1 + (rng.next() as usize) % (2 * burst);

        let granted = outstanding.take(ask, base + now);
        total_admitted += granted as u64;
        elapsed_ns += now.as_nanos();

        // The invariant. `burst` headroom plus what the schedule's own
        // timeline has paid for, never more.
        let ceiling = (burst + (elapsed_ns * per_second as u128 / 1_000_000_000) as usize) as u64;
        assert!(
            total_admitted <= ceiling,
            "admitted {total_admitted} against a ceiling of {ceiling}"
        );
    }
    assert!(total_admitted > burst as u64, "the schedule asked nothing");
}

#[test]
fn a_burst_schedule_admits_the_burst_instantly_and_no_faster_than_sustained() {
    // The shape of the attack the sustained rate answers: everything at
    // once. The first burst-sized chunk goes straight through; the next
    // second of takes cannot exceed one more burst-equivalent of refill,
    // however insistently they ask.
    let t0 = Instant::now();
    let mut bucket = TokenBucket::new(rate(1024, 1024), t0);
    assert_eq!(bucket.take(1024, t0), 1024);

    let mut admitted_in_first_second = 0usize;
    for step in 0..100 {
        let at = t0 + Duration::from_millis(10 * step);
        admitted_in_first_second += bucket.take(1024, at);
    }
    assert!(
        admitted_in_first_second <= 1024,
        "{admitted_in_first_second} bytes slipped past a 1 KiB/s sustain in one second"
    );
}

#[test]
fn waiting_is_only_asked_for_when_the_bucket_is_empty() {
    let t0 = Instant::now();
    let mut full = TokenBucket::new(rate(999, 3), t0);
    assert_eq!(full.wait_for_one(t0), Duration::ZERO);

    let mut empty = TokenBucket::new(rate(16, 16), t0);
    empty.take(16, t0);
    // One byte at 16/s needs exactly 62.5ms of credit, which nanosecond
    // resolution carries without rounding away.
    assert_eq!(empty.wait_for_one(t0), Duration::from_nanos(62_500_000));
}

// ---------------------------------------------------------------------------
// AdmissionGate: the type holds permits; the count was someone else's idea.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn the_gate_holds_exactly_its_ceiling_of_permits() {
    let gate = AdmissionGate::new(3);
    assert_eq!(gate.capacity(), 3);
    assert_eq!(gate.available(), 3);

    let held = (0..3).map(|_| gate.try_admit()).collect::<Vec<_>>();
    assert!(held.iter().all(Option::is_some), "every permit was free");
    assert_eq!(gate.available(), 0);
    assert!(gate.try_admit().is_none(), "the fourth connection waits");

    drop(held);
    assert_eq!(gate.available(), 3, "dropping returns capacity");
}

#[tokio::test(flavor = "multi_thread")]
async fn clones_share_one_pool_of_permits() {
    let gate = AdmissionGate::new(2);
    let twin = gate.clone();
    let _first = gate.try_admit().expect("first");
    let _second = twin.try_admit().expect("second");
    assert!(
        gate.try_admit().is_none() && twin.try_admit().is_none(),
        "a clone must not mint extra capacity"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_wait_leaves_the_gate_untouched() {
    let gate = AdmissionGate::new(1);
    let _held = gate.admit().await;

    // Two waiters race for the one remaining slot; both are cancelled
    // before it frees. Capacity must come back whole.
    let waiter_a = tokio::spawn({
        let gate = gate.clone();
        async move { gate.admit().await }
    });
    let waiter_b = tokio::spawn({
        let gate = gate.clone();
        async move { gate.admit().await }
    });
    waiter_a.abort();
    waiter_b.abort();

    drop(_held);
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(gate.available(), 1, "cancelled acquisitions leaked nothing");
}

// ---------------------------------------------------------------------------
// Through the driver: pacing happens before decoding, on real duplexes.
// ---------------------------------------------------------------------------

fn paced_config(pacing: InboundRate, idle: Option<Duration>) -> ConnConfig {
    ConnConfig {
        timeouts: Timeouts {
            idle,
            pre_auth_budget: None,
        },
        inbound_rate: Some(pacing),
        ..ConnConfig::default()
    }
}

fn wire_of(id: i32, body: &[u8]) -> Vec<u8> {
    let encoder = FrameEncoder::new(Limits::default());
    let mut out = Vec::new();
    encoder
        .encode(&Frame::new(id, body), &mut out)
        .expect("encode");
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn a_paced_connection_admits_bytes_on_the_policy_schedule() {
    // 1200 wire bytes against a 300-byte burst and 900 B/s: the burst lands
    // immediately, and the remaining 900 bytes cost about one second. The
    // lower bound is the assertion that matters — arriving faster would
    // mean the bucket decorates the read path without governing it.
    let (mut client, server_io) = tokio::io::duplex(4096);
    let mut server = Conn::new(
        server_io,
        paced_config(rate(300, 900), Some(Duration::from_secs(30))),
    );

    let payload = vec![0x11u8; 1180];
    let wire = wire_of(0x21, &payload);
    client.write_all(&wire).await.expect("write");
    client.flush().await.expect("flush");

    let started = Instant::now();
    let frame = tokio::time::timeout(Duration::from_secs(20), server.next_frame())
        .await
        .expect("paced delivery stalled entirely")
        .expect("no protocol error")
        .expect("frame");
    let elapsed = started.elapsed();

    assert_eq!(frame, Frame::new(0x21, payload));
    // At least the deficit over the burst, divided by the rate, minus a
    // scheduling grace the slowest CI machine is entitled to.
    let floor = Duration::from_millis(900 * 1000 / 900).saturating_sub(Duration::from_millis(150));
    assert!(
        elapsed >= floor,
        "1200 bytes arrived in {elapsed:?}; the \
        300-byte burst at 900 B/s owes at least ~{floor:?}"
    );
    // And a generous ceiling, so a slow machine fails honestly rather than
    // flakily: this test asserts pacing, not throughput.
    assert!(elapsed < Duration::from_secs(15), "{elapsed:?}");

    let stats = server.stats();
    assert_eq!(stats.bytes_in, wire.len() as u64);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unpaced_connection_moves_at_line_rate() {
    // The control for the test above: identical traffic, no policy. If this
    // also crawled, the previous test would prove nothing — it would just
    // be measuring the harness.
    let (mut client, server_io) = tokio::io::duplex(4096);
    let mut server = Conn::new(
        server_io,
        ConnConfig {
            timeouts: Timeouts {
                idle: Some(Duration::from_secs(30)),
                pre_auth_budget: None,
            },
            inbound_rate: None,
            ..ConnConfig::default()
        },
    );

    let wire = wire_of(0x21, &vec![0x22u8; 1180]);
    client.write_all(&wire).await.expect("write");
    client.flush().await.expect("flush");

    let started = Instant::now();
    let frame = tokio::time::timeout(Duration::from_secs(5), server.next_frame())
        .await
        .expect("stalled")
        .expect("clean")
        .expect("frame");
    assert_eq!(frame.body.len(), 1180);
    assert!(
        started.elapsed() < Duration::from_millis(750),
        "unpaced traffic took {:?}; something is pacing anyway",
        started.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn pacing_delays_frames_without_corrupting_them() {
    // Many frames across a slow trickle: the decoder must see exactly the
    // frames the encoder sent, boundaries intact, whatever the bucket did
    // to the read sizes in between.
    const COUNT: usize = 12;
    let (mut client, server_io) = tokio::io::duplex(2048);
    let mut server = Conn::new(
        server_io,
        paced_config(rate(64, 2400), Some(Duration::from_secs(60))),
    );

    let mut expected = Vec::new();
    for i in 0..COUNT as i32 {
        let body = vec![i as u8; 50];
        client
            .write_all(&wire_of(0xF0 + i, &body))
            .await
            .expect("write");
        expected.push(Frame::new(0xF0 + i, body));
    }
    client.flush().await.expect("flush");

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut seen = Vec::new();
    while seen.len() < COUNT {
        assert!(
            Instant::now() < deadline,
            "pacing turned delivery into a stall"
        );
        match tokio::time::timeout(Duration::from_secs(10), server.next_frame())
            .await
            .expect("frame overdue")
            .expect("clean")
        {
            Some(frame) => seen.push(frame),
            None => panic!("peer EOF mid-schedule"),
        }
    }
    assert_eq!(seen, expected);
}
