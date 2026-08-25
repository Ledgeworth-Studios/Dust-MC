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

use std::fmt;
use std::io::Read;

use flate2::read::{GzDecoder, ZlibDecoder};
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
        Compression::Gzip => inflate(GzDecoder::new(bytes), scheme, limit).map(Into::into),
        Compression::Zlib => inflate(ZlibDecoder::new(bytes), scheme, limit).map(Into::into),
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
