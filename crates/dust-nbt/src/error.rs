//! What can go wrong reading or writing binary NBT.
//!
//! Every variant names the offset it happened at and what was expected there.
//! That is not politeness: this parser is reachable from a packet an attacker
//! chose the bytes of, so its errors end up in an operator's log, and "invalid
//! NBT" in a log is a line nobody can act on. An offset and a tag name turn the
//! same line into `xxd -s <offset>`.

use std::fmt;

use crate::mutf8::{Mutf8Error, StringTooLong};
use crate::tag::TagType;

/// A read or write that could not be completed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The input ended in the middle of something.
    UnexpectedEnd {
        offset: usize,
        needed: usize,
        available: usize,
        /// What was being read — `"TAG_Int payload"`, `"a list header"`.
        while_reading: &'static str,
    },
    /// A tag id byte that is not one of the thirteen.
    UnknownTagId { offset: usize, id: u8 },
    /// A `TAG_End` where a value was required — as the type of a compound
    /// field, or as the element type of a list that claims to have elements.
    UnexpectedEndTag {
        offset: usize,
        context: &'static str,
    },
    /// A length prefix that is negative. Every length in NBT is a signed
    /// `i32` or `i16`, so this is representable and has to be refused.
    NegativeLength {
        offset: usize,
        length: i32,
        tag: TagType,
    },
    /// A length prefix larger than the input could possibly satisfy.
    ///
    /// This is the allocation guard. It fires *before* any capacity is
    /// reserved, so a header claiming four billion elements costs a comparison
    /// rather than sixteen gigabytes.
    LengthExceedsInput {
        offset: usize,
        /// Elements or bytes, as the tag counts them.
        claimed: usize,
        /// The smallest number of bytes those elements could occupy.
        minimum_bytes: usize,
        available: usize,
        tag: TagType,
    },
    /// Nesting deeper than the configured limit.
    TooDeep { offset: usize, limit: usize },
    /// The decoded document would occupy more memory than the caller allowed.
    ///
    /// Distinct from [`Error::LengthExceedsInput`]: this fires on input that is
    /// entirely honest about its own size and simply expands. A megabyte of
    /// one-byte empty compounds inside a list is a megabyte of input and tens
    /// of megabytes of tags.
    HeapBudgetExceeded {
        offset: usize,
        used: usize,
        limit: usize,
    },
    /// A list element whose type is not the list's declared type.
    ///
    /// Unreachable from the reader, which reads each element *as* the declared
    /// type and so cannot produce a mismatch; it is reachable from the writer,
    /// which is handed a [`crate::List`] a caller built.
    HeterogeneousList {
        index: usize,
        expected: TagType,
        found: TagType,
    },
    /// A string that is not modified UTF-8.
    Utf8 {
        /// Offset of the string payload in the document, so that the payload
        /// offset inside the inner error can be added to it.
        offset: usize,
        source: Mutf8Error,
    },
    /// A string too long for its `u16` length prefix.
    StringTooLong(StringTooLong),
    /// An array with more than `i32::MAX` elements, whose length cannot be
    /// written. Only reachable on a 64-bit host with a great deal of memory.
    ArrayTooLong { len: usize, tag: TagType },
    /// Bytes left over after a complete document.
    ///
    /// Only reported by the `*_exact` entry points. The plain ones return how
    /// far they read, because a region file packs a chunk into a slot with
    /// padding after it and the padding is not an error.
    TrailingBytes { offset: usize, remaining: usize },
    /// Decompression failed, or produced more than the caller allowed.
    Compression(crate::compression::CompressionError),
}

impl Error {
    /// Where in the document the trouble is, in bytes from its start.
    ///
    /// `None` for the two errors that are about a value in memory rather than
    /// a position in a document.
    pub fn offset(&self) -> Option<usize> {
        Some(match self {
            Self::UnexpectedEnd { offset, .. }
            | Self::UnknownTagId { offset, .. }
            | Self::UnexpectedEndTag { offset, .. }
            | Self::NegativeLength { offset, .. }
            | Self::LengthExceedsInput { offset, .. }
            | Self::TooDeep { offset, .. }
            | Self::HeapBudgetExceeded { offset, .. }
            | Self::TrailingBytes { offset, .. } => *offset,
            Self::Utf8 { offset, source } => *offset + source.offset(),
            Self::HeterogeneousList { .. }
            | Self::StringTooLong(_)
            | Self::ArrayTooLong { .. }
            | Self::Compression(_) => return None,
        })
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEnd {
                offset,
                needed,
                available,
                while_reading,
            } => write!(
                f,
                "at byte {offset}: reading {while_reading} needs {needed} bytes but only \
                 {available} remain"
            ),
            Self::UnknownTagId { offset, id } => write!(
                f,
                "at byte {offset}: tag id {id} is not one of the thirteen (0-12)"
            ),
            Self::UnexpectedEndTag { offset, context } => write!(
                f,
                "at byte {offset}: TAG_End appears as {context}, where it has no meaning"
            ),
            Self::NegativeLength {
                offset,
                length,
                tag,
            } => write!(
                f,
                "at byte {offset}: {tag} declares a length of {length}; lengths are signed \
                 in this format and a negative one is malformed"
            ),
            Self::LengthExceedsInput {
                offset,
                claimed,
                minimum_bytes,
                available,
                tag,
            } => write!(
                f,
                "at byte {offset}: {tag} claims {claimed} entries, which need at least \
                 {minimum_bytes} bytes, and {available} remain in the input"
            ),
            Self::TooDeep { offset, limit } => write!(
                f,
                "at byte {offset}: nesting deeper than {limit}, the configured limit"
            ),
            Self::HeapBudgetExceeded {
                offset,
                used,
                limit,
            } => write!(
                f,
                "at byte {offset}: the decoded document has reached {used} bytes and the \
                 limit is {limit}"
            ),
            Self::HeterogeneousList {
                index,
                expected,
                found,
            } => write!(
                f,
                "list element {index} is {found} but the list declares {expected}"
            ),
            Self::Utf8 { offset, source } => {
                write!(f, "in the string at byte {offset}: {source}")
            }
            Self::StringTooLong(inner) => write!(f, "{inner}"),
            Self::ArrayTooLong { len, tag } => write!(
                f,
                "{tag} has {len} elements and its length is written as an i32, so at most \
                 {} can be written",
                i32::MAX
            ),
            Self::TrailingBytes { offset, remaining } => write!(
                f,
                "the document ends at byte {offset} and {remaining} bytes follow it"
            ),
            Self::Compression(inner) => write!(f, "{inner}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Utf8 { source, .. } => Some(source),
            Self::StringTooLong(inner) => Some(inner),
            Self::Compression(inner) => Some(inner),
            _ => None,
        }
    }
}

impl From<StringTooLong> for Error {
    fn from(value: StringTooLong) -> Self {
        Self::StringTooLong(value)
    }
}

impl From<crate::compression::CompressionError> for Error {
    fn from(value: crate::compression::CompressionError) -> Self {
        Self::Compression(value)
    }
}

/// The result of anything in this crate that touches bytes.
pub type Result<T> = std::result::Result<T, Error>;
