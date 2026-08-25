//! DEFLATE decompression (RFC 1951), for reading zipped datapacks.
//!
//! # Why this is written out rather than pulled in
//!
//! A zipped datapack is a small, fixed subset of the format: stored and
//! deflated entries, no encryption, no zip64. Three hundred lines of RFC 1951
//! buys that subset with no new dependency, which matters here for a reason
//! beyond taste — a zip arrives from an operator's `datapacks/` directory,
//! which is untrusted input, and the guards that keep a malicious archive from
//! exhausting memory belong to whoever owns the decoder. See [`crate::zip`] for
//! the rest of them.
//!
//! # What proves this is right
//!
//! Not a round trip. This crate has no compressor, so there is no encode side
//! to agree with, and if there were, agreeing with it would prove nothing —
//! that is the lesson from the block-state extractor, where every one of 26,684
//! states round-tripped perfectly with every chest at the wrong id.
//!
//! Two things from outside sit beside it instead. Every entry carries a
//! **CRC-32 computed by the compressor**, which [`crate::zip`] checks on every
//! read, so a wrong byte anywhere fails against a number this code did not
//! produce. And the test suite decompresses archives written by the system
//! `zip` command and compares against the original files, so the bytes are
//! checked against an implementation nobody here wrote.

/// The longest Huffman code DEFLATE allows.
const MAX_BITS: usize = 15;

/// Base lengths for the length codes 257..=285.
const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
/// Extra bits read after each length code.
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
/// Base distances for the distance codes 0..=29.
const DISTANCE_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
/// Extra bits read after each distance code.
const DISTANCE_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
/// The order the code-length code lengths are written in a dynamic block.
const CODE_LENGTH_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// Why a deflate stream could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InflateError {
    /// The stream ended in the middle of something.
    Truncated,
    /// Block type 3, which RFC 1951 reserves and nothing emits.
    ReservedBlockType,
    /// A stored block's length and its one's-complement disagree, which means
    /// the stream is not what it claims to be.
    StoredLengthMismatch,
    /// A Huffman table that does not describe a prefix code.
    MalformedHuffmanTable,
    /// A code that is not in the table it was read from.
    InvalidCode,
    /// A back-reference pointing before the start of the output.
    DistanceTooFar { distance: usize, produced: usize },
    /// The output exceeded the caller's limit. See [`inflate`].
    TooLarge { limit: usize },
}

impl std::fmt::Display for InflateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated => f.write_str("the compressed data ends part-way through"),
            Self::ReservedBlockType => {
                f.write_str("uses reserved block type 3, so it is not a deflate stream")
            }
            Self::StoredLengthMismatch => f.write_str(
                "has an uncompressed block whose length and check field disagree, \
                 so the data is corrupt",
            ),
            Self::MalformedHuffmanTable => {
                f.write_str("has a Huffman table that is not a valid prefix code")
            }
            Self::InvalidCode => f.write_str("contains a symbol that is not in its own table"),
            Self::DistanceTooFar { distance, produced } => write!(
                f,
                "refers back {distance} bytes when only {produced} have been produced, \
                 so the data is corrupt"
            ),
            Self::TooLarge { limit } => write!(
                f,
                "expands past the {limit}-byte limit Dust allows for one file in a pack"
            ),
        }
    }
}

impl std::error::Error for InflateError {}

/// Decompress a raw DEFLATE stream.
///
/// `limit` caps the output. A zip entry declares its uncompressed size in the
/// central directory, and [`crate::zip`] passes that, so a lying header is
/// caught here rather than by the machine running out of memory — which is the
/// whole of a zip bomb.
pub fn inflate(input: &[u8], limit: usize) -> Result<Vec<u8>, InflateError> {
    let mut bits = BitReader::new(input);
    let mut out: Vec<u8> = Vec::with_capacity(limit.min(1 << 20));

    loop {
        let last = bits.take(1)? == 1;
        match bits.take(2)? {
            0 => stored(&mut bits, &mut out, limit)?,
            1 => {
                let (literals, distances) = fixed_tables();
                block(&mut bits, &mut out, &literals, &distances, limit)?;
            }
            2 => {
                let (literals, distances) = dynamic_tables(&mut bits)?;
                block(&mut bits, &mut out, &literals, &distances, limit)?;
            }
            _ => return Err(InflateError::ReservedBlockType),
        }
        if last {
            return Ok(out);
        }
    }
}

/// Reads bits least-significant-first within each byte, as DEFLATE does.
struct BitReader<'a> {
    data: &'a [u8],
    /// Index of the next byte to pull into the accumulator.
    at: usize,
    accumulator: u32,
    held: u32,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            at: 0,
            accumulator: 0,
            held: 0,
        }
    }

    fn take(&mut self, count: u32) -> Result<u32, InflateError> {
        while self.held < count {
            let byte = *self.data.get(self.at).ok_or(InflateError::Truncated)?;
            self.at += 1;
            self.accumulator |= u32::from(byte) << self.held;
            self.held += 8;
        }
        let value = self.accumulator & ((1u32 << count) - 1);
        self.accumulator >>= count;
        self.held -= count;
        Ok(value)
    }

    /// Drop the rest of the current byte, as a stored block requires.
    fn align(&mut self) {
        let extra = self.held % 8;
        self.accumulator >>= extra;
        self.held -= extra;
    }

    /// Take whole bytes, using anything already in the accumulator first.
    fn bytes(&mut self, count: usize, out: &mut Vec<u8>) -> Result<(), InflateError> {
        for _ in 0..count {
            if self.held >= 8 {
                out.push((self.accumulator & 0xff) as u8);
                self.accumulator >>= 8;
                self.held -= 8;
            } else {
                let byte = *self.data.get(self.at).ok_or(InflateError::Truncated)?;
                self.at += 1;
                out.push(byte);
            }
        }
        Ok(())
    }
}

/// A canonical Huffman decoding table, in the counts-and-symbols form used by
/// zlib's reference `puff`: `count[n]` codes of length `n`, and `symbol` in
/// code order. Decoding walks one bit at a time rather than building a lookup
/// table, which is slower per symbol and much shorter to get right.
#[derive(Debug)]
struct Huffman {
    count: [u16; MAX_BITS + 1],
    symbol: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Result<Self, InflateError> {
        let mut count = [0u16; MAX_BITS + 1];
        for &length in lengths {
            count[length as usize] += 1;
        }
        // Length 0 means "this symbol does not appear", not a zero-bit code.
        count[0] = 0;

        // A prefix code is complete when the Kraft sum is exactly 1. Left over
        // is an over-subscribed table, which decodes garbage rather than
        // failing, so it is checked here.
        let mut left = 1i32;
        for &at_length in count.iter().take(MAX_BITS + 1).skip(1) {
            left <<= 1;
            left -= i32::from(at_length);
            if left < 0 {
                return Err(InflateError::MalformedHuffmanTable);
            }
        }

        let mut offsets = [0u16; MAX_BITS + 2];
        for length in 1..=MAX_BITS {
            offsets[length + 1] = offsets[length] + count[length];
        }
        let mut symbol = vec![0u16; lengths.len()];
        for (index, &length) in lengths.iter().enumerate() {
            if length != 0 {
                symbol[offsets[length as usize] as usize] = index as u16;
                offsets[length as usize] += 1;
            }
        }
        Ok(Self { count, symbol })
    }

    fn decode(&self, bits: &mut BitReader<'_>) -> Result<u16, InflateError> {
        let mut code = 0i32;
        let mut first = 0i32;
        let mut index = 0i32;
        for length in 1..=MAX_BITS {
            code |= bits.take(1)? as i32;
            let count = i32::from(self.count[length]);
            if code - count < first {
                return Ok(self.symbol[(index + (code - first)) as usize]);
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err(InflateError::InvalidCode)
    }
}

fn stored(bits: &mut BitReader<'_>, out: &mut Vec<u8>, limit: usize) -> Result<(), InflateError> {
    bits.align();
    let length = bits.take(16)? as usize;
    let check = bits.take(16)? as usize;
    if length != (!check) & 0xffff {
        return Err(InflateError::StoredLengthMismatch);
    }
    if out.len() + length > limit {
        return Err(InflateError::TooLarge { limit });
    }
    bits.bytes(length, out)
}

/// The fixed tables of RFC 1951 section 3.2.6, built on demand.
///
/// Rebuilding them per block rather than caching them in a `static` is a few
/// microseconds against reading a file off a disk, and it keeps this module
/// free of lazily-initialised global state.
fn fixed_tables() -> (Huffman, Huffman) {
    let mut literal_lengths = [0u8; 288];
    for (symbol, length) in literal_lengths.iter_mut().enumerate() {
        *length = match symbol {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    (
        Huffman::new(&literal_lengths).expect("the fixed literal table is a valid prefix code"),
        Huffman::new(&[5u8; 30]).expect("the fixed distance table is a valid prefix code"),
    )
}

fn dynamic_tables(bits: &mut BitReader<'_>) -> Result<(Huffman, Huffman), InflateError> {
    let literal_count = bits.take(5)? as usize + 257;
    let distance_count = bits.take(5)? as usize + 1;
    let code_length_count = bits.take(4)? as usize + 4;
    if literal_count > 286 || distance_count > 30 {
        return Err(InflateError::MalformedHuffmanTable);
    }

    let mut code_lengths = [0u8; 19];
    for &position in CODE_LENGTH_ORDER.iter().take(code_length_count) {
        code_lengths[position] = bits.take(3)? as u8;
    }
    let code_table = Huffman::new(&code_lengths)?;

    let total = literal_count + distance_count;
    let mut lengths = vec![0u8; total];
    let mut at = 0usize;
    while at < total {
        let symbol = code_table.decode(bits)?;
        match symbol {
            0..=15 => {
                lengths[at] = symbol as u8;
                at += 1;
            }
            16 => {
                // Repeat the previous length. With nothing before it there is
                // no previous length, and a stream that asks for one is corrupt.
                if at == 0 {
                    return Err(InflateError::MalformedHuffmanTable);
                }
                let previous = lengths[at - 1];
                let repeat = 3 + bits.take(2)? as usize;
                fill(&mut lengths, &mut at, previous, repeat)?;
            }
            17 => {
                let repeat = 3 + bits.take(3)? as usize;
                fill(&mut lengths, &mut at, 0, repeat)?;
            }
            18 => {
                let repeat = 11 + bits.take(7)? as usize;
                fill(&mut lengths, &mut at, 0, repeat)?;
            }
            _ => return Err(InflateError::InvalidCode),
        }
    }

    Ok((
        Huffman::new(&lengths[..literal_count])?,
        Huffman::new(&lengths[literal_count..])?,
    ))
}

fn fill(lengths: &mut [u8], at: &mut usize, value: u8, repeat: usize) -> Result<(), InflateError> {
    if *at + repeat > lengths.len() {
        return Err(InflateError::MalformedHuffmanTable);
    }
    lengths[*at..*at + repeat].fill(value);
    *at += repeat;
    Ok(())
}

fn block(
    bits: &mut BitReader<'_>,
    out: &mut Vec<u8>,
    literals: &Huffman,
    distances: &Huffman,
    limit: usize,
) -> Result<(), InflateError> {
    loop {
        let symbol = literals.decode(bits)?;
        match symbol {
            0..=255 => {
                if out.len() >= limit {
                    return Err(InflateError::TooLarge { limit });
                }
                out.push(symbol as u8);
            }
            256 => return Ok(()),
            257..=285 => {
                let index = symbol as usize - 257;
                let length =
                    LENGTH_BASE[index] as usize + bits.take(LENGTH_EXTRA[index].into())? as usize;

                let distance_symbol = distances.decode(bits)? as usize;
                if distance_symbol >= DISTANCE_BASE.len() {
                    return Err(InflateError::InvalidCode);
                }
                let distance = DISTANCE_BASE[distance_symbol] as usize
                    + bits.take(DISTANCE_EXTRA[distance_symbol].into())? as usize;

                if distance > out.len() {
                    return Err(InflateError::DistanceTooFar {
                        distance,
                        produced: out.len(),
                    });
                }
                if out.len() + length > limit {
                    return Err(InflateError::TooLarge { limit });
                }
                // Byte at a time, because the copy is allowed to overlap its
                // own output — that is how a run of one repeated byte is
                // encoded, and a block copy would read the wrong bytes.
                let start = out.len() - distance;
                for offset in 0..length {
                    out.push(out[start + offset]);
                }
            }
            _ => return Err(InflateError::InvalidCode),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stored (uncompressed) block: final, type 0, then LEN/NLEN and bytes.
    fn stored_block(payload: &[u8]) -> Vec<u8> {
        let mut out = vec![0x01];
        let len = payload.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn a_stored_block_comes_back_unchanged() {
        let payload = b"{\"values\": []}";
        assert_eq!(inflate(&stored_block(payload), 1024).unwrap(), payload);
    }

    #[test]
    fn an_empty_stream_is_an_empty_result() {
        assert_eq!(inflate(&stored_block(b""), 16).unwrap(), b"");
    }

    #[test]
    fn a_corrupt_stored_length_is_caught() {
        let mut block = stored_block(b"abc");
        block[3] ^= 0xff;
        assert_eq!(
            inflate(&block, 1024),
            Err(InflateError::StoredLengthMismatch)
        );
    }

    #[test]
    fn a_truncated_stream_fails_rather_than_returning_what_it_had() {
        let block = stored_block(b"abcdef");
        assert_eq!(
            inflate(&block[..block.len() - 2], 1024),
            Err(InflateError::Truncated)
        );
    }

    #[test]
    fn reserved_block_type_three_is_refused() {
        // final=1, type=3 -> 0b111
        assert_eq!(inflate(&[0x07], 16), Err(InflateError::ReservedBlockType));
    }

    #[test]
    fn the_limit_stops_a_stored_block_that_would_blow_past_it() {
        assert_eq!(
            inflate(&stored_block(&[0u8; 100]), 10),
            Err(InflateError::TooLarge { limit: 10 })
        );
    }

    #[test]
    fn the_fixed_tables_are_a_complete_prefix_code() {
        // If they were not, `fixed_tables` would panic on its own expect —
        // which is the assertion. This states it as a test so the reason the
        // expect is safe is written down somewhere that runs.
        let (literals, distances) = fixed_tables();
        assert_eq!(literals.symbol.len(), 288);
        assert_eq!(distances.symbol.len(), 30);
    }

    #[test]
    fn an_over_subscribed_table_is_rejected_rather_than_decoding_garbage() {
        // Three symbols claiming one bit each cannot be a prefix code.
        assert_eq!(
            Huffman::new(&[1, 1, 1]).unwrap_err(),
            InflateError::MalformedHuffmanTable
        );
    }
}
