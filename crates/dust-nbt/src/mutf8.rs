//! Modified UTF-8: the encoding NBT strings actually use.
//!
//! Every string in a binary NBT document is written by Java's
//! `DataOutputStream.writeUTF` and read by `DataInputStream.readUTF`. That is
//! not UTF-8. It differs in two places, and a reader built on
//! [`str::from_utf8`] is wrong on both:
//!
//! * `U+0000` is written as the two bytes `C0 80`, never as `00`. This exists
//!   so that no character in the payload can be a NUL, which C string handling
//!   in the original JVM depended on. In UTF-8 that two-byte form is an
//!   overlong encoding and is forbidden.
//! * A character above the BMP is written as its **UTF-16 surrogate pair**,
//!   each surrogate encoded as its own three-byte sequence — the CESU-8 form,
//!   six bytes total. UTF-8 would use one four-byte sequence, which
//!   `readUTF` rejects outright.
//!
//! So an emoji in a player's item name is six bytes here and four bytes in
//! UTF-8, and a `str::from_utf8` reader silently accepts the four-byte form
//! nobody sends while rejecting the six-byte form everybody sends.
//!
//! # Where these numbers come from
//!
//! The behaviour below is not taken from a wiki page. It was recorded from the
//! JDK's own `DataOutputStream.writeUTF` and `DataInputStream.readUTF`
//! (OpenJDK 22, `java version "22" 2024-03-19`) by encoding and decoding the
//! cases one at a time and printing the bytes. The programme is small enough to
//! rewrite in a minute and the results are in `tests/mutf8.rs` as hex literals
//! with the input that produced each one, so any of them can be re-derived
//! rather than believed.
//!
//! | character | modified UTF-8 |
//! |---|---|
//! | `U+0000` | `c0 80` |
//! | `U+007F` | `7f` |
//! | `U+0080` | `c2 80` |
//! | `U+07FF` | `df bf` |
//! | `U+0800` | `e0 a0 80` |
//! | `U+FFFF` | `ef bf bf` |
//! | `U+10000` | `ed a0 80 ed b0 80` |
//! | `U+1F600` | `ed a0 bd ed b8 80` |
//! | `U+10FFFF` | `ed af bf ed bf bf` |
//!
//! # Where this is deliberately stricter than Java
//!
//! `readUTF` is permissive in ways that were also recorded rather than
//! assumed. It accepts a raw `00` byte as `U+0000`; it accepts the overlong
//! forms `c1 bf` as `U+007F` and `e0 80 80` as `U+0000`; and it accepts an
//! unpaired surrogate, handing back a `char` that is not a scalar value.
//!
//! [`decode`] rejects all four, for two different reasons.
//!
//! The overlongs and the raw NUL are rejected because accepting them would
//! break byte-identity: we would decode `00` and re-encode it as `c0 80`, so a
//! document read and written back would not be the document that arrived. They
//! are also the classic way to smuggle a byte past something that inspected the
//! encoded form. No writer in the Minecraft ecosystem produces them.
//!
//! The unpaired surrogate is rejected because Rust's `String` cannot hold one.
//! The alternatives are to substitute `U+FFFD`, which changes the bytes on
//! rewrite and quietly corrupts a name, or to carry a second string type
//! through the whole crate for a case that only a hand-written client produces.
//! **What this does not catch** is the reverse: because we reject it here, Dust
//! cannot round-trip a document that a Java tool wrote with an unpaired
//! surrogate in it, and will refuse the whole document rather than the one
//! string. If that ever turns up in the wild it is a decision to revisit, not a
//! bug in this function.
//!
//! # Which error an unpaired surrogate produces
//!
//! Always [`Mutf8Error::UnpairedSurrogate`], at the *high* surrogate's own
//! offset, whether its partner is missing outright, cut off by the end of the
//! payload, or present but not a low surrogate. A string carries its own length
//! prefix, so bytes that stop after a complete high surrogate are not a
//! truncated document — they are a complete document that holds a character
//! Java permits and Rust cannot, and [`Mutf8Error::Truncated`] would send an
//! operator looking for corruption that is not there. `Truncated` stays
//! reserved for a sequence cut short mid-way, where bytes genuinely are
//! missing: `ed a0` or `c2` at the end of a payload.

use std::borrow::Cow;
use std::fmt;

/// What can be wrong with a run of bytes that claims to be modified UTF-8.
///
/// Every variant carries the offset **within the string payload**, not within
/// the document, because that is the unit this module was handed. The reader
/// adds the document offset when it turns one of these into a
/// [`crate::Error`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mutf8Error {
    /// A byte that cannot start a sequence: `0xF0`–`0xFF` (standard UTF-8's
    /// four-byte form, which `readUTF` also rejects) or `0x80`–`0xBF`
    /// (a continuation byte with nothing to continue).
    InvalidStart { offset: usize, byte: u8 },
    /// A multi-byte sequence ran off the end of the payload.
    Truncated {
        offset: usize,
        needed: usize,
        available: usize,
    },
    /// A byte in the middle of a sequence was not `10xxxxxx`.
    InvalidContinuation { offset: usize, byte: u8 },
    /// A character written with more bytes than it needs — `c0 80` for
    /// anything but `U+0000`, or a three-byte form below `U+0800`.
    Overlong { offset: usize, value: u32 },
    /// A surrogate with no partner, which no `String` can hold.
    ///
    /// Also the error for a high surrogate whose partner is cut off by the end
    /// of the payload or is not a low surrogate; see the module note on this
    /// error. The offset is the high surrogate's own.
    UnpairedSurrogate { offset: usize, value: u32 },
}

impl Mutf8Error {
    /// The offset within the payload where the trouble starts.
    pub fn offset(&self) -> usize {
        match self {
            Self::InvalidStart { offset, .. }
            | Self::Truncated { offset, .. }
            | Self::InvalidContinuation { offset, .. }
            | Self::Overlong { offset, .. }
            | Self::UnpairedSurrogate { offset, .. } => *offset,
        }
    }
}

impl fmt::Display for Mutf8Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidStart { offset, byte } => write!(
                f,
                "byte {offset} of the string is {byte:#04x}, which cannot start a \
                 modified-UTF-8 sequence (0xf0-0xff is standard UTF-8's four-byte form, \
                 which this encoding writes as a surrogate pair instead)"
            ),
            Self::Truncated {
                offset,
                needed,
                available,
            } => write!(
                f,
                "the sequence at byte {offset} of the string needs {needed} bytes but \
                 only {available} remain"
            ),
            Self::InvalidContinuation { offset, byte } => write!(
                f,
                "byte {offset} of the string is {byte:#04x}, which is not a continuation \
                 byte (10xxxxxx)"
            ),
            Self::Overlong { offset, value } => write!(
                f,
                "the sequence at byte {offset} of the string encodes U+{value:04X} in more \
                 bytes than it needs; only U+0000 may use the two-byte form"
            ),
            Self::UnpairedSurrogate { offset, value } => write!(
                f,
                "the sequence at byte {offset} of the string is the unpaired surrogate \
                 U+{value:04X}; Java allows one and a Rust string cannot hold one"
            ),
        }
    }
}

impl std::error::Error for Mutf8Error {}

/// Why a string cannot be written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StringTooLong {
    /// The length the string would have had once encoded.
    pub encoded_len: usize,
    /// The first characters of it, so a log line identifies which string.
    pub prefix: String,
}

impl fmt::Display for StringTooLong {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "string is {} bytes once encoded as modified UTF-8 and the length prefix is a \
             u16, so {} is the most that can be written; the string starts {:?}",
            self.encoded_len,
            u16::MAX,
            self.prefix
        )
    }
}

impl std::error::Error for StringTooLong {}

/// The largest string a `u16` length prefix can describe.
pub const MAX_ENCODED_LEN: usize = u16::MAX as usize;

/// How many bytes `text` occupies once encoded.
///
/// Cheap: it walks the string's chars and adds up, without building anything.
/// The writer calls this first so that a string too long to write is refused
/// before any of it has been appended to the output.
pub fn encoded_len(text: &str) -> usize {
    let mut total = 0;
    for c in text.chars() {
        total += match c as u32 {
            0 => 2,
            0x1..=0x7f => 1,
            0x80..=0x7ff => 2,
            0x800..=0xffff => 3,
            // A surrogate pair: two three-byte sequences.
            _ => 6,
        };
    }
    total
}

/// Encode `text`, appending to `out`.
///
/// The caller is expected to have checked the length; this does not, because
/// the length prefix has to be written before the payload and only the caller
/// knows where it went.
pub fn encode_into(text: &str, out: &mut Vec<u8>) {
    out.reserve(text.len());
    for c in text.chars() {
        let value = c as u32;
        match value {
            // U+0000 takes the two-byte form. This is the whole reason the
            // encoding has a name of its own.
            0 => out.extend_from_slice(&[0xc0, 0x80]),
            0x1..=0x7f => out.push(value as u8),
            0x80..=0x7ff => {
                out.extend_from_slice(&[0xc0 | (value >> 6) as u8, 0x80 | (value & 0x3f) as u8])
            }
            0x800..=0xffff => out.extend_from_slice(&[
                0xe0 | (value >> 12) as u8,
                0x80 | ((value >> 6) & 0x3f) as u8,
                0x80 | (value & 0x3f) as u8,
            ]),
            _ => {
                // The UTF-16 decomposition, then each half as three bytes.
                let adjusted = value - 0x1_0000;
                let high = 0xd800 + (adjusted >> 10);
                let low = 0xdc00 + (adjusted & 0x3ff);
                for half in [high, low] {
                    out.extend_from_slice(&[
                        0xe0 | (half >> 12) as u8,
                        0x80 | ((half >> 6) & 0x3f) as u8,
                        0x80 | (half & 0x3f) as u8,
                    ]);
                }
            }
        }
    }
}

/// Encode `text` into a fresh buffer. Convenience for tests and callers that
/// are not writing into a document.
pub fn encode(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    encode_into(text, &mut out);
    out
}

/// Decode a string payload.
///
/// Borrows when it can. A payload that is already valid UTF-8 with no `C0`
/// byte and no `ED` byte in it is, by construction, the same bytes it would
/// decode to — this is the common case for every key and almost every value in
/// a real document, and it costs a scan and no allocation. Anything else is
/// decoded character by character into an owned `String`.
///
/// The returned `Cow` is a borrow of `bytes`, so a caller building an owned
/// [`crate::Tag::String`] still allocates once. What it avoids is allocating
/// *twice* — once for a scratch buffer and once for the string — and it lets a
/// caller that only wants to compare a key against a name do so without
/// allocating at all.
pub fn decode(bytes: &[u8]) -> Result<Cow<'_, str>, Mutf8Error> {
    // `00`, `c0`, and everything from `ed` up are the only bytes whose meaning
    // differs between the two encodings: a raw NUL that this encoding forbids,
    // the lead of the two-byte NUL, the lead of a surrogate half — and the lead
    // of standard UTF-8's four-byte form, which `readUTF` refuses and which
    // must not be waved through by `from_utf8`. A payload with none of them
    // means the same thing in both, so `from_utf8` settles it. The scan is the
    // shape the optimiser vectorises, and it lets the common case — every key
    // and nearly every value in a real document — skip the decoder and the
    // allocation entirely.
    if !bytes.iter().any(|&b| b == 0 || b == 0xc0 || b >= 0xed) {
        if let Ok(text) = std::str::from_utf8(bytes) {
            return Ok(Cow::Borrowed(text));
        }
    }
    decode_slow(bytes).map(Cow::Owned)
}

fn decode_slow(bytes: &[u8]) -> Result<String, Mutf8Error> {
    let mut out = String::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let start = index;
        let lead = bytes[index];
        match lead {
            // A raw NUL is refused: this encoding writes U+0000 as `c0 80`,
            // and accepting `00` would mean a rewrite changed the bytes.
            0 => {
                return Err(Mutf8Error::InvalidStart {
                    offset: start,
                    byte: 0,
                })
            }
            0x01..=0x7f => {
                out.push(lead as char);
                index += 1;
            }
            0xc0..=0xdf => {
                let bytes_needed = 2;
                let second = continuation(bytes, index, 1, bytes_needed)?;
                let value = (u32::from(lead & 0x1f) << 6) | u32::from(second & 0x3f);
                // `c0 80` is U+0000 and is the one overlong this encoding
                // requires; every other two-byte value below 0x80 is one it
                // forbids. Java accepts them; see the module note.
                if value != 0 && value < 0x80 {
                    return Err(Mutf8Error::Overlong {
                        offset: start,
                        value,
                    });
                }
                push_scalar(&mut out, value, start)?;
                index += bytes_needed;
            }
            0xe0..=0xef => {
                let bytes_needed = 3;
                let second = continuation(bytes, index, 1, bytes_needed)?;
                let third = continuation(bytes, index, 2, bytes_needed)?;
                let value = (u32::from(lead & 0x0f) << 12)
                    | (u32::from(second & 0x3f) << 6)
                    | u32::from(third & 0x3f);
                if value < 0x800 {
                    return Err(Mutf8Error::Overlong {
                        offset: start,
                        value,
                    });
                }
                index += bytes_needed;
                if (0xd800..0xdc00).contains(&value) {
                    // A high surrogate. Its partner has to be the next
                    // sequence, or the string is not representable.
                    let low = read_low_surrogate(bytes, index, start)?;
                    index += 3;
                    let scalar = 0x1_0000 + ((value - 0xd800) << 10) + (low - 0xdc00);
                    push_scalar(&mut out, scalar, start)?;
                } else if (0xdc00..0xe000).contains(&value) {
                    return Err(Mutf8Error::UnpairedSurrogate {
                        offset: start,
                        value,
                    });
                } else {
                    push_scalar(&mut out, value, start)?;
                }
            }
            _ => {
                return Err(Mutf8Error::InvalidStart {
                    offset: start,
                    byte: lead,
                })
            }
        }
    }
    Ok(out)
}

fn read_low_surrogate(bytes: &[u8], index: usize, start: usize) -> Result<u32, Mutf8Error> {
    let Some(window) = bytes.get(index..index + 3) else {
        // The payload ended before a partner could appear. A document is not
        // truncated here — the string carries its own length, so these are all
        // the bytes it has — and what it holds is a high surrogate with
        // nothing to complete it, which is the thing to name.
        return Err(Mutf8Error::UnpairedSurrogate {
            offset: start,
            value: high_surrogate_value(bytes, start),
        });
    };
    if window[0] & 0xf0 != 0xe0 || window[1] & 0xc0 != 0x80 || window[2] & 0xc0 != 0x80 {
        // The high surrogate is the thing that is wrong here, not whatever
        // followed it: on its own it is not a character.
        return Err(Mutf8Error::UnpairedSurrogate {
            offset: start,
            value: high_surrogate_value(bytes, start),
        });
    }
    let value = (u32::from(window[0] & 0x0f) << 12)
        | (u32::from(window[1] & 0x3f) << 6)
        | u32::from(window[2] & 0x3f);
    if !(0xdc00..0xe000).contains(&value) {
        return Err(Mutf8Error::UnpairedSurrogate {
            offset: start,
            value: high_surrogate_value(bytes, start),
        });
    }
    Ok(value)
}

fn high_surrogate_value(bytes: &[u8], start: usize) -> u32 {
    (u32::from(bytes[start] & 0x0f) << 12)
        | (u32::from(bytes[start + 1] & 0x3f) << 6)
        | u32::from(bytes[start + 2] & 0x3f)
}

fn continuation(bytes: &[u8], start: usize, which: usize, needed: usize) -> Result<u8, Mutf8Error> {
    let Some(&byte) = bytes.get(start + which) else {
        return Err(Mutf8Error::Truncated {
            offset: start,
            needed,
            available: bytes.len() - start,
        });
    };
    if byte & 0xc0 != 0x80 {
        return Err(Mutf8Error::InvalidContinuation {
            offset: start + which,
            byte,
        });
    }
    Ok(byte)
}

fn push_scalar(out: &mut String, value: u32, offset: usize) -> Result<(), Mutf8Error> {
    match char::from_u32(value) {
        Some(c) => {
            out.push(c);
            Ok(())
        }
        // Only reachable for a surrogate, every other u32 up to 0x10FFFF is a
        // scalar and nothing here can produce a larger one.
        None => Err(Mutf8Error::UnpairedSurrogate { offset, value }),
    }
}
