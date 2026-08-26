//! The compression threshold as a boundary, walked cell by cell.
//!
//! Everything else in this crate treats the threshold as a number; these
//! tests treat it as a *decision*. For every threshold there are exactly two
//! wire forms a sender may choose between, the choice is forced by one
//! comparison against that threshold, and vanilla settles the comparison
//! direction: **a payload of exactly the threshold bytes is compressed**
//! (`>=`, not `>`). Written backwards, every payload of exactly the threshold
//! becomes a protocol error against a real client, and no test confined to
//! this crate's own halves can see it, because an encoder and a decoder that
//! are wrong together agree perfectly.
//!
//! So the matrix below is checked structurally, against the *bytes*, in both
//! directions:
//!
//! | mode                | payload < threshold          | payload >= threshold |
//! |---------------------|------------------------------|----------------------|
//! | disabled            | no header, ever              | no header, ever      |
//! | enabled, threshold t| data length 0, payload plain | data length = real size, zlib |
//!
//! plus the two degenerate thresholds the formula has to get right: **0**,
//! where nothing is ever below the line so the raw form cannot exist, and
//! **1**, where the same is true because a payload is never empty — the
//! packet id alone gives it a byte.
//!
//! The randomized half of the file is a property sweep: seeded, deterministic,
//! sizes concentrated around every boundary, feeds split into random chunks
//! like a socket would split them. It exists because hand-picked boundaries
//! are exactly as good as the person who picked them.

use dust_net::frame::{Compress, Frame, FrameDecoder, FrameEncoder, FrameError, Limits};
use dust_net::varint::{read_var_int, write_var_int};

/// SplitMix64: three multiplies and a shift per value, identical on every
/// platform, so a failure reproduces from the seed alone. Chosen over pulling
/// a randomness crate in for what is a deterministic test, not a gamble.
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

    /// A value in `0..n`; `n` must be nonzero.
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn decoder_with(compression: Compress) -> FrameDecoder {
    let mut decoder = FrameDecoder::new(Limits::default());
    decoder.set_compression(compression);
    decoder
}

fn encoded(frame: &Frame, compression: Compress) -> Vec<u8> {
    let mut encoder = FrameEncoder::new(Limits::default());
    encoder.set_compression(compression);
    let mut out = Vec::new();
    encoder.encode(frame, &mut out).expect("encode");
    out
}

/// Skip the outer length prefix and read the compression header's data
/// length: the number that decides which form the frame claims.
fn claimed_data_length(wire: &[u8]) -> i32 {
    let (_, prefix) = read_var_int(wire).expect("outer prefix");
    let (data_len, _) = read_var_int(&wire[prefix..]).expect("data length");
    data_len
}

fn payload_len(id: i32, body: usize) -> usize {
    Frame::new(id, vec![0u8; body]).payload_len()
}

#[test]
fn the_disabled_mode_has_no_header_at_all() {
    let frame = Frame::new(0x21, vec![0xAB; 40]);
    let wire = encoded(&frame, Compress::Disabled);

    let (_, prefix) = read_var_int(&wire).expect("prefix");
    let (id, id_len) = read_var_int(&wire[prefix..]).expect("the payload starts with the id");
    assert_eq!(id, 0x21);
    assert_eq!(&wire[prefix + id_len..], &frame.body[..]);
}

#[test]
fn exactly_at_the_threshold_is_compressed_and_one_below_is_not() {
    // The convention itself, asserted on the bytes for several thresholds so
    // a coincidence of sizing cannot fake a pass. Note what "payload" means:
    // the id is part of it, which is why the smallest frame any threshold
    // can leave un-compressed has a body of `threshold - 2` bytes.
    for threshold in [2usize, 7, 64, 256] {
        let mut encoder = FrameEncoder::new(Limits::default());
        encoder.set_compression(Compress::At { threshold });

        // One byte below the line: data length 0, payload verbatim.
        let small = Frame::new(3, vec![0x11; threshold - 2]);
        assert_eq!(small.payload_len(), threshold - 1);
        let wire = encoded(&small, Compress::At { threshold });
        assert_eq!(
            claimed_data_length(&wire),
            0,
            "threshold {threshold}: {} bytes must go raw",
            small.payload_len()
        );

        // Exactly on it: compressed, declaring its true size.
        let exact = Frame::new(3, vec![0x11; threshold - 1]);
        assert_eq!(exact.payload_len(), threshold);
        let wire = encoded(&exact, Compress::At { threshold });
        assert_eq!(
            claimed_data_length(&wire),
            threshold as i32,
            "threshold {threshold}: a payload of exactly the threshold must be \
             compressed, which is the vanilla convention"
        );
    }

    // Threshold one has no below-the-line payload at all: even an empty body
    // leaves the id byte, and one byte is on the line.
    let smallest = Frame::new(3, Vec::<u8>::new());
    assert_eq!(smallest.payload_len(), 1);
    let wire = encoded(&smallest, Compress::At { threshold: 1 });
    assert_eq!(claimed_data_length(&wire), 1);
}

#[test]
fn threshold_zero_leaves_no_raw_form_to_send() {
    // With the line at zero, no payload is below it, so every frame takes
    // the compressed form whatever its size — including one-byte payloads
    // that gain nothing from the attempt.
    let mut encoder = FrameEncoder::new(Limits::default());
    encoder.set_compression(Compress::At { threshold: 0 });

    for body in [0usize, 1, 7] {
        let frame = Frame::new(5, vec![0x22; body]);
        let wire = encoded(&frame, Compress::At { threshold: 0 });
        assert_ne!(
            claimed_data_length(&wire),
            0,
            "{}-byte payload went raw under threshold zero",
            frame.payload_len()
        );
    }

    // And a client that sends the raw form anyway — legal at every other
    // threshold for small payloads — is refusing a compression nobody can
    // opt out of. Refused, naming the zero.
    let mut inner_bytes = Vec::new();
    write_var_int(0, &mut inner_bytes);
    inner_bytes.push(0x00); // the payload: just the id, one byte long
    let mut wire = Vec::new();
    write_var_int(inner_bytes.len() as i32, &mut wire);
    wire.extend_from_slice(&inner_bytes);

    let mut decoder = decoder_with(Compress::At { threshold: 0 });
    decoder.feed(&wire);
    assert_eq!(
        decoder.next_frame(),
        Err(FrameError::UncompressedOverThreshold {
            len: 1,
            threshold: 0,
        })
    );
}

#[test]
fn threshold_one_is_the_same_line_for_an_unskippable_id_byte() {
    // A payload is never empty — the id is part of it — so threshold one
    // behaves exactly like threshold zero: the raw form has nothing left to
    // say. This is the case most likely to be special-cased wrongly, because
    // it looks like "only tiny frames" rather than "no frames".
    for body in [0usize, 1, 30] {
        let frame = Frame::new(9, vec![0x33; body]);
        let wire = encoded(&frame, Compress::At { threshold: 1 });
        assert_ne!(claimed_data_length(&wire), 0);
    }

    let mut inner_bytes = Vec::new();
    write_var_int(0, &mut inner_bytes);
    inner_bytes.push(0x00);
    let mut wire = Vec::new();
    write_var_int(inner_bytes.len() as i32, &mut wire);
    wire.extend_from_slice(&inner_bytes);
    let mut decoder = decoder_with(Compress::At { threshold: 1 });
    decoder.feed(&wire);
    assert_eq!(
        decoder.next_frame(),
        Err(FrameError::UncompressedOverThreshold {
            len: 1,
            threshold: 1,
        })
    );
}

#[test]
fn the_decoder_accepts_exactly_the_cells_the_encoder_produces() {
    // Both directions of the forced choice, through hand-built frames, so a
    // decoder that merely mirrored an encoder bug would fail here.
    const THRESHOLD: usize = 64;
    let mut decoder = decoder_with(Compress::At {
        threshold: THRESHOLD,
    });

    // Legal cell: raw below the line. The id is part of the payload, so a
    // body of `THRESHOLD - 2` puts the payload exactly one under.
    let mut small = Vec::new();
    write_var_int(0, &mut small);
    small.push(0x00); // the packet id
    small.extend_from_slice(&[0xEE; THRESHOLD - 2]);
    let mut wire = Vec::new();
    write_var_int(small.len() as i32, &mut wire);
    wire.extend_from_slice(&small);
    decoder.feed(&wire);
    assert_eq!(
        decoder.next_frame(),
        Ok(Some(Frame::new(0, vec![0xEE; THRESHOLD - 2]))),
        "raw below the threshold is the correct form"
    );

    // Illegal cell: raw at or above the line.
    let mut big = Vec::new();
    write_var_int(0, &mut big);
    big.push(0x00); // the packet id
    big.extend_from_slice(&[0xFF; THRESHOLD - 1]);
    let mut wire = Vec::new();
    write_var_int(big.len() as i32, &mut wire);
    wire.extend_from_slice(&big);
    decoder.feed(&wire);
    assert_eq!(
        decoder.next_frame(),
        Err(FrameError::UncompressedOverThreshold {
            len: THRESHOLD,
            threshold: THRESHOLD,
        })
    );
}

#[test]
fn a_corrupt_zlib_stream_is_a_named_error_and_not_a_crash() {
    // Three distinct ways to be rotten: never zlib at all, zlib that stops
    // early, and zlib whose integrity check fails at the end. All three must
    // arrive as `Corrupt` or a sibling naming the problem — the decoder's job
    // is to die loudly and specifically, never badly.
    let threshold = 16;

    // Not zlib at all.
    let mut inner = Vec::new();
    write_var_int(64, &mut inner);
    inner.extend_from_slice(b"\x00\x01\x02 this is not deflate");
    let mut wire = Vec::new();
    write_var_int(inner.len() as i32, &mut wire);
    wire.extend_from_slice(&inner);
    let mut decoder = decoder_with(Compress::At { threshold });
    decoder.feed(&wire);
    assert!(matches!(decoder.next_frame(), Err(FrameError::Corrupt(_))));

    // zlib that stops mid-stream.
    let full = dust_net::frame::Frame::new(1, vec![0x5A; 200]);
    let mut whole = encoded(&full, Compress::At { threshold });
    // Locate the compressed section and cut it in half.
    let (_, prefix) = read_var_int(&whole).unwrap();
    let (_, header) = read_var_int(&whole[prefix..]).unwrap();
    let compressed_start = prefix + header;
    let cut = compressed_start + whole[compressed_start..].len() / 2;
    whole.truncate(cut);
    // Fix the outer prefix so the truncation is purely a corruption of the
    // stream, not of the framing: the decoder should see a complete frame
    // carrying a broken zlib stream. Rebuild the frame around the shortened
    // bytes instead of trusting arithmetic twice.
    let mut inner = Vec::new();
    write_var_int(200, &mut inner);
    inner.extend_from_slice(&whole[compressed_start..]);
    let mut rebuilt = Vec::new();
    write_var_int(inner.len() as i32, &mut rebuilt);
    rebuilt.extend_from_slice(&inner);

    let mut decoder = decoder_with(Compress::At { threshold });
    decoder.feed(&rebuilt);
    match decoder.next_frame() {
        // A stream cut short is refused by name: either the decompressor
        // notices the missing end-of-stream marker, or the frame's declared
        // size exposes the shortfall. What it may never do is decode.
        Err(error @ FrameError::Corrupt(_)) | Err(error @ FrameError::LengthMismatch { .. }) => {
            assert!(!error.to_string().is_empty())
        }
        other => panic!("expected a named corruption error, got {other:?}"),
    }

    // zlib whose checksum fails on the last byte of input.
    let good = encoded(&full, Compress::At { threshold });
    let last = good.len() - 1;
    let mut tampered = good.clone();
    tampered[last] ^= 0xFF;
    let mut decoder = decoder_with(Compress::At { threshold });
    decoder.feed(&tampered);
    match decoder.next_frame() {
        Err(FrameError::Corrupt(_)) => {}
        other => panic!("expected the checksum failure to be named, got {other:?}"),
    }
}

#[test]
fn the_declared_size_refuses_before_decompression_begins() {
    // The declared length is checked against the cap while the only thing
    // read is the header. The zlib stream here is genuinely valid — it would
    // decompress to four bytes — but its frame *claims* a hundred thousand
    // under a limit of two thousand, and that claim dies before a single
    // deflate block is interpreted. If this ever returns `LengthMismatch`
    // instead, decompression ran first, and the bomb defence became a bomb
    // report.
    fn compressed_section_of(frame: &Frame) -> Vec<u8> {
        let mut encoder = FrameEncoder::new(Limits::default());
        encoder.set_compression(Compress::At { threshold: 1 });
        let mut tmp = Vec::new();
        encoder.encode(frame, &mut tmp).expect("encode");
        let (_, prefix) = read_var_int(&tmp).expect("prefix");
        let (_, header) = read_var_int(&tmp[prefix..]).expect("header");
        tmp[prefix + header..].to_vec()
    }

    let mut inner = Vec::new();
    write_var_int(100_000, &mut inner);
    inner.extend_from_slice(&compressed_section_of(&Frame::new(1, b"dust")));
    let mut wire = Vec::new();
    write_var_int(inner.len() as i32, &mut wire);
    wire.extend_from_slice(&inner);

    let limits = Limits {
        max_frame_len: 2_097_151,
        max_decompressed_len: 2_000,
    };
    let mut decoder = FrameDecoder::new(limits);
    decoder.set_compression(Compress::At { threshold: 1 });
    decoder.feed(&wire);
    assert_eq!(
        decoder.next_frame(),
        Err(FrameError::DeclaredTooLarge {
            declared: 100_000,
            limit: 2_000,
        })
    );
}

#[test]
fn random_payloads_around_every_boundary_round_trip_through_random_chunks() {
    // Seeded, deterministic, and dense at the edges: for every threshold,
    // five hand-placed sizes straddle the line, and a few hundred more land
    // wherever the generator puts them. Feeds are split into random chunks
    // because sockets do not respect frame boundaries and neither should the
    // test.
    let mut rng = SplitMix64::new(0x0D57_C0DE_F00D_BABE);
    let modes = [
        Compress::Disabled,
        Compress::At { threshold: 0 },
        Compress::At { threshold: 1 },
        Compress::At { threshold: 16 },
        Compress::At { threshold: 256 },
        Compress::At { threshold: 4096 },
    ];

    for mode in modes {
        let threshold = match mode {
            Compress::Disabled => None,
            Compress::At { threshold } => Some(threshold),
        };

        let mut sizes: Vec<usize> = match threshold {
            Some(t) => vec![t.saturating_sub(2), t.saturating_sub(1), t, t + 1, t + 2],
            None => vec![0, 1, 63, 64, 65],
        };
        for _ in 0..300 {
            sizes.push(rng.below(2048) as usize);
        }

        for &body in &sizes {
            // Alternate compressible and incompressible content: the first
            // exercises the compressor's happy path, the second makes the
            // compressed form *larger* than the payload, which is the shape
            // that tempts an implementation into "optimising" the threshold.
            let compressible = rng.below(2) == 0;
            let filler = if compressible {
                rng.below(256) as u8
            } else {
                0u8 // replaced below
            };
            let mut body_bytes = Vec::with_capacity(body);
            for _ in 0..body {
                body_bytes.push(if compressible {
                    filler
                } else {
                    rng.below(256) as u8
                });
            }
            let id = rng.below(0x80) as i32;
            let frame = Frame::new(id, body_bytes);

            let wire = encoded(&frame, mode);

            // Where the mode forces a choice, the wire agrees with the
            // vanilla rule.
            if let Some(t) = threshold {
                let expected = if frame.payload_len() < t {
                    0
                } else {
                    frame.payload_len() as i32
                };
                assert_eq!(
                    claimed_data_length(&wire),
                    expected,
                    "mode {mode:?}: payload {} bytes",
                    frame.payload_len()
                );
            }

            // Feed it back in random pieces, some absurdly small.
            let mut decoder = decoder_with(mode);
            let mut cursor = 0usize;
            let mut got = None;
            while cursor < wire.len() {
                let take = (rng.below(24) + 1) as usize;
                let end = (cursor + take).min(wire.len());
                decoder.feed(&wire[cursor..end]);
                cursor = end;
                if let Some(decoded) = decoder.next_frame().expect("decode") {
                    got = Some(decoded);
                    break;
                }
            }
            assert_eq!(
                got.as_ref(),
                Some(&frame),
                "mode {mode:?}, payload {} bytes",
                frame.payload_len()
            );
            assert_eq!(decoder.buffered(), 0);
        }
    }
}

#[test]
fn the_wire_form_survives_a_payload_larger_than_the_threshold_by_far() {
    // One deliberately large case, well above every default-ish threshold,
    // because the property sweep caps at two kilobytes and the three-byte
    // length prefixes start mattering beyond that.
    let frame = Frame::new(0x32, vec![0x41; 70_000]);
    let wire = encoded(&frame, Compress::At { threshold: 256 });
    let mut decoder = decoder_with(Compress::At { threshold: 256 });
    decoder.feed(&wire);
    assert_eq!(decoder.next_frame(), Ok(Some(frame)));
}
