//! The three wrappers NBT arrives inside on disk.
//!
//! Minecraft stores NBT compressed, and does not use one scheme:
//!
//! * `level.dat`, `raids.dat`, player files and structure `.nbt` files are
//!   **gzip**.
//! * A chunk inside a region file is **gzip, zlib or uncompressed**, chosen per
//!   chunk. Its 5-byte header is a big-endian `i32` length followed by one
//!   *scheme* byte: 1 gzip, 2 zlib, 3 uncompressed. In practice everything
//!   Minecraft has written since Beta is 2.
//! * NBT in a packet is not compressed here at all. Packet compression, if the
//!   connection negotiated it, wraps the whole packet; the NBT inside is raw.
//!
//! # Detection is a fallback, not the mechanism
//!
//! [`Compression::detect`] exists because a `.dat` file arrives with no header
//! to say what it is. Where a header does say — a region-file chunk — the
//! header is authoritative and [`Compression::from_region_scheme`] is the
//! function to use. The two are kept apart deliberately: a reader that sniffs
//! when it was told is a reader that can be lied to.
//!
//! # The decompression limit is where a file's size is bounded
//!
//! [`crate::Limits::FILE`] leaves the tag reader's heap budget effectively
//! unbounded, and this is why it can. A 4 KiB region-file slot can hold a
//! deflate stream that expands to a gigabyte, and the tag reader would never
//! see the header that did it — by the time it runs, the gigabyte exists.
//! Bounding the *output* of decompression is the only place that particular
//! bomb can be caught, so every function here takes a limit and none of them
//! has a default that means "no limit".
//!
//! Completeness is checked as well as size. A deflate body that stops early is
//! reported as malformed rather than returned as a short, plausible document —
//! a prefix of a chunk parses exactly wrong enough to corrupt a world. Bytes
//! *after* a complete stream are refused for the same reason; a caller holding
//! a padded region-file slot slices to the header's length before asking here.
//!
//! # Streaming
//!
//! The buffer functions want the whole compressed payload in memory — which a
//! region read already has, since the slot was read as one block. When the
//! payload arrives from something longer than memory should hold, or the
//! caller simply reads it incrementally, [`StreamingDecompress`] inflates from
//! any [`std::io::Read`] as the bytes arrive and [`decompress_stream`] drains
//! one to a `Vec`. Both enforce the limit with the buffer API's semantics —
//! the moment output would pass it, [`CompressionError::TooLarge`], and no
//! further bytes — and both keep its completeness rules: a stream that ends
//! before its final block, fails its container checksum, or carries bytes
//! after the end is malformed no matter how slowly it arrived.
//!
//! That last clause is why neither streaming function goes through flate2's
//! read adapters. An adapter reports end-of-input much like end-of-stream,
//! which is precisely the confusion [`inflate_zlib`] was written to remove;
//! the streaming engine drives raw deflate itself and owns the gzip and zlib
//! framing around it, so header, checksum and trailer are all ours to check.

use std::fmt;
use std::io::Read;

use flate2::read::GzDecoder;
use flate2::write::{GzEncoder, ZlibEncoder};
use flate2::Compression as Level;

/// How a document is wrapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    /// Raw NBT.
    None,
    /// gzip (RFC 1952): `1f 8b`, then a deflate stream.
    Gzip,
    /// zlib (RFC 1950): a two-byte header, then a deflate stream.
    Zlib,
}

impl Compression {
    /// The scheme byte a region-file chunk header carries.
    ///
    /// Returns `None` for anything else, which includes 4 — LZ4, added in
    /// 1.21.5 — and the high-bit form (`0x80 | scheme`) that marks a chunk
    /// stored outside the region file in its own `.mcc`. Both are real values
    /// that a future world may contain and neither is supported here, so they
    /// are refused by name rather than by being mistaken for something else.
    pub fn from_region_scheme(scheme: u8) -> Option<Self> {
        match scheme {
            1 => Some(Self::Gzip),
            2 => Some(Self::Zlib),
            3 => Some(Self::None),
            _ => None,
        }
    }

    /// The scheme byte this scheme is written as.
    pub fn region_scheme(self) -> u8 {
        match self {
            Self::Gzip => 1,
            Self::Zlib => 2,
            Self::None => 3,
        }
    }

    /// Guess from the first bytes.
    ///
    /// gzip is unambiguous: `1f 8b` is its magic number and no NBT document
    /// starts that way, because `1f` is not one of the thirteen tag ids.
    ///
    /// zlib has no magic number, only a two-byte header whose low nibble of the
    /// first byte is 8 and whose sixteen bits are a multiple of 31. That test
    /// is what everyone uses and it is a *heuristic*: `08 1f`, `18 09` and
    /// `78 9c` all pass it, and only the last is really a zlib stream. What
    /// makes it safe enough here is that a document which is not compressed
    /// starts with a tag id in `0..=12`, and the low nibble of such a byte is
    /// the id itself, so only id 8 — `TAG_String` — can collide, and a
    /// `TAG_String` root is not something Minecraft writes.
    ///
    /// **What this does not catch**: a hand-made document with a `TAG_String`
    /// root whose name length happens to make the header a multiple of 31 would
    /// be taken for zlib and fail to inflate. Use
    /// [`Compression::from_region_scheme`] wherever a scheme byte exists.
    pub fn detect(bytes: &[u8]) -> Self {
        match bytes {
            [0x1f, 0x8b, ..] => Self::Gzip,
            [first, second, ..]
                if first & 0x0f == 0x08
                    && (u16::from(*first) * 256 + u16::from(*second)) % 31 == 0 =>
            {
                Self::Zlib
            }
            _ => Self::None,
        }
    }
}

/// Decompression refused or failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompressionError {
    /// The stream did not inflate.
    Malformed {
        scheme: Compression,
        /// `io::Error` is neither `Clone` nor `PartialEq`, and every error this
        /// can produce is an inflate failure whose only useful content is its
        /// message, so the message is what is kept.
        detail: String,
    },
    /// Inflating produced more than the caller allowed.
    ///
    /// Reported as soon as the limit is passed, so the memory actually used is
    /// bounded by the limit plus one read buffer, not by whatever the stream
    /// would eventually have produced.
    TooLarge { limit: usize },
    /// Compressing failed, which in practice means the allocator did.
    CompressFailed { scheme: Compression, detail: String },
}

impl fmt::Display for CompressionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed { scheme, detail } => {
                write!(
                    f,
                    "the {scheme:?} stream could not be decompressed: {detail}"
                )
            }
            Self::TooLarge { limit } => write!(
                f,
                "decompressing produced more than the {limit} bytes allowed"
            ),
            Self::CompressFailed { scheme, detail } => {
                write!(f, "compressing as {scheme:?} failed: {detail}")
            }
        }
    }
}

impl std::error::Error for CompressionError {}

/// A limit for documents read from a world directory.
///
/// A vanilla chunk decompresses to a few hundred kilobytes; the largest seen in
/// practice, a chunk full of shulker boxes full of written books, is a few
/// megabytes. 32 MiB is far above anything legitimate and far below anything
/// that would trouble a server, so it separates the two cases without a tuning
/// knob nobody would know how to set.
pub const DEFAULT_FILE_LIMIT: usize = 32 * 1024 * 1024;

/// Decompress `bytes` according to `scheme`, refusing to produce more than
/// `limit` bytes.
///
/// [`Compression::None`] borrows and copies nothing.
pub fn decompress(
    bytes: &[u8],
    scheme: Compression,
    limit: usize,
) -> std::result::Result<std::borrow::Cow<'_, [u8]>, CompressionError> {
    match scheme {
        Compression::None => Ok(std::borrow::Cow::Borrowed(bytes)),
        // The gzip container validates itself — the trailer carries the
        // payload's CRC32 and length, and the decoder refuses to report the
        // end of input without them — so a plain bounded read is enough.
        Compression::Gzip => inflate(GzDecoder::new(bytes), scheme, limit).map(Into::into),
        Compression::Zlib => inflate_zlib(bytes, scheme, limit).map(Into::into),
    }
}

/// [`decompress`], choosing the scheme with [`Compression::detect`].
pub fn decompress_detected(
    bytes: &[u8],
    limit: usize,
) -> std::result::Result<std::borrow::Cow<'_, [u8]>, CompressionError> {
    decompress(bytes, Compression::detect(bytes), limit)
}

/// Read `source` to the end, stopping the moment it passes `limit`.
///
/// Written as a bounded loop rather than `take(limit).read_to_end(...)` for one
/// reason: `take` produces exactly `limit` bytes and then reports success, so a
/// stream that would have produced more is silently truncated into a document
/// that parses and is wrong. Reading one byte past the limit and failing is the
/// difference between a rejected file and a corrupted one.
fn inflate<R: Read>(
    mut source: R,
    scheme: Compression,
    limit: usize,
) -> std::result::Result<Vec<u8>, CompressionError> {
    // A quarter of a page per read is enough that syscall-free inflate is not
    // dominated by loop overhead, and small enough that the overshoot past the
    // limit is bounded by it.
    const CHUNK: usize = 64 * 1024;
    let mut out = Vec::new();
    loop {
        let start = out.len();
        out.resize(start + CHUNK, 0);
        match source.read(&mut out[start..]) {
            Ok(0) => {
                out.truncate(start);
                return Ok(out);
            }
            Ok(n) => {
                out.truncate(start + n);
                if out.len() > limit {
                    return Err(CompressionError::TooLarge { limit });
                }
            }
            Err(error) => {
                return Err(CompressionError::Malformed {
                    scheme,
                    detail: error.to_string(),
                })
            }
        }
    }
}

/// Inflate a zlib stream, refusing anything that is not the whole stream.
///
/// This cannot go through the plain read loop above, because the backend under
/// flate2 reports running out of *input* much like running out of *stream*: a
/// truncated body inflates happily into a short `Ok`, a plausible prefix of
/// the document that would parse exactly wrong enough to corrupt a world.
/// Driving [`flate2::mem::Decompress`] directly gives the one signal that
/// matters — `Status::StreamEnd`, which for zlib also means the adler32 has
/// been checked — and lets success be defined as three conditions at once:
/// the stream said it ended, it consumed every byte it was given, and it never
/// produced more than `limit`.
fn inflate_zlib(
    bytes: &[u8],
    scheme: Compression,
    limit: usize,
) -> std::result::Result<Vec<u8>, CompressionError> {
    use flate2::{Decompress, FlushDecompress, Status};

    const CHUNK: usize = 64 * 1024;
    let mut decoder = Decompress::new(true);
    let mut out = Vec::new();
    loop {
        // Reserve the next block of output here rather than letting the
        // decoder grow the buffer: `decompress_vec` writes only into spare
        // capacity, so memory stays bounded by `limit` plus one block instead
        // of by whatever the stream claimed.
        out.reserve(CHUNK);
        let start = out.len();
        let consumed_before = decoder.total_in() as usize;
        let status = decoder
            .decompress_vec(&bytes[consumed_before..], &mut out, FlushDecompress::None)
            .map_err(|error| CompressionError::Malformed {
                scheme,
                detail: error.to_string(),
            })?;
        if out.len() > limit {
            return Err(CompressionError::TooLarge { limit });
        }
        match status {
            Status::StreamEnd => break,
            Status::Ok | Status::BufError => {}
        }
        if decoder.total_in() as usize == bytes.len() {
            return Err(CompressionError::Malformed {
                scheme,
                detail: "the input ran out before the stream's final block".to_owned(),
            });
        }
        if decoder.total_in() as usize == consumed_before && out.len() == start {
            return Err(CompressionError::Malformed {
                scheme,
                detail: "the stream made no progress".to_owned(),
            });
        }
    }
    let consumed = decoder.total_in() as usize;
    if consumed != bytes.len() {
        return Err(CompressionError::Malformed {
            scheme,
            detail: format!(
                "the stream ended after {} bytes but {} were given",
                consumed,
                bytes.len()
            ),
        });
    }
    Ok(out)
}

/// Compress `bytes`.
pub fn compress(
    bytes: &[u8],
    scheme: Compression,
) -> std::result::Result<Vec<u8>, CompressionError> {
    use std::io::Write as _;
    match scheme {
        Compression::None => Ok(bytes.to_vec()),
        Compression::Gzip => {
            let mut encoder = GzEncoder::new(Vec::new(), Level::default());
            encoder
                .write_all(bytes)
                .and_then(|()| encoder.finish())
                .map_err(|error| CompressionError::CompressFailed {
                    scheme,
                    detail: error.to_string(),
                })
        }
        Compression::Zlib => {
            let mut encoder = ZlibEncoder::new(Vec::new(), Level::default());
            encoder
                .write_all(bytes)
                .and_then(|()| encoder.finish())
                .map_err(|error| CompressionError::CompressFailed {
                    scheme,
                    detail: error.to_string(),
                })
        }
    }
}

/// Decompress by pulling from `source`, producing no more than `limit` bytes.
///
/// The streaming twin of [`decompress`]: memory held is the output so far plus
/// one input block, never the whole compressed payload on top of it. The
/// completeness rules are the buffer API's, not weaker ones — a stream that
/// runs out before its final block, fails its checksum, or is followed by
/// further bytes is [`CompressionError::Malformed`] here exactly as it is
/// there.
///
/// ```
/// use dust_nbt::compression::{self, Compression};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let wrapped = compression::compress(b"hello", Compression::Zlib)?;
/// let out = compression::decompress_stream(&wrapped[..], Compression::Zlib, 1024)?;
/// assert_eq!(out, b"hello");
/// # Ok(())
/// # }
/// ```
pub fn decompress_stream<R: Read>(
    source: R,
    scheme: Compression,
    limit: usize,
) -> std::result::Result<Vec<u8>, CompressionError> {
    let mut reader = StreamingDecompress::new(source, scheme, limit);
    let mut out = Vec::new();
    let mut chunk = [0u8; 64 * 1024];
    loop {
        // Zero bytes means the engine reached a verified end: header checked,
        // final block seen, trailer matched. Anything short of that errors.
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&chunk[..n]),
            Err(error) => return Err(error_from_io(scheme, error)),
        }
    }
    debug_assert!(out.len() <= limit, "the reader enforces the limit itself");
    // A complete stream followed by more bytes is the concatenation case: two
    // documents where one was promised. The buffer API refuses it by comparing
    // consumed against length; here one probe read past the verified end
    // settles it. (`None` borrows the whole slice as-is, so there is no end
    // for anything to come after.)
    if scheme != Compression::None {
        let mut trailing = [0u8; 1];
        let mut rest = reader.into_inner();
        if matches!(rest.read(&mut trailing), Ok(n) if n > 0) {
            return Err(CompressionError::Malformed {
                scheme,
                detail: "bytes follow the end of the compressed stream".to_owned(),
            });
        }
    }
    Ok(out)
}

/// A [`Read`] that inflates `source` as it goes, under a hard output limit.
///
/// Reads never produce more than `limit` bytes in total. Passing the limit
/// delivers the bytes up to it once — trimmed to the boundary — and every
/// later read fails with [`CompressionError::TooLarge`]. An incremental
/// consumer may already have used earlier bytes, which cannot happen through
/// [`decompress`]; that all-or-nothing form of the contract lives in
/// [`decompress_stream`] and in [`decompress_stream`]'s buffer twin.
///
/// Errors surface as [`std::io::Error`] values wrapping this crate's own type:
/// `error.into_inner().downcast::<CompressionError>()` recovers it unchanged,
/// keeping one taxonomy across both APIs. Spurious interruptions from the
/// source (`io::ErrorKind::Interrupted`) are retried internally rather than
/// handed up; an adapter that leaks them makes every caller re-implement the
/// retry loop the standard library says `Interrupted` calls for.
pub struct StreamingDecompress<R: Read> {
    mode: Mode<R>,
    limit: usize,
    produced: usize,
    failed: Option<CompressionError>,
}

/// `None` needs no engine â the bytes are the document â so it passes
/// through untouched; the wrapped schemes share the hand-driven one.
enum Mode<R: Read> {
    Passthrough { source: R, seen_end: bool },
    Inflating { engine: Engine, source: R },
}

impl<R: Read> StreamingDecompress<R> {
    /// Wrap `source`, inflating according to `scheme`, refusing output past
    /// `limit`.
    ///
    /// Construction reads nothing and cannot fail; the container header is
    /// parsed on the first read.
    pub fn new(source: R, scheme: Compression, limit: usize) -> Self {
        let mode = match scheme {
            Compression::None => Mode::Passthrough {
                source,
                seen_end: false,
            },
            Compression::Gzip | Compression::Zlib => Mode::Inflating {
                engine: Engine::new(scheme),
                source,
            },
        };
        Self {
            mode,
            limit,
            produced: 0,
            failed: None,
        }
    }

    /// Whether the compressed stream has run to its verified end: header
    /// accepted, final block seen, container checksum matched.
    ///
    /// Until a read has returned zero bytes under this condition, the stream
    /// either has not been fully drained or ended early;
    /// [`decompress_stream`] turns the second case into
    /// [`CompressionError::Malformed`].
    pub fn stream_finished(&self) -> bool {
        match &self.mode {
            // Pass-through has nothing to verify; its end is the source's.
            Mode::Passthrough { seen_end, .. } => *seen_end,
            Mode::Inflating { engine, .. } => engine.stage == Stage::Done,
        }
    }

    /// The uncompressed bytes delivered so far.
    pub fn produced(&self) -> usize {
        self.produced
    }

    /// The limit this reader enforces.
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// The wrapped source.
    pub fn get_ref(&self) -> &R {
        match &self.mode {
            Mode::Passthrough { source, .. } => source,
            Mode::Inflating { source, .. } => source,
        }
    }

    /// Consume this reader, returning the wrapped source.
    pub fn into_inner(self) -> R {
        match self.mode {
            Mode::Passthrough { source, .. } => source,
            Mode::Inflating { source, .. } => source,
        }
    }

    /// The scheme this reader was built for, for naming in errors.
    fn scheme(&self) -> Compression {
        match &self.mode {
            Mode::Passthrough { .. } => Compression::None,
            Mode::Inflating { engine, .. } => engine.scheme,
        }
    }

    /// Record `error` as permanent and hand it up as an `io::Error`.
    ///
    /// Sticky by design: once a stream has failed, every later read reports
    /// the same failure, because "the next read worked" after corruption is
    /// exactly the kind of quiet recovery this module refuses to perform.
    fn fail(&mut self, error: CompressionError) -> std::io::Error {
        if self.failed.is_none() {
            self.failed = Some(error);
        }
        io_error(self.failed.as_ref().expect("just stored"))
    }

    /// Account `delivered` against the limit, trimming a read that lands
    /// across it.
    ///
    /// The crossing read delivers everything up to the boundary and no more;
    /// if nothing was left within it, the read errors at once. A trimmed read
    /// must never surface as `Ok(0)` â zero bytes is end-of-input, and the
    /// caller would stop reading believing a lie.
    fn account(&mut self, delivered: usize) -> std::result::Result<usize, CompressionError> {
        if self.produced.saturating_add(delivered) > self.limit {
            let allowed = self.limit - self.produced.min(self.limit);
            self.produced = self.limit;
            let error = CompressionError::TooLarge { limit: self.limit };
            if allowed > 0 {
                self.failed = Some(error);
                Ok(allowed)
            } else {
                Err(error)
            }
        } else {
            self.produced += delivered;
            Ok(delivered)
        }
    }
}

impl<R: Read> Read for StreamingDecompress<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if let Some(error) = &self.failed {
            return Err(io_error(error));
        }
        match &mut self.mode {
            Mode::Passthrough { source, seen_end } => {
                let n = match fill(buf, source) {
                    Ok(n) => n,
                    Err(error) => return Err(self.fail(malformed(self.scheme(), error))),
                };
                if n == 0 {
                    *seen_end = true;
                    return Ok(0);
                }
                self.account(n).map_err(|error| self.fail(error))
            }
            Mode::Inflating { engine, source } => match engine.pump(buf, source) {
                Ok(Pump::Bytes(n)) => self.account(n).map_err(|error| self.fail(error)),
                Ok(Pump::End) => Ok(0),
                Err(error) => Err(self.fail(error)),
            },
        }
    }
}

/// What one turn of [`Engine::pump`] accomplished.
enum Pump {
    /// `n` fresh output bytes are in the caller's buffer.
    Bytes(usize),
    /// The stream finished and its container checksum verified.
    End,
}

/// Debug by the facts a caller can act on rather than by decoder internals,
/// which are opaque either way.
impl<R: Read> std::fmt::Debug for StreamingDecompress<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamingDecompress")
            .field("finished", &self.stream_finished())
            .field("limit", &self.limit)
            .field("produced", &self.produced)
            .field("failed", &self.failed)
            .finish()
    }
}

/// Where the hand-driven inflate stands.
#[derive(Debug, PartialEq, Eq)]
enum Stage {
    /// Collecting the fixed front of the header (2 bytes zlib, 10 gzip).
    Head,
    /// Reading the two little-endian bytes of a gzip extra field's length.
    ExtraLen,
    /// Discarding that many bytes of an extra field.
    SkipExtra { remaining: usize },
    /// Discarding a zero-terminated field: name, then comment.
    SkipZeroTerminated,
    /// Discarding a two-byte header CRC nobody in this ecosystem writes but
    /// the format permits.
    SkipHeaderCrc,
    /// Inflating the deflate body.
    Body,
    /// Collecting the trailer (4 bytes zlib, 8 gzip) for verification.
    Tail,
    /// Everything verified; nothing left to produce.
    Done,
}

/// The hand-driven inflate for one container, independent of the `Read` it is
/// fed from.
///
/// Raw deflate comes from flate2; everything around it — framing, optional
/// fields, checksums, the definition of "ended" — is ours. Header and trailer
/// are tiny and arrive byte-at-a-time; the body pulls input in blocks.
struct Engine {
    scheme: Compression,
    stage: Stage,
    decoder: flate2::Decompress,
    /// Which optional gzip fields remain, in RFC 1952 order.
    fields: u8,
    /// The header accumulated so far, parsed at each stage boundary.
    head: Vec<u8>,
    /// How many header bytes the current pass wants in total.
    want: usize,
    /// Length of the extra field currently being skipped.
    extra_len: usize,
    /// Staged compressed input for the body, one block at a time.
    staging: Vec<u8>,
    filled: usize,
    eaten: usize,
    /// Trailer bytes collected so far.
    tail: [u8; 8],
    tail_len: usize,
    /// Running CRC32 over produced bytes, stored pre-finalised (gzip).
    crc: u32,
    /// Running Adler-32 over produced bytes, as `(s1, s2)` (zlib).
    adler: (u32, u32),
    /// Total produced bytes, for gzip's ISIZE cross-check.
    total_out: u64,
}

const FHCRC: u8 = 0x02;
const FEXTRA: u8 = 0x04;
const FNAME: u8 = 0x08;
const FCOMMENT: u8 = 0x10;

impl Engine {
    fn new(scheme: Compression) -> Self {
        Self {
            scheme,
            stage: Stage::Head,
            // Raw deflate: the framing around the compressed body is ours.
            decoder: flate2::Decompress::new(false),
            fields: 0,
            head: Vec::with_capacity(16),
            want: match scheme {
                Compression::Zlib => 2,
                _ => 10,
            },
            extra_len: 0,
            staging: vec![0; 64 * 1024],
            filled: 0,
            eaten: 0,
            tail: [0; 8],
            tail_len: 0,
            crc: 0xffff_ffff,
            adler: (1, 0),
            total_out: 0,
        }
    }

    /// Pull one byte, retrying interruptions, mapping failure and exhaustion
    /// onto this crate's errors with the phase named.
    fn take(&mut self, source: &mut dyn Read) -> Result<u8, CompressionError> {
        let mut one = [0u8; 1];
        let byte = loop {
            break match source.read(&mut one) {
                Ok(1) => Ok(Some(one[0])),
                Ok(_) => Ok(None),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => Err(malformed(self.scheme, error)),
            };
        };
        let during = if matches!(
            self.stage,
            Stage::Head
                | Stage::ExtraLen
                | Stage::SkipExtra { .. }
                | Stage::SkipZeroTerminated
                | Stage::SkipHeaderCrc
        ) {
            "the container header"
        } else {
            "the container trailer"
        };
        byte.and_then(|byte| {
            byte.map_or_else(
                || {
                    Err(CompressionError::Malformed {
                        scheme: self.scheme,
                        detail: format!("the input ran out during {during}"),
                    })
                },
                Ok,
            )
        })
    }

    /// The next byte of framing: anything left staged ahead of the source
    /// first — the body reader over-pulls by design — then the source itself.
    fn next_byte(&mut self, source: &mut dyn Read) -> Result<u8, CompressionError> {
        if self.eaten < self.filled {
            let byte = self.staging[self.eaten];
            self.eaten += 1;
            return Ok(byte);
        }
        self.take(source)
    }

    /// After any optional field completes, start the next one or reach the
    /// body. RFC 1952 fixes the order: extra, name, comment, header CRC.
    fn next_field(&mut self) {
        if self.fields & FEXTRA != 0 {
            self.fields &= !FEXTRA;
            self.stage = Stage::ExtraLen;
        } else if self.fields & FNAME != 0 {
            self.fields &= !FNAME;
            self.stage = Stage::SkipZeroTerminated;
        } else if self.fields & FCOMMENT != 0 {
            self.fields &= !FCOMMENT;
            // Only reachable when both bits were set: a name was skipped
            // first, so this skip is the comment.
            self.stage = Stage::SkipZeroTerminated;
        } else if self.fields & FHCRC != 0 {
            self.fields &= !FHCRC;
            self.stage = Stage::SkipHeaderCrc;
        } else {
            self.stage = Stage::Body;
        }
    }

    /// Advance the state machine until it produces, ends, or genuinely needs
    /// to be called again.
    fn pump(&mut self, buf: &mut [u8], source: &mut dyn Read) -> Result<Pump, CompressionError> {
        use flate2::{FlushDecompress, Status};

        loop {
            match self.stage {
                Stage::Done => {
                    // The verified end happened; bytes staged past it are the
                    // concatenation case. (Bytes still unread in the source
                    // itself are caught by `decompress_stream`'s probe, which
                    // can see further than staging ever holds.)
                    if self.eaten < self.filled {
                        return Err(CompressionError::Malformed {
                            scheme: self.scheme,
                            detail: format!(
                                "{} bytes follow the end of the compressed stream",
                                self.filled - self.eaten
                            ),
                        });
                    }
                    return Ok(Pump::End);
                }

                Stage::Head => {
                    while self.head.len() < self.want {
                        let byte = self.take(source)?;
                        self.head.push(byte);
                    }
                    if self.scheme == Compression::Zlib {
                        // Same heuristic `detect` uses, now as a rule: low
                        // nibble 8, first two bytes a multiple of 31.
                        let cmf = self.head[0];
                        let flg = self.head[1];
                        if cmf & 0x0f != 0x08 || (u16::from(cmf) * 256 + u16::from(flg)) % 31 != 0 {
                            return Err(CompressionError::Malformed {
                                scheme: self.scheme,
                                detail: "the two header bytes are not a valid zlib header"
                                    .to_owned(),
                            });
                        }
                        self.stage = Stage::Body;
                        continue;
                    }
                    if self.head.len() == 10 {
                        // The fixed part of a gzip header. MTIME, XFL and OS
                        // are informational and nobody downstream of them.
                        let head = &self.head;
                        if head[0] != 0x1f || head[1] != 0x8b {
                            return Err(CompressionError::Malformed {
                                scheme: self.scheme,
                                detail: "the magic number is not gzip's 1f 8b".to_owned(),
                            });
                        }
                        if head[2] != 8 {
                            return Err(CompressionError::Malformed {
                                scheme: self.scheme,
                                detail: format!("gzip method {} is not deflate", head[2]),
                            });
                        }
                        self.fields = head[3];
                        self.next_field();
                    }
                    // Ten more bytes were wanted for an FHCRC's two; fall back
                    // into collecting them.
                    if self.want > self.head.len() {
                        self.want = self.head.len();
                    }
                    continue;
                }

                Stage::ExtraLen => {
                    while self.head.len() < self.want {
                        let byte = self.take(source)?;
                        self.head.push(byte);
                    }
                    self.extra_len =
                        u16::from_le_bytes([self.head[self.want - 2], self.head[self.want - 1]])
                            as usize;
                    self.stage = Stage::SkipExtra {
                        remaining: self.extra_len,
                    };
                    continue;
                }

                Stage::SkipExtra { .. } => {
                    // The count is copied out and the stage borrowed afresh
                    // per byte, because `take` needs the whole of `self` and
                    // could not run inside a borrow of one of its fields.
                    let remaining = match self.stage {
                        Stage::SkipExtra { remaining } => remaining,
                        _ => unreachable!("arm cannot change mid-step"),
                    };
                    for _ in 0..remaining {
                        let _ = self.take(source)?;
                    }
                    self.next_field();
                    continue;
                }

                Stage::SkipZeroTerminated | Stage::SkipHeaderCrc => {
                    if self.stage == Stage::SkipZeroTerminated {
                        while self.take(source)? != 0 {}
                    } else {
                        let _ = self.take(source)?;
                        let _ = self.take(source)?;
                    }
                    self.next_field();
                    continue;
                }

                Stage::Body => {
                    if self.eaten == self.filled {
                        self.filled = fill(&mut self.staging, source)
                            .map_err(|e| malformed(self.scheme, e))?;
                        self.eaten = 0;
                        if self.filled == 0 {
                            return Err(CompressionError::Malformed {
                                scheme: self.scheme,
                                detail: "the input ran out before the stream's final block"
                                    .to_owned(),
                            });
                        }
                    }

                    let in_before = self.decoder.total_in();
                    let out_before = self.decoder.total_out();
                    let status = self
                        .decoder
                        .decompress(
                            &self.staging[self.eaten..self.filled],
                            buf,
                            FlushDecompress::None,
                        )
                        .map_err(|error| CompressionError::Malformed {
                            scheme: self.scheme,
                            detail: error.to_string(),
                        })?;
                    self.eaten += (self.decoder.total_in() - in_before) as usize;
                    let produced = (self.decoder.total_out() - out_before) as usize;
                    std::eprintln!(
                        "BODY status={status:?} produced={produced} total_in={} eaten={}",
                        self.decoder.total_in(),
                        self.eaten
                    );
                    let produced_bytes = &buf[..produced];
                    self.crc = crc_update(self.crc, produced_bytes);
                    self.adler = adler_update(self.adler, produced_bytes);
                    self.total_out += produced as u64;

                    match status {
                        Status::StreamEnd => {
                            // Whatever is staged beyond the final block is the
                            // trailer and possibly garbage after it; the tail
                            // stage consumes the first and the `Done` stage
                            // names the second.
                            self.stage = Stage::Tail;
                            self.tail_len = 0;
                            if produced > 0 {
                                return Ok(Pump::Bytes(produced));
                            }
                        }
                        Status::Ok | Status::BufError => {}
                    }

                    if produced > 0 {
                        return Ok(Pump::Bytes(produced));
                    }
                    if self.eaten < self.filled {
                        // Input went in, room to write existed, and neither
                        // bytes nor an end came back out: stalled, and pumping
                        // again would spin forever.
                        return Err(CompressionError::Malformed {
                            scheme: self.scheme,
                            detail: "the stream made no progress".to_owned(),
                        });
                    }
                    // Everything staged was consumed without producing; refill
                    // and go around.
                }

                Stage::Tail => {
                    let need = if self.scheme == Compression::Zlib {
                        4
                    } else {
                        8
                    };
                    while self.tail_len < need {
                        let byte = self.next_byte(source)?;
                        self.tail[self.tail_len] = byte;
                        self.tail_len += 1;
                    }
                    if self.scheme == Compression::Zlib {
                        let expected = u32::from_be_bytes([
                            self.tail[0],
                            self.tail[1],
                            self.tail[2],
                            self.tail[3],
                        ]);
                        let computed = (self.adler.1 % 65_521) << 16 | (self.adler.0 % 65_521);
                        if expected != computed {
                            return Err(CompressionError::Malformed {
                                scheme: self.scheme,
                                detail: format!(
                                    "the adler32 trailer says {expected:#010x} but the data \
                                     hashes to {computed:#010x}"
                                ),
                            });
                        }
                    } else {
                        let expected_crc = u32::from_le_bytes([
                            self.tail[0],
                            self.tail[1],
                            self.tail[2],
                            self.tail[3],
                        ]);
                        let computed_crc = !self.crc;
                        if expected_crc != computed_crc {
                            return Err(CompressionError::Malformed {
                                scheme: self.scheme,
                                detail: format!(
                                    "the crc32 trailer says {expected_crc:#010x} but the data \
                                     hashes to {computed_crc:#010x}"
                                ),
                            });
                        }
                        let expected_len = u32::from_le_bytes([
                            self.tail[4],
                            self.tail[5],
                            self.tail[6],
                            self.tail[7],
                        ]);
                        if expected_len != self.total_out as u32 {
                            return Err(CompressionError::Malformed {
                                scheme: self.scheme,
                                detail: format!(
                                    "the gzip trailer counts {} bytes but {} were produced",
                                    expected_len, self.total_out
                                ),
                            });
                        }
                    }
                    // Fall through to the `Done` stage rather than ending
                    // here: it owns the last look for bytes staged past the
                    // stream, and this arm cannot see them.
                    self.stage = Stage::Done;
                    continue;
                }
            }
        }
    }
}

/// Pull a block from `source`, retrying the one error that is not really an
/// error.
fn fill(buf: &mut [u8], source: &mut dyn Read) -> std::io::Result<usize> {
    loop {
        match source.read(buf) {
            Ok(n) => return Ok(n),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
}

/// An inflate failure reported as plain I/O damage by a layer underneath.
fn malformed(scheme: Compression, error: std::io::Error) -> CompressionError {
    CompressionError::Malformed {
        scheme,
        detail: error.to_string(),
    }
}

/// Wrap one of this crate's errors for transport through [`Read`].
fn io_error(error: &CompressionError) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.clone())
}

/// Recover a wrapped crate error, or fall back to treating the `io::Error` as
/// damage the inflate itself reported.
fn error_from_io(scheme: Compression, error: std::io::Error) -> CompressionError {
    match error.into_inner() {
        Some(inner) => inner
            .downcast::<CompressionError>()
            .map(|boxed| *boxed)
            .unwrap_or_else(|detail| CompressionError::Malformed {
                scheme,
                detail: detail.to_string(),
            }),
        None => malformed(scheme, std::io::Error::other("the stream failed")),
    }
}

/// The CRC-32 of ISO 3309 as ISO/IEC and RFC 1952 use it: reflected, polynomial
/// 0xEDB88320, pre- and post-conditioned with all ones. Table-driven, with the
/// table built on first use — fifteen lines instead of a dependency, and the
/// checksum runs per produced byte exactly once either way.
fn crc_update(crc: u32, data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut table = [0u32; 256];
        for (index, slot) in table.iter_mut().enumerate() {
            let mut c = index as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xedb8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *slot = c;
        }
        table
    });
    let mut crc = crc;
    for &byte in data {
        crc = table[((crc ^ u32::from(byte)) & 0xff) as usize] ^ (crc >> 8);
    }
    crc
}

/// Adler-32 over produced bytes, carried as `(s1, s2)` between calls.
///
/// Batched at 5552 bytes, the largest run whose sums cannot overflow a `u32`
/// before reduction — the same bound RFC 1950's reference implementation uses.
fn adler_update((s1, s2): (u32, u32), data: &[u8]) -> (u32, u32) {
    const MOD: u32 = 65_521;
    let mut s1 = s1;
    let mut s2 = s2;
    for chunk in data.chunks(5552) {
        for &byte in chunk {
            s1 += u32::from(byte);
            s2 += s1;
        }
        s1 %= MOD;
        s2 %= MOD;
    }
    (s1 % MOD, s2 % MOD)
}
