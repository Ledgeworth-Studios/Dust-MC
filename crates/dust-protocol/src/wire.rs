//! The wire primitives a packet body is built from.
//!
//! # The seam, and how it closed
//!
//! This crate was written before `dust-net` existed, against the day it would:
//! a packet body cannot be read without a VarInt, because almost every length
//! prefix in the protocol is one, and the transport layer that owns sockets,
//! framing, compression and encryption was not there yet. So the contract
//! lived here as [`WireRead`] and [`WireWrite`], the field codecs in
//! [`crate::types`] were written against the *traits*, and [`Reader`] and
//! [`Writer`] were a small stopgap implementation.
//!
//! The merge has happened. There is **one** VarInt in this workspace, in
//! [`crate::varint`] — `dust-net`'s decoder, adopted as-is because it was the
//! stricter of the two and strictness here is a security property rather than
//! pedantry. It lives below both crates because nothing may depend on
//! `dust-net`; `dust-net` re-exports it, so its callers see no change. The
//! variable-length methods below delegate to it, which is what makes the rule
//! project-wide rather than per-crate: there is no second decoder left to
//! disagree with. The one historical disagreement is settled for good — the
//! stopgap reader discarded junk high bits the way vanilla does, and now the
//! canonical-encoding rule wins everywhere. See `crate::conformance` for the
//! vectors both sides are checked against, including the byte strings this
//! crate once accepted and now refuses.
//!
//! What did not change: [`read_var_int`](WireRead::read_var_int) and its three
//! companions are still **required** methods with no default body. A default
//! would let any future implementor forget to route through
//! [`crate::varint`] and quietly fork the format again. The plain big-endian
//! reads *are* defaulted, because nobody disagrees about how a `u16` is laid
//! out.
//!
//! # What the traits do not cover
//!
//! Framing. A [`WireRead`] is handed a packet body that already has its length
//! prefix stripped and its compression and encryption undone. It has no idea
//! what a packet boundary is, and `remaining` is the length of one body. That
//! is deliberate: the boundary is exactly where `dust-net`'s job stops and
//! this crate's begins.

use std::fmt;

/// Why a packet body could not be read.
///
/// Every one of these is a named error rather than a panic, and that is not
/// stylistic. A decoder's input is whatever an unauthenticated socket sent, so
/// every branch here is attacker-reachable and a panic is a remote crash. The
/// same reasoning forbids a silent default: a field that quietly becomes zero
/// when the bytes are nonsense hands the rest of the server a lie.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// The body ended in the middle of a field.
    UnexpectedEnd { wanted: usize, remaining: usize },
    /// A VarInt or VarLong ran past the width it can represent.
    VarIntTooLong { bits: u32 },
    /// A VarInt or VarLong was cut off after `available` byte(s).
    ///
    /// Distinguished from [`DecodeError::UnexpectedEnd`] because a truncated
    /// variable-length integer is worth naming as such in a packet capture:
    /// the length prefixes around it make the two failures read differently
    /// even when both are "the body ended early".
    TruncatedVarInt { available: usize },
    /// A variable-length integer was written in more bytes than it needs.
    ///
    /// Rejected rather than tolerated so that the map from byte strings to
    /// values stays a bijection — see [`crate::varint`] for the full
    /// argument, which is about frame identity rather than arithmetic.
    Overlong { used: usize, canonical: usize },
    /// The final byte of a maximum-length variable-length integer set bits
    /// beyond the width of the type.
    ///
    /// Vanilla shifts them off and accepts; refusing keeps two different byte
    /// strings from meaning one number.
    FinalByteOverflow { final_byte: u8, allowed: u8 },
    /// A length prefix was negative, which no length is.
    NegativeLength { field: &'static str, value: i32 },
    /// A string's bytes were not UTF-8.
    NotUtf8,
    /// A string was longer than the field allows.
    ///
    /// `limit` and `actual` are both in **UTF-16 code units**, which is the
    /// unit Minecraft counts a string's length in. See
    /// [`crate::types::read_string`].
    StringTooLong { limit: usize, actual: usize },
    /// A VarInt-tagged enum carried a discriminant outside its known range.
    UnknownVariant { name: &'static str, value: i32 },
    /// A namespaced id was not one.
    BadIdentifier { value: String },
    /// A structured value carried a key this crate does not model.
    ///
    /// The key is owned because it came off the wire. Refusing rather than
    /// skipping is the point: a skipped key renders a different message than
    /// was sent, and nothing downstream would ever know.
    UnknownField {
        container: &'static str,
        key: String,
    },
    /// A packet id that this state and direction has no packet for.
    UnknownPacket {
        state: &'static str,
        direction: &'static str,
        protocol_id: u32,
    },
    /// The packet decoded, and there were bytes left over.
    ///
    /// Always an error, never a shrug: trailing bytes mean the layout this
    /// crate believes is not the layout that was sent, and the next packet on
    /// a shared buffer would start in the wrong place.
    TrailingBytes { left: usize },
    /// An NBT value could not be walked. See [`crate::nbt`].
    Nbt { why: &'static str },
    /// A field this crate does not implement. See the field's own docs for why.
    Unsupported {
        field: &'static str,
        why: &'static str,
    },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEnd { wanted, remaining } => write!(
                f,
                "the body ended early: {wanted} more byte(s) were needed and {remaining} were left"
            ),
            Self::VarIntTooLong { bits } => {
                write!(f, "a variable-length integer ran past {bits} bits")
            }
            Self::TruncatedVarInt { available } => write!(
                f,
                "a variable-length integer was cut off after {available} byte(s)"
            ),
            Self::Overlong { used, canonical } => write!(
                f,
                "a variable-length integer was written in {used} bytes where {canonical} encode \
                 the same value; Dust requires the canonical encoding"
            ),
            Self::FinalByteOverflow {
                final_byte,
                allowed,
            } => write!(
                f,
                "a variable-length integer ended with byte {final_byte:#04x}, which sets bits \
                 beyond the type's width; the last byte may be at most {allowed:#04x}"
            ),
            Self::NegativeLength { field, value } => {
                write!(f, "{field} has length {value}, and no length is negative")
            }
            Self::NotUtf8 => write!(f, "a string's bytes are not UTF-8"),
            Self::StringTooLong { limit, actual } => write!(
                f,
                "a string is {actual} UTF-16 code units and the field allows {limit}"
            ),
            Self::UnknownVariant { name, value } => {
                write!(f, "{value} is not a {name}")
            }
            Self::BadIdentifier { value } => write!(f, "`{value}` is not a namespaced id"),
            Self::UnknownField { container, key } => {
                write!(f, "{container} carries the key `{key}`, which is not modelled")
            }
            Self::UnknownPacket {
                state,
                direction,
                protocol_id,
            } => write!(f, "{state}/{direction} has no packet with id {protocol_id}"),
            Self::TrailingBytes { left } => write!(
                f,
                "the packet decoded with {left} byte(s) left over, so its layout is not what was sent"
            ),
            Self::Nbt { why } => write!(f, "an NBT value could not be read: {why}"),
            Self::Unsupported { field, why } => write!(f, "{field} is not implemented: {why}"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Why a packet body could not be written.
///
/// Much shorter than [`DecodeError`], and for a reason worth stating: encoding
/// takes values this server built, so almost nothing can be wrong with them.
/// The one thing that can is a string longer than its field allows, which is
/// reachable from a player-supplied name or message and so is checked rather
/// than trusted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeError {
    /// Limit and actual are in UTF-16 code units, as Minecraft counts them.
    StringTooLong { limit: usize, actual: usize },
    /// A collection longer than a VarInt count can hold.
    TooManyElements { count: usize },
    /// A packet this crate can build and the target version has no id for.
    ///
    /// Reachable because a definition claims a set of versions and a caller
    /// may hold one outside it — sending a 1.21 packet to a 1.20 client is a
    /// refusal here rather than a frame nobody can read.
    NotInVersion {
        name: &'static str,
        version: &'static str,
    },
    /// A field this crate does not implement. See the field's own docs.
    Unsupported {
        field: &'static str,
        why: &'static str,
    },
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StringTooLong { limit, actual } => write!(
                f,
                "a string is {actual} UTF-16 code units and the field allows {limit}"
            ),
            Self::TooManyElements { count } => {
                write!(f, "{count} elements is more than a length prefix can carry")
            }
            Self::NotInVersion { name, version } => {
                write!(f, "{version} has no packet {name}")
            }
            Self::Unsupported { field, why } => write!(f, "{field} is not implemented: {why}"),
        }
    }
}

impl std::error::Error for EncodeError {}

/// Reading the primitives a packet body is built from.
///
/// See the module docs for why the variable-length methods have no default
/// body and the fixed-width ones do.
pub trait WireRead {
    /// How many bytes of this packet body are left.
    fn remaining(&self) -> usize;

    /// Take `len` bytes without copying them.
    fn read_slice(&mut self, len: usize) -> Result<&[u8], DecodeError>;

    /// Every byte left, **without consuming any of them**.
    ///
    /// Part of the contract because exactly one field type needs it: an NBT
    /// value carries no length, so the only way to know where it ends is to
    /// walk its structure first and consume afterwards. See [`crate::nbt`].
    ///
    /// This is also the one thing here that constrains what `dust-net`'s
    /// reader may be. A body has to be contiguous in memory by the time it
    /// reaches this crate — which it is, since framing, decompression and
    /// decryption all complete before a body exists — but a reader that
    /// streamed from a socket could not implement this, and that is a fact
    /// about the seam worth stating rather than discovering.
    fn peek(&self) -> &[u8];

    /// A VarInt: up to five bytes, seven bits each, little-endian groups,
    /// carrying an `i32` as two's complement.
    ///
    /// Delegates to [`crate::varint`], whose canonical-encoding rule is the
    /// project-wide one: overlong encodings and final bytes wider than the
    /// type are refused, not silently shifted off.
    fn read_var_int(&mut self) -> Result<i32, DecodeError>;

    /// A VarLong: the same encoding, up to ten bytes, carrying an `i64`.
    fn read_var_long(&mut self) -> Result<i64, DecodeError>;

    fn read_u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.read_slice(1)?[0])
    }

    fn read_i8(&mut self) -> Result<i8, DecodeError> {
        Ok(self.read_u8()? as i8)
    }

    fn read_bool(&mut self) -> Result<bool, DecodeError> {
        // Vanilla writes 0 or 1 and reads anything non-zero as true. Matching
        // that rather than rejecting 2 is deliberate: a stricter reader than
        // the reference implementation disconnects clients the real server
        // accepts, and this crate is not the place to be inventing policy.
        Ok(self.read_u8()? != 0)
    }

    fn read_u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_be_bytes(self.read_array()?))
    }

    fn read_i16(&mut self) -> Result<i16, DecodeError> {
        Ok(i16::from_be_bytes(self.read_array()?))
    }

    fn read_i32(&mut self) -> Result<i32, DecodeError> {
        Ok(i32::from_be_bytes(self.read_array()?))
    }

    fn read_i64(&mut self) -> Result<i64, DecodeError> {
        Ok(i64::from_be_bytes(self.read_array()?))
    }

    fn read_f32(&mut self) -> Result<f32, DecodeError> {
        Ok(f32::from_be_bytes(self.read_array()?))
    }

    fn read_f64(&mut self) -> Result<f64, DecodeError> {
        Ok(f64::from_be_bytes(self.read_array()?))
    }

    fn read_array<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.read_slice(N)?);
        Ok(out)
    }

    /// Take `len` bytes and keep them.
    fn read_vec(&mut self, len: usize) -> Result<Vec<u8>, DecodeError> {
        Ok(self.read_slice(len)?.to_vec())
    }
}

/// Writing the primitives a packet body is built from.
pub trait WireWrite {
    fn write_slice(&mut self, bytes: &[u8]);
    fn write_var_int(&mut self, value: i32);
    fn write_var_long(&mut self, value: i64);

    fn write_u8(&mut self, value: u8) {
        self.write_slice(&[value]);
    }

    fn write_i8(&mut self, value: i8) {
        self.write_u8(value as u8);
    }

    fn write_bool(&mut self, value: bool) {
        self.write_u8(u8::from(value));
    }

    fn write_u16(&mut self, value: u16) {
        self.write_slice(&value.to_be_bytes());
    }

    fn write_i16(&mut self, value: i16) {
        self.write_slice(&value.to_be_bytes());
    }

    fn write_i32(&mut self, value: i32) {
        self.write_slice(&value.to_be_bytes());
    }

    fn write_i64(&mut self, value: i64) {
        self.write_slice(&value.to_be_bytes());
    }

    fn write_f32(&mut self, value: f32) {
        self.write_slice(&value.to_be_bytes());
    }

    fn write_f64(&mut self, value: f64) {
        self.write_slice(&value.to_be_bytes());
    }
}

/// The seam's reader: a cursor over one packet body.
///
/// What began as a stopgap is now a thin one: the variable-length reads
/// delegate to [`crate::varint`], the workspace's single implementation, and
/// this type contributes only cursor mechanics — bounds checks and position.
/// Nothing here should grow a buffer, a socket or a frame — the moment it
/// wants one, the seam has been crossed in the wrong direction.
#[derive(Debug)]
pub struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    /// How many bytes have been consumed. Useful to a caller measuring how far
    /// a decode got before it failed.
    pub fn position(&self) -> usize {
        self.position
    }

    /// Everything not yet read, without consuming it.
    pub fn rest(&self) -> &'a [u8] {
        &self.bytes[self.position..]
    }
}

impl WireRead for Reader<'_> {
    fn remaining(&self) -> usize {
        self.bytes.len() - self.position
    }

    fn peek(&self) -> &[u8] {
        &self.bytes[self.position..]
    }

    fn read_slice(&mut self, len: usize) -> Result<&[u8], DecodeError> {
        let remaining = self.remaining();
        if len > remaining {
            return Err(DecodeError::UnexpectedEnd {
                wanted: len,
                remaining,
            });
        }
        let start = self.position;
        self.position += len;
        Ok(&self.bytes[start..self.position])
    }

    fn read_var_int(&mut self) -> Result<i32, DecodeError> {
        // The value came out of this crate's own VarInt reader, which produced
        // it from an `i32`, so the narrowing cast is exact.
        let (value, used) = read_var(self.bytes, self.position, 32)?;
        self.position += used;
        Ok(value as i32)
    }

    fn read_var_long(&mut self) -> Result<i64, DecodeError> {
        let (value, used) = read_var(self.bytes, self.position, 64)?;
        self.position += used;
        Ok(value)
    }
}

/// Read one variable-length integer out of `bytes` at `position`, through the
/// one implementation in [`crate::varint`].
///
/// `bits` is 32 or 64 and selects VarInt from VarLong. The error mapping is
/// total: every way [`crate::varint::VarIntError`] can refuse an input has a
/// named home in [`DecodeError`], so nothing reaches a caller as a stringly
/// loss.
pub(crate) fn read_var(
    bytes: &[u8],
    position: usize,
    bits: u32,
) -> Result<(i64, usize), DecodeError> {
    let rest = &bytes[position..];
    let outcome = if bits == 32 {
        crate::varint::read_var_int(rest).map(|(v, n)| (i64::from(v), n))
    } else {
        crate::varint::read_var_long(rest)
    };
    outcome.map_err(|error| match error {
        crate::varint::VarIntError::TooLong { .. } => DecodeError::VarIntTooLong { bits },
        crate::varint::VarIntError::Overlong {
            used,
            canonical,
            kind: _,
        } => DecodeError::Overlong { used, canonical },
        crate::varint::VarIntError::Overflow {
            final_byte,
            allowed,
            kind: _,
        } => DecodeError::FinalByteOverflow {
            final_byte,
            allowed,
        },
        crate::varint::VarIntError::Incomplete { available, .. } => {
            DecodeError::TruncatedVarInt { available }
        }
    })
}

/// The seam's writer: a growable body.
#[derive(Debug, Default, Clone)]
pub struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

impl WireWrite for Writer {
    fn write_slice(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    fn write_var_int(&mut self, value: i32) {
        crate::varint::write_var_int(value, &mut self.bytes);
    }

    fn write_var_long(&mut self, value: i64) {
        crate::varint::write_var_long(value, &mut self.bytes);
    }
}
