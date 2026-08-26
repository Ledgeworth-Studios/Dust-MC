//! The server-list icon: a 64x64 PNG, read from disk and carried as the data
//! URI a status document puts it in.
//!
//! # Why this validates rather than forwards
//!
//! The client's behaviour on a favicon it cannot use is to show nothing —
//! no error, no log line, no difference from a server that set none. An
//! operator who points `favicon` at the wrong file therefore gets exactly the
//! outcome of not setting it, and no way to tell the two apart. So the checks
//! happen here, at boot, where a wrong file is a refusal that names the
//! problem. This is the same rule the ore resolver follows for unknown ore
//! names: a setting that silently does nothing is the worst outcome available.
//!
//! The dimensions matter as much as the format. Vanilla's client scales
//! nothing; a 128x128 PNG is a valid PNG that renders wrong.

use std::fmt;
use std::path::{Path, PathBuf};

/// PNG's fixed eight-byte signature.
///
/// The first byte is deliberately non-ASCII and the pair after the tag is a
/// CRLF/LF trap, so this catches a JPEG renamed to `.png` and also a PNG that
/// went through a text-mode transfer.
const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// What the protocol requires, and the only size a vanilla client renders.
pub const REQUIRED_SIDE: u32 = 64;

/// An upper bound on the file, so a boot cannot be made to read a large file
/// by pointing a setting at one.
///
/// A 64x64 PNG is a few kilobytes; a megabyte is four hundred times generous
/// and still small enough that refusing above it can never surprise anybody
/// with a real icon.
pub const MAX_BYTES: u64 = 1024 * 1024;

/// A validated icon, ready to be pasted into a status document.
#[derive(Clone, PartialEq, Eq)]
pub struct Favicon {
    data_uri: String,
}

impl fmt::Debug for Favicon {
    /// The base64 payload is several kilobytes of noise that would swamp every
    /// structure it appears inside, so the debug form is its length.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Favicon")
            .field("data_uri_len", &self.data_uri.len())
            .finish()
    }
}

impl Favicon {
    /// The `data:image/png;base64,...` string the status document carries.
    pub fn data_uri(&self) -> &str {
        &self.data_uri
    }

    /// Read and validate an icon from disk.
    pub fn load(path: &Path) -> Result<Self, FaviconError> {
        let meta = std::fs::metadata(path).map_err(|source| FaviconError::Unreadable {
            path: path.to_path_buf(),
            source,
        })?;
        if meta.len() > MAX_BYTES {
            return Err(FaviconError::TooLarge {
                path: path.to_path_buf(),
                bytes: meta.len(),
            });
        }
        let bytes = std::fs::read(path).map_err(|source| FaviconError::Unreadable {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_png(path, &bytes)
    }

    /// Validate bytes already in hand. Separated from [`Favicon::load`] so the
    /// tests can hand it malformed pictures without writing files.
    pub fn from_png(path: &Path, bytes: &[u8]) -> Result<Self, FaviconError> {
        let (width, height) = png_dimensions(bytes).ok_or_else(|| FaviconError::NotAPng {
            path: path.to_path_buf(),
        })?;
        if width != REQUIRED_SIDE || height != REQUIRED_SIDE {
            return Err(FaviconError::WrongSize {
                path: path.to_path_buf(),
                width,
                height,
            });
        }
        let mut data_uri = String::from("data:image/png;base64,");
        base64_into(bytes, &mut data_uri);
        Ok(Self { data_uri })
    }
}

/// Read a PNG's width and height, or `None` if these bytes are not a PNG whose
/// header says so.
///
/// The layout is fixed by the specification and needs no decoder: the
/// eight-byte signature, then a chunk length, then the four-byte type `IHDR`,
/// then width and height as big-endian `u32`s. Anything that disagrees at any
/// of those points is refused rather than guessed at — a full PNG decoder
/// would be a parser of operator-supplied bytes for no gain, since nothing
/// here needs a single pixel.
fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    // Signature (8) + length (4) + type (4) + width (4) + height (4).
    if bytes.len() < 24 || bytes[..8] != PNG_MAGIC || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    // A zero dimension is malformed per the specification, and would otherwise
    // reach the size check and be reported as the wrong size rather than as
    // not a picture.
    if width == 0 || height == 0 {
        return None;
    }
    Some((width, height))
}

/// Standard base64, appended to `out`.
///
/// Hand-written rather than taken from a crate, for the reason the network
/// crate gives about its own dependencies: this is a sixty-four entry table
/// and a three-bytes-to-four-characters loop, used once per boot on a file the
/// operator chose, and taking a dependency to get it widens the licence audit
/// and the supply chain for something with no room to be subtly wrong. It is
/// pinned against the RFC 4648 test vectors below, which is the check that
/// makes writing it defensible.
fn base64_into(bytes: &[u8], out: &mut String) {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    out.reserve(bytes.len().div_ceil(3) * 4);
    let mut chunks = bytes.chunks_exact(3);
    for chunk in &mut chunks {
        let n = u32::from(chunk[0]) << 16 | u32::from(chunk[1]) << 8 | u32::from(chunk[2]);
        for shift in [18, 12, 6, 0] {
            out.push(ALPHABET[(n >> shift & 0x3f) as usize] as char);
        }
    }
    // The tail is one or two bytes, padded to a four-character group. The bits
    // that were never supplied are written as zero and marked absent with '=',
    // which is what makes the encoding reversible.
    match chunks.remainder() {
        [a] => {
            let n = u32::from(*a) << 16;
            out.push(ALPHABET[(n >> 18 & 0x3f) as usize] as char);
            out.push(ALPHABET[(n >> 12 & 0x3f) as usize] as char);
            out.push('=');
            out.push('=');
        }
        [a, b] => {
            let n = u32::from(*a) << 16 | u32::from(*b) << 8;
            out.push(ALPHABET[(n >> 18 & 0x3f) as usize] as char);
            out.push(ALPHABET[(n >> 12 & 0x3f) as usize] as char);
            out.push(ALPHABET[(n >> 6 & 0x3f) as usize] as char);
            out.push('=');
        }
        _ => {}
    }
}

/// Why an icon was refused. Every variant names the path, because the setting
/// that produced it is the thing the operator has to change.
#[derive(Debug)]
pub enum FaviconError {
    Unreadable {
        path: PathBuf,
        source: std::io::Error,
    },
    TooLarge {
        path: PathBuf,
        bytes: u64,
    },
    NotAPng {
        path: PathBuf,
    },
    WrongSize {
        path: PathBuf,
        width: u32,
        height: u32,
    },
}

impl fmt::Display for FaviconError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable { path, source } => {
                write!(f, "favicon {} could not be read: {source}", path.display())
            }
            Self::TooLarge { path, bytes } => write!(
                f,
                "favicon {} is {bytes} bytes, over the {MAX_BYTES} byte limit",
                path.display()
            ),
            Self::NotAPng { path } => write!(
                f,
                "favicon {} is not a PNG: no PNG signature and IHDR header",
                path.display()
            ),
            Self::WrongSize {
                path,
                width,
                height,
            } => write!(
                f,
                "favicon {} is {width}x{height}; the client renders only \
                 {REQUIRED_SIDE}x{REQUIRED_SIDE} and shows nothing at all for \
                 any other size",
                path.display()
            ),
        }
    }
}

impl std::error::Error for FaviconError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PNG header claiming `width` by `height`, with no image data. Nothing
    /// here decodes pixels, so nothing here needs any.
    fn png_header(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = PNG_MAGIC.to_vec();
        bytes.extend_from_slice(&13u32.to_be_bytes());
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes
    }

    #[test]
    fn base64_matches_the_rfc_4648_vectors() {
        // All three tail lengths, which is where a hand-written encoder goes
        // wrong. Taken from RFC 4648 section 10 rather than from this
        // implementation's own output, which would prove only self-agreement.
        for (input, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            let mut out = String::new();
            base64_into(input.as_bytes(), &mut out);
            assert_eq!(out, expected, "encoding {input:?}");
        }
    }

    #[test]
    fn base64_covers_every_alphabet_entry_including_the_last_two() {
        // Indices 62 and 63 are '+' and '/', the two characters a
        // URL-safe alphabet replaces. Encoding all 256 byte values walks the
        // whole table, so an entry mistyped anywhere in it shows up here.
        let all: Vec<u8> = (0..=255u8).collect();
        let mut out = String::new();
        base64_into(&all, &mut out);
        assert!(out.contains('+'), "index 62 must be '+'");
        assert!(out.contains('/'), "index 63 must be '/'");
        assert!(
            out.chars()
                .all(|c| c.is_ascii_alphanumeric() || "+/=".contains(c)),
            "no character outside the alphabet"
        );
    }

    #[test]
    fn a_sixty_four_square_png_becomes_a_data_uri() {
        let icon = Favicon::from_png(Path::new("icon.png"), &png_header(64, 64)).expect("valid");
        assert!(icon.data_uri().starts_with("data:image/png;base64,"));
        assert!(icon.data_uri().len() > "data:image/png;base64,".len());
    }

    #[test]
    fn a_png_of_the_wrong_size_is_refused_by_its_size() {
        let err = Favicon::from_png(Path::new("big.png"), &png_header(128, 128))
            .expect_err("128x128 is not renderable");
        let message = err.to_string();
        assert!(message.contains("128x128"), "{message}");
        assert!(message.contains("64x64"), "{message}");
    }

    #[test]
    fn a_file_that_is_not_a_png_is_refused_as_that_rather_than_as_a_size() {
        // A JPEG's first bytes. The failure must name the format, because
        // "wrong size" would send an operator to resize a file that was never
        // going to work.
        let err = Favicon::from_png(Path::new("photo.png"), &[0xff, 0xd8, 0xff, 0xe0])
            .expect_err("a JPEG is not a PNG");
        assert!(err.to_string().contains("not a PNG"), "{err}");
    }

    #[test]
    fn a_truncated_png_header_is_refused_rather_than_read_past() {
        let full = png_header(64, 64);
        for cut in 0..full.len() {
            assert!(
                Favicon::from_png(Path::new("cut.png"), &full[..cut]).is_err(),
                "{cut} bytes of a header is not a picture"
            );
        }
    }

    #[test]
    fn a_zero_dimension_is_not_a_picture_rather_than_the_wrong_size() {
        let err =
            Favicon::from_png(Path::new("zero.png"), &png_header(0, 64)).expect_err("malformed");
        assert!(err.to_string().contains("not a PNG"), "{err}");
    }
}
