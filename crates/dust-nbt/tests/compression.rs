//! The compression wrappers: round-trips, refusals, and bombs.
//!
//! # What is external and what is self-consistent here
//!
//! Compress-then-decompress with this crate on both sides only proves flate2
//! agrees with itself. The *external* half — inflating streams produced by
//! Java's `GZIPOutputStream` — lives in `tests/vanilla.rs`, which reads every
//! structure file out of the server jar. What belongs here is the rest of the
//! contract: the schemes round-trip, a corrupted stream is refused rather than
//! silently truncated, and a stream that would expand past the caller's limit
//! stops at the limit instead of arriving as a gigabyte. Those are properties
//! of this crate's own code, not of the deflate implementation, because the
//! limit check and the error mapping are ours.

mod support;

use dust_nbt::compression::{self, Compression, CompressionError};

/// A payload with real structure: repeated prose compresses well, which is
/// what makes it the right input for the bomb tests below.
fn document() -> Vec<u8> {
    let mut out = Vec::new();
    for i in 0..50_000u32 {
        out.extend_from_slice(format!("entry:{i}:minecraft:some_block_id\n").as_bytes());
    }
    out
}

#[test]
fn both_schemes_round_trip_and_none_borrows() {
    let plain = document();

    for scheme in [Compression::Gzip, Compression::Zlib] {
        let wrapped = compression::compress(&plain, scheme).expect("compresses");
        assert_eq!(
            compression::decompress(&wrapped, scheme, compression::DEFAULT_FILE_LIMIT)
                .expect("decompresses")
                .as_ref(),
            plain.as_slice(),
            "{scheme:?} did not round-trip"
        );
    }

    // `None` is required to be free: no copy, no allocation, and the limit
    // does not apply to bytes that were never inflated.
    let borrowed = compression::decompress(&plain, Compression::None, 0).expect("borrows");
    assert!(matches!(borrowed, std::borrow::Cow::Borrowed(_)));
}

/// Detection from the first bytes, for the `.dat` files that arrive with no
/// header saying what they are.
#[test]
fn detection_recognises_each_scheme() {
    let plain = document();
    assert_eq!(
        Compression::detect(&compression::compress(&plain, Compression::Gzip).expect("writes")),
        Compression::Gzip,
        "the gzip magic number is unambiguous"
    );
    let zlib = compression::compress(&plain, Compression::Zlib).expect("writes");
    assert_eq!(Compression::detect(&zlib), Compression::Zlib);
    assert_eq!(Compression::detect(&plain), Compression::None);
    assert_eq!(Compression::detect(&[]), Compression::None);

    // The zlib header heuristic in the wild: low nibble 8, and the first two
    // bytes a multiple of 31. `78 9c` is the classic default.
    assert_eq!(Compression::detect(&[0x78, 0x9c]), Compression::Zlib);

    // And the region-file scheme byte maps exactly the three supported values,
    // refusing LZ4 (4), the out-of-file marker form (0x80 | scheme), and
    // everything else by name rather than by mistaking them.
    for (byte, expected) in [
        (1u8, Some(Compression::Gzip)),
        (2, Some(Compression::Zlib)),
        (3, Some(Compression::None)),
        (4, None),
        (0, None),
        (0x82, None),
        (255, None),
    ] {
        assert_eq!(Compression::from_region_scheme(byte), expected);
        if let Some(scheme) = expected {
            assert_eq!(scheme.region_scheme(), byte);
        }
    }
}

/// Corrupted input is refused with `Malformed`, naming the scheme — never
/// silently truncated into a plausible-looking prefix of the document. Each
/// case corrupts one specific part of the container so the suite says *which*
/// damage is covered.
#[test]
fn corrupted_streams_are_refused_with_the_scheme_named() {
    let plain = document();

    // --- gzip -------------------------------------------------------------
    let gzip = compression::compress(&plain, Compression::Gzip).expect("writes");

    // Bad magic: flip a bit of `1f 8b`. Asked explicitly for Gzip, the decoder
    // refuses; nothing about those bytes is a deflate stream it can trust.
    let mut bad_magic = gzip.clone();
    bad_magic[1] ^= 0xff;
    assert!(matches!(
        compression::decompress(&bad_magic, Compression::Gzip, usize::MAX),
        Err(CompressionError::Malformed {
            scheme: Compression::Gzip,
            ..
        })
    ));

    // Truncation: half a stream has no final block and no trailer.
    let truncated = &gzip[..gzip.len() / 2];
    assert!(matches!(
        compression::decompress(truncated, Compression::Gzip, usize::MAX),
        Err(CompressionError::Malformed {
            scheme: Compression::Gzip,
            ..
        })
    ));

    // Bad CRC32: valid deflate, damaged trailer. This one matters most — the
    // body inflates perfectly and only the checksum knows better.
    let mut bad_crc = gzip.clone();
    let last = bad_crc.len();
    bad_crc[last - 5] ^= 0x01;
    assert!(matches!(
        compression::decompress(&bad_crc, Compression::Gzip, usize::MAX),
        Err(CompressionError::Malformed {
            scheme: Compression::Gzip,
            ..
        })
    ));

    // --- zlib ---------------------------------------------------------------
    let zlib = compression::compress(&plain, Compression::Zlib).expect("writes");

    // Bad adler32: the zlib twin of the CRC case above.
    let mut bad_adler = zlib.clone();
    let last = bad_adler.len();
    bad_adler[last - 1] ^= 0x80;
    assert!(matches!(
        compression::decompress(&bad_adler, Compression::Zlib, usize::MAX),
        Err(CompressionError::Malformed {
            scheme: Compression::Zlib,
            ..
        })
    ));

    // Broken header check: the second byte must make the first two a multiple
    // of 31; anything else is refused before inflation starts.
    let mut bad_header = zlib.clone();
    bad_header[1] ^= 0x01;
    assert!(matches!(
        compression::decompress(&bad_header, Compression::Zlib, usize::MAX),
        Err(CompressionError::Malformed {
            scheme: Compression::Zlib,
            ..
        })
    ));

    // Truncated mid-stream, as a region file with a short chunk would be.
    assert!(matches!(
        compression::decompress(&zlib[..zlib.len() / 2], Compression::Zlib, usize::MAX),
        Err(CompressionError::Malformed {
            scheme: Compression::Zlib,
            ..
        })
    ));
}

/// A decompression bomb is stopped at the limit, not after it.
///
/// A few hundred kilobytes of zeros inflate to far more than any legitimate
/// document needs; the reader must report `TooLarge` having held memory to
/// roughly the limit, and must do it for both wrapped schemes. This is the
/// bound that makes [`dust_nbt::Limits::FILE`] safe to leave without a heap
/// quota: the tag reader never sees bytes the decompressor did not produce,
/// and the decompressor produces no more than it was allowed.
#[test]
fn decompression_bombs_stop_at_the_limit() {
    let bomb_payload = vec![0u8; 64 * 1024 * 1024];

    for scheme in [Compression::Gzip, Compression::Zlib] {
        let bomb = compression::compress(&bomb_payload, scheme).expect("compresses");
        assert!(
            bomb.len() < 200_000,
            "the test relies on {scheme:?} compressing well; got {} bytes",
            bomb.len()
        );

        match compression::decompress(&bomb, scheme, 1024 * 1024) {
            Err(CompressionError::TooLarge { limit }) => assert_eq!(limit, 1024 * 1024),
            other => panic!(
                "{scheme:?}: expected TooLarge, got {:?}",
                other.map(|c| c.len())
            ),
        }

        // Just under the limit is fine, which is what separates a cap from a
        // blanket refusal.
        let small = compression::compress(b"level format 19133", scheme).expect("compresses");
        assert!(compression::decompress(&small, scheme, 1024 * 1024).is_ok());
    }
}

// ---------------------------------------------------------------------------
// Streaming
//
// The streaming API must hold the buffer API's contract, not merely resemble
// it: byte-for-byte agreement, the same completeness refusals, and the same
// limit — met from either side of a read boundary. Its own extra surface,
// reading from something that produces bytes over time, brings two failure
// modes of its own that get pinned here too: sources that report spurious
// interruptions mid-stream, and sources that fail for real halfway through.
// ---------------------------------------------------------------------------

/// Yield one byte per call, to force every buffering path through its slowest
/// shape.
struct OneByteAtATime<'a>(&'a [u8]);

impl std::io::Read for OneByteAtATime<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.0.is_empty() || buf.is_empty() {
            return Ok(0);
        }
        buf[0] = self.0[0];
        self.0 = &self.0[1..];
        Ok(1)
    }
}

/// Report `ErrorKind::Interrupted` on every other call before yielding any
/// data: the error every `Read` consumer is supposed to retry.
struct Interrupting<'a> {
    payload: &'a [u8],
    calls: usize,
    /// How many bytes were handed out before the failure was injected.
    fail_after: Option<usize>,
    given: usize,
}

impl<'a> Interrupting<'a> {
    /// Interruptions throughout; the stream still completes.
    fn always(payload: &'a [u8]) -> Self {
        Self {
            payload,
            calls: 0,
            fail_after: None,
            given: 0,
        }
    }

    /// A hard failure (`ErrorKind::Other`) once `at` bytes are past.
    fn dying(payload: &'a [u8], at: usize) -> Self {
        Self {
            payload,
            calls: 0,
            fail_after: Some(at),
            given: 0,
        }
    }
}

impl std::io::Read for Interrupting<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.calls += 1;
        if let Some(at) = self.fail_after {
            if self.given >= at {
                return Err(std::io::Error::other("the reader died mid-payload"));
            }
        }
        if self.calls % 2 == 0 && !self.payload.is_empty() {
            return Err(std::io::ErrorKind::Interrupted.into());
        }
        let n = buf.len().min(self.payload.len());
        buf[..n].copy_from_slice(&self.payload[..n]);
        self.payload = &self.payload[n..];
        self.given += n;
        Ok(n)
    }
}

/// Whatever the scheme, streaming gives back exactly what the buffer API
/// does — even when fed one byte at a time.
#[test]
fn streaming_matches_the_buffer_api_byte_for_byte() {
    let plain = document();

    for scheme in [Compression::Gzip, Compression::Zlib] {
        let wrapped = compression::compress(&plain, scheme).expect("compresses");

        let whole =
            compression::decompress_stream(&wrapped[..], scheme, compression::DEFAULT_FILE_LIMIT)
                .expect("streams");
        assert_eq!(
            compression::decompress(&wrapped, scheme, compression::DEFAULT_FILE_LIMIT)
                .expect("buffers"),
            whole,
            "{scheme:?}: streaming and buffered disagree"
        );

        let dribbled = compression::decompress_stream(
            OneByteAtATime(&wrapped),
            scheme,
            compression::DEFAULT_FILE_LIMIT,
        )
        .expect("streams one byte at a time");
        assert_eq!(dribbled, plain, "{scheme:?} lost bytes in the dribble");
    }

    // `None` passes through, limit untouched by the wrapper's own bookkeeping.
    let passthrough =
        compression::decompress_stream(&plain[..], Compression::None, usize::MAX).expect("copies");
    assert_eq!(passthrough, plain);
}

/// Truncation is an error when met incrementally too: half a stream never
/// becomes a short, plausible document just because it arrived slowly.
#[test]
fn truncated_streams_are_refused_when_read_incrementally() {
    let plain = document();
    for scheme in [Compression::Gzip, Compression::Zlib] {
        let wrapped = compression::compress(&plain, scheme).expect("compresses");
        let truncated = &wrapped[..wrapped.len() / 2];
        assert!(
            matches!(
                compression::decompress_stream(truncated, scheme, usize::MAX),
                Err(CompressionError::Malformed { .. })
            ),
            "{scheme:?} accepted a truncated stream"
        );
    }
}

/// Spurious interruptions land mid-read by construction, and the stream still
/// completes with the right bytes.
#[test]
fn interruptions_mid_read_are_retried_not_fatal() {
    let plain = document();
    for scheme in [Compression::Gzip, Compression::Zlib] {
        let wrapped = compression::compress(&plain, scheme).expect("compresses");
        let out =
            compression::decompress_stream(Interrupting::always(&wrapped), scheme, usize::MAX)
                .expect("interruptions are retried until they stop");
        assert_eq!(
            out, plain,
            "{scheme:?} survived interruptions but lost bytes"
        );
    }
}

/// A source that fails for real halfway through yields `Malformed`, and the
/// partial output it had produced goes with it — never half a chunk.
#[test]
fn a_source_that_fails_halfway_is_malformed_with_no_partial_document() {
    let plain = document();
    for scheme in [Compression::Gzip, Compression::Zlib] {
        let wrapped = compression::compress(&plain, scheme).expect("compresses");
        let dying = Interrupting::dying(&wrapped, wrapped.len() / 4);

        match compression::decompress_stream(dying, scheme, usize::MAX) {
            Err(CompressionError::Malformed {
                scheme: named,
                detail,
            }) => {
                assert_eq!(named, scheme);
                assert!(
                    detail.contains("mid-payload"),
                    "detail names the cause: {detail}"
                );
            }
            other => panic!("{scheme:?}: expected Malformed from a dead source, got {other:?}"),
        }
    }
}

/// The streaming bomb stops at the limit like the buffered one, whether the
/// caller drains through [`compression::decompress_stream`] or reads by hand.
#[test]
fn streaming_bombs_stop_at_the_limit() {
    let bomb_payload = vec![0u8; 64 * 1024 * 1024];

    for scheme in [Compression::Gzip, Compression::Zlib] {
        let bomb = compression::compress(&bomb_payload, scheme).expect("compresses");
        match compression::decompress_stream(&bomb[..], scheme, 1024 * 1024) {
            Err(CompressionError::TooLarge { limit }) => assert_eq!(limit, 1024 * 1024),
            other => panic!("{scheme:?}: expected TooLarge streaming, got {other:?}"),
        }

        // By hand: deliveries stop at exactly the limit, then name it.
        let mut reader = compression::StreamingDecompress::new(&bomb[..], scheme, 1024 * 1024);
        let mut total = 0usize;
        let mut chunk = [0u8; 256 * 1024];
        loop {
            match std::io::Read::read(&mut reader, &mut chunk) {
                Ok(0) => break,
                Ok(n) => total += n,
                Err(error) => {
                    let inner = error
                        .into_inner()
                        .and_then(|inner| inner.downcast::<CompressionError>().ok())
                        .map(|boxed| *boxed);
                    assert_eq!(
                        inner,
                        Some(CompressionError::TooLarge { limit: 1024 * 1024 }),
                        "{scheme:?}: the io error carries the crate's own"
                    );
                    break;
                }
            }
            assert!(
                total <= 1024 * 1024,
                "{scheme:?} delivered {total} bytes against a 1 MiB limit"
            );
        }
        assert_eq!(
            total,
            1024 * 1024,
            "{scheme:?}: the last allowed bytes arrive"
        );
        assert_eq!(reader.produced(), 1024 * 1024);
    }
}

/// Two documents where one was promised stay refused: a complete stream plus
/// anything after it is malformed, as the buffer API has it.
#[test]
fn bytes_following_a_complete_stream_are_refused_when_streaming() {
    let first = b"first document".to_vec();
    let second = b"second document".to_vec();

    for scheme in [Compression::Gzip, Compression::Zlib] {
        let a = compression::compress(&first, scheme).expect("compresses");
        let b = compression::compress(&second, scheme).expect("compresses");
        let mut concatenated = a;
        concatenated.extend_from_slice(&b);

        assert!(
            matches!(
                compression::decompress_stream(&concatenated[..], scheme, usize::MAX),
                Err(CompressionError::Malformed { .. })
            ),
            "{scheme:?} accepted a concatenation"
        );
    }

    // And a single clean stream passes the same probe untouched.
    let single = compression::compress(&first, Compression::Zlib).expect("compresses");
    assert_eq!(
        compression::decompress_stream(&single[..], Compression::Zlib, usize::MAX)
            .expect("one stream is fine"),
        first
    );
}
