//! The encryption switch, where one byte of stream position decides whether
//! everything is noise or meaning.
//!
//! [`crate::crypt`] documents the two ways this transition fails — bytes
//! lost to a buffer that read past the response, bytes double-encrypted by a
//! queue drained through the wrong cipher — and notes that neither is caught
//! by encrypt-a-buffer-and-decrypt-it tests. These tests drive the real
//! transition over real duplex streams, in both directions, with the hostile
//! shape included: a peer that pipelines its next frames into the same write
//! as its Encryption Response, which no honest client produces and nothing
//! stops a dishonest one from producing.
//!
//! The transport mechanics are what is under test here, so the key exchange
//! itself is skipped: both sides agree on a fixed secret, and the RSA round
//! trip is `tests/login_session.rs`'s job.

use std::time::Duration;

use dust_net::crypt::{Cipher, SharedSecret};
use dust_net::frame::{Compress, Frame, FrameDecoder, FrameEncoder, Limits};
use dust_net::io::{Conn, ConnConfig, Timeouts};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// The session secret both ends use. Fixed because this file is about *where*
/// the cipher starts, not about what the key is.
fn secret() -> SharedSecret {
    SharedSecret::from_bytes([
        0x42, 0x24, 0x11, 0x99, 0x88, 0x77, 0x66, 0x55, 0xAB, 0xCD, 0xEF, 0x01, 0x34, 0x12, 0x78,
        0x56,
    ])
}

fn config() -> ConnConfig {
    ConnConfig {
        timeouts: Timeouts {
            idle: Some(Duration::from_secs(10)),
            pre_auth_budget: None,
        },
        ..ConnConfig::default()
    }
}

fn encoder() -> FrameEncoder {
    FrameEncoder::new(Limits::default())
}

/// The wire form of a frame under compression at 64.
fn deflated(frame: &Frame) -> Vec<u8> {
    let mut e = encoder();
    e.set_compression(Compress::At { threshold: 64 });
    let mut out = Vec::new();
    e.encode(frame, &mut out).expect("encode");
    out
}

/// Append a frame's encrypted wire form to `burst`, using — and advancing —
/// the caller's cipher. A stream's frames are sealed by **one continuous
/// encryptor**: CFB8's register carries from frame to frame, so a fresh
/// cipher per frame would produce bytes no honest receiver can decrypt after
/// the first one. (This test file got that wrong once; the failure looked
/// exactly like corruption, which is the lesson.)
fn seal_with(cipher: &mut Cipher, frame: &Frame, out: &mut Vec<u8>) {
    let start = out.len();
    encoder().encode(frame, out).expect("encode");
    cipher.encrypt(&mut out[start..]);
}

/// Compressed *and* encrypted, the form every frame takes after both
/// switches have passed.
fn seal_deflated_with(cipher: &mut Cipher, frame: &Frame, out: &mut Vec<u8>) {
    let start = out.len();
    out.extend_from_slice(&deflated(frame));
    cipher.encrypt(&mut out[start..]);
}

#[tokio::test(flavor = "multi_thread")]
async fn bytes_pipelined_past_the_response_are_ciphertext_not_misread_plaintext() {
    // One write from the peer contains the Encryption Response followed by
    // two complete encrypted frames. A reader that buffers greedily before
    // the switch treats those frames as plaintext and desynchronizes; a
    // reader that switches at exactly the right byte decodes them cleanly.
    let (mut client, server) = tokio::io::duplex(4096);
    let mut server = Conn::new(server, config());

    let mut burst = Vec::new();
    encoder()
        .encode(&Frame::new(0x02, b"the response"), &mut burst)
        .expect("encode");
    // One continuous encryptor over the whole post-switch tail.
    let mut cipher = Cipher::disabled();
    cipher.enable(&secret());
    seal_with(
        &mut cipher,
        &Frame::new(0x30, b"first pipelined"),
        &mut burst,
    );
    seal_with(
        &mut cipher,
        &Frame::new(0x31, b"second pipelined"),
        &mut burst,
    );
    client.write_all(&burst).await.expect("burst");
    drop(client);

    // The response arrives as plaintext...
    let response = server.next_frame().await.expect("read").expect("response");
    assert_eq!(response, Frame::new(0x02, b"the response"));

    // ...and the switch happens here, mid-stream, one byte after it.
    server.enable_encryption(&secret()).await.expect("enable");

    // The pipelined tail was ciphertext all along: the buffered prefix did
    // not eat it, and the cipher was not fed it twice.
    let first = server.next_frame().await.expect("read").expect("frame");
    assert_eq!(first, Frame::new(0x30, b"first pipelined"));
    let second = server.next_frame().await.expect("read").expect("frame");
    assert_eq!(second, Frame::new(0x31, b"second pipelined"));
}

#[tokio::test(flavor = "multi_thread")]
async fn frames_queued_before_the_switch_reach_the_wire_as_plaintext() {
    // The queue-ordering guarantee, checked at the byte level rather than by
    // decoding twice: whatever is accepted before `enable_encryption` goes
    // out in the clear even if it had not been written yet, and the first
    // bytes after the switch do not decode as plain framing.
    let (client_io, mut server_raw) = tokio::io::duplex(4096);
    let mut client = Conn::new(client_io, config());

    client
        .send(Frame::new(0x01, b"plaintext request"))
        .await
        .expect("queued");
    client.enable_encryption(&secret()).await.expect("enabled");
    client
        .send(Frame::new(0x03, b"ciphertext reply"))
        .await
        .expect("queued");
    client.close().await.expect("flushed");

    let mut wire = Vec::new();
    server_raw.read_to_end(&mut wire).await.expect("read");

    // First frame: plaintext, decodable by the bare decoder.
    let mut decoder = FrameDecoder::new(Limits::default());
    decoder.feed(&wire);
    let first = decoder.next_frame().expect("decode").expect("first frame");
    assert_eq!(first, Frame::new(0x01, b"plaintext request"));
    let consumed = wire.len() - decoder.buffered();

    // Everything after it must not be the plaintext form of the second
    // frame — the assertion an encoder/decoder pair that agreed on skipping
    // encryption would fail.
    let rest = &wire[consumed..];
    let mut plainly = Vec::new();
    encoder()
        .encode(&Frame::new(0x03, b"ciphertext reply"), &mut plainly)
        .expect("encode");
    assert_ne!(
        rest,
        plainly.as_slice(),
        "the switched frame went out in the clear"
    );

    let mut cipher = Cipher::disabled();
    cipher.enable(&secret());
    let mut decrypted = rest.to_vec();
    cipher.decrypt(&mut decrypted);
    let mut tail = FrameDecoder::new(Limits::default());
    tail.feed(&decrypted);
    let second = tail.next_frame().expect("decode").expect("second frame");
    assert_eq!(second, Frame::new(0x03, b"ciphertext reply"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_length_prefix_straggling_in_single_bytes_across_the_switch_decodes() {
    // The read-path discipline from `crypt.rs`: under encryption the length
    // prefix is consumed a byte at a time, because a speculative bulk read
    // would run the cipher past the frame it is entitled to interpret. Feed
    // an encrypted frame in single-byte writes so any over-read would show
    // up as corruption rather than as success.
    let (client, server) = tokio::io::duplex(4096);
    let mut server = Conn::new(server, config());

    // An initial plaintext frame puts the connection where a login would be;
    // written before the dribble task takes the client half away.
    let mut out = Vec::new();
    encoder()
        .encode(&Frame::new(0x00, b"login start"), &mut out)
        .expect("encode");
    let mut client = client;
    client.write_all(&out).await.expect("write");
    let start = server.next_frame().await.expect("read").expect("start");
    assert_eq!(start, Frame::new(0x00, b"login start"));
    server.enable_encryption(&secret()).await.expect("enable");

    let payload = vec![0x7Eu8; 300];
    let mut sealed_frame = Vec::new();
    {
        let mut cipher = Cipher::disabled();
        cipher.enable(&secret());
        seal_with(
            &mut cipher,
            &Frame::new(0x44, payload.clone()),
            &mut sealed_frame,
        );
    }
    let dribble = tokio::spawn(async move {
        for piece in sealed_frame.chunks(1) {
            client.write_all(piece).await.expect("dribble");
            client.flush().await.expect("dribble");
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });

    let decoded = tokio::time::timeout(Duration::from_secs(10), server.next_frame())
        .await
        .expect("stalled mid-prefix")
        .expect("decoded")
        .expect("frame");
    assert_eq!(decoded, Frame::new(0x44, payload));
    dribble.await.expect("dribbler joins");
}

#[tokio::test(flavor = "multi_thread")]
async fn interleaved_traffic_in_both_directions_stays_synchronized() {
    // Two drivers, both switched, trading frames with upstream traffic cut
    // across downstream traffic. Each direction has its own feedback
    // register; sharing state between them would pass any test that speaks
    // strictly in turns, so this deliberately does not speak in turns.
    let (client_io, server_io) = tokio::io::duplex(4096);
    let mut server = Conn::new(server_io, config());
    let mut client = Conn::new(client_io, config());

    client.send(Frame::new(0x00, b"hello")).await.expect("send");
    let hello = server.next_frame().await.expect("read").expect("hello");
    assert_eq!(hello, Frame::new(0x00, b"hello"));
    server.enable_encryption(&secret()).await.expect("enable");
    client.enable_encryption(&secret()).await.expect("enable");

    for round in 0..12u8 {
        // Both ends speak in this round; every send below is encrypted by
        // its own direction's register.
        server
            .send(Frame::new(0x10 + round as i32, vec![round; round as usize]))
            .await
            .expect("downstream");
        // On every third round the client queues an extra upstream frame
        // *before* its ack, so the server must see them in queue order.
        if round % 3 == 0 {
            client
                .send(Frame::new(0x80 + round as i32, b"up"))
                .await
                .expect("upstream");
        }
        client
            .send(Frame::new(0x20 + round as i32, b"ack"))
            .await
            .expect("ack");

        let down = client.next_frame().await.expect("read").expect("down");
        assert_eq!(down.id, 0x10 + round as i32);

        if round % 3 == 0 {
            let up = server.next_frame().await.expect("read").expect("up");
            assert_eq!(up.id, 0x80 + round as i32);
        }
        let ack = server.next_frame().await.expect("read").expect("ack");
        assert_eq!(ack.id, 0x20 + round as i32);
    }

    client.close().await.expect("client close");
    let ended = server.next_frame().await.expect("read");
    assert_eq!(ended, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn compression_and_encryption_switches_compose_in_protocol_order() {
    // The full mode stack, applied in the order the protocol applies it:
    // plaintext frames; then Set Compression, which itself travels plain;
    // then compressed frames; then the switch to encryption, after which
    // frames are deflated *and* encrypted.
    let (mut client_raw, server) = tokio::io::duplex(8192);
    let mut server = Conn::new(server, config());

    // Plain, before anything is announced.
    let mut w1 = Vec::new();
    encoder()
        .encode(&Frame::new(0x01, b"plain"), &mut w1)
        .expect("encode");
    client_raw.write_all(&w1).await.expect("w1");
    let f1 = server.next_frame().await.expect("r1").expect("f1");
    assert_eq!(f1, Frame::new(0x01, b"plain"));

    // The server announces compression and turns it on for everything after.
    server
        .send(Frame::new(0x03, b"threshold=64"))
        .await
        .expect("announce");
    server.set_compression(Compress::At { threshold: 64 });

    // The announcement arrives still readable by a bare decoder.
    let mut announcement = vec![0u8; 4096];
    let n = client_raw.read(&mut announcement).await.expect("r2");
    let mut plain_decoder = FrameDecoder::new(Limits::default());
    plain_decoder.feed(&announcement[..n]);
    let announced = plain_decoder.next_frame().expect("d2").expect("setcomp");
    assert_eq!(announced, Frame::new(0x03, b"threshold=64"));
    assert_eq!(
        plain_decoder.buffered(),
        0,
        "the announcement must be the whole read; nothing else was sent yet"
    );

    // Now compressed frames cross, and the server decodes them compressed.
    let w2 = deflated(&Frame::new(0x05, vec![0xA; 200]));
    client_raw.write_all(&w2).await.expect("w2");
    let f2 = server.next_frame().await.expect("r2b").expect("f2");
    assert_eq!(f2, Frame::new(0x05, vec![0xA; 200]));

    // Then the encryption switch, after which a frame is both at once. The
    // client's encryptor is created once and carried across every remaining
    // write, exactly as a real client's would be.
    server.enable_encryption(&secret()).await.expect("enable");
    let mut client_cipher = Cipher::disabled();
    client_cipher.enable(&secret());
    let mut w3 = Vec::new();
    seal_deflated_with(
        &mut client_cipher,
        &Frame::new(0x07, vec![0xB; 200]),
        &mut w3,
    );
    client_raw.write_all(&w3).await.expect("w3");
    let f3 = server.next_frame().await.expect("r3").expect("f3");
    assert_eq!(f3, Frame::new(0x07, vec![0xB; 200]));

    // And one back the other way through the same stack, same continuous
    // encryptor as the previous frame.
    let mut w4 = Vec::new();
    seal_deflated_with(&mut client_cipher, &Frame::new(0x08, b"and back"), &mut w4);
    client_raw.write_all(&w4).await.expect("w4");
    let f4 = server.next_frame().await.expect("r4").expect("f4");
    assert_eq!(f4, Frame::new(0x08, b"and back"));
}
