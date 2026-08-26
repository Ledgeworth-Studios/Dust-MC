//! The counters, driven end to end.
//!
//! [`crate::metrics`] explains why the driver counts instead of logs. These
//! tests hold the counts to the driver's own delivery semantics: a frame is
//! counted inbound when it is handed up complete, outbound when it is
//! accepted for sending, and bytes are counted as wire cost on both sides —
//! before decryption and decompression coming in, after encryption going
//! out. A counter that measured anything else would still be *a* number,
//! just not the one the docs promise.

use std::time::Duration;

use dust_net::crypt::SharedSecret;
use dust_net::frame::{Frame, FrameEncoder, Limits};
use dust_net::io::{Conn, ConnConfig, ConnError, Timeouts};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn config(idle: Option<Duration>) -> ConnConfig {
    ConnConfig {
        timeouts: Timeouts {
            idle,
            pre_auth_budget: None,
        },
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
async fn frames_and_bytes_are_counted_in_both_directions() {
    let (mut client_raw, server_io) = tokio::io::duplex(4096);
    let mut server = Conn::new(server_io, config(Some(Duration::from_secs(10))));

    // Two frames up the wire, then one down.
    let first = wire_of(0x01, b"hello");
    let second = wire_of(0x02, &[0x7; 100]);
    client_raw.write_all(&first).await.expect("write");
    client_raw.write_all(&second).await.expect("write");
    client_raw.flush().await.expect("flush");

    let up1 = server.next_frame().await.expect("read").expect("frame");
    let up2 = server.next_frame().await.expect("read").expect("frame");
    assert_eq!(up1.id, 0x01);
    assert_eq!(up2.body.len(), 100);

    server.send(Frame::new(0x40, b"down")).await.expect("send");

    // Wait until the socket has actually taken the frame, so the writer's
    // byte count has settled before it is read.
    let expected = wire_of(0x40, b"down");
    let mut seen = vec![0u8; expected.len()];
    tokio::time::timeout(Duration::from_secs(5), client_raw.read_exact(&mut seen))
        .await
        .expect("drain")
        .expect("read");
    assert_eq!(seen, expected);

    let stats = server.stats();
    assert_eq!(stats.frames_in, 2);
    // Wire cost, prefix included — not payload size.
    assert_eq!(stats.bytes_in, (first.len() + second.len()) as u64);

    // Accepted at send time; the close above flushed them to the socket, so
    // the written total has settled by the time the peer saw EOF.
    assert_eq!(stats.frames_out, 1);
    assert_eq!(stats.bytes_out, expected.len() as u64);
    assert_eq!(stats.total_errors(), 0);

    server.close().await.expect("close flushes");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_idle_timeout_is_counted_once_as_itself() {
    let (client, server_io) = tokio::io::duplex(1024);
    let mut server = Conn::new(server_io, config(Some(Duration::from_millis(40))));

    let outcome = server.next_frame().await;
    assert!(matches!(outcome, Err(ConnError::IdleTimeout { .. })));
    // Polling again afterwards returns Closed — the absence of a connection,
    // not a second event — so the count must stay at one.
    assert!(matches!(server.next_frame().await, Err(ConnError::Closed)));

    let stats = server.stats();
    assert_eq!(stats.idle_timeouts, 1);
    assert_eq!(stats.pre_auth_deadlines, 0);
    drop(client);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_malformed_frame_is_counted_as_a_protocol_error() {
    let (mut client, server_io) = tokio::io::duplex(1024);
    let mut server = Conn::new(server_io, config(Some(Duration::from_secs(10))));

    // A length prefix whose final byte sets bits beyond an i32: the VarInt
    // reader refuses it as itself, and the codec surfaces that refusal.
    client
        .write_all(&[0xff, 0xff, 0xff, 0xff, 0x7f])
        .await
        .expect("write");

    let outcome = server.next_frame().await;
    assert!(matches!(
        outcome,
        Err(ConnError::Protocol(dust_net::frame::FrameError::Length(
            dust_net::varint::VarIntError::Overflow { .. }
        )))
    ));
    let stats = server.stats();
    assert_eq!(stats.protocol_errors, 1);
    assert_eq!(stats.total_errors(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_truncated_hangup_is_counted_as_its_own_kind() {
    let (mut client, server_io) = tokio::io::duplex(1024);
    let mut server = Conn::new(server_io, config(Some(Duration::from_secs(10))));

    // Half a length prefix that promises a frame which never comes.
    client.write_all(&[0xac, 0x02]).await.expect("write");
    tokio::time::sleep(Duration::from_millis(20)).await;
    drop(client);

    let outcome = server.next_frame().await;
    assert!(matches!(
        outcome,
        Err(ConnError::TruncatedFrame { pending: 2 })
    ));
    assert_eq!(server.stats().truncated_frames, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn encrypted_egress_is_counted_at_its_wire_size() {
    // After the switch, what leaves is ciphertext: the byte count that
    // matters to the network is the encrypted length, and since CFB8 does
    // not pad, that equals the plaintext framing — but the count must be
    // taken after the cipher ran regardless, because that is the invariant
    // a future mode with expansion would depend on.
    let secret = SharedSecret::from_bytes([0x5A; 16]);
    let (mut client_raw, server_io) = tokio::io::duplex(4096);
    let mut server = Conn::new(server_io, config(Some(Duration::from_secs(10))));

    server.enable_encryption(&secret).await.expect("enable");
    server
        .send(Frame::new(0x09, b"secret"))
        .await
        .expect("send");

    // Wait until the ciphertext has actually left for the peer, so the
    // writer's count has settled before it is read.
    let mut wire = vec![0u8; wire_of(0x09, b"secret").len()];
    tokio::time::timeout(Duration::from_secs(5), client_raw.read_exact(&mut wire))
        .await
        .expect("drain")
        .expect("read");

    let stats = server.stats();
    assert_eq!(stats.frames_out, 1);
    assert_eq!(
        stats.bytes_out as usize,
        wire.len(),
        "written bytes == socket bytes"
    );

    // The bytes on the wire are not the plaintext form: counting happened
    // after encryption, and so did the writing.
    let mut plain = Vec::new();
    FrameEncoder::new(Limits::default())
        .encode(&Frame::new(0x09, b"secret"), &mut plain)
        .expect("encode");
    assert_ne!(wire, plain);

    server.close().await.expect("flush");
}
