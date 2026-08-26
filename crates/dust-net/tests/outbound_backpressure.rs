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
async fn a_stalled_consumer_blocks_sends_at_the_bound_and_recovers() {
    // Sequential by design: one producer, so every observation below is
    // free of counter races, and the bound shows itself as the thing it is —
    // the sixth send waiting for room that a wedged writer cannot make.
    const CAPACITY: usize = 4;
    const REST: usize = 11;

    // The duplex swallows fewer bytes than one frame, so the very first
    // write wedges partway through and stays wedged while nobody reads.
    let (mut client, server) = tokio::io::duplex(32);
    let mut conn = Conn::new(server, config(CAPACITY, Limits::default()));
    let body = vec![0u8; 32];
    let frame_bytes = wire_len(0, 32);

    // One accepted; the writer takes it off the queue and sticks.
    conn.send(Frame::new(0, body.clone())).await.expect("first");
    tokio::time::sleep(Duration::from_millis(60)).await;

    // Fill the rest of the queue. Each of these returns promptly: accepting
    // is cheap right up to the ceiling, which is the whole point.
    for _ in 1..=CAPACITY {
        conn.send(Frame::new(0, body.clone())).await.expect("fill");
    }
    let gauge = conn.outbound_queued();
    assert!(
        gauge >= 1 && gauge <= CAPACITY + 1,
        "the gauge read {gauge}; it must sit at the ceiling, allowing for \
         the writer's moment of take-lag"
    );

    // Past the ceiling there is no fifth room: this send waits, and keeps
    // waiting, for as long as the peer refuses to read.
    let blocked = tokio::time::timeout(Duration::from_millis(400), async {
        conn.send(Frame::new(0, body.clone())).await
    })
    .await;
    assert!(
        blocked.is_err(),
        "a send past the bound did not block; the queue grew or the send lied"
    );

    // Unblock the pipe by reading while the rest is sent: the writer is
    // still wedged on frame one's tail, so acceptance cannot resume until
    // the peer starts draining. One accounting note the code has to live
    // with: the probe above was cancelled mid-send, and a cancelled send may
    // (by tokio's documented behaviour) have landed its frame in the queue
    // anyway. The total on the wire is therefore the frames certainly
    // accepted, plus zero or one; the assertion below admits exactly that
    // window, and nothing else — any other count means frames were lost,
    // duplicated, or torn.
    let received = Arc::new(AtomicUsize::new(0));
    let drainer = {
        let received = Arc::clone(&received);
        tokio::spawn(async move {
            let mut client = client;
            // Read right up to the shutdown the graceful close promises.
            loop {
                let mut chunk = [0u8; 4096];
                let n = client.read(&mut chunk).await.expect("peer read");
                if n == 0 {
                    break;
                }
                received.fetch_add(n, Ordering::Relaxed);
            }
        })
    };

    for _ in 0..REST {
        conn.send(Frame::new(0, body.clone())).await.expect("rest");
    }
    conn.close().await.expect("graceful close flushes");
    drainer.await.expect("drainer joins");

    fn frames_of(bytes: usize, frame: usize) -> usize {
        assert_eq!(
            bytes % frame,
            0,
            "{bytes} bytes is not whole frames of {frame}"
        );
        bytes / frame
    }
    let delivered = frames_of(received.load(Ordering::Relaxed), frame_bytes);
    let certain = 1 + CAPACITY + REST;
    assert!(
        delivered == certain || delivered == certain + 1,
        "{delivered} frames arrived against {certain} certain accepts plus at          most one cancelled-but-maybe-sent probe"
    );

    // And the driver took work again afterwards, which is the recovery half
    // of the story: blocking was temporary, not terminal.
    let (_client_io, probe_server) = tokio::io::duplex(4096);
    let mut probe_conn = Conn::new(probe_server, config(CAPACITY, Limits::default()));
    probe_conn
        .send(Frame::new(9, b"recovered"))
        .await
        .expect("a fresh connection still works");
    probe_conn.abort();
}

/// How many frames the stalled consumer receives: the certainly-accepted
/// ones, plus the probe that may or may not have landed.

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
    let deadline = Instant::now() + Duration::from_secs(30);
    while seen.len() < COUNT {
        assert!(
            Instant::now() < deadline,
            "reading the sequence took too long"
        );
        match decoder.next_frame().expect("decode") {
            Some(frame) => seen.push(frame.id),
            // No complete frame, whether the buffer is empty or holding a
            // partial one: read more. Skipping the read while a partial
            // frame sits buffered would spin forever against a writer that
            // cannot finish its frame until somebody reads — which is a
            // deadlock this test once genuinely produced.
            None => {
                let mut chunk = [0u8; 1024];
                let n = client.read(&mut chunk).await.expect("peer read");
                assert!(n > 0, "stream ended after {} of {COUNT} frames", seen.len());
                decoder.feed(&chunk[..n]);
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
        started.elapsed() < Duration::from_millis(250),
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
