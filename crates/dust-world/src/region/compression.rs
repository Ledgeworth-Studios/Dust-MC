//! The compression byte that precedes every chunk payload in a region file.
//!
//! One byte, and it carries two things: a scheme in the low bits and a flag in
//! the high bit saying the payload is not here at all. The flag is the part
//! that matters. A reader that masks it off and decompresses what follows finds
//! four bytes of nothing where a chunk should be; a reader that does not know
//! about it at all reads `0x82` as an unknown scheme and refuses a chunk that
//! is perfectly intact in the file next door.

use std::io::{Read as _, Write as _};

/// Set in the compression byte when the payload lives in a `.mcc` file.
pub const EXTERNAL_FLAG: u8 = 0x80;

/// How a chunk payload is compressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Compression {
    /// Scheme 1. Written by very old worlds; still read.
    Gzip,
    /// Scheme 2. What a modern vanilla server writes.
    Zlib,
    /// Scheme 3. The payload is stored as-is.
    None,
}

impl Compression {
    /// The byte that names this scheme, without the external flag.
    #[must_use]
    pub const fn to_byte(self) -> u8 {
        match self {
            Self::Gzip => 1,
            Self::Zlib => 2,
            Self::None => 3,
        }
    }

    /// The scheme's usual name, for messages.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Gzip => "gzip",
            Self::Zlib => "zlib",
            Self::None => "uncompressed",
        }
    }

    /// Read a compression byte.
    ///
    /// Returns the scheme and whether the external flag was set. `Err` carries
    /// the byte back so the caller can name it in an error; see
    /// [`UnsupportedScheme`] for the two values that are real and unimplemented
    /// rather than merely unrecognised.
    pub fn from_byte(byte: u8) -> Result<(Self, bool), UnsupportedScheme> {
        let external = byte & EXTERNAL_FLAG != 0;
        let scheme = match byte & !EXTERNAL_FLAG {
            1 => Self::Gzip,
            2 => Self::Zlib,
            3 => Self::None,
            other => {
                return Err(UnsupportedScheme {
                    byte,
                    scheme: other,
                })
            }
        };
        Ok((scheme, external))
    }

    /// Decompress a payload.
    pub fn decompress(self, data: &[u8]) -> std::io::Result<Vec<u8>> {
        let mut out = Vec::with_capacity(data.len() * 4);
        match self {
            Self::Gzip => {
                flate2::read::GzDecoder::new(data).read_to_end(&mut out)?;
            }
            Self::Zlib => {
                flate2::read::ZlibDecoder::new(data).read_to_end(&mut out)?;
            }
            Self::None => out.extend_from_slice(data),
        }
        Ok(out)
    }

    /// Compress a payload.
    ///
    /// The level is flate2's default, which is zlib's level 6 — the same level
    /// vanilla's `Deflater` uses when it is constructed with no argument. This
    /// is not a correctness requirement, since nothing reads a region file
    /// expecting particular compressed bytes, but it keeps a rewritten world
    /// roughly the size the original was.
    pub fn compress(self, data: &[u8]) -> std::io::Result<Vec<u8>> {
        match self {
            Self::Gzip => {
                let mut encoder =
                    flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
                encoder.write_all(data)?;
                encoder.finish()
            }
            Self::Zlib => {
                let mut encoder =
                    flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
                encoder.write_all(data)?;
                encoder.finish()
            }
            Self::None => Ok(data.to_vec()),
        }
    }
}

/// A compression byte this crate will not decode.
///
/// Two of these are real schemes rather than damage, and saying so is the whole
/// reason this is a struct rather than a bare byte: 1.20.5 added an LZ4 scheme
/// and a "custom" escape, and a server started with
/// `-Dminecraft.regionFileCompressionType=lz4` writes worlds Dust cannot read.
/// Dust refuses them by name instead of calling them corrupt, because an
/// operator who is told "chunk (12, -5) is corrupt" deletes a world that was
/// fine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnsupportedScheme {
    /// The byte as it appeared, external flag included.
    pub byte: u8,
    /// The byte with the external flag masked off.
    pub scheme: u8,
}

impl UnsupportedScheme {
    /// What the scheme is, when it is a scheme Minecraft defines.
    #[must_use]
    pub const fn known_name(self) -> Option<&'static str> {
        match self.scheme {
            4 => Some("lz4"),
            127 => Some("a custom scheme registered by a mod"),
            _ => None,
        }
    }
}

impl std::fmt::Display for UnsupportedScheme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.known_name() {
            Some(name) => write!(
                f,
                "compression byte {:#04x} is {name}, which Minecraft supports and Dust does not \
                 read yet",
                self.byte
            ),
            None => write!(
                f,
                "compression byte {:#04x} is not a scheme any Minecraft version defines; \
                 1 is gzip, 2 is zlib, 3 is uncompressed",
                self.byte
            ),
        }
    }
}

impl std::error::Error for UnsupportedScheme {}
