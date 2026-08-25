//! Packed integer arrays: N-bit values in a `[i64]`.
//!
//! # The 1.16 change, and why reading it wrong is silent
//!
//! Minecraft stores block state indices, biome indices and heightmaps as arrays
//! of small unsigned integers packed into 64-bit words. There are two packings,
//! and they are not distinguishable by looking at the bytes:
//!
//! * **Before 1.16** the values were a single bit stream. Value `i` began at bit
//!   `i * bits` and, when `bits` did not divide 64, ran on into the next long.
//!   The array was `ceil(len * bits / 64)` longs and no bit was wasted.
//! * **Since 1.16** no value spans a long boundary. Each long holds
//!   `floor(64 / bits)` values, and the top `64 - floor(64 / bits) * bits` bits
//!   of every long are padding that vanilla writes as zero. The array is
//!   `ceil(len / floor(64 / bits))` longs, which for a width that does not
//!   divide 64 is *more* longs than the old format needed.
//!
//! This module implements the modern format only. The distinction matters more
//! than it looks: a reader that assumes the old format against modern data does
//! not fail. It returns values — wrong ones, drawn from the right general
//! region of the array — and the failure surfaces later as terrain made of the
//! wrong blocks, or a heightmap of implausible numbers, with nothing pointing
//! back here. The two formats agree exactly whenever `bits` divides 64
//! (1, 2, 4, 8, 16, 32), so a test corpus made only of those widths cannot tell
//! them apart at all. See `tests/vanilla_corpus.rs`, which checks the packing
//! against arrays a real server wrote at nine bits.
//!
//! **What this module does not catch:** it validates the *length* of a long
//! array against the width and value count it is told to expect, and it will
//! not notice that the width itself is wrong when the resulting length happens
//! to agree. Width comes from the palette, so a palette misread lands here as a
//! plausible-looking array of wrong values.

/// The widest value this storage will pack.
///
/// Vanilla never exceeds this either: the global block-state palette on 1.21.1
/// needs 15 bits, and a `u32` value with a 32-bit width is already the whole
/// value space.
pub const MAX_BITS: u32 = 32;

/// Something wrong with a packed array, named rather than assumed away.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BitStorageError {
    /// A width this module will not pack. Zero is legal and means "every value
    /// is zero, and no longs are stored"; above [`MAX_BITS`] is not.
    UnsupportedWidth { bits: u32 },
    /// The long array is not the length the width and value count require.
    ///
    /// This is the error that catches a pre-1.16 array being handed to a
    /// modern reader, for every width that does not divide 64.
    WrongLongCount {
        bits: u32,
        values: usize,
        expected: usize,
        found: usize,
    },
}

impl std::fmt::Display for BitStorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedWidth { bits } => write!(
                f,
                "a packed array of {bits}-bit values is not something this can store; \
                 widths run from 0 to {MAX_BITS}"
            ),
            Self::WrongLongCount {
                bits,
                values,
                expected,
                found,
            } => write!(
                f,
                "{values} values at {bits} bits each need {expected} longs, but {found} \
                 were supplied; {} long{} out",
                expected.abs_diff(*found),
                if expected.abs_diff(*found) == 1 {
                    ""
                } else {
                    "s"
                }
            ),
        }
    }
}

impl std::error::Error for BitStorageError {}

/// How many values one long holds at this width.
///
/// Zero-width values are not stored at all, so the question does not arise and
/// the answer is zero rather than an infinity.
#[must_use]
pub const fn values_per_long(bits: u32) -> usize {
    match 64u32.checked_div(bits) {
        Some(per_long) => per_long as usize,
        None => 0,
    }
}

/// How many longs `values` values need at this width.
///
/// This is vanilla's arithmetic and not the obvious one: it divides by the
/// number of values that fit in a long, not by 64. `ceil(values * bits / 64)`
/// is the *pre-1.16* answer, and it is smaller for every width that does not
/// divide 64.
#[must_use]
pub const fn long_count(values: usize, bits: u32) -> usize {
    let per_long = values_per_long(bits);
    if per_long == 0 {
        0
    } else {
        values.div_ceil(per_long)
    }
}

/// An array of `len` unsigned values, each `bits` wide, packed into longs.
///
/// The backing array is `i64` rather than `u64` because that is what it is
/// everywhere it is exchanged: NBT has a long array tag and no unsigned one,
/// and the protocol writes the same signed longs. Keeping the sign here means
/// the cast happens once, at the arithmetic, instead of at every boundary where
/// a mistake would be invisible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BitStorage {
    bits: u32,
    len: usize,
    data: Vec<i64>,
}

impl BitStorage {
    /// A zeroed storage of `len` values at `bits` wide.
    ///
    /// # Panics
    ///
    /// If `bits` exceeds [`MAX_BITS`]. A width is chosen by this crate from a
    /// palette size, never by an operator or a peer, so a bad one is a bug
    /// here and not a condition to be reported. [`BitStorage::try_new`] is the
    /// checked form for the paths that do take a width from a file.
    #[must_use]
    pub fn new(bits: u32, len: usize) -> Self {
        Self::try_new(bits, len).expect("width chosen by this crate must be storable")
    }

    /// A zeroed storage, or [`BitStorageError::UnsupportedWidth`].
    pub fn try_new(bits: u32, len: usize) -> Result<Self, BitStorageError> {
        if bits > MAX_BITS {
            return Err(BitStorageError::UnsupportedWidth { bits });
        }
        Ok(Self {
            bits,
            len,
            data: vec![0; long_count(len, bits)],
        })
    }

    /// Wrap longs that came from somewhere else — a region file, a packet.
    ///
    /// The long count is checked, because it is the one property of the array
    /// that a caller cannot get wrong by accident and that catches the whole
    /// class of "this was packed under the other convention" mistakes.
    ///
    /// The padding bits are **not** checked. Vanilla writes them as zero and
    /// [`BitStorage::padding_is_zero`] asserts that of what this crate writes,
    /// but nothing in the format requires it of an incoming array, every read
    /// masks them off anyway, and refusing an otherwise-sound chunk over bits
    /// that carry no meaning would lose a player's world to a pedantry.
    pub fn from_longs(bits: u32, len: usize, data: Vec<i64>) -> Result<Self, BitStorageError> {
        if bits > MAX_BITS {
            return Err(BitStorageError::UnsupportedWidth { bits });
        }
        let expected = long_count(len, bits);
        if data.len() != expected {
            return Err(BitStorageError::WrongLongCount {
                bits,
                values: len,
                expected,
                found: data.len(),
            });
        }
        Ok(Self { bits, len, data })
    }

    /// The number of values, which is not the number of longs.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The width of one value in bits.
    #[must_use]
    pub const fn bits(&self) -> u32 {
        self.bits
    }

    /// The largest value this storage can hold.
    #[must_use]
    pub const fn max_value(&self) -> u32 {
        if self.bits == 0 {
            0
        } else if self.bits >= 32 {
            u32::MAX
        } else {
            (1u32 << self.bits) - 1
        }
    }

    /// The packed longs, as they would be written to disk or the wire.
    #[must_use]
    pub fn as_longs(&self) -> &[i64] {
        &self.data
    }

    /// The packed longs, taken.
    #[must_use]
    pub fn into_longs(self) -> Vec<i64> {
        self.data
    }

    const fn mask(&self) -> u64 {
        if self.bits >= 64 {
            u64::MAX
        } else {
            (1u64 << self.bits) - 1
        }
    }

    /// The value at `index`.
    ///
    /// # Panics
    ///
    /// If `index` is not less than [`BitStorage::len`], like any other index.
    #[must_use]
    pub fn get(&self, index: usize) -> u32 {
        assert!(
            index < self.len,
            "index {index} is past the end of a {}-value storage",
            self.len
        );
        if self.bits == 0 {
            return 0;
        }
        let per_long = values_per_long(self.bits);
        let cell = index / per_long;
        let offset = (index % per_long) as u32 * self.bits;
        (((self.data[cell] as u64) >> offset) & self.mask()) as u32
    }

    /// Overwrite the value at `index`.
    ///
    /// # Panics
    ///
    /// If `index` is out of range, or `value` does not fit in [`BitStorage::bits`].
    /// A value too large is a caller that did not resize first, which is a bug
    /// in this crate rather than bad input — and masking it off silently is
    /// precisely how a container ends up storing a different block than it was
    /// asked to.
    pub fn set(&mut self, index: usize, value: u32) {
        assert!(
            index < self.len,
            "index {index} is past the end of a {}-value storage",
            self.len
        );
        assert!(
            value <= self.max_value(),
            "{value} does not fit in {} bits; the storage needed resizing first",
            self.bits
        );
        if self.bits == 0 {
            return;
        }
        let per_long = values_per_long(self.bits);
        let cell = index / per_long;
        let offset = (index % per_long) as u32 * self.bits;
        let mask = self.mask();
        let word = self.data[cell] as u64;
        self.data[cell] =
            ((word & !(mask << offset)) | ((u64::from(value) & mask) << offset)) as i64;
    }

    /// Every value, in index order.
    pub fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        (0..self.len).map(|i| self.get(i))
    }

    /// The same values, repacked at a new width.
    ///
    /// # Panics
    ///
    /// If any stored value does not fit in `new_bits`. See
    /// [`BitStorage::checked_resized`].
    #[must_use]
    pub fn resized(&self, new_bits: u32) -> Self {
        self.checked_resized(new_bits)
            .unwrap_or_else(|| panic!("a value in this storage does not fit in {new_bits} bits"))
    }

    /// The same values, repacked at a new width, or `None` if a value would be
    /// lost.
    ///
    /// Widening always succeeds. Narrowing is allowed when it happens to fit,
    /// because a container that has had its palette rebuilt does narrow, and
    /// having the operation refuse rather than truncate is the whole point.
    #[must_use]
    pub fn checked_resized(&self, new_bits: u32) -> Option<Self> {
        let mut out = Self::try_new(new_bits, self.len).ok()?;
        let ceiling = out.max_value();
        for index in 0..self.len {
            let value = self.get(index);
            if value > ceiling {
                return None;
            }
            out.set(index, value);
        }
        Some(out)
    }

    /// Whether every bit that carries no value is zero.
    ///
    /// Two kinds of bit qualify: the high padding of every long, and the unused
    /// slots of the final long. Vanilla writes both as zero and a vanilla
    /// client reads the same longs Dust writes, so this being true of everything
    /// this crate produces is asserted rather than assumed. It is a statement
    /// about Dust's writer, not a requirement placed on other people's data —
    /// see [`BitStorage::from_longs`].
    #[must_use]
    pub fn padding_is_zero(&self) -> bool {
        if self.bits == 0 {
            return self.data.is_empty();
        }
        let per_long = values_per_long(self.bits);
        let used_bits = per_long as u32 * self.bits;
        let high_padding = if used_bits >= 64 {
            0
        } else {
            !((1u64 << used_bits) - 1)
        };
        for (cell, word) in self.data.iter().enumerate() {
            let word = *word as u64;
            if word & high_padding != 0 {
                return false;
            }
            // The slots of the last long beyond the final value.
            let first_slot = cell * per_long;
            if first_slot + per_long > self.len {
                let live = self.len.saturating_sub(first_slot);
                let live_bits = live as u32 * self.bits;
                let tail = if live_bits >= 64 {
                    0
                } else {
                    !((1u64 << live_bits) - 1)
                } & !high_padding;
                if word & tail != 0 {
                    return false;
                }
            }
        }
        true
    }
}
