//! Minecraft's variable-length integers, and the two decisions that make a
//! decoder of them safe rather than merely correct.
//!
//! # The encoding
//!
//! Seven bits of payload per byte, the eighth bit set on every byte but the
//! last. The groups are written **least significant first**, which is the
//! detail a round-trip test cannot see: an encoder and a decoder that both put
//! the most significant group first agree with each other perfectly and agree
//! with no Minecraft client that has ever existed. The only thing that catches
//! that is a byte string from outside — the constants in this module's tests,
//! and the live vanilla server in `tests/vanilla_status.rs`.
//!
//! The value is **signed**, and is the two's-complement bit pattern of an
//! `i32` (or `i64`), not a zigzag encoding. So `-1` is `0xFF 0xFF 0xFF 0xFF
//! 0x0F` — five bytes, the longest a VarInt gets — and not one byte. Protobuf
//! readers reach for zigzag here and are wrong; Minecraft does not zigzag.
//!
//! # The length cap is a security control
//!
//! A VarInt is at most 5 bytes and a VarLong at most 10, because that is how
//! many 7-bit groups it takes to cover 32 and 64 bits. A decoder that loops
//! "while the continuation bit is set" without that cap will read forever on a
//! stream of `0x80`, which costs an attacker one byte per iteration and costs
//! the server a core. [`MAX_VAR_INT_LEN`] and [`MAX_VAR_LONG_LEN`] are that
//! cap, and `a_run_of_continuation_bytes_is_refused` is the test that an
//! endless `0x80` stream is what stops it.
//!
//! # Overlong encodings are rejected
//!
//! `0x80 0x80 0x80 0x80 0x00` is five bytes that a naive reader decodes to
//! zero: every group is empty, and the value fits in one byte. Vanilla's own
//! reader accepts this. **Dust rejects it**, for a reason that is about
//! identity rather than arithmetic.
//!
//! A decoder that accepts overlong encodings makes the map from byte strings
//! to values many-to-one. Two frames that are byte-for-byte different then
//! carry the same packet, and anything that hashes, deduplicates, caches or
//! compares frames — a replay guard, a rate limiter keyed on packet identity,
//! a proxy that forwards what it received rather than what it parsed — is
//! quietly answering a different question than it thinks it is. Rejecting
//! non-canonical encodings makes the map a bijection, and a bijection is a
//! thing you can reason about.
//!
//! The same argument covers the final byte's unused bits. A five-byte VarInt's
//! last group supplies bits 28..35, of which only 28..32 exist in an `i32`.
//! Vanilla shifts the other three off the end and never notices; here, a final
//! byte above `0x0F` is [`VarIntError::Overflow`]. `0xFF 0xFF 0xFF 0xFF 0x7F`
//! and `0xFF 0xFF 0xFF 0xFF 0x0F` are both `-1` to vanilla and are not both
//! `-1` here.
//!
//! **What this decision does not catch, and what it costs.** It is stricter
//! than vanilla, so a client that emitted a non-canonical VarInt would be
//! disconnected by Dust and accepted by Mojang's server. No released client
//! does — the vanilla writer is canonical by construction, and the live-server
//! exchange in `tests/vanilla_status.rs` is the check that a real session
//! never trips it. A hostile client can still send any *canonical* nonsense it
//! likes; canonicity is a statement about encoding, not about meaning, and
//! nothing here says a decoded number is a sensible one. That is the frame
//! layer's job.

/// The most bytes a VarInt may occupy: five 7-bit groups cover 32 bits.
pub const MAX_VAR_INT_LEN: usize = 5;

/// The most bytes a VarLong may occupy: ten 7-bit groups cover 64 bits.
pub const MAX_VAR_LONG_LEN: usize = 10;

/// The largest value the final byte of a maximum-length VarInt may hold.
///
/// Four groups carry bits 0..28; the fifth carries 28..32, so four bits.
const VAR_INT_FINAL_MASK: u8 = 0x0F;

/// The largest value the final byte of a maximum-length VarLong may hold.
///
/// Nine groups carry bits 0..63; the tenth carries only bit 63.
const VAR_LONG_FINAL_MASK: u8 = 0x01;

/// The continuation bit: set on every byte of an encoding but the last.
const CONTINUE: u8 = 0x80;

/// The payload bits of one byte.
const PAYLOAD: u8 = 0x7F;

/// Why a variable-length integer could not be read.
///
/// Every variant names the input that produced it. An error that says only
/// "invalid VarInt" turns a packet capture into a guessing game, and this is
/// the layer where the packet capture is all anyone has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarIntError {
    /// The continuation bit was still set after the last byte the type allows.
    ///
    /// This is what a run of `0x80` produces, and refusing it is what stops
    /// the read rather than the connection dying of old age.
    TooLong {
        /// `"VarInt"` or `"VarLong"`.
        kind: &'static str,
        /// The cap that was exceeded, in bytes.
        limit: usize,
    },
    /// The encoding was longer than the value needs — padded with groups that
    /// contribute nothing. See the module docs for why this is refused.
    Overlong {
        kind: &'static str,
        /// How many bytes were used.
        used: usize,
        /// How many the value actually needs.
        canonical: usize,
    },
    /// The final byte set bits beyond the width of the target type.
    ///
    /// Vanilla shifts these off the end silently; refusing them keeps the
    /// encoding a bijection.
    Overflow {
        kind: &'static str,
        /// The offending final byte, as it appeared on the wire.
        final_byte: u8,
        /// The largest value that byte may take.
        allowed: u8,
    },
    /// The input ended mid-encoding.
    ///
    /// On a socket this is not an error at all — see [`VarIntReader`], which
    /// reports it as "need more" instead. It is an error only when the input
    /// is a complete buffer that claimed to hold an integer.
    Incomplete {
        kind: &'static str,
        /// How many bytes were available.
        available: usize,
    },
}

impl std::fmt::Display for VarIntError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLong { kind, limit } => write!(
                f,
                "{kind} ran past its {limit}-byte limit with the continuation bit still set"
            ),
            Self::Overlong {
                kind,
                used,
                canonical,
            } => write!(
                f,
                "{kind} was written in {used} bytes where {canonical} encode the same value; \
                 Dust requires the canonical encoding"
            ),
            Self::Overflow {
                kind,
                final_byte,
                allowed,
            } => write!(
                f,
                "{kind} ended with byte {final_byte:#04x}, which sets bits beyond the type's \
                 width; the last byte may be at most {allowed:#04x}"
            ),
            Self::Incomplete { kind, available } => {
                write!(f, "{kind} was cut off after {available} byte(s)")
            }
        }
    }
}

impl std::error::Error for VarIntError {}

/// How many bytes [`write_var_int`] will produce for `value`.
///
/// Exact, not an upper bound: the frame encoder needs the true length to
/// compute a length prefix without encoding twice.
pub fn var_int_len(value: i32) -> usize {
    // The cast is the whole trick: the encoding is of the two's-complement bit
    // pattern, so a negative number is a very large unsigned one and takes the
    // full five bytes.
    // Zero needs one byte, not none.
    groups_for(u64::from(value as u32))
}

/// How many bytes [`write_var_long`] will produce for `value`.
pub fn var_long_len(value: i64) -> usize {
    groups_for(value as u64)
}

/// How many 7-bit groups the canonical encoding of a two's-complement pattern
/// takes. One, for zero — the empty encoding is not a thing.
fn groups_for(bits: u64) -> usize {
    let significant = (64 - bits.leading_zeros()) as usize;
    significant.max(1).div_ceil(7)
}

/// Append the canonical encoding of `value` to `out`, and return its length.
pub fn write_var_int(value: i32, out: &mut Vec<u8>) -> usize {
    write_groups(value as u32 as u64, out)
}

/// Append the canonical encoding of `value` to `out`, and return its length.
pub fn write_var_long(value: i64, out: &mut Vec<u8>) -> usize {
    write_groups(value as u64, out)
}

/// The shared writer. Both types are the same loop over a `u64` holding the
/// two's-complement pattern, differing only in how many bits are in it.
fn write_groups(mut bits: u64, out: &mut Vec<u8>) -> usize {
    let start = out.len();
    loop {
        let group = (bits & u64::from(PAYLOAD)) as u8;
        bits >>= 7;
        if bits == 0 {
            out.push(group);
            break;
        }
        out.push(group | CONTINUE);
    }
    out.len() - start
}

/// Read a VarInt from the front of `input`, returning it and how many bytes it
/// took.
///
/// Trailing bytes are not an error; this is a stream format and the caller
/// advances by the returned length.
pub fn read_var_int(input: &[u8]) -> Result<(i32, usize), VarIntError> {
    let mut reader = VarIntReader::new();
    for (index, &byte) in input.iter().take(MAX_VAR_INT_LEN).enumerate() {
        if let Some(value) = reader.push(byte)? {
            return Ok((value, index + 1));
        }
    }
    // Either the input ran out, or it held MAX_VAR_INT_LEN bytes that all had
    // the continuation bit set. The reader has already refused the second case
    // inside the loop, so only the first can reach here.
    Err(VarIntError::Incomplete {
        kind: "VarInt",
        available: input.len(),
    })
}

/// Read a VarLong from the front of `input`, returning it and its length.
pub fn read_var_long(input: &[u8]) -> Result<(i64, usize), VarIntError> {
    let mut reader = VarLongReader::new();
    for (index, &byte) in input.iter().take(MAX_VAR_LONG_LEN).enumerate() {
        if let Some(value) = reader.push(byte)? {
            return Ok((value, index + 1));
        }
    }
    Err(VarIntError::Incomplete {
        kind: "VarLong",
        available: input.len(),
    })
}

/// The accumulator both incremental readers are made of.
///
/// Kept private because its `Option` return means "not finished yet", and that
/// only reads correctly through the two wrappers, where the alternative to
/// finishing is spelled out in the type's documentation.
#[derive(Debug, Clone, Copy)]
struct Groups {
    bits: u64,
    shift: u32,
    seen: usize,
}

impl Groups {
    const fn new() -> Self {
        Self {
            bits: 0,
            shift: 0,
            seen: 0,
        }
    }

    /// Feed one byte.
    ///
    /// `Ok(None)` means the encoding continues. `Ok(Some(bits))` means it
    /// ended and `bits` holds the two's-complement pattern.
    fn push(
        &mut self,
        byte: u8,
        kind: &'static str,
        limit: usize,
        final_mask: u8,
    ) -> Result<Option<u64>, VarIntError> {
        if self.seen >= limit {
            // Reachable only if a caller keeps pushing after an error; the
            // check below fires first on a well-behaved stream. Kept because
            // "the caller ignored an error" must not become "the accumulator
            // shifts past 64 and the value is nonsense".
            return Err(VarIntError::TooLong { kind, limit });
        }
        self.seen += 1;
        let last = byte & CONTINUE == 0;

        if self.seen == limit {
            if !last {
                return Err(VarIntError::TooLong { kind, limit });
            }
            if byte > final_mask {
                return Err(VarIntError::Overflow {
                    kind,
                    final_byte: byte,
                    allowed: final_mask,
                });
            }
        }

        self.bits |= u64::from(byte & PAYLOAD) << self.shift;
        self.shift += 7;

        if !last {
            return Ok(None);
        }

        // Canonicity. A multi-byte encoding whose last group is empty could
        // have been written shorter, and by the module's rule that is a
        // different byte string for the same number, which is exactly what is
        // being refused.
        if self.seen > 1 && byte == 0 {
            return Err(VarIntError::Overlong {
                kind,
                used: self.seen,
                // Computed from the value rather than assumed to be one byte
                // shorter: `0x80 0x80 0x80 0x80 0x00` is five bytes for zero,
                // and zero's canonical form is one byte, not four.
                canonical: groups_for(self.bits),
            });
        }
        Ok(Some(self.bits))
    }
}

/// A VarInt decoder that can be fed one byte at a time.
///
/// A socket does not deliver packet boundaries. `read()` returns whatever
/// arrived, which is routinely three bytes of a five-byte length prefix, and a
/// decoder that treats that as malformed input disconnects honest clients on a
/// busy network. This one says "not yet" by returning `Ok(None)` and keeps its
/// state, so the caller can wait for more without re-parsing what it has.
///
/// It still refuses everything the one-shot reader refuses, at the same byte:
/// the cap, overlong encodings and a too-wide final byte are all decided as
/// the offending byte arrives, not after the value is complete. That matters
/// for the cap in particular — the point of it is to stop reading, and a check
/// that happens "at the end" of an endless encoding never happens.
#[derive(Debug, Clone, Copy)]
pub struct VarIntReader(Groups);

impl VarIntReader {
    pub const fn new() -> Self {
        Self(Groups::new())
    }

    /// Feed one byte. `Ok(None)` means more bytes are needed.
    ///
    /// After an error the reader must be discarded, not reused; the connection
    /// it was reading is not recoverable anyway, because there is no way to
    /// know where the next packet starts.
    pub fn push(&mut self, byte: u8) -> Result<Option<i32>, VarIntError> {
        Ok(self
            .0
            .push(byte, "VarInt", MAX_VAR_INT_LEN, VAR_INT_FINAL_MASK)?
            .map(|bits| bits as u32 as i32))
    }

    /// How many bytes have been fed so far.
    pub fn len(&self) -> usize {
        self.0.seen
    }

    /// Whether nothing has been fed yet.
    pub fn is_empty(&self) -> bool {
        self.0.seen == 0
    }
}

impl Default for VarIntReader {
    fn default() -> Self {
        Self::new()
    }
}

/// A VarLong decoder that can be fed one byte at a time. See [`VarIntReader`].
#[derive(Debug, Clone, Copy)]
pub struct VarLongReader(Groups);

impl VarLongReader {
    pub const fn new() -> Self {
        Self(Groups::new())
    }

    /// Feed one byte. `Ok(None)` means more bytes are needed.
    pub fn push(&mut self, byte: u8) -> Result<Option<i64>, VarIntError> {
        Ok(self
            .0
            .push(byte, "VarLong", MAX_VAR_LONG_LEN, VAR_LONG_FINAL_MASK)?
            .map(|bits| bits as i64))
    }

    /// How many bytes have been fed so far.
    pub fn len(&self) -> usize {
        self.0.seen
    }

    /// Whether nothing has been fed yet.
    pub fn is_empty(&self) -> bool {
        self.0.seen == 0
    }
}

impl Default for VarLongReader {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The protocol's own VarInt table, byte for byte.
    ///
    /// **This is the outside check.** It is copied from the community protocol
    /// documentation's sample table, not produced by the code below it, and
    /// that is the entire point: [`write_var_int`] and [`read_var_int`] agree
    /// with each other under any convention, including one with the groups the
    /// wrong way round. Only a byte string somebody else wrote down can say
    /// which convention is Minecraft's.
    ///
    /// The rows that carry the argument are `25565` — a value whose three
    /// groups are all different, so reversing them changes the bytes — and the
    /// negatives, which say that this is a two's-complement encoding of a
    /// signed integer and not a zigzag one. A zigzag reader makes `-1` into
    /// `0x01`.
    ///
    /// The second, independent outside check is `tests/vanilla_status.rs`,
    /// where a real 1.21.1 server has to answer bytes this module produced.
    const VAR_INT_TABLE: &[(i32, &[u8])] = &[
        (0, &[0x00]),
        (1, &[0x01]),
        (2, &[0x02]),
        (127, &[0x7f]),
        (128, &[0x80, 0x01]),
        (255, &[0xff, 0x01]),
        (25565, &[0xdd, 0xc7, 0x01]),
        (2097151, &[0xff, 0xff, 0x7f]),
        (2147483647, &[0xff, 0xff, 0xff, 0xff, 0x07]),
        (-1, &[0xff, 0xff, 0xff, 0xff, 0x0f]),
        (-2147483648, &[0x80, 0x80, 0x80, 0x80, 0x08]),
    ];

    /// The protocol's VarLong table. Same provenance and same argument.
    const VAR_LONG_TABLE: &[(i64, &[u8])] = &[
        (0, &[0x00]),
        (1, &[0x01]),
        (2, &[0x02]),
        (127, &[0x7f]),
        (128, &[0x80, 0x01]),
        (255, &[0xff, 0x01]),
        (2147483647, &[0xff, 0xff, 0xff, 0xff, 0x07]),
        (
            9223372036854775807,
            &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f],
        ),
        (
            -1,
            &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01],
        ),
        (
            -2147483648,
            &[0x80, 0x80, 0x80, 0x80, 0xf8, 0xff, 0xff, 0xff, 0xff, 0x01],
        ),
        (
            -9223372036854775808,
            &[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01],
        ),
    ];

    fn encoded(value: i32) -> Vec<u8> {
        let mut out = Vec::new();
        write_var_int(value, &mut out);
        out
    }

    #[test]
    fn var_int_encoding_matches_the_published_table() {
        for &(value, bytes) in VAR_INT_TABLE {
            assert_eq!(encoded(value), bytes, "encoding {value}");
        }
    }

    #[test]
    fn var_int_decoding_matches_the_published_table() {
        for &(value, bytes) in VAR_INT_TABLE {
            assert_eq!(read_var_int(bytes), Ok((value, bytes.len())), "{value}");
        }
    }

    #[test]
    fn var_long_encoding_matches_the_published_table() {
        for &(value, bytes) in VAR_LONG_TABLE {
            let mut out = Vec::new();
            write_var_long(value, &mut out);
            assert_eq!(out, bytes, "encoding {value}");
        }
    }

    #[test]
    fn var_long_decoding_matches_the_published_table() {
        for &(value, bytes) in VAR_LONG_TABLE {
            assert_eq!(read_var_long(bytes), Ok((value, bytes.len())), "{value}");
        }
    }

    #[test]
    fn declared_length_matches_what_is_written() {
        // The frame encoder computes a length prefix from `var_int_len`
        // without encoding first. If these ever disagree, every frame it
        // writes is off by a byte and nothing else in the crate would notice.
        for value in [0, 1, -1, 127, 128, 16383, 16384, i32::MIN, i32::MAX, 25565] {
            assert_eq!(var_int_len(value), encoded(value).len(), "{value}");
        }
        for value in [0i64, 1, -1, 127, 128, i64::MIN, i64::MAX, i32::MIN as i64] {
            let mut out = Vec::new();
            write_var_long(value, &mut out);
            assert_eq!(var_long_len(value), out.len(), "{value}");
        }
    }

    #[test]
    fn every_boundary_round_trips() {
        // Weaker than the table above and kept anyway: it covers the values
        // between the documented ones. It proves the halves agree, nothing
        // more, which is why it is not the first test in this file.
        for value in [
            0,
            1,
            -1,
            127,
            128,
            -128,
            16383,
            16384,
            2097151,
            2097152,
            268435455,
            268435456,
            i32::MIN,
            i32::MAX,
            i32::MIN + 1,
            i32::MAX - 1,
        ] {
            let bytes = encoded(value);
            assert!(bytes.len() <= MAX_VAR_INT_LEN, "{value} took {bytes:?}");
            assert_eq!(read_var_int(&bytes), Ok((value, bytes.len())), "{value}");
        }
        for value in [
            0i64,
            1,
            -1,
            127,
            128,
            16383,
            16384,
            i64::from(i32::MAX),
            i64::from(i32::MIN),
            i64::MIN,
            i64::MAX,
        ] {
            let mut bytes = Vec::new();
            write_var_long(value, &mut bytes);
            assert!(bytes.len() <= MAX_VAR_LONG_LEN, "{value} took {bytes:?}");
            assert_eq!(read_var_long(&bytes), Ok((value, bytes.len())), "{value}");
        }
    }

    #[test]
    fn a_run_of_continuation_bytes_is_refused() {
        // The attack this module's cap exists for: `0x80` forever costs the
        // sender one byte per iteration. The assertion that matters is not
        // only that it errors but *when* — after five bytes, having read no
        // further, which is what makes the cost of the attack finite.
        let flood = [0x80u8; 4096];
        assert_eq!(
            read_var_int(&flood),
            Err(VarIntError::TooLong {
                kind: "VarInt",
                limit: MAX_VAR_INT_LEN
            })
        );
        assert_eq!(
            read_var_long(&flood),
            Err(VarIntError::TooLong {
                kind: "VarLong",
                limit: MAX_VAR_LONG_LEN
            })
        );

        // And the same through the incremental reader, counting the bytes it
        // consumed before it stopped.
        let mut reader = VarIntReader::new();
        let mut fed = 0;
        for &byte in &flood {
            fed += 1;
            if reader.push(byte).is_err() {
                break;
            }
        }
        assert_eq!(
            fed, MAX_VAR_INT_LEN,
            "the cap must stop the read, not the input"
        );
    }

    #[test]
    fn overlong_encodings_are_refused() {
        // Five bytes that a permissive reader calls zero.
        assert_eq!(
            read_var_int(&[0x80, 0x80, 0x80, 0x80, 0x00]),
            Err(VarIntError::Overlong {
                kind: "VarInt",
                used: 5,
                canonical: 1,
            })
        );
        // Two bytes that a permissive reader calls one.
        assert_eq!(
            read_var_int(&[0x81, 0x00]),
            Err(VarIntError::Overlong {
                kind: "VarInt",
                used: 2,
                canonical: 1,
            })
        );
        // A value that genuinely needs two bytes, padded to three.
        assert_eq!(
            read_var_int(&[0x80, 0x81, 0x00]),
            Err(VarIntError::Overlong {
                kind: "VarInt",
                used: 3,
                canonical: 2,
            })
        );
        assert_eq!(
            read_var_long(&[0x80, 0x80, 0x00]),
            Err(VarIntError::Overlong {
                kind: "VarLong",
                used: 3,
                canonical: 1,
            })
        );
    }

    #[test]
    fn two_byte_strings_never_mean_the_same_number() {
        // The property the overlong rule exists to give: the map from byte
        // strings to values is injective. Anything that hashes, deduplicates
        // or compares frames depends on it, and this is where it is claimed
        // out loud rather than left as a consequence.
        //
        // The check is that every accepted prefix re-encodes to itself. That
        // is enough: if two different strings decoded to one value, they would
        // both have to equal that value's single encoding, which they cannot.
        // Doing it this way costs no memory, so the sweep can be exhaustive
        // over all three-byte strings rather than sampled — and three bytes is
        // where the interesting non-canonical forms first appear.
        let mut accepted = 0u32;
        for first in 0u16..=0xff {
            for second in 0u16..=0xff {
                for third in 0u16..=0xff {
                    let bytes = [first as u8, second as u8, third as u8];
                    let Ok((value, used)) = read_var_int(&bytes) else {
                        continue;
                    };
                    accepted += 1;
                    assert_eq!(
                        encoded(value),
                        bytes[..used],
                        "{bytes:x?} decoded to {value}, which encodes to something else"
                    );
                }
            }
        }
        // A loop that accepted nothing would pass the assertion above and
        // prove nothing, so the count is checked too. The arithmetic:
        // 128 first bytes terminate immediately and the other two are free
        // (128 * 65536); 128 * 127 two-byte forms with a free third byte, the
        // 127 excluding the overlong `0x00` terminator; and 128 * 128 * 127
        // three-byte forms.
        assert_eq!(accepted, 128 * 65536 + 128 * 127 * 256 + 128 * 128 * 127);
    }

    #[test]
    fn a_final_byte_wider_than_the_type_is_refused() {
        // Vanilla decodes both of these to -1 by shifting the extra bits off
        // the end. Accepting them would put two byte strings on one value,
        // which is the same defect as an overlong encoding wearing a different
        // hat.
        assert_eq!(
            read_var_int(&[0xff, 0xff, 0xff, 0xff, 0x7f]),
            Err(VarIntError::Overflow {
                kind: "VarInt",
                final_byte: 0x7f,
                allowed: 0x0f,
            })
        );
        assert_eq!(
            read_var_int(&[0xff, 0xff, 0xff, 0xff, 0x1f]),
            Err(VarIntError::Overflow {
                kind: "VarInt",
                final_byte: 0x1f,
                allowed: 0x0f,
            })
        );
        // The largest final byte that is still legal decodes fine.
        assert_eq!(read_var_int(&[0xff, 0xff, 0xff, 0xff, 0x0f]), Ok((-1, 5)));

        assert_eq!(
            read_var_long(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x03]),
            Err(VarIntError::Overflow {
                kind: "VarLong",
                final_byte: 0x03,
                allowed: 0x01,
            })
        );
    }

    #[test]
    fn a_truncated_encoding_is_incomplete_and_not_malformed() {
        for value in [128, 16384, i32::MIN, -1] {
            let bytes = encoded(value);
            for cut in 1..bytes.len() {
                assert_eq!(
                    read_var_int(&bytes[..cut]),
                    Err(VarIntError::Incomplete {
                        kind: "VarInt",
                        available: cut
                    }),
                    "{value} cut to {cut}"
                );
            }
        }
        assert_eq!(
            read_var_int(&[]),
            Err(VarIntError::Incomplete {
                kind: "VarInt",
                available: 0
            })
        );
    }

    #[test]
    fn the_incremental_reader_accepts_one_byte_at_a_time() {
        // The case a socket produces and a buffer never does: the value
        // arrives split across reads, and the reader has to say "not yet"
        // rather than "malformed".
        for &(value, bytes) in VAR_INT_TABLE {
            let mut reader = VarIntReader::new();
            for (index, &byte) in bytes.iter().enumerate() {
                let step = reader.push(byte).expect("table entries are well formed");
                if index + 1 == bytes.len() {
                    assert_eq!(step, Some(value), "{value} at its last byte");
                } else {
                    assert_eq!(
                        step,
                        None,
                        "{value} must want more after {} bytes",
                        index + 1
                    );
                }
            }
            assert_eq!(reader.len(), bytes.len());
        }
        for &(value, bytes) in VAR_LONG_TABLE {
            let mut reader = VarLongReader::new();
            let mut got = None;
            for &byte in bytes {
                got = reader.push(byte).expect("table entries are well formed");
            }
            assert_eq!(got, Some(value));
        }
    }

    #[test]
    fn the_incremental_reader_refuses_what_the_one_shot_reader_refuses() {
        // Two decoders that disagree about what is valid are a parser
        // differential, and a parser differential in front of a length prefix
        // is how a frame gets past one check and is read by the other.
        let inputs: &[&[u8]] = &[
            &[0x80, 0x80, 0x80, 0x80, 0x80],
            &[0x80, 0x80, 0x80, 0x80, 0x00],
            &[0x81, 0x00],
            &[0xff, 0xff, 0xff, 0xff, 0x7f],
            &[0xdd, 0xc7, 0x01],
            &[0x00],
            &[0xff, 0xff, 0xff, 0xff, 0x0f],
        ];
        for input in inputs {
            let one_shot = read_var_int(input).map(|(value, _)| value);
            let mut reader = VarIntReader::new();
            let mut incremental = Err(VarIntError::Incomplete {
                kind: "VarInt",
                available: input.len(),
            });
            for &byte in *input {
                match reader.push(byte) {
                    Ok(Some(value)) => {
                        incremental = Ok(value);
                        break;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        incremental = Err(error);
                        break;
                    }
                }
            }
            assert_eq!(one_shot, incremental, "disagreement on {input:x?}");
        }
    }

    #[test]
    fn errors_name_the_input_that_caused_them() {
        // A packet capture is all anyone has at this layer, so the message has
        // to be enough to find the byte.
        let message = read_var_int(&[0xff, 0xff, 0xff, 0xff, 0x7f])
            .unwrap_err()
            .to_string();
        assert!(message.contains("0x7f"), "{message}");
        let message = read_var_int(&[0x81, 0x00]).unwrap_err().to_string();
        assert!(message.contains('2') && message.contains('1'), "{message}");
    }
}
