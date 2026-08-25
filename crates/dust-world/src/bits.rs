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

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic value that fits in `bits`, varied enough that a value
    /// landing in the wrong slot changes it.
    ///
    /// Not random: a test that fails on one seed in fifty is a test nobody
    /// trusts. The multiplier is Knuth's and spreads consecutive indices across
    /// the whole range, which is what makes a neighbouring-slot mistake show up
    /// rather than being masked by two adjacent values happening to agree.
    fn sample(index: usize, bits: u32) -> u32 {
        if bits == 0 {
            return 0;
        }
        let span = 1u64 << bits;
        ((index as u64).wrapping_mul(2_654_435_761) % span) as u32
    }

    /// The long count worked out by placing the values one at a time, which
    /// shares no arithmetic with the closed form it is checking.
    fn long_count_by_placement(values: usize, bits: u32) -> usize {
        if bits == 0 || values == 0 {
            return 0;
        }
        let per_long = (64 / bits) as usize;
        let mut highest = 0;
        for index in 0..values {
            highest = highest.max(index / per_long);
        }
        highest + 1
    }

    #[test]
    fn every_width_from_one_to_thirty_two_round_trips() {
        // Every width, not a sample of them. The interesting ones are the
        // widths that do not divide 64 -- everything else agrees with the
        // pre-1.16 packing and proves nothing -- and there is no reason to
        // guess which of those matter when all thirty-two take milliseconds.
        for bits in 1..=MAX_BITS {
            for len in [1usize, 2, 63, 64, 65, 256, 4095, 4096, 4097] {
                let mut storage = BitStorage::new(bits, len);
                assert_eq!(storage.bits(), bits);
                assert_eq!(storage.len(), len);
                for index in 0..len {
                    storage.set(index, sample(index, bits));
                }
                for index in 0..len {
                    assert_eq!(
                        storage.get(index),
                        sample(index, bits),
                        "{bits} bits, {len} values, index {index}"
                    );
                }
                assert!(
                    storage.padding_is_zero(),
                    "{bits} bits, {len} values: a vanilla client reads these longs"
                );
            }
        }
    }

    #[test]
    fn the_long_count_matches_the_arithmetic_vanilla_uses() {
        // This is the off-by-one's home. `long_count` divides by the number of
        // values that fit in a long; the check divides nothing and places every
        // value by hand.
        for bits in 1..=MAX_BITS {
            for len in [
                0usize, 1, 2, 3, 7, 63, 64, 65, 127, 255, 256, 257, 4095, 4096, 4097,
            ] {
                assert_eq!(
                    long_count(len, bits),
                    long_count_by_placement(len, bits),
                    "{len} values at {bits} bits"
                );
                assert_eq!(
                    BitStorage::new(bits, len).as_longs().len(),
                    long_count(len, bits),
                    "{len} values at {bits} bits: the array is not the length it claims"
                );
            }
        }
    }

    #[test]
    fn the_pre_1_16_packing_is_a_different_length_exactly_where_it_matters() {
        // The two conventions agree for every width that divides 64 and differ
        // for every width that does not. That is why a corpus of 4-bit and
        // 8-bit arrays cannot tell them apart, and why the nine-bit heightmap
        // in a real chunk is the best evidence there is.
        for bits in 1..=MAX_BITS {
            let values = 4096usize;
            let modern = long_count(values, bits);
            let pre_1_16 = (values * bits as usize).div_ceil(64);
            if 64 % bits == 0 {
                assert_eq!(modern, pre_1_16, "{bits} bits divides 64");
            } else {
                assert!(
                    modern > pre_1_16,
                    "{bits} bits: modern {modern}, pre-1.16 {pre_1_16}"
                );
            }
        }
    }

    #[test]
    fn from_longs_refuses_an_array_packed_the_old_way() {
        // 256 nine-bit values: 37 longs now, 36 before 1.16. Handing the old
        // array to the modern reader has to be an error, because every value it
        // would return is plausible.
        let modern = long_count(256, 9);
        assert_eq!(modern, 37);
        let old = (256 * 9usize).div_ceil(64);
        assert_eq!(old, 36);

        let err = BitStorage::from_longs(9, 256, vec![0; old]).expect_err("36 longs is not 37");
        assert_eq!(
            err,
            BitStorageError::WrongLongCount {
                bits: 9,
                values: 256,
                expected: 37,
                found: 36,
            }
        );
        assert!(err.to_string().contains("37 longs"), "{err}");
        assert!(BitStorage::from_longs(9, 256, vec![0; modern]).is_ok());
    }

    #[test]
    fn a_width_past_the_ceiling_is_named_rather_than_wrapped() {
        let err = BitStorage::try_new(33, 16).expect_err("33 bits is too wide");
        assert_eq!(err, BitStorageError::UnsupportedWidth { bits: 33 });
        assert!(err.to_string().contains("33"), "{err}");
    }

    #[test]
    fn setting_one_value_leaves_its_neighbours_alone() {
        // The bug this catches is a mask built from the wrong width, which
        // clears bits belonging to the value in the next slot -- and only ever
        // shows up when the neighbour is not already zero.
        for bits in 1..=MAX_BITS {
            let mut storage = BitStorage::new(bits, 200);
            let max = storage.max_value();
            for index in 0..200 {
                storage.set(index, max);
            }
            for index in 0..200 {
                storage.set(index, 0);
                for other in 0..200 {
                    let expected = if other <= index { 0 } else { max };
                    assert_eq!(
                        storage.get(other),
                        expected,
                        "{bits} bits: writing {index} disturbed {other}"
                    );
                }
            }
        }
    }

    #[test]
    fn padding_is_zero_notices_a_dirty_long() {
        // The positive control. Without it this guard could be a function that
        // returns true, and every test that asserts it would still pass.
        let mut storage = BitStorage::new(9, 256);
        for index in 0..256 {
            storage.set(index, sample(index, 9));
        }
        assert!(storage.padding_is_zero());

        // 9 bits x 7 values per long = 63 used bits, so bit 63 is padding.
        let mut longs = storage.as_longs().to_vec();
        longs[0] |= 1i64 << 63;
        let dirty = BitStorage::from_longs(9, 256, longs).expect("still 37 longs");
        assert!(
            !dirty.padding_is_zero(),
            "bit 63 of a 9-bit long is padding"
        );
        for index in 0..256 {
            assert_eq!(dirty.get(index), sample(index, 9), "and it changes nothing");
        }

        // The unused slots of the final long count too. 256 values at 7 per
        // long fill 36 longs and put 4 in the 37th, leaving 3 slots spare.
        let mut longs = storage.as_longs().to_vec();
        longs[36] |= 1i64 << (4 * 9);
        let dirty = BitStorage::from_longs(9, 256, longs).expect("still 37 longs");
        assert!(
            !dirty.padding_is_zero(),
            "slot 4 of the last long is unused"
        );
    }

    #[test]
    fn resizing_preserves_every_value_at_every_width() {
        for bits in 1..MAX_BITS {
            let mut storage = BitStorage::new(bits, 4096);
            for index in 0..4096 {
                storage.set(index, sample(index, bits));
            }
            for wider in bits + 1..=MAX_BITS {
                let grown = storage.resized(wider);
                assert_eq!(grown.bits(), wider);
                assert_eq!(grown.len(), 4096);
                assert_eq!(grown.as_longs().len(), long_count(4096, wider));
                assert!(grown.padding_is_zero());
                for index in 0..4096 {
                    assert_eq!(
                        grown.get(index),
                        sample(index, bits),
                        "{bits} -> {wider} bits at index {index}"
                    );
                }
                // And back again, since nothing grew past the old ceiling.
                let shrunk = grown.checked_resized(bits).expect("nothing was widened");
                assert_eq!(shrunk, storage);
            }
        }
    }

    #[test]
    fn narrowing_refuses_rather_than_truncating() {
        let mut storage = BitStorage::new(8, 64);
        storage.set(0, 255);
        assert_eq!(storage.checked_resized(7), None);
        storage.set(0, 127);
        assert!(storage.checked_resized(7).is_some());
    }

    #[test]
    fn zero_bits_is_a_run_of_zeroes_with_no_longs() {
        // The single-valued palette's storage. It has a length and no array,
        // and a container asks it for values 4096 times.
        let storage = BitStorage::new(0, 4096);
        assert_eq!(storage.len(), 4096);
        assert_eq!(storage.bits(), 0);
        assert!(storage.as_longs().is_empty());
        assert_eq!(storage.max_value(), 0);
        assert!(storage.iter().all(|v| v == 0));
        assert!(storage.padding_is_zero());
    }

    #[test]
    fn iterating_agrees_with_indexing() {
        for bits in [1u32, 3, 5, 9, 17, 32] {
            let mut storage = BitStorage::new(bits, 1000);
            for index in 0..1000 {
                storage.set(index, sample(index, bits));
            }
            let collected: Vec<u32> = storage.iter().collect();
            assert_eq!(collected.len(), 1000);
            for (index, value) in collected.into_iter().enumerate() {
                assert_eq!(value, storage.get(index), "{bits} bits");
            }
        }
    }

    #[test]
    #[should_panic(expected = "does not fit in 4 bits")]
    fn a_value_too_wide_panics_rather_than_being_masked() {
        BitStorage::new(4, 16).set(0, 16);
    }

    #[test]
    #[should_panic(expected = "past the end")]
    fn an_index_past_the_end_panics() {
        let _ = BitStorage::new(4, 16).get(16);
    }
}
