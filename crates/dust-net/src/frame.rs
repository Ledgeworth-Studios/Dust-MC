//! Minecraft's frame layer: a length prefix, and after the login handshake a
//! compression header inside it.
//!
//! # What a frame is here
//!
//! A [`Frame`] is a packet id and a body of bytes. **This crate does not know
//! what any of them mean** — see the crate docs for where the `dust-protocol`
//! seam is. The id is decoded because it is a VarInt at a known offset and
//! somebody has to strip it; nothing here looks it up.
//!
//! # The two wire forms
//!
//! Before compression is enabled:
//!
//! ```text
//! [VarInt length][length bytes: [VarInt packet id][body]]
//! ```
//!
//! After a Set Compression packet with threshold `t`:
//!
//! ```text
//! [VarInt length][VarInt data length][data]
//! ```
//!
//! where a data length of **0** means `data` is the plain payload, sent
//! uncompressed because it was shorter than `t`, and any other value is the
//! length the zlib stream in `data` decompresses to.
//!
//! # The threat model, and the defences
//!
//! Every byte here arrived from an unauthenticated stranger. The handshake and
//! status paths need no credentials at all, and compression is enabled during
//! login — before the session server has said who anybody is. So the
//! compressed path, the more dangerous of the two, is reachable pre-auth.
//!
//! Each of these is a defence with a test that proves it bites; the test names
//! are in `tests/frame_defences.rs` and in this file's own test module.
//!
//! 1. **A length cap, applied before anything is allocated.** A prefix
//!    claiming two gigabytes is refused while the only thing read is the
//!    prefix. [`Limits::max_frame_len`] defaults to [`MAX_FRAME_LEN`], the
//!    largest three-byte VarInt, which is what vanilla uses.
//! 2. **A negative length is a length.** The prefix is a *signed* VarInt, so
//!    `0xff 0xff 0xff 0xff 0x0f` is `-1`. A decoder that casts it to `usize`
//!    before comparing it with a maximum gets `18446744073709551615` and a
//!    comparison that passes on some paths and allocates on others. It is
//!    range-checked as an `i32`, before any cast.
//! 3. **A frame that claims to be uncompressed but is not small.** Data length
//!    `0` with a payload at or above the threshold is a client skipping
//!    compression it was told to use. Refused.
//! 4. **A frame that claims to be compressed but is small.** Data length below
//!    the threshold should have been sent as form 3. Refused. Both directions
//!    are needed: a server that checks only one still lets the other through,
//!    and the pair of them is what makes the wire form a function of the
//!    payload size rather than a client's choice.
//! 5. **Decompression bombs.** The output is bounded twice — by the declared
//!    data length and by [`Limits::max_decompressed_len`] — and the declared
//!    length is never used as an allocation size. A 2 KiB zlib stream that
//!    expands to a gigabyte hits the bound after roughly two megabytes and is
//!    refused, having allocated roughly two megabytes rather than a gigabyte.
//! 6. **A declared length that disagrees with what came out.** Even under the
//!    bound, a stream that decompresses to a different size than declared is
//!    refused rather than accepted at its real size, because the declared
//!    length is what any downstream length arithmetic was written against.
//! 7. **Trailing bytes after the zlib stream.** A frame whose compressed data
//!    ends before the frame does is carrying something the decompressor never
//!    looked at. That is a request smuggling primitive, and it is refused.
//!
//! # What these guards do not catch
//!
//! They bound *size* and *shape*, and say nothing about *meaning*. A frame of
//! two million bytes with a valid packet id, sent a thousand times a second,
//! passes every check in this file; that is the byte budget and the rate
//! limiting in [`crate::io`], not this. A frame whose body is structurally
//! nonsense for its id passes too — that is `dust-protocol`'s to reject. And
//! `max_frame_len` is a policy number, not a protocol constant: raising it to
//! accommodate a modded client raises the cost of every one of these attacks
//! by the same factor.

use std::io::Read as _;

use flate2::bufread::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;

use crate::varint::{read_var_int, var_int_len, write_var_int, VarIntError, MAX_VAR_INT_LEN};

/// The largest frame vanilla will send or accept: the largest three-byte
/// VarInt, `2^21 - 1`.
///
/// It is not a round number because it is not a size somebody chose; it is the
/// point at which the length prefix would need a fourth byte. Vanilla's own
/// encoder asserts it, which is why a Dust that accepted more would be
/// accepting frames no vanilla client can produce.
pub const MAX_FRAME_LEN: usize = 2_097_151;

/// How much output buffer to reserve before decompressing.
///
/// Deliberately unrelated to the declared length. Reserving what a stranger
/// declared is the allocation half of a decompression bomb: the attacker
/// spends five bytes and the server spends two megabytes, before a single byte
/// of the zlib stream has been looked at. The buffer grows geometrically from
/// here if the data really is that big.
const DECOMPRESS_RESERVE: usize = 8 * 1024;

/// One packet, as this layer sees it: an id and an opaque body.
///
/// The body excludes the id. Nothing in this crate interprets either field —
/// [`Frame::id`] is decoded only because it is a VarInt at a fixed offset and
/// something has to strip it before the body starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// The packet id, as a signed VarInt. Signed because the wire type is:
    /// a negative id is malformed rather than impossible, and saying so needs
    /// a type that can hold it.
    pub id: i32,
    /// Everything after the id.
    pub body: Vec<u8>,
}

impl Frame {
    pub fn new(id: i32, body: impl Into<Vec<u8>>) -> Self {
        Self {
            id,
            body: body.into(),
        }
    }

    /// How many bytes this frame's payload occupies before any compression.
    pub fn payload_len(&self) -> usize {
        var_int_len(self.id) + self.body.len()
    }
}

/// The size bounds a connection enforces.
///
/// Separate from the codec so a test can set them small enough to trip in a
/// millisecond, and so an operator can lower — never silently raise — them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The largest length prefix that will be accepted.
    pub max_frame_len: usize,
    /// The largest a compressed frame may expand to, whatever it declares.
    ///
    /// The second of the two bombs bounds. The first is the declared length
    /// itself; this one exists because the declared length is a number the
    /// attacker chose.
    pub max_decompressed_len: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_frame_len: MAX_FRAME_LEN,
            max_decompressed_len: MAX_FRAME_LEN,
        }
    }
}

/// Why a frame could not be read.
///
/// Every variant carries the numbers that produced it. At this layer the only
/// evidence anybody has is a packet capture, and "malformed frame" turns
/// reading one into a guessing game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    /// The length prefix itself was not a readable VarInt.
    Length(VarIntError),
    /// The compression header's data length was not a readable VarInt.
    DataLength(VarIntError),
    /// The packet id was not a readable VarInt.
    PacketId(VarIntError),
    /// The length prefix was negative. It is a signed VarInt, so this is a
    /// value a client can send, not a value that cannot happen.
    NegativeLength { declared: i32 },
    /// The length prefix exceeded the cap. Reported before allocating.
    TooLarge { declared: usize, limit: usize },
    /// A frame with no bytes at all, which cannot even hold a packet id.
    Empty,
    /// A frame declaring data length 0 — "not compressed" — whose payload is
    /// at or above the threshold. The client is skipping compression.
    UncompressedOverThreshold { len: usize, threshold: usize },
    /// A frame declaring a compressed size below the threshold, which should
    /// have been sent uncompressed.
    CompressedUnderThreshold { declared: usize, threshold: usize },
    /// The declared uncompressed size was negative.
    NegativeDataLength { declared: i32 },
    /// The declared uncompressed size exceeded the cap, before decompressing.
    DeclaredTooLarge { declared: usize, limit: usize },
    /// Decompression produced more bytes than the frame declared. This is the
    /// bomb: the stream kept expanding past the size it promised.
    Bomb { limit: usize },
    /// Decompression finished, having produced a different number of bytes
    /// than the frame declared.
    LengthMismatch { declared: usize, actual: usize },
    /// The zlib stream ended before the frame did, leaving bytes nothing read.
    TrailingBytes { unread: usize },
    /// zlib refused the stream. The message is the decompressor's.
    Corrupt(String),
    /// A frame too large to encode, caught on the way out rather than on the
    /// way in. A server that can be made to emit an oversized frame has been
    /// made to disconnect its own clients.
    Oversize { len: usize, limit: usize },
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Length(e) => write!(f, "the frame's length prefix is unreadable: {e}"),
            Self::DataLength(e) => {
                write!(f, "the compression header's data length is unreadable: {e}")
            }
            Self::PacketId(e) => write!(f, "the packet id is unreadable: {e}"),
            Self::NegativeLength { declared } => write!(
                f,
                "the frame declared a length of {declared}; the prefix is a signed VarInt and \
                 a negative one is not a size"
            ),
            Self::TooLarge { declared, limit } => write!(
                f,
                "the frame declared {declared} bytes and the limit is {limit}; refused before \
                 anything was allocated"
            ),
            Self::Empty => write!(f, "the frame is empty, so it cannot hold even a packet id"),
            Self::UncompressedOverThreshold { len, threshold } => write!(
                f,
                "the frame declared itself uncompressed at {len} bytes, which is at or above \
                 the compression threshold of {threshold}; it should have been compressed"
            ),
            Self::CompressedUnderThreshold {
                declared,
                threshold,
            } => write!(
                f,
                "the frame is compressed and declares {declared} bytes, below the compression \
                 threshold of {threshold}; it should have been sent uncompressed"
            ),
            Self::NegativeDataLength { declared } => write!(
                f,
                "the frame declared an uncompressed size of {declared}, which is not a size"
            ),
            Self::DeclaredTooLarge { declared, limit } => write!(
                f,
                "the frame declared it decompresses to {declared} bytes and the limit is \
                 {limit}; refused before decompressing"
            ),
            Self::Bomb { limit } => write!(
                f,
                "decompression passed {limit} bytes and was stopped; the frame declared less \
                 than it produced, which is what a decompression bomb looks like"
            ),
            Self::LengthMismatch { declared, actual } => write!(
                f,
                "the frame declared it decompresses to {declared} bytes and it decompressed to \
                 {actual}"
            ),
            Self::TrailingBytes { unread } => write!(
                f,
                "the zlib stream ended with {unread} byte(s) of the frame unread; a frame that \
                 carries data the decompressor never saw is refused"
            ),
            Self::Corrupt(message) => write!(f, "the compressed data is not valid zlib: {message}"),
            Self::Oversize { len, limit } => write!(
                f,
                "refusing to send a {len}-byte frame; the limit is {limit}"
            ),
        }
    }
}

impl std::error::Error for FrameError {}

/// How many more bytes [`FrameDecoder::next_frame`] needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Needed {
    /// The length prefix is not complete, so the frame's size is not yet
    /// knowable. Feed one more byte and ask again.
    Unknown,
    /// Exactly this many more bytes. Zero means a frame — or the error that
    /// replaces it — is ready now.
    Exactly(usize),
}

/// Whether compression is on, and the size at which it starts.
///
/// The threshold is a `usize` rather than the protocol's signed VarInt because
/// a negative threshold is not "compress everything" — it is a Set Compression
/// packet meaning *disable*, which is [`Compression::Disabled`] here. Making
/// that a different variant rather than a negative number means no arm of any
/// match on it can forget the case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compress {
    /// No compression header. The frame's payload starts at its packet id.
    Disabled,
    /// Payloads of at least `threshold` bytes are zlib compressed.
    ///
    /// "At least", not "more than": vanilla's encoder compresses when the
    /// payload length is `>= threshold`, and getting that boundary backwards
    /// makes every payload of exactly `threshold` bytes a protocol error
    /// against a real client.
    At { threshold: usize },
}

impl Compress {
    fn threshold(self) -> Option<usize> {
        match self {
            Self::Disabled => None,
            Self::At { threshold } => Some(threshold),
        }
    }
}

/// Reads frames out of a byte stream that arrives in arbitrary pieces.
///
/// The decoder owns its buffer because frame boundaries and read boundaries
/// have nothing to do with each other: one `read()` may deliver half a length
/// prefix, or nine frames and a fragment. Callers [`feed`](Self::feed) whatever
/// arrived and then drain [`next_frame`](Self::next_frame) until it returns
/// `Ok(None)`.
///
/// The buffer is bounded by one frame plus whatever the caller fed in its last
/// call, because a frame is never buffered without its length having passed
/// [`Limits::max_frame_len`] first. It is the caller's read chunk size that
/// bounds the second term, which is why [`crate::io`] reads in fixed chunks.
#[derive(Debug)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
    /// How much of `buffer` has been handed out already. The prefix is dropped
    /// on compaction rather than on every frame, so draining nine small frames
    /// is nine slices and one memmove instead of nine.
    start: usize,
    limits: Limits,
    compression: Compress,
}

impl FrameDecoder {
    pub fn new(limits: Limits) -> Self {
        Self {
            buffer: Vec::new(),
            start: 0,
            limits,
            compression: Compress::Disabled,
        }
    }

    /// Turn compression on or off, as a Set Compression packet says to.
    ///
    /// The switch takes effect on the *next* frame read, which is the same
    /// rule the encryption switch follows and for the same reason: the packet
    /// that changes the mode is itself sent in the old mode.
    pub fn set_compression(&mut self, compression: Compress) {
        self.compression = compression;
    }

    pub fn compression(&self) -> Compress {
        self.compression
    }

    /// Add bytes that arrived from the socket.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.compact();
        self.buffer.extend_from_slice(bytes);
    }

    /// How many more bytes the decoder wants before it can produce a frame.
    ///
    /// This exists for the encrypted read path, and it is the reason that path
    /// is safe. CFB8 state advances per byte, so a reader that decrypts a
    /// whole socket chunk speculatively has committed the cipher to bytes it
    /// may not be entitled to interpret yet — and at the moment encryption is
    /// switched on, the bytes past the Encryption Response are exactly that.
    /// [`crate::io`] instead decrypts a byte at a time until the length prefix
    /// is complete, then exactly the body in one call, so nothing beyond the
    /// current frame is ever fed through the cipher.
    ///
    /// [`Needed::Exactly(0)`](Needed::Exactly) also covers "the next call will
    /// return an error": a malformed or oversized prefix is reported by
    /// [`next_frame`](Self::next_frame), not here, so there is one place that
    /// decides what is wrong with a frame.
    pub fn needed(&self) -> Needed {
        let available = &self.buffer[self.start..];
        match read_var_int(available) {
            Ok((declared, prefix)) if declared > 0 => {
                let declared = declared as usize;
                if declared > self.limits.max_frame_len {
                    // `next_frame` is about to refuse it; asking for the bytes
                    // would be reading a body that will never be looked at.
                    return Needed::Exactly(0);
                }
                Needed::Exactly((prefix + declared).saturating_sub(available.len()))
            }
            // Zero, negative, or unreadable: all errors, all reported by
            // `next_frame`.
            Ok(_) => Needed::Exactly(0),
            Err(VarIntError::Incomplete { .. }) => Needed::Unknown,
            Err(_) => Needed::Exactly(0),
        }
    }

    /// How many unread bytes are buffered.
    pub fn buffered(&self) -> usize {
        self.buffer.len() - self.start
    }

    /// The next complete frame, or `Ok(None)` if more bytes are needed.
    ///
    /// `Ok(None)` is not a failure and does not consume anything; an error is
    /// fatal to the connection, because after a malformed length prefix there
    /// is no way to know where the next frame starts.
    pub fn next_frame(&mut self) -> Result<Option<Frame>, FrameError> {
        let available = &self.buffer[self.start..];
        let (declared, prefix_len) = match read_var_int(available) {
            Ok(pair) => pair,
            // Not enough bytes for the prefix yet. Anything else — a run of
            // continuation bytes, an overlong encoding — is fatal.
            Err(VarIntError::Incomplete { .. }) => return Ok(None),
            Err(error) => return Err(FrameError::Length(error)),
        };

        // Order matters: sign, then magnitude, then availability. Casting
        // first is how `-1` becomes eighteen quintillion.
        if declared < 0 {
            return Err(FrameError::NegativeLength { declared });
        }
        let declared = declared as usize;
        if declared > self.limits.max_frame_len {
            return Err(FrameError::TooLarge {
                declared,
                limit: self.limits.max_frame_len,
            });
        }
        if declared == 0 {
            return Err(FrameError::Empty);
        }
        if available.len() < prefix_len + declared {
            return Ok(None);
        }

        let payload_start = self.start + prefix_len;
        let payload_end = payload_start + declared;
        let frame = self.decode_payload(payload_start, payload_end)?;
        self.start = payload_end;
        Ok(Some(frame))
    }

    /// Everything after the length prefix, in whichever of the two forms is
    /// in effect.
    fn decode_payload(&self, start: usize, end: usize) -> Result<Frame, FrameError> {
        let payload = &self.buffer[start..end];
        let Some(threshold) = self.compression.threshold() else {
            return split_id(payload);
        };

        let (data_len, header_len) = read_var_int(payload).map_err(FrameError::DataLength)?;
        let rest = &payload[header_len..];

        if data_len < 0 {
            return Err(FrameError::NegativeDataLength { declared: data_len });
        }
        let data_len = data_len as usize;

        if data_len == 0 {
            // Form 3: the client says this was too small to compress. Check
            // that it was, or a client can opt out of compression entirely and
            // spend the server's bandwidth budget however it likes.
            if rest.len() >= threshold {
                return Err(FrameError::UncompressedOverThreshold {
                    len: rest.len(),
                    threshold,
                });
            }
            return split_id(rest);
        }

        // Form 4. Both directions of the threshold check are enforced; see the
        // module docs for why one is not enough.
        if data_len < threshold {
            return Err(FrameError::CompressedUnderThreshold {
                declared: data_len,
                threshold,
            });
        }
        if data_len > self.limits.max_decompressed_len {
            return Err(FrameError::DeclaredTooLarge {
                declared: data_len,
                limit: self.limits.max_decompressed_len,
            });
        }

        let plain = decompress(rest, data_len, self.limits.max_decompressed_len)?;
        split_id(&plain)
    }

    /// Drop the already-read prefix.
    ///
    /// Only when it is worth a memmove. Doing it on every frame turns a burst
    /// of small frames into a quadratic copy, which is a denial of service an
    /// attacker reaches by sending many small frames — the cheapest thing to
    /// send.
    fn compact(&mut self) {
        if self.start == 0 {
            return;
        }
        if self.start == self.buffer.len() {
            self.buffer.clear();
            self.start = 0;
            return;
        }
        if self.start >= 64 * 1024 || self.start * 2 >= self.buffer.len() {
            self.buffer.drain(..self.start);
            self.start = 0;
        }
    }
}

/// Split a decompressed payload into its packet id and body.
fn split_id(payload: &[u8]) -> Result<Frame, FrameError> {
    if payload.is_empty() {
        return Err(FrameError::Empty);
    }
    let (id, id_len) = read_var_int(payload).map_err(FrameError::PacketId)?;
    Ok(Frame {
        id,
        body: payload[id_len..].to_vec(),
    })
}

/// Decompress `input`, refusing to produce more than the frame promised.
///
/// The bound is `declared.min(absolute)` plus one byte, so that a stream which
/// would have gone further is caught producing the one extra byte rather than
/// inferred from a length that matched. The output buffer starts at a fixed
/// reserve and grows; the declared length is never an allocation size.
fn decompress(input: &[u8], declared: usize, absolute: usize) -> Result<Vec<u8>, FrameError> {
    let limit = declared.min(absolute);
    let mut out = Vec::with_capacity(DECOMPRESS_RESERVE.min(limit));
    let mut reader = ZlibDecoder::new(input).take(limit as u64 + 1);

    match reader.read_to_end(&mut out) {
        Ok(_) => {}
        Err(error) => return Err(FrameError::Corrupt(error.to_string())),
    }

    if out.len() > limit {
        return Err(FrameError::Bomb { limit });
    }
    if out.len() != declared {
        return Err(FrameError::LengthMismatch {
            declared,
            actual: out.len(),
        });
    }

    // `total_in` counts the compressed bytes the decompressor consumed. Fewer
    // than arrived means the frame carries a tail nothing looked at.
    let consumed = reader.into_inner().total_in() as usize;
    if consumed < input.len() {
        return Err(FrameError::TrailingBytes {
            unread: input.len() - consumed,
        });
    }
    Ok(out)
}

/// Writes frames in whichever form is in effect.
///
/// Symmetrical with [`FrameDecoder`] on purpose, including the threshold
/// boundary: what this encoder produces is what that decoder accepts, and the
/// `round_trips_through_the_decoder` tests hold the two together. That
/// agreement is *not* evidence that either matches Minecraft — see the crate
/// docs — which is what `tests/vanilla_status.rs` is for.
#[derive(Debug)]
pub struct FrameEncoder {
    limits: Limits,
    compression: Compress,
    level: Compression,
}

impl FrameEncoder {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            compression: Compress::Disabled,
            // Vanilla's default. Higher costs CPU on the server for bandwidth
            // the client already has; lower gives most of the CPU back and
            // most of the bandwidth away.
            level: Compression::new(6),
        }
    }

    pub fn set_compression(&mut self, compression: Compress) {
        self.compression = compression;
    }

    pub fn compression(&self) -> Compress {
        self.compression
    }

    /// Append the wire form of `frame` to `out`.
    pub fn encode(&self, frame: &Frame, out: &mut Vec<u8>) -> Result<(), FrameError> {
        let mut payload = Vec::with_capacity(frame.payload_len());
        write_var_int(frame.id, &mut payload);
        payload.extend_from_slice(&frame.body);

        match self.compression.threshold() {
            None => self.emit(&payload, out),
            Some(threshold) if payload.len() < threshold => {
                // Form 3: a data length of zero, then the payload as it is.
                let mut framed = Vec::with_capacity(payload.len() + 1);
                write_var_int(0, &mut framed);
                framed.extend_from_slice(&payload);
                self.emit(&framed, out)
            }
            Some(_) => {
                let mut encoder = ZlibEncoder::new(Vec::new(), self.level);
                std::io::Write::write_all(&mut encoder, &payload)
                    .and_then(|()| encoder.finish())
                    .map_err(|error| FrameError::Corrupt(error.to_string()))
                    .and_then(|compressed| {
                        let mut framed = Vec::with_capacity(compressed.len() + MAX_VAR_INT_LEN);
                        write_var_int(payload.len() as i32, &mut framed);
                        framed.extend_from_slice(&compressed);
                        self.emit(&framed, out)
                    })
            }
        }
    }

    /// Prefix `framed` with its length, refusing to emit an oversized frame.
    fn emit(&self, framed: &[u8], out: &mut Vec<u8>) -> Result<(), FrameError> {
        if framed.len() > self.limits.max_frame_len {
            return Err(FrameError::Oversize {
                len: framed.len(),
                limit: self.limits.max_frame_len,
            });
        }
        write_var_int(framed.len() as i32, out);
        out.extend_from_slice(framed);
        Ok(())
    }
}
