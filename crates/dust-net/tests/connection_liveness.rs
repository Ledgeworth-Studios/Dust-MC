//! The liveness policy, end to end over an in-memory connection.
//!
//! The unit tests inside `io` pin the arithmetic of choosing a window; these
//! run the whole driver against a peer that misbehaves in the four ways peers
//! actually misbehave: going silent, stalling deliberately, hanging up mid-
//! frame, and refusing to listen. Every duration here is tens of
//! milliseconds — long enough that a slow CI box cannot spuriously fail,
//! short enough that the suite stays fast — and every upper bound is seconds
//! wide for the same reason.

use std::time::{Duration, Instant};

use dust_net::frame::{Frame, FrameEncoder};
use dust_net::io::{Conn, ConnConfig, ConnError, Timeouts};
use dust_net::state::State;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

fn config(idle: Option<Duration>, pre_auth: Option<Duration>) -> ConnConfig {
    ConnConfig {
        timeouts: Timeouts {
            idle,
            pre_auth_budget: pre_auth,
        },
        ..ConnConfig::default()
    }
}

/// A wire form of one frame, written by hand so the tests say exactly which
/// bytes are in flight.
fn wire_of(id: i32, body: &[u8]) -> Vec<u8> {
    let encoder = FrameEncoder::new(dust_net::frame::Limits::default());
    let mut out = Vec::new();
    encoder
        .encode(&Frame::new(id, body), &mut out)
        .expect("encode");
    out
}

/// One end of an in-memory connection the tests drive by hand.
fn pair(buffer: usize) -> (DuplexStream, Conn<DuplexStream>) {
    let (client, server) = tokio::io::duplex(buffer);
    (
        client,
        Conn::new(server, config(Some(Duration::from_secs(30)), None)),
    )
}

// ---------------------------------------------------------------------------
// Silence.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_peer_that_sends_nothing_hits_the_idle_timeout() {
    // The client half stays alive and silent: an open socket with nobody
    // talking is the situation the idle clock exists for, as opposed to a
    // closed one, which is what EOF is for.
    let (client, server) = tokio::io::duplex(1024);
    let mut stalled = Conn::new(server, config(Some(Duration::from_millis(40)), None));
    let started = Instant::now();
    let outcome = stalled.next_frame().await;

    let elapsed = started.elapsed();
    match outcome {
        Err(ConnError::IdleTimeout { limit }) => {
            assert_eq!(limit, Duration::from_millis(40));
        }
        other => panic!("expected the idle timeout, got {other:?}"),
    }
    assert!(
        elapsed >= Duration::from_millis(40),
        "fired early: {elapsed:?}"
    );
    assert!(elapsed < Duration::from_secs(3), "fired late: {elapsed:?}");
    assert!(stalled.has_ended());
    drop(client);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_idle_timeout_applies_after_authentication_too() {
    // The point of this test is the *variant*, not the firing: an
    // unauthenticated-only deadline would leave every authenticated
    // connection free to hold its socket forever.
    let (client, server) = tokio::io::duplex(1024);
    let mut server = Conn::new(server, config(Some(Duration::from_millis(40)), None));
    server.handshake(2).expect("login intent");
    server
        .transition(State::Configuration)
        .expect("authentication complete");

    let started = Instant::now();
    let outcome = tokio::time::timeout(Duration::from_secs(3), server.next_frame())
        .await
        .expect("the idle clock never fired");

    match outcome {
        Err(ConnError::IdleTimeout { limit }) => {
            assert_eq!(limit, Duration::from_millis(40));
        }
        other => panic!("expected an idle timeout, got {other:?}"),
    }
    assert!(started.elapsed() < Duration::from_secs(3));
    let _ = client;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_pre_auth_deadline_fires_despite_steady_traffic() {
    // The attack the idle clock cannot see: stay under it by dribbling, and
    // never get anywhere. Only the total budget answers this, and it must
    // answer despite bytes arriving the whole time.
    let (client, server) = tokio::io::duplex(1024);
    let mut server = Conn::new(
        server,
        config(
            Some(Duration::from_millis(200)),
            Some(Duration::from_millis(80)),
        ),
    );

    let dribble = tokio::spawn(async move {
        let mut client = client;
        // Four bytes is short of any complete length prefix, so the dribble
        // never produces a frame for the decoder to rule on.
        for byte in [0x81u8, 0x41, 0x42, 0x43, 0x44, 0x45] {
            client.write_all(&[byte]).await.expect("dribble");
            client.flush().await.expect("dribble");
            tokio::time::sleep(Duration::from_millis(12)).await;
        }
        // Hold the socket open afterwards: silence from here must not turn
        // the outcome into an idle timeout.
        tokio::time::sleep(Duration::from_millis(600)).await;
    });

    let started = Instant::now();
    let outcome = server.next_frame().await;
    let elapsed = started.elapsed();

    assert!(
        matches!(
            outcome,
            Err(ConnError::PreAuthDeadline {
                budget
            }) if budget == Duration::from_millis(80)
        ),
        "expected the pre-authentication deadline, got {outcome:?}"
    );
    // Around eighty milliseconds, certainly not the two hundred the idle
    // clock would have waited.
    assert!(elapsed >= Duration::from_millis(60), "{elapsed:?}");
    assert!(elapsed < Duration::from_millis(190), "{elapsed:?}");
    dribble.await.expect("dribbler joins");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_budget_stops_applying_once_authentication_completes() {
    // A legitimate player on a slow link: past the point where the budget
    // would have expired, but authenticated, and therefore allowed to sit
    // quiet for longer than the budget ever permitted.
    let (mut client, mut server) = pair(1024);
    server.handshake(2).expect("login intent");
    server
        .transition(State::Configuration)
        .expect("authenticated");

    // Outlive the fifty-millisecond budget before saying anything.
    tokio::time::sleep(Duration::from_millis(120)).await;
    client
        .write_all(&wire_of(0x05, b"late"))
        .await
        .expect("write");
    client.flush().await.expect("flush");

    let frame = tokio::time::timeout(Duration::from_secs(3), server.next_frame())
        .await
        .expect("not stuck")
        .expect("no protocol error");
    assert_eq!(frame, Some(Frame::new(0x05, b"late")));
    let _ = client.shutdown().await;
}

// ---------------------------------------------------------------------------
// Hanging up.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_clean_hangup_between_frames_is_ok_none() {
    let (mut client, mut server) = pair(1024);
    client
        .write_all(&wire_of(0x01, b"ping"))
        .await
        .expect("write");
    let frame = server.next_frame().await.expect("read").expect("a frame");
    assert_eq!(frame, Frame::new(0x01, b"ping"));

    drop(client);
    let ended = tokio::time::timeout(Duration::from_secs(3), server.next_frame())
        .await
        .expect("eof did not arrive");
    match ended {
        Ok(None) => {}
        other => panic!("clean EOF between frames is not an error: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn hanging_up_mid_frame_is_a_truncation_not_a_clean_end() {
    let (mut client, mut server) = pair(1024);
    // Two bytes of a length prefix claiming three hundred: a frame that
    // will never finish.
    client.write_all(&[0xAC, 0x02]).await.expect("write");
    tokio::time::sleep(Duration::from_millis(20)).await;
    drop(client);

    let outcome = tokio::time::timeout(Duration::from_secs(3), server.next_frame())
        .await
        .expect("eof did not arrive");
    match outcome {
        Err(ConnError::TruncatedFrame { pending }) => {
            assert_eq!(pending, 2, "half a frame must be named as such");
        }
        other => panic!("expected a truncation, got {other:?}"),
    }
    assert!(server.has_ended());
    assert!(matches!(server.next_frame().await, Err(ConnError::Closed)));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_frame_split_across_slow_reads_is_reassembled() {
    let (mut client, mut server) = pair(64);
    let wire = wire_of(0x2C, &[0x9F; 90]);
    let dribble = tokio::spawn(async move {
        for piece in wire.chunks(3) {
            client.write_all(piece).await.expect("piece");
            client.flush().await.expect("piece");
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    });
    let frame = tokio::time::timeout(Duration::from_secs(3), server.next_frame())
        .await
        .expect("reassembling stalled")
        .expect("reassembled frame is valid");
    assert_eq!(
        frame,
        Some(Frame::new(0x2C, vec![0x9F; 90])),
        "reassembled frame is intact"
    );
    dribble.await.expect("writer joins");
}

// ---------------------------------------------------------------------------
// Ending well and ending now.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_graceful_close_flushes_then_signals_eof() {
    // Both ends are drivers here: the receiving end proves the guarantee by
    // getting the frame and then the EOF, in that order, off the same stream.
    let (client, server) = tokio::io::duplex(64);
    let mut receiver = Conn::new(client, config(None, None));
    let mut sender = Conn::new(server, config(None, None));

    sender
        .send(Frame::new(0x07, b"last words"))
        .await
        .expect("queued");
    sender.close().await.expect("graceful close flushes");

    let frame = tokio::time::timeout(Duration::from_secs(3), receiver.next_frame())
        .await
        .expect("close left the reader waiting")
        .expect("flushed frame is intact");
    assert_eq!(frame, Some(Frame::new(0x07, b"last words")));

    let ended = tokio::time::timeout(Duration::from_secs(3), receiver.next_frame())
        .await
        .expect("eof did not arrive");
    match ended {
        Ok(None) => {}
        other => panic!("expected the clean end of the stream, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_with_a_jammed_queue_still_delivers_everything_accepted() {
    // The hard version of the graceful guarantee: the peer stops reading
    // *while* frames are being accepted, and the close still owes them all.
    let (client, server) = tokio::io::duplex(256);
    let mut sender = Conn::new(
        server,
        ConnConfig {
            outbound_capacity: 8,
            ..config(None, None)
        },
    );
    let wire = wire_of(0x09, &[0xEE; 40]);

    let mut accepted = 0;
    let _ = tokio::time::timeout(Duration::from_millis(150), async {
        loop {
            if sender.send(Frame::new(0x09, vec![0xEE; 40])).await.is_err() {
                break;
            }
            accepted += 1;
        }
    })
    .await;
    assert!(accepted > 0, "nothing was ever accepted");
    assert!(
        sender.outbound_queued() <= 8,
        "the queue held {} frames against a bound of eight",
        sender.outbound_queued()
    );

    let close = tokio::spawn(async move { sender.close().await });
    let mut received = Vec::new();
    let mut client = client;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let mut chunk = [0u8; 256];
            let n = client.read(&mut chunk).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            received.extend_from_slice(&chunk[..n]);
        }
    })
    .await
    .expect("draining the peer took too long");
    let closed = tokio::time::timeout(Duration::from_secs(3), close)
        .await
        .expect("close never finished")
        .expect("close task joined");
    assert!(closed.is_ok(), "graceful close failed: {closed:?}");

    // Every accepted frame arrived: the byte count matches what was queued.
    assert_eq!(
        received.len(),
        accepted * wire.len(),
        "{} frames accepted, {} bytes arrived",
        accepted,
        received.len()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_abort_discards_the_backlog_and_returns_promptly() {
    let (client, server) = tokio::io::duplex(256);
    let mut sender = Conn::new(
        server,
        ConnConfig {
            outbound_capacity: 8,
            ..config(None, None)
        },
    );

    // Wedge the writer: the peer never reads, the duplex buffer fills, and
    // the queue behind it fills too.
    let mut accepted = 0usize;
    let _ = tokio::time::timeout(Duration::from_millis(150), async {
        loop {
            if sender.send(Frame::new(0x09, vec![0xEE; 40])).await.is_err() {
                break;
            }
            accepted += 1;
        }
    })
    .await;
    assert!(sender.outbound_queued() > 0, "expected a jammed queue");

    let started = Instant::now();
    sender.abort();
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "abort waited {:?} on a connection it owed nothing",
        started.elapsed()
    );

    // The peer sees a truncated stream: whatever escaped before the abort,
    // then EOF, promptly.
    let mut client = client;
    let mut received = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let mut chunk = [0u8; 128];
            let n = client.read(&mut chunk).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            received.extend_from_slice(&chunk[..n]);
        }
    })
    .await
    .expect("the peer was never told");
    assert!(
        received.len() < accepted * 44,
        "the whole backlog ({}) survived an abort; something flushed it",
        received.len()
    );
}
