//! One test per defence in `dust_net::frame`, each built to fail if the
//! defence were removed.
//!
//! A guard with no test is a comment. These are written so that deleting the
//! check they name turns the test red — where that is not obviously true, the
//! test says why in a comment.
//!
//! The frames here are assembled by hand from `flate2` rather than by this
//! crate's encoder, deliberately. An attacker does not use our encoder, and a
//! test that can only produce what the encoder produces can only test the
//! frames that are already well formed.

mod support;

use std::io::Write as _;
use std::time::Instant;

use dust_net::frame::{
    Compress, Frame, FrameDecoder, FrameEncoder, FrameError, Limits, MAX_FRAME_LEN,
};
use dust_net::varint::write_var_int;
use flate2::write::ZlibEncoder;
use flate2::Compression;

#[global_allocator]
static ALLOCATOR: support::Counting = support::Counting;

/// A VarInt on its own, for building length prefixes by hand.
fn var_int(value: i32) -> Vec<u8> {
    let mut out = Vec::new();
    write_var_int(value, &mut out);
    out
}

fn zlib(data: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(9));
    encoder.write_all(data).expect("writing to a Vec");
    encoder.finish().expect("finishing a Vec")
}

/// Build a frame body in the compressed wire form, with the declared
/// uncompressed length under the caller's control so it can be made to lie.
fn compressed_frame(declared: i32, payload: &[u8]) -> Vec<u8> {
    let mut inner = var_int(declared);
    inner.extend_from_slice(&zlib(payload));
    let mut out = var_int(inner.len() as i32);
    out.extend_from_slice(&inner);
    out
}

/// Build a frame body in the "small enough not to compress" wire form.
fn uncompressed_in_compressed_mode(payload: &[u8]) -> Vec<u8> {
    let mut inner = var_int(0);
    inner.extend_from_slice(payload);
    let mut out = var_int(inner.len() as i32);
    out.extend_from_slice(&inner);
    out
}

fn decoder(compression: Compress) -> FrameDecoder {
    let mut decoder = FrameDecoder::new(Limits::default());
    decoder.set_compression(compression);
    decoder
}

// ---------------------------------------------------------------------------
// Defence 1: a length cap, applied before anything is allocated.
// ---------------------------------------------------------------------------

#[test]
fn a_length_prefix_claiming_two_gigabytes_is_refused() {
    let _gate = support::serial();
    let mut decoder = decoder(Compress::Disabled);
    let mut bytes = var_int(2_000_000_000);
    bytes.push(0x00);
    decoder.feed(&bytes);
    assert_eq!(
        decoder.next_frame(),
        Err(FrameError::TooLarge {
            declared: 2_000_000_000,
            limit: MAX_FRAME_LEN,
        })
    );
}

#[test]
fn the_cap_is_refused_before_anything_is_allocated() {
    let _gate = support::serial();
    // The distinction the previous test cannot draw: rejecting a two-gigabyte
    // claim *after* reserving two gigabytes is still a denial of service. This
    // is grouped into one `#[test]` because the allocator is process-wide and
    // concurrent tests would pollute the measurement.
    let baseline = support::reset_peak();
    let mut decoder = decoder(Compress::Disabled);
    let mut bytes = var_int(2_000_000_000);
    bytes.push(0x00);
    decoder.feed(&bytes);
    let outcome = decoder.next_frame();
    let peak = support::peak_above(baseline);

    assert!(matches!(outcome, Err(FrameError::TooLarge { .. })));
    assert!(
        peak < 64 * 1024,
        "rejecting a 2 GB claim peaked at {peak} bytes; the point of checking the length \
         before reading the body is that it costs nothing"
    );
}

#[test]
fn a_frame_of_exactly_the_limit_is_accepted_and_one_byte_more_is_not() {
    let _gate = support::serial();
    // A boundary written the wrong way round is the most common way a cap
    // stops matching vanilla, and it fails in the direction nobody notices:
    // honest clients disconnect.
    let limits = Limits {
        max_frame_len: 64,
        max_decompressed_len: 64,
    };

    let mut decoder = FrameDecoder::new(limits);
    let mut at_limit = var_int(64);
    at_limit.push(0x00); // packet id 0
    at_limit.extend(std::iter::repeat_n(0xABu8, 63));
    decoder.feed(&at_limit);
    assert_eq!(
        decoder.next_frame(),
        Ok(Some(Frame::new(0, vec![0xAB; 63]))),
    );

    let mut decoder = FrameDecoder::new(limits);
    let mut over = var_int(65);
    over.extend(std::iter::repeat_n(0u8, 65));
    decoder.feed(&over);
    assert_eq!(
        decoder.next_frame(),
        Err(FrameError::TooLarge {
            declared: 65,
            limit: 64
        })
    );
}

// ---------------------------------------------------------------------------
// Defence 2: a negative length is a length, not a very large one.
// ---------------------------------------------------------------------------

#[test]
fn a_negative_length_prefix_is_refused_as_negative() {
    let _gate = support::serial();
    // The bug this is written against: casting the signed prefix to `usize`
    // before the range check. `-1 as usize` is 18446744073709551615, which is
    // greater than the cap and so would still be *refused* — but `-1 as u32 as
    // usize` on a 32-bit target, or an `i32` compared against a `i32` cap
    // after a sign-losing round trip, is how this becomes an accepted length.
    // Asserting on the variant rather than merely on "it errored" is what
    // pins the ordering of the two checks.
    let mut decoder = decoder(Compress::Disabled);
    decoder.feed(&[0xff, 0xff, 0xff, 0xff, 0x0f, 0x00]);
    assert_eq!(
        decoder.next_frame(),
        Err(FrameError::NegativeLength { declared: -1 })
    );
}

#[test]
fn an_empty_frame_is_refused() {
    let _gate = support::serial();
    let mut decoder = decoder(Compress::Disabled);
    decoder.feed(&[0x00]);
    assert_eq!(decoder.next_frame(), Err(FrameError::Empty));
}

// ---------------------------------------------------------------------------
// Defence 3 and 4: both directions of the compression threshold.
// ---------------------------------------------------------------------------

#[test]
fn a_frame_claiming_to_be_uncompressed_above_the_threshold_is_refused() {
    let _gate = support::serial();
    // The client says "data length 0, this was too small to compress" and then
    // sends something that was not too small. Accepting it lets any client opt
    // out of compression while the server keeps paying for the bandwidth.
    let mut decoder = decoder(Compress::At { threshold: 64 });
    let payload = vec![0x00; 200];
    decoder.feed(&uncompressed_in_compressed_mode(&payload));
    assert_eq!(
        decoder.next_frame(),
        Err(FrameError::UncompressedOverThreshold {
            len: 200,
            threshold: 64
        })
    );
}

#[test]
fn a_frame_claiming_to_be_compressed_below_the_threshold_is_refused() {
    let _gate = support::serial();
    // The other direction. A server that checks only the first still accepts
    // this, and the pair is what makes the wire form a function of the payload
    // size rather than something the client picks.
    let mut decoder = decoder(Compress::At { threshold: 256 });
    let payload = vec![0x11; 100];
    decoder.feed(&compressed_frame(100, &payload));
    assert_eq!(
        decoder.next_frame(),
        Err(FrameError::CompressedUnderThreshold {
            declared: 100,
            threshold: 256
        })
    );
}

#[test]
fn a_payload_of_exactly_the_threshold_is_compressed_not_sent_raw() {
    let _gate = support::serial();
    // Vanilla compresses at `>= threshold`. Written as `>`, every payload of
    // exactly the threshold is a protocol error against a real client, and no
    // round-trip test inside this crate can see it because the encoder and the
    // decoder would be wrong together. So this asserts on the *bytes*: the
    // header of a threshold-sized frame must carry a nonzero data length.
    let threshold = 32;
    let mut encoder = FrameEncoder::new(Limits::default());
    encoder.set_compression(Compress::At { threshold });

    let frame = Frame::new(0, vec![0x5A; threshold - 1]);
    assert_eq!(frame.payload_len(), threshold);
    let mut out = Vec::new();
    encoder.encode(&frame, &mut out).expect("small frame");

    // Skip the outer length prefix and read the data length.
    let (_, prefix) = dust_net::varint::read_var_int(&out).expect("length prefix");
    let (data_len, _) = dust_net::varint::read_var_int(&out[prefix..]).expect("data length");
    assert_eq!(
        data_len, threshold as i32,
        "a payload of exactly the threshold must be compressed, not sent with data length 0"
    );

    // And one byte smaller must go the other way.
    let frame = Frame::new(0, vec![0x5A; threshold - 2]);
    assert_eq!(frame.payload_len(), threshold - 1);
    let mut out = Vec::new();
    encoder.encode(&frame, &mut out).expect("smaller frame");
    let (_, prefix) = dust_net::varint::read_var_int(&out).expect("length prefix");
    let (data_len, _) = dust_net::varint::read_var_int(&out[prefix..]).expect("data length");
    assert_eq!(data_len, 0, "one byte below the threshold must be sent raw");
}

// ---------------------------------------------------------------------------
// Defence 5: decompression bombs.
// ---------------------------------------------------------------------------

/// A real bomb: sixty-four megabytes of zeros, which zlib turns into a few
/// tens of kilobytes.
fn bomb() -> (Vec<u8>, usize) {
    const EXPANDED: usize = 64 * 1024 * 1024;
    let compressed = zlib(&vec![0u8; EXPANDED]);
    (compressed, EXPANDED)
}

#[test]
fn a_bomb_that_declares_its_real_size_is_refused_before_decompressing() {
    let _gate = support::serial();
    let (compressed, expanded) = bomb();
    assert!(
        compressed.len() < 128 * 1024,
        "the bomb should be small; it is {} bytes",
        compressed.len()
    );

    let mut inner = var_int(expanded as i32);
    inner.extend_from_slice(&compressed);
    let mut wire = var_int(inner.len() as i32);
    wire.extend_from_slice(&inner);

    let mut decoder = decoder(Compress::At { threshold: 256 });
    decoder.feed(&wire);
    assert_eq!(
        decoder.next_frame(),
        Err(FrameError::DeclaredTooLarge {
            declared: expanded,
            limit: MAX_FRAME_LEN,
        })
    );
}

#[test]
fn a_bomb_that_lies_about_its_size_is_stopped_mid_expansion() {
    let _gate = support::serial();
    // The interesting one. The frame declares a small, plausible, in-range
    // size so the cheap pre-check passes, and the stream then expands to
    // sixty-four megabytes. Only a bound on the decompressor's *output* stops
    // this; a check on the declared length alone does not.
    //
    // The declared size must be at or above the threshold, or defence 4 would
    // reject it first and this test would pass without exercising the bound.
    let (compressed, _) = bomb();
    let declared = 4096;

    let mut inner = var_int(declared);
    inner.extend_from_slice(&compressed);
    let mut wire = var_int(inner.len() as i32);
    wire.extend_from_slice(&inner);

    let baseline = support::reset_peak();
    let started = Instant::now();
    let mut decoder = decoder(Compress::At { threshold: 256 });
    decoder.feed(&wire);
    let outcome = decoder.next_frame();
    let elapsed = started.elapsed();
    let peak = support::peak_above(baseline);

    assert_eq!(
        outcome,
        Err(FrameError::Bomb {
            limit: declared as usize
        })
    );
    assert!(
        peak < 8 * 1024 * 1024,
        "refusing the bomb peaked at {peak} bytes of live allocation; the whole point of \
         bounding the decompressor is that a 64 MiB expansion never lands"
    );
    assert!(
        elapsed.as_secs() < 5,
        "refusing the bomb took {elapsed:?}; it should stop at the bound, not at the end \
         of the stream"
    );
}

#[test]
fn the_absolute_cap_bounds_a_frame_that_declares_a_legal_size() {
    let _gate = support::serial();
    // The second of the two bounds, on its own. The declared length is within
    // `max_frame_len` but above this connection's `max_decompressed_len`, so
    // only the absolute cap can refuse it.
    let (compressed, _) = bomb();
    let limits = Limits {
        max_frame_len: MAX_FRAME_LEN,
        max_decompressed_len: 1024,
    };
    let declared = 100_000;

    let mut inner = var_int(declared);
    inner.extend_from_slice(&compressed);
    let mut wire = var_int(inner.len() as i32);
    wire.extend_from_slice(&inner);

    let mut decoder = FrameDecoder::new(limits);
    decoder.set_compression(Compress::At { threshold: 256 });
    decoder.feed(&wire);
    assert_eq!(
        decoder.next_frame(),
        Err(FrameError::DeclaredTooLarge {
            declared: declared as usize,
            limit: 1024,
        })
    );
}

// ---------------------------------------------------------------------------
// Defence 6 and 7: the declared length must be the truth, and the whole frame
// must be part of the stream.
// ---------------------------------------------------------------------------

#[test]
fn a_declared_length_that_disagrees_with_what_decompressed_is_refused() {
    let _gate = support::serial();
    // Under the bound, and still a lie: the stream really does decompress, to
    // a size other than the one declared. Accepting it at its real size would
    // mean every length calculation downstream was written against a number
    // that turned out not to be the number.
    let payload = vec![0x77; 500];
    let mut decoder = decoder(Compress::At { threshold: 256 });
    decoder.feed(&compressed_frame(600, &payload));
    assert_eq!(
        decoder.next_frame(),
        Err(FrameError::LengthMismatch {
            declared: 600,
            actual: 500
        })
    );
}

#[test]
fn bytes_after_the_end_of_the_zlib_stream_are_refused() {
    let _gate = support::serial();
    // A frame whose compressed data ends early carries a tail the decompressor
    // never saw. Two implementations that disagree about whether the tail is
    // part of the frame is exactly the shape of a request smuggling bug.
    let payload = vec![0x33; 400];
    let mut inner = var_int(400);
    inner.extend_from_slice(&zlib(&payload));
    inner.extend_from_slice(b"smuggled");
    let mut wire = var_int(inner.len() as i32);
    wire.extend_from_slice(&inner);

    let mut decoder = decoder(Compress::At { threshold: 256 });
    decoder.feed(&wire);
    assert_eq!(
        decoder.next_frame(),
        Err(FrameError::TrailingBytes { unread: 8 })
    );
}

#[test]
fn corrupt_compressed_data_is_a_named_error() {
    let _gate = support::serial();
    let mut inner = var_int(400);
    inner.extend_from_slice(&[0x78, 0x9c, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
    let mut wire = var_int(inner.len() as i32);
    wire.extend_from_slice(&inner);

    let mut decoder = decoder(Compress::At { threshold: 256 });
    decoder.feed(&wire);
    match decoder.next_frame() {
        Err(FrameError::Corrupt(message)) => assert!(!message.is_empty()),
        other => panic!("expected a named corruption error, got {other:?}"),
    }
}

#[test]
fn a_compressed_frame_whose_header_is_a_bad_varint_is_refused() {
    let _gate = support::serial();
    let mut inner = vec![0x80, 0x80, 0x80, 0x80, 0x80];
    inner.extend_from_slice(&[0u8; 8]);
    let mut wire = var_int(inner.len() as i32);
    wire.extend_from_slice(&inner);

    let mut decoder = decoder(Compress::At { threshold: 4 });
    decoder.feed(&wire);
    assert!(matches!(
        decoder.next_frame(),
        Err(FrameError::DataLength(_))
    ));
}

// ---------------------------------------------------------------------------
// Stream reassembly.
// ---------------------------------------------------------------------------

#[test]
fn a_frame_arriving_one_byte_at_a_time_is_reassembled() {
    let _gate = support::serial();
    // What a socket actually does. A decoder that needs whole frames per read
    // works on loopback and fails on the internet.
    let frame = Frame::new(0x2C, vec![0x9F; 300]);
    let encoder = FrameEncoder::new(Limits::default());
    let mut wire = Vec::new();
    encoder.encode(&frame, &mut wire).expect("encode");

    let mut decoder = decoder(Compress::Disabled);
    for (index, byte) in wire.iter().enumerate() {
        decoder.feed(&[*byte]);
        let step = decoder.next_frame().expect("no error mid-frame");
        if index + 1 == wire.len() {
            assert_eq!(step, Some(frame.clone()));
        } else {
            assert_eq!(step, None, "completed early at byte {}", index + 1);
        }
    }
}

#[test]
fn several_frames_in_one_read_are_all_returned() {
    let _gate = support::serial();
    let encoder = FrameEncoder::new(Limits::default());
    let frames: Vec<Frame> = (0..9)
        .map(|i| Frame::new(i, vec![i as u8; (i as usize) * 7]))
        .collect();
    let mut wire = Vec::new();
    for frame in &frames {
        encoder.encode(frame, &mut wire).expect("encode");
    }

    let mut decoder = decoder(Compress::Disabled);
    decoder.feed(&wire);
    let mut got = Vec::new();
    while let Some(frame) = decoder.next_frame().expect("no error") {
        got.push(frame);
    }
    assert_eq!(got, frames);
    assert_eq!(decoder.buffered(), 0);
}

#[test]
fn a_burst_of_small_frames_does_not_grow_the_buffer() {
    let _gate = support::serial();
    // The compaction rule. Without it the read buffer keeps every frame the
    // connection ever received, and an attacker's cheapest packet — a tiny
    // one — is also the one that costs the most memory.
    let encoder = FrameEncoder::new(Limits::default());
    let mut decoder = decoder(Compress::Disabled);
    for _ in 0..50_000 {
        let mut wire = Vec::new();
        encoder
            .encode(&Frame::new(0, vec![1, 2, 3]), &mut wire)
            .expect("encode");
        decoder.feed(&wire);
        decoder.next_frame().expect("no error").expect("a frame");
    }
    assert_eq!(decoder.buffered(), 0);
}

#[test]
fn round_trips_through_the_decoder_in_both_modes() {
    let _gate = support::serial();
    // Deliberately last, and deliberately labelled. This proves the encoder
    // and the decoder in this crate agree with each other. It does not prove
    // either agrees with Minecraft, and it would pass just as green with the
    // VarInt groups reversed in both halves. The published wire tables in
    // `varint.rs`'s own tests, and the externally generated fixtures in
    // `testkeys.rs`, are what stand between this and self-consistent
    // nonsense.
    let modes = [
        Compress::Disabled,
        Compress::At { threshold: 1 },
        Compress::At { threshold: 256 },
        Compress::At { threshold: 100_000 },
    ];
    let bodies = [
        Vec::new(),
        vec![0u8; 1],
        vec![0xEEu8; 255],
        vec![0x01u8; 256],
        (0..4096).map(|i| (i % 251) as u8).collect(),
    ];

    for mode in modes {
        let mut encoder = FrameEncoder::new(Limits::default());
        encoder.set_compression(mode);
        let mut decoder = decoder(mode);
        for id in [0, 1, 127, 128, 255, 0x7F_FF] {
            for body in &bodies {
                let frame = Frame::new(id, body.clone());
                let mut wire = Vec::new();
                encoder.encode(&frame, &mut wire).expect("encode");
                decoder.feed(&wire);
                assert_eq!(
                    decoder.next_frame(),
                    Ok(Some(frame)),
                    "mode {mode:?}, id {id}, body {} bytes",
                    body.len()
                );
            }
        }
    }
}

#[test]
fn the_encoder_refuses_to_emit_an_oversized_frame() {
    let _gate = support::serial();
    // A server that can be talked into sending a frame no client will accept
    // has been talked into disconnecting its own players.
    let limits = Limits {
        max_frame_len: 128,
        max_decompressed_len: 128,
    };
    let encoder = FrameEncoder::new(limits);
    let mut out = Vec::new();
    assert!(matches!(
        encoder.encode(&Frame::new(0, vec![0u8; 500]), &mut out),
        Err(FrameError::Oversize { .. })
    ));
    assert!(out.is_empty(), "a refused frame must not be half written");
}
