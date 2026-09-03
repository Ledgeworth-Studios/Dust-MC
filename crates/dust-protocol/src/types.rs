//! The field types a packet body is built from.
//!
//! Minecraft's packets are assembled from about forty field types. Most are
//! obvious. The ones that are not are documented at length below, because each
//! of them is the kind of mistake that compiles, round-trips, passes review and
//! then fails on one player.
//!
//! Everything here is written against [`WireRead`] and [`WireWrite`] rather
//! than against a concrete buffer, so that `dust-net` replacing the seam
//! changes nothing in this file. See [`crate::wire`].
//!
//! # The version parameter
//!
//! [`Encode`] and [`Decode`] both take a [`ProtocolVersion`]. On 1.21.1 no
//! field type reads it, and it is there anyway: field *layouts* change between
//! releases at least as often as packet ids do — `Slot` was rebuilt in 1.20.5,
//! and a text component stopped being JSON in 1.20.3 — so a codec that could
//! not see the version would have to be forked wholesale the first time one
//! moved. D3 is explicit that this dimension exists from the first commit.

use std::fmt;

use crate::wire::{DecodeError, EncodeError, WireRead, WireWrite};
use crate::ProtocolVersion;

/// A value that can be written into a packet body.
pub trait Encode {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError>;
}

/// A value that can be read from a packet body.
pub trait Decode: Sized {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError>;
}

// ---------------------------------------------------------------------------
// Fixed-width primitives
// ---------------------------------------------------------------------------

macro_rules! fixed_width {
    ($($ty:ty => $read:ident / $write:ident),* $(,)?) => {$(
        impl Decode for $ty {
            fn decode<R: WireRead + ?Sized>(
                input: &mut R,
                _version: ProtocolVersion,
            ) -> Result<Self, DecodeError> {
                input.$read()
            }
        }
        impl Encode for $ty {
            fn encode<W: WireWrite + ?Sized>(
                &self,
                out: &mut W,
                _version: ProtocolVersion,
            ) -> Result<(), EncodeError> {
                out.$write(*self);
                Ok(())
            }
        }
    )*};
}

fixed_width! {
    bool => read_bool / write_bool,
    u8 => read_u8 / write_u8,
    i8 => read_i8 / write_i8,
    u16 => read_u16 / write_u16,
    i16 => read_i16 / write_i16,
    i32 => read_i32 / write_i32,
    i64 => read_i64 / write_i64,
    f32 => read_f32 / write_f32,
    f64 => read_f64 / write_f64,
}

/// A fixed-arity field whose element count both sides know in advance — sign
/// lines, or any future field with the same shape.
///
/// Distinct from [`Vec`], which carries a VarInt count. A field that is
/// *always* four lines must not grow a prefix: wrapping it in a counted type
/// writes bytes vanilla never sends and desynchronises every field after it.
/// The count lives in the type, where a reader cannot get it wrong.
impl<T: Decode, const N: usize> Decode for [T; N] {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        // Element by element; every element must decode or the array does
        // not exist.
        let mut out = std::array::from_fn(|_| None);
        for slot in &mut out {
            *slot = Some(T::decode(input, version)?);
        }
        Ok(out.map(Option::unwrap))
    }
}

impl<T: Encode, const N: usize> Encode for [T; N] {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        for value in self {
            value.encode(out, version)?;
        }
        Ok(())
    }
}

/// An `i32` in the protocol's variable-length encoding.
///
/// A distinct type from `i32` on purpose. Both appear in packet bodies and
/// they are different on the wire, so a field that says `i32` when it means
/// `VarInt` is a bug the type system should be catching rather than a comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct VarInt(pub i32);

impl Decode for VarInt {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        _version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        input.read_var_int().map(Self)
    }
}

impl Encode for VarInt {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        out.write_var_int(self.0);
        Ok(())
    }
}

/// An `i64` in the protocol's variable-length encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct VarLong(pub i64);

impl Decode for VarLong {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        _version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        input.read_var_long().map(Self)
    }
}

impl Encode for VarLong {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        out.write_var_long(self.0);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Strings
// ---------------------------------------------------------------------------

/// The largest string any vanilla field allows, and the default for a field
/// whose limit nothing narrower is known for.
pub const DEFAULT_STRING_LIMIT: usize = 32767;

/// A string is at most three UTF-8 bytes per UTF-16 code unit.
///
/// A code point in the Basic Multilingual Plane is one UTF-16 unit and up to
/// three UTF-8 bytes; one outside it is two UTF-16 units and four UTF-8 bytes,
/// a ratio of two. So three is the maximum and the bound below is tight.
const MAX_UTF8_BYTES_PER_UTF16_UNIT: usize = 3;

/// The length of `text` in the unit Minecraft measures a string's length in.
///
/// **UTF-16 code units, not bytes and not characters.** This is the single
/// most important line in this file.
///
/// Minecraft is a Java program and a field limit like "a username is at most
/// 16" is `String.length()`, which counts UTF-16 code units. The length prefix
/// on the wire counts *bytes*. For an ASCII string those are the same number,
/// which is why an implementation that checks the byte length looks correct
/// for years: every English username passes both readings. It breaks the first
/// time somebody's name contains a non-ASCII character, where a 16-unit name
/// can be 48 bytes — a byte-length check rejects a name vanilla accepts, and a
/// byte-length *limit* on encode truncates one vanilla sends.
///
/// The two are also different from the character count, which is what a
/// careless Rust implementation reaches for: `str::chars().count()` counts code
/// points, and an emoji is one code point and *two* UTF-16 units. So all three
/// of the obvious readings — bytes, chars, UTF-16 — differ, and only one is
/// right.
pub fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// Read a length-prefixed string, bounded at `limit` UTF-16 code units.
///
/// Two checks, in the order vanilla does them and for the reason vanilla does:
///
/// 1. The byte length is refused above `limit * 3` **before any bytes are
///    read**. This is the bound that stops a hostile length prefix asking for
///    a two-gigabyte allocation; it has to come first, and it has to be in
///    bytes because bytes are all that is known at that point.
/// 2. The decoded string's UTF-16 length is refused above `limit`. This is the
///    real limit. The first check cannot be it, because a string can be under
///    `limit * 3` bytes and still over `limit` units.
pub fn read_string<R: WireRead + ?Sized>(
    input: &mut R,
    limit: usize,
) -> Result<String, DecodeError> {
    let byte_len = input.read_var_int()?;
    let byte_len = usize::try_from(byte_len).map_err(|_| DecodeError::NegativeLength {
        field: "string",
        value: byte_len,
    })?;
    if byte_len > limit * MAX_UTF8_BYTES_PER_UTF16_UNIT {
        // Reported in UTF-16 units, because that is the unit of the limit the
        // caller set. The byte length cannot be converted to units exactly
        // without the bytes, so this is the tightest true statement available:
        // the string is at least this long.
        return Err(DecodeError::StringTooLong {
            limit,
            actual: byte_len.div_ceil(MAX_UTF8_BYTES_PER_UTF16_UNIT),
        });
    }
    let bytes = input.read_slice(byte_len)?;
    let text = std::str::from_utf8(bytes).map_err(|_| DecodeError::NotUtf8)?;
    let actual = utf16_len(text);
    if actual > limit {
        return Err(DecodeError::StringTooLong { limit, actual });
    }
    Ok(text.to_owned())
}

/// Write a length-prefixed string, bounded at `limit` UTF-16 code units.
///
/// The prefix is the **byte** length and the check is the **UTF-16** length.
/// They are deliberately different quantities; see [`utf16_len`].
pub fn write_string<W: WireWrite + ?Sized>(
    out: &mut W,
    text: &str,
    limit: usize,
) -> Result<(), EncodeError> {
    let actual = utf16_len(text);
    if actual > limit {
        return Err(EncodeError::StringTooLong { limit, actual });
    }
    let bytes = text.as_bytes();
    out.write_var_int(bytes.len() as i32);
    out.write_slice(bytes);
    Ok(())
}

/// A string field whose limit is part of its type.
///
/// `BoundedString<16>` is a username and `BoundedString<32767>` is a chat
/// payload, and mixing them up stops compiling. The alternative — a plain
/// `String` and a limit passed at each call site — puts the limit in the
/// codec instead of in the definition, which is exactly where a packet
/// definition should not have to look for it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct BoundedString<const MAX: usize>(pub String);

impl<const MAX: usize> BoundedString<MAX> {
    /// Build one, checking the limit up front rather than at encode time.
    pub fn new(text: impl Into<String>) -> Result<Self, EncodeError> {
        let text = text.into();
        let actual = utf16_len(&text);
        if actual > MAX {
            return Err(EncodeError::StringTooLong { limit: MAX, actual });
        }
        Ok(Self(text))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<const MAX: usize> fmt::Display for BoundedString<MAX> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<const MAX: usize> Decode for BoundedString<MAX> {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        _version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        read_string(input, MAX).map(Self)
    }
}

impl<const MAX: usize> Encode for BoundedString<MAX> {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        write_string(out, &self.0, MAX)
    }
}

/// A string with the protocol's default bound.
pub type ProtocolString = BoundedString<DEFAULT_STRING_LIMIT>;

// ---------------------------------------------------------------------------
// Identifier
// ---------------------------------------------------------------------------

/// A namespaced id — `minecraft:stone`, `dust:something`.
///
/// Stored split rather than as one string, because every use of one splits it,
/// and a type that makes the caller re-parse on each use is a type that will
/// eventually be parsed two different ways.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Identifier {
    pub namespace: String,
    pub path: String,
}

impl Identifier {
    /// The namespace a bare path gets, matching vanilla.
    pub const DEFAULT_NAMESPACE: &'static str = "minecraft";

    /// Parse `namespace:path`, or a bare `path` in the default namespace.
    ///
    /// The bare form is accepted here and *not* in `dust-registry`'s block
    /// lookup, and the difference is deliberate: this is a wire format that
    /// vanilla defines as accepting a bare path, so refusing it would refuse
    /// packets a real client sends. A registry lookup is an internal API where
    /// two spellings of one name is a bug.
    pub fn parse(text: &str) -> Result<Self, DecodeError> {
        let (namespace, path) = match text.split_once(':') {
            Some((namespace, path)) => (namespace, path),
            None => (Self::DEFAULT_NAMESPACE, text),
        };
        let bad = |_| DecodeError::BadIdentifier {
            value: text.to_owned(),
        };
        if namespace.is_empty() || path.is_empty() {
            return Err(bad(()));
        }
        if !namespace
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-'))
        {
            return Err(bad(()));
        }
        if !path
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'/'))
        {
            return Err(bad(()));
        }
        Ok(Self {
            namespace: namespace.to_owned(),
            path: path.to_owned(),
        })
    }
}

impl fmt::Display for Identifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.namespace, self.path)
    }
}

impl Decode for Identifier {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        _version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        Self::parse(&read_string(input, DEFAULT_STRING_LIMIT)?)
    }
}

impl Encode for Identifier {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        write_string(out, &self.to_string(), DEFAULT_STRING_LIMIT)
    }
}

// ---------------------------------------------------------------------------
// UUID
// ---------------------------------------------------------------------------

/// A UUID, on the wire as two big-endian `u64`s — most significant first.
///
/// Which is the same bytes as one big-endian `u128`, and is written that way.
/// Worth saying out loud because "two longs" invites an implementation that
/// gets the halves the wrong way round, and that produces a valid-looking UUID
/// for the wrong player.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Uuid(pub u128);

impl Uuid {
    pub const NIL: Self = Self(0);

    pub fn from_halves(most_significant: u64, least_significant: u64) -> Self {
        Self((u128::from(most_significant) << 64) | u128::from(least_significant))
    }

    pub fn halves(self) -> (u64, u64) {
        ((self.0 >> 64) as u64, self.0 as u64)
    }
}

impl fmt::Display for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hex = format!("{:032x}", self.0);
        write!(
            f,
            "{}-{}-{}-{}-{}",
            &hex[0..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..32]
        )
    }
}

impl Decode for Uuid {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        _version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        Ok(Self(u128::from_be_bytes(input.read_array()?)))
    }
}

impl Encode for Uuid {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        out.write_slice(&self.0.to_be_bytes());
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Position
// ---------------------------------------------------------------------------

/// A block position, packed into one `u64` since 1.14.
///
/// # The layout, and the part that gets missed
///
/// 26 bits of x, then 26 bits of z, then 12 bits of y — in that order from the
/// top of the word, which is *not* the order the fields are usually named in.
///
/// All three are **signed**, and that is where implementations break. Masking
/// the bits out gives an unsigned number; a position at `x = -100` decodes to
/// `x = 67108764` unless the top bit of the field is propagated. The failure
/// mode is exact and recognisable: everything works around spawn, where the
/// coordinates are small and positive, and breaks the moment a player walks
/// west or digs below `y = 0` — which, since 1.18, is ordinary play.
///
/// The decode below sign-extends by shifting each field to the top of an `i64`
/// and back down with an arithmetic shift, which is the whole of the trick and
/// costs nothing.
///
/// # Range
///
/// x and z hold −33,554,432..=33,554,431 and y holds −2048..=2047. Encoding
/// masks, exactly as vanilla's does, so a coordinate outside those wraps
/// rather than failing — the round trip is the identity only inside the range.
/// [`Position::is_representable`] is there for a caller that wants to know
/// before it finds out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Position {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl Position {
    pub const X_BITS: u32 = 26;
    pub const Z_BITS: u32 = 26;
    pub const Y_BITS: u32 = 12;

    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }

    /// Whether every coordinate fits its field, so that packing is lossless.
    pub fn is_representable(self) -> bool {
        fn fits(value: i32, bits: u32) -> bool {
            let half = 1i32 << (bits - 1);
            value >= -half && value < half
        }
        fits(self.x, Self::X_BITS) && fits(self.z, Self::Z_BITS) && fits(self.y, Self::Y_BITS)
    }

    /// Pack into the wire's `u64`, masking as vanilla does.
    pub fn to_bits(self) -> u64 {
        let x = (self.x as u64) & 0x3FF_FFFF;
        let z = (self.z as u64) & 0x3FF_FFFF;
        let y = (self.y as u64) & 0xFFF;
        (x << 38) | (z << 12) | y
    }

    /// Unpack from the wire's `u64`, sign-extending every field.
    pub fn from_bits(bits: u64) -> Self {
        let bits = bits as i64;
        Self {
            // Arithmetic shifts: left to put the field's top bit at bit 63,
            // right to bring it back with the sign carried down.
            x: (bits >> 38) as i32,
            z: ((bits << 26) >> 38) as i32,
            y: ((bits << 52) >> 52) as i32,
        }
    }
}

impl Decode for Position {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        _version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        Ok(Self::from_bits(input.read_i64()? as u64))
    }
}

impl Encode for Position {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        out.write_i64(self.to_bits() as i64);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Angle
// ---------------------------------------------------------------------------

/// A rotation as one byte: a full turn in 256 steps.
///
/// The type is the byte, and that is the point. A step is 1.40625°, so degrees
/// do not survive a round trip and this deliberately does not pretend they do:
/// `Angle -> degrees -> Angle` is the identity, `degrees -> Angle -> degrees`
/// is not and cannot be. Storing an `f32` here and converting at the edges
/// would hide a quantisation that callers need to know about — an entity's
/// yaw genuinely only has 256 values on the wire, and code that compares a
/// sent angle to a computed one has to compare in steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Angle(pub u8);

impl Angle {
    /// Degrees per step: 360/256.
    pub const STEP_DEGREES: f32 = 360.0 / 256.0;

    /// Nearest step to `degrees`, wrapping a full turn.
    pub fn from_degrees(degrees: f32) -> Self {
        let steps = (degrees / Self::STEP_DEGREES).round().rem_euclid(256.0);
        Self(steps as u8)
    }

    /// The midpoint of this step, in 0..360.
    pub fn to_degrees(self) -> f32 {
        f32::from(self.0) * Self::STEP_DEGREES
    }
}

impl Decode for Angle {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        _version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        input.read_u8().map(Self)
    }
}

impl Encode for Angle {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        out.write_u8(self.0);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Collections
// ---------------------------------------------------------------------------

/// A VarInt count followed by that many values.
///
/// # The allocation this must not make
///
/// The count is attacker-controlled and can say two billion. `Vec::with_capacity`
/// on it is an out-of-memory abort reachable from an unauthenticated socket in
/// one packet. So the reservation is capped at the number of bytes actually
/// left in the body, which is an upper bound on how many elements can possibly
/// follow — every element in the protocol costs at least one byte — and a count
/// that lied then fails on the first short read instead.
///
/// Capping rather than rejecting is deliberate. Rejecting `count > remaining`
/// up front would be wrong for a hypothetical zero-byte element type, and this
/// crate would rather be slow on a lie than wrong on a truth.
impl<T: Decode> Decode for Vec<T> {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let count = input.read_var_int()?;
        let count = usize::try_from(count).map_err(|_| DecodeError::NegativeLength {
            field: "array",
            value: count,
        })?;
        let mut out = Self::with_capacity(count.min(input.remaining()));
        for _ in 0..count {
            out.push(T::decode(input, version)?);
        }
        Ok(out)
    }
}

impl<T: Encode> Encode for Vec<T> {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        let count = i32::try_from(self.len())
            .map_err(|_| EncodeError::TooManyElements { count: self.len() })?;
        out.write_var_int(count);
        for value in self {
            value.encode(out, version)?;
        }
        Ok(())
    }
}

/// A bool, and the value only if it was true.
///
/// The protocol calls this "Optional X" and it is exactly `Option<T>`, so it is
/// `Option<T>` rather than a newtype. Note that this is not the same shape as
/// "the rest of the packet, if there is any" — a few fields are optional by
/// running out rather than by a flag, and those are spelled with the field type
/// that means that.
impl<T: Decode> Decode for Option<T> {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        if input.read_bool()? {
            Ok(Some(T::decode(input, version)?))
        } else {
            Ok(None)
        }
    }
}

impl<T: Encode> Encode for Option<T> {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        match self {
            Some(value) => {
                out.write_bool(true);
                value.encode(out, version)
            }
            None => {
                out.write_bool(false);
                Ok(())
            }
        }
    }
}

/// Every byte left in the body, with no length prefix.
///
/// Only valid as the last field of a packet, which is why it is a distinct type
/// rather than a `Vec<u8>`: a `RestOfPacket` in the middle of a definition
/// swallows the fields after it, and a named type makes that visible in the
/// definition instead of at three in the morning.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RestOfPacket(pub Vec<u8>);

impl Decode for RestOfPacket {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        _version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let len = input.remaining();
        input.read_vec(len).map(Self)
    }
}

impl Encode for RestOfPacket {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        out.write_slice(&self.0);
        Ok(())
    }
}

/// A VarInt byte count followed by that many bytes, bounded at `MAX`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PrefixedBytes<const MAX: usize>(pub Vec<u8>);

impl<const MAX: usize> Decode for PrefixedBytes<MAX> {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        _version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let len = input.read_var_int()?;
        let len = usize::try_from(len).map_err(|_| DecodeError::NegativeLength {
            field: "byte array",
            value: len,
        })?;
        if len > MAX {
            return Err(DecodeError::StringTooLong {
                limit: MAX,
                actual: len,
            });
        }
        input.read_vec(len).map(Self)
    }
}

impl<const MAX: usize> Encode for PrefixedBytes<MAX> {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        out.write_var_int(self.0.len() as i32);
        out.write_slice(&self.0);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Bit sets
// ---------------------------------------------------------------------------

/// A growable bit set: a VarInt-prefixed array of longs, least significant bit
/// of the first long is bit 0.
///
/// Distinct from [`FixedBitSet`], which is the same *idea* with a different
/// encoding — a fixed-size one has no length prefix and is packed into bytes
/// rather than longs. Conflating them produces a stream that is off by a few
/// bytes in a way nothing local notices.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BitSet(pub Vec<u64>);

impl BitSet {
    pub fn get(&self, index: usize) -> bool {
        self.0
            .get(index / 64)
            .is_some_and(|word| word >> (index % 64) & 1 == 1)
    }

    pub fn set(&mut self, index: usize, value: bool) {
        let word = index / 64;
        if word >= self.0.len() {
            self.0.resize(word + 1, 0);
        }
        let mask = 1u64 << (index % 64);
        if value {
            self.0[word] |= mask;
        } else {
            self.0[word] &= !mask;
        }
    }
}

impl Decode for BitSet {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        _version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let count = input.read_var_int()?;
        let count = usize::try_from(count).map_err(|_| DecodeError::NegativeLength {
            field: "bit set",
            value: count,
        })?;
        let mut words = Vec::with_capacity(count.min(input.remaining() / 8 + 1));
        for _ in 0..count {
            words.push(input.read_i64()? as u64);
        }
        Ok(Self(words))
    }
}

impl Encode for BitSet {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        let count = i32::try_from(self.0.len()).map_err(|_| EncodeError::TooManyElements {
            count: self.0.len(),
        })?;
        out.write_var_int(count);
        for word in &self.0 {
            out.write_i64(*word as i64);
        }
        Ok(())
    }
}

/// A bit set of a length both sides already know: `BITS` bits packed into
/// `ceil(BITS / 8)` bytes, with **no length prefix**.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixedBitSet<const BITS: usize>(pub Vec<u8>);

impl<const BITS: usize> FixedBitSet<BITS> {
    pub const BYTES: usize = BITS.div_ceil(8);

    pub fn new() -> Self {
        Self(vec![0; Self::BYTES])
    }

    pub fn get(&self, index: usize) -> bool {
        index < BITS && self.0[index / 8] >> (index % 8) & 1 == 1
    }

    pub fn set(&mut self, index: usize, value: bool) {
        if index >= BITS {
            return;
        }
        let mask = 1u8 << (index % 8);
        if value {
            self.0[index / 8] |= mask;
        } else {
            self.0[index / 8] &= !mask;
        }
    }
}

impl<const BITS: usize> Default for FixedBitSet<BITS> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const BITS: usize> Decode for FixedBitSet<BITS> {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        _version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        input.read_vec(Self::BYTES).map(Self)
    }
}

impl<const BITS: usize> Encode for FixedBitSet<BITS> {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        out.write_slice(&self.0);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// VarInt-tagged enums
// ---------------------------------------------------------------------------

/// Declare an enum that travels as a VarInt discriminant.
///
/// The decode arm that matters is the last one: a discriminant outside the
/// known set is [`DecodeError::UnknownVariant`], never a panic and never a
/// silent fallback to the first variant. This is unauthenticated input, so a
/// panic is a remote crash and a silent default is worse — it hands the rest of
/// the server a value the peer did not send, and the bug surfaces somewhere
/// with no connection to the packet that caused it.
#[macro_export]
macro_rules! var_int_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $($(#[$variant_meta:meta])* $variant:ident = $value:expr),* $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        $vis enum $name {
            $($(#[$variant_meta])* $variant = $value),*
        }

        impl $name {
            /// Every variant, for tests and for iteration.
            pub const ALL: &'static [Self] = &[$(Self::$variant),*];

            pub const fn discriminant(self) -> i32 {
                match self {
                    $(Self::$variant => $value),*
                }
            }

            pub const fn from_discriminant(value: i32) -> Option<Self> {
                match value {
                    $($value => Some(Self::$variant),)*
                    _ => None,
                }
            }
        }

        impl $crate::types::Decode for $name {
            fn decode<R: $crate::wire::WireRead + ?Sized>(
                input: &mut R,
                _version: $crate::ProtocolVersion,
            ) -> ::core::result::Result<Self, $crate::wire::DecodeError> {
                let value = input.read_var_int()?;
                Self::from_discriminant(value).ok_or($crate::wire::DecodeError::UnknownVariant {
                    name: ::core::stringify!($name),
                    value,
                })
            }
        }

        impl $crate::types::Encode for $name {
            fn encode<W: $crate::wire::WireWrite + ?Sized>(
                &self,
                out: &mut W,
                _version: $crate::ProtocolVersion,
            ) -> ::core::result::Result<(), $crate::wire::EncodeError> {
                out.write_var_int(self.discriminant());
                Ok(())
            }
        }
    };
}

var_int_enum! {
    /// Where a handshake is going: status, login, or a transfer that is a login
    /// carrying the fact that another server sent the player.
    pub enum NextState {
        Status = 1,
        Login = 2,
        Transfer = 3,
    }
}

var_int_enum! {
    /// What a player wants to see of chat.
    pub enum ChatVisibility {
        Full = 0,
        System = 1,
        Hidden = 2,
    }
}

var_int_enum! {
    /// Which hand a player holds a tool in.
    pub enum MainHand {
        Left = 0,
        Right = 1,
    }
}

var_int_enum! {
    /// What a client did with a resource pack it was pushed.
    pub enum ResourcePackResult {
        SuccessfullyLoaded = 0,
        Declined = 1,
        FailedToDownload = 2,
        Accepted = 3,
        Downloaded = 4,
        InvalidUrl = 5,
        FailedToReload = 6,
        Discarded = 7,
    }
}

// ---------------------------------------------------------------------------
// Slot
// ---------------------------------------------------------------------------

/// An item stack, in the data-component form 1.20.5 introduced.
///
/// A count, and if it is positive an item id and a **component patch** — the
/// components this stack adds to its item's defaults and the ones it strips
/// from them. The patch is the whole of what makes one diamond sword different
/// from another: its name, its enchantments, how worn it is, what is inside it.
///
/// # Why the patch is bytes rather than a type per component
///
/// A component carries no length. It is a VarInt type id followed by that
/// type's own layout, and there are fifty-seven of them. A reader that meets
/// one it cannot walk does not lose that component, it loses the position of
/// every field after it — so there is no partial credit on offer and never was.
///
/// What there *is*, and what this crate did not have when the refusal above was
/// written, is the difference between **walking** a component and modelling
/// one. [`crate::components`] walks all fifty-seven, and the bytes it walked
/// are kept as they arrived, compared as they arrived, and sent back as they
/// arrived. A server that never has to ask what an enchantment *is* can still
/// carry one without losing it.
///
/// See [`crate::components`] for the layouts, for how the type ids reach this
/// crate without being written down in it, and for why two patches are equal
/// when their canonical bytes are.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Slot {
    /// No item. On the wire, a count of zero and nothing else.
    #[default]
    Empty,
    Present {
        count: i32,
        item_id: i32,
        /// What this stack adds to, and removes from, its item's defaults.
        components: crate::components::ComponentPatch,
    },
}

impl Decode for Slot {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        _version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let count = input.read_var_int()?;
        if count <= 0 {
            return Ok(Self::Empty);
        }
        let item_id = input.read_var_int()?;
        let components = crate::components::ComponentPatch::decode(input)?;
        Ok(Self::Present {
            count,
            item_id,
            components,
        })
    }
}

impl Encode for Slot {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        _version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        match self {
            Self::Empty => out.write_var_int(0),
            Self::Present {
                count,
                item_id,
                components,
            } => {
                out.write_var_int(*count);
                out.write_var_int(*item_id);
                components.encode(out)?;
            }
        }
        Ok(())
    }
}
