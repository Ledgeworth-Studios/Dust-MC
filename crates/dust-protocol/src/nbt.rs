//! The NBT seam, and why a text component is not JSON any more.
//!
//! # The change that makes a correct-looking decoder wrong
//!
//! Until 1.20.3 a chat component on the wire was a JSON string: a length
//! prefix and some UTF-8 that a JSON parser handled. Since 1.20.3 it is **NBT**
//! — the same document, in Minecraft's binary object format, inline in the
//! packet. This is not a subtle change and it is not optional; a decoder
//! written against the JSON form does not merely produce a worse result on
//! 1.21.1, it loses the position of every field after the component and
//! therefore the rest of the packet.
//!
//! The trap is that the change is not uniform, which is worse than if it were.
//! `login_disconnect` still carries **JSON**, because it is sent to a client
//! that has not finished login and the vanilla codec for that packet was left
//! alone; `configuration/disconnect` carries **NBT**. Two packets, one word
//! apart in the report, opposite encodings. So this crate has two types —
//! [`TextComponent`] and [`JsonTextComponent`] — rather than one type with a
//! version switch, and each packet field says which it is.
//!
//! # The seam
//!
//! `dust-nbt` is being built and will own NBT. This crate needs one thing from
//! it that cannot wait: **where does an NBT value end?** Without that, a
//! component in the middle of a packet cannot be stepped over, and packets
//! containing one cannot be decoded at all.
//!
//! So [`scan`] walks a network-NBT value's structure and returns its length,
//! interpreting nothing. It is the smallest thing that unblocks this crate, it
//! is marked for deletion, and [`TextComponent`] holds the raw bytes so that
//! the day `dust-nbt` arrives the change is parsing bytes this crate already
//! delimits correctly rather than re-deciding where they stop.
//!
//! The differential test is in [`crate::conformance`]: a vector table of NBT
//! values with their known lengths, and a runner `dust-nbt` calls against its
//! own reader. Two implementations checked against the same table cannot
//! disagree with each other.
//!
//! # What this does not check
//!
//! Everything except structure. [`scan`] does not validate that a string is
//! well-formed modified UTF-8, does not reject a compound with duplicate keys,
//! and has no opinion about whether the document is a valid text component. It
//! answers one question — where does this value end — and a caller that reads
//! more into a successful scan than that is reading in something that is not
//! there.

use crate::types::{Decode, Encode, ProtocolString};
use crate::wire::{DecodeError, EncodeError, WireRead, WireWrite};
use crate::ProtocolVersion;

/// How deep a value may nest before this gives up.
///
/// The same limit vanilla uses. It is not a style choice: `scan` recurses, the
/// input is an unauthenticated socket, and a few kilobytes of nothing but
/// "open a list" is a stack overflow — which in Rust is an abort, not an error
/// anyone can catch. The bound is the whole defence.
pub const MAX_DEPTH: u32 = 512;

const TAG_END: u8 = 0;
const TAG_BYTE: u8 = 1;
const TAG_SHORT: u8 = 2;
const TAG_INT: u8 = 3;
const TAG_LONG: u8 = 4;
const TAG_FLOAT: u8 = 5;
const TAG_DOUBLE: u8 = 6;
const TAG_BYTE_ARRAY: u8 = 7;
const TAG_STRING: u8 = 8;
const TAG_LIST: u8 = 9;
const TAG_COMPOUND: u8 = 10;
const TAG_INT_ARRAY: u8 = 11;
const TAG_LONG_ARRAY: u8 = 12;

/// How long the network-NBT value at the start of `bytes` is.
///
/// Network NBT — the form used inside packets since 1.20.2 — is a bare tag: a
/// type byte and then the payload, with **no root name**. The file format has a
/// name there and a reader that expects one is off by two bytes plus a name on
/// every value it sees.
pub fn scan(bytes: &[u8]) -> Result<usize, DecodeError> {
    let mut cursor = Cursor { bytes, at: 0 };
    let tag = cursor.byte()?;
    if tag == TAG_END {
        // A bare TAG_End is how an absent value is spelled in some fields. It
        // is one byte and it is legal; it is not an error to be reported.
        return Ok(1);
    }
    cursor.payload(tag, 0)?;
    Ok(cursor.at)
}

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Cursor<'_> {
    fn take(&mut self, len: usize) -> Result<&[u8], DecodeError> {
        let end = self.at.checked_add(len).ok_or(DecodeError::Nbt {
            why: "a length overflowed",
        })?;
        if end > self.bytes.len() {
            return Err(DecodeError::UnexpectedEnd {
                wanted: len,
                remaining: self.bytes.len() - self.at,
            });
        }
        let slice = &self.bytes[self.at..end];
        self.at = end;
        Ok(slice)
    }

    fn byte(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<usize, DecodeError> {
        let bytes = self.take(2)?;
        Ok(usize::from(u16::from_be_bytes([bytes[0], bytes[1]])))
    }

    /// An NBT array length is a signed 32-bit int, and a negative one is a
    /// hostile input rather than an empty array.
    fn i32_len(&mut self) -> Result<usize, DecodeError> {
        let bytes = self.take(4)?;
        let value = i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        usize::try_from(value).map_err(|_| DecodeError::Nbt {
            why: "an array length is negative",
        })
    }

    fn string(&mut self) -> Result<(), DecodeError> {
        let len = self.u16()?;
        self.take(len)?;
        Ok(())
    }

    fn payload(&mut self, tag: u8, depth: u32) -> Result<(), DecodeError> {
        if depth > MAX_DEPTH {
            return Err(DecodeError::Nbt {
                why: "the value nests deeper than the limit",
            });
        }
        match tag {
            TAG_BYTE => self.take(1).map(drop),
            TAG_SHORT => self.take(2).map(drop),
            TAG_INT | TAG_FLOAT => self.take(4).map(drop),
            TAG_LONG | TAG_DOUBLE => self.take(8).map(drop),
            TAG_BYTE_ARRAY => {
                let len = self.i32_len()?;
                self.take(len).map(drop)
            }
            TAG_STRING => self.string(),
            TAG_LIST => {
                let element = self.byte()?;
                let len = self.i32_len()?;
                if element == TAG_END {
                    // How an empty list is written. A non-empty list of TAG_End
                    // has no payload to step over and no end, so it is refused
                    // rather than looped on.
                    return if len == 0 {
                        Ok(())
                    } else {
                        Err(DecodeError::Nbt {
                            why: "a list of TAG_End cannot have elements",
                        })
                    };
                }
                for _ in 0..len {
                    self.payload(element, depth + 1)?;
                }
                Ok(())
            }
            TAG_COMPOUND => loop {
                let entry = self.byte()?;
                if entry == TAG_END {
                    return Ok(());
                }
                self.string()?;
                self.payload(entry, depth + 1)?;
            },
            TAG_INT_ARRAY => {
                let len = self.i32_len()?;
                let bytes = len.checked_mul(4).ok_or(DecodeError::Nbt {
                    why: "an array length overflowed",
                })?;
                self.take(bytes).map(drop)
            }
            TAG_LONG_ARRAY => {
                let len = self.i32_len()?;
                let bytes = len.checked_mul(8).ok_or(DecodeError::Nbt {
                    why: "an array length overflowed",
                })?;
                self.take(bytes).map(drop)
            }
            _ => Err(DecodeError::Nbt {
                why: "an unknown tag type",
            }),
        }
    }
}

/// An NBT value, held as the bytes it arrived as.
///
/// Opaque on purpose. This crate delimits NBT and does not interpret it; the
/// interpretation is `dust-nbt`'s, and holding the raw bytes means the handover
/// adds a parse rather than repeating a decision. Round-tripping is exact,
/// because the round trip is a copy.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Nbt(pub Vec<u8>);

impl Nbt {
    /// The bytes, as they were on the wire.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// A bare `TAG_End`: the one-byte spelling of "nothing here".
    pub fn empty() -> Self {
        Self(vec![TAG_END])
    }
}

impl Decode for Nbt {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        _version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        // Measure first, then consume exactly what was measured. This is the
        // one field type that has to look ahead, and the reason `peek` is on
        // the seam's contract at all.
        let len = scan(input.peek())?;
        input.read_vec(len).map(Self)
    }
}

impl Encode for Nbt {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        out.write_slice(&self.0);
        Ok(())
    }
}

/// A text component in the NBT form used since 1.20.3.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TextComponent(pub Nbt);

impl Decode for TextComponent {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        Nbt::decode(input, version).map(Self)
    }
}

impl Encode for TextComponent {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        self.0.encode(out, version)
    }
}

/// A text component in the JSON string form.
///
/// Still used by `login_disconnect` and by the status response on 1.21.1. Not
/// a legacy alias for [`TextComponent`] — a live 1.21.1 server sends this
/// encoding for those two fields, and a decoder that "modernised" them would
/// be wrong today rather than tidy.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct JsonTextComponent(pub ProtocolString);

impl Decode for JsonTextComponent {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        ProtocolString::decode(input, version).map(Self)
    }
}

impl Encode for JsonTextComponent {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        self.0.encode(out, version)
    }
}
