//! The outbound bound: what happens when a client stops listening.
//!
//! A write to a stream is a transfer into a buffer somebody else drains. When
//! they stop draining, a server has exactly two honest choices — block the
//! senders, or grow the queue without limit and call it buffering. These
//! tests hold the driver to the first choice: the queue never holds more than
//! its configured capacity of frames, whatever the peer does, and the cost of
//! a stalled consumer lands on the sender's latency instead of the server's
//! memory.
//!
//! The in-memory duplex makes the peer's refusal total and exact: its buffer
//! size is chosen per test, so once that many bytes sit unread, every further
//! write blocks with nothing to negotiate.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dust_net::frame::{Frame, FrameDecoder, FrameEncoder, FrameError, Limits};
use dust_net::io::{Conn, ConnConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn config(capacity: usize, limits: Limits) -> ConnConfig {
    ConnConfig {
        limits,
        outbound_capacity: capacity,
        ..ConnConfig::default()
    }
}

/// The wire length of a frame with an id and a zero-filled body.
fn wire_len(id: i32, body: usize) -> usize {
    let encoder = FrameEncoder::new(Limits::default());
    let mut out = Vec::new();
    encoder
        .encode(&Frame::new(id, vec![0u8; body]), &mut out)
        .expect("encode");
    out.len()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stalled_consumer_blocks_senders_instead_of_dropping_or_growing() {
    const CAPACITY: usize = 4;
    const TOTAL: usize = 64;
    let frame_bytes = wire_len(0, 32);

    let (mut client, server) = tokio::io::duplex(256);
    let mut conn = Conn::new(server, config(CAPACITY, Limits::default()));

    let accepted = Arc::new(AtomicUsize::new(0));
    let worker = {
        let accepted = Arc::clone(&accepted);
        tokio::spawn(async move {
            for _ in 0..TOTAL {
                if conn.send(Frame::new(0, vec![0u8; 32])).await.is_err() {
                    break;
                }
                accepted.fetch_add(1, Ordering::Relaxed);
                // Sampled from the accepting side, which is where an overrun
                // would be visible: this path is what must stop when the
                // ceiling is reached.
                assert!(
                    conn.outbound_queued() <= CAPACITY,
                    "the gauge reached {} against a capacity of {CAPACITY}",
                    conn.outbound_queued()
                );
            }
            conn
        })
    };

    // Hold the reader back long enough for the duplex buffer to fill and the
    // queue behind it to reach its ceiling. A driver that buffered instead of
    // blocking would sail past the bound here; one that dropped would never
    // deliver everything later.
    tokio::time::sleep(Duration::from_millis(120)).await;
    let stalled_at = accepted.load(Ordering::Relaxed);
    assert!(
        stalled_at < TOTAL,
        "{stalled_at} of {TOTAL} sends completed against an unread peer; \
         nothing was ever blocked"
    );

    // Unblock the consumer: the entire backlog must drain, byte for byte.
    let target = TOTAL * frame_bytes;
    let mut received = 0usize;
    let drained = tokio::time::timeout(Duration::from_secs(5), async {
        while received < target {
            let mut chunk = [0u8; 4096];
            let n = client.read(&mut chunk).await.expect("peer read");
            assert!(n > 0, "the stream ended {received} of {target} bytes in");
            received += n;
        }
    })
    .await;
    assert!(drained.is_ok(), "the peer never received the whole backlog");

    let conn = tokio::time::timeout(Duration::from_secs(5), worker)
        .await
        .expect("the sender stayed wedged")
        .expect("sender joins");
    assert_eq!(
        accepted.load(Ordering::Relaxed),
        TOTAL,
        "blocking is not dropping: every frame was eventually accepted"
    );
    assert_eq!(conn.outbound_queued(), 0, "the queue drained empty");
}

#[tokio::test(flavor = "multi_thread")]
async fn frames_leave_in_the_order_they_were_accepted() {
    // FIFO under pressure: fill the pipeline well past its depth while the
    // peer dawdles, then verify the ids arrive ascending. An outbound queue
    // that reordered under load would quietly corrupt every sequence of
    // packets that ever went through it.
    const COUNT: usize = 48;
    let (mut client, server) = tokio::io::duplex(128);
    let mut conn = Conn::new(server, config(4, Limits::default()));

    let worker = tokio::spawn(async move {
        for i in 0..COUNT as i32 {
            conn.send(Frame::new(i, vec![i as u8; 16]))
                .await
                .expect("queued");
        }
        conn.close().await.expect("graceful close flushes")
    });

    let mut seen = Vec::new();
    let mut decoder = FrameDecoder::new(Limits::default());
    let deadline = Instant::now() + Duration::from_secs(5);
    while seen.len() < COUNT {
        assert!(
            Instant::now() < deadline,
            "reading the sequence took too long"
        );
        match decoder.next_frame().expect("decode") {
            Some(frame) => seen.push(frame.id),
            None => {
                if decoder.buffered() == 0 {
                    let mut chunk = [0u8; 1024];
                    let n = client.read(&mut chunk).await.expect("peer read");
                    assert!(n > 0, "stream ended after {} of {COUNT} frames", seen.len());
                    decoder.feed(&chunk[..n]);
                }
            }
        }
    }

    let expected: Vec<i32> = (0..COUNT as i32).collect();
    assert_eq!(seen, expected, "order must survive the queue");
    worker.await.expect("closer joins");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_oversized_frame_is_refused_even_while_the_writer_is_wedged() {
    // Encoding happens on the accepting side precisely so that this fails
    // here — immediately, whether or not the writer is stuck, and without
    // occupying a queue slot a well-formed frame could use.
    let (mut client, server) = tokio::io::duplex(32);
    let tiny = Limits {
        max_frame_len: 64,
        max_decompressed_len: 64,
    };
    let mut conn = Conn::new(server, config(8, tiny));

    // Wedge the writer: one legal-sized frame, larger than the duplex
    // buffer, against a peer that never reads. Every later write now blocks.
    conn.send(Frame::new(0, vec![0u8; 40]))
        .await
        .expect("the legal frame was accepted");
    tokio::time::sleep(Duration::from_millis(60)).await;

    // Against that wedged writer, an oversized frame is still refused
    // immediately — the check cannot be waiting behind the queue, because
    // there is no version of this call that waits at all.
    let started = Instant::now();
    let error = conn
        .send(Frame::new(0, vec![0u8; 500]))
        .await
        .expect_err("an oversized frame must be refused");
    assert!(
        started.elapsed() < Duration::from_millis(50),
        "refusal waited {:?}; it must not depend on the writer",
        started.elapsed()
    );
    match error {
        dust_net::io::ConnError::Protocol(FrameError::Oversize { len, limit }) => {
            assert_eq!(limit, 64);
            assert!(len > limit);
        }
        other => panic!("expected Oversize, got {other:?}"),
    }

    // And the connection itself is unharmed: refusing a caller bug is not
    // ending the session. The next legal frame is accepted (it will queue
    // behind the wedged writer, which is ordinary backpressure).
    conn.send(Frame::new(7, b"fine"))
        .await
        .expect("a legal frame after a refused one");

    conn.abort();
    let _ = client.shutdown().await;
}
