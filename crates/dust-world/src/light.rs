//! Per-section light arrays: 4096 four-bit levels, packed two to a byte.
//!
//! Every chunk section carries two of these — sky light and block light — and
//! both are stored and sent as 2048 bytes, one nibble per block cell. This
//! module is that array and nothing more, which is a decision worth stating:
//! the hard part of light is not storing it.
//!
//! # The boundary with the light engine
//!
//! A real light system is a propagation algorithm: a placed torch raises the
//! levels of its neighbours, each neighbour raises *its* neighbours one lower,
//! values fade across section borders, and a half-written section must never
//! be read by a pass that assumes its neighbours were consistent. None of
//! that is here. What is here is the storage those passes will read and
//! write — get, set, and the byte form that goes into a chunk file and comes
//! back out of one — so that when the engine lands, it lands on data
//! structures whose encoding is already pinned against the format, and not on
//! `Vec<u8>` with a convention remembered in a comment.
//!
//! The split is deliberate rather than deferred. Propagation correctness is
//! a property of a *schedule* of updates across many sections; nothing about
//! it can be checked on one array in isolation, and pretending otherwise by
//! putting a "propagate" method next to `set` would invite callers into a
//! cross-section write this type cannot make safe. Encoding, by contrast, is
//! checkable right here, and so it is checked exhaustively below.
//!
//! **What this does not catch:** a wrong level stored faithfully. Zeroes are
//! legal, fifteen is legal, and a section in perfect darkness that should be
//! sunlit reads back exactly as written.

/// Block cells per section: sixteen cubed.
pub const CELLS: usize = 4096;

/// Bytes per section once the cells are nibble-packed.
pub const BYTES: usize = CELLS / 2;

/// Something wrong with a byte run offered as a section's light.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LightArrayError {
    /// The array is not the 2048 bytes a section's light occupies.
    WrongLength {
        /// How many bytes a section's light packs into.
        expected: usize,
        /// How many bytes arrived.
        found: usize,
    },
}

impl std::fmt::Display for LightArrayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongLength { expected, found } => write!(
                f,
                "a section's light packs {CELLS} four-bit levels into {expected} bytes, but \
                 {found} bytes were supplied"
            ),
        }
    }
}

impl std::error::Error for LightArrayError {}

/// The light levels of one section: 4096 nibbles in 2048 bytes.
///
/// Levels run 0 (dark) to 15 (full), one per block cell, indexed in the same
/// order as the section's block states — `y` slowest, then `z`, then `x` —
/// because every question the game asks ("what is the light at the block next
/// to this one") is asked about pairs of blocks and light in the same breath,
/// and one index arithmetic shared by both is one arithmetic to get right.
#[derive(Clone, PartialEq, Eq)]
pub struct LightArray {
    nibbles: Box<[u8; BYTES]>,
}

impl std::fmt::Debug for LightArray {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Two kilobytes of nibbles printed in full would bury the one byte a
        // failure is about. The prefix identifies the array; equality does
        // the rest when a test needs it.
        f.debug_struct("LightArray")
            .field("first_bytes", &&self.nibbles[..8])
            .finish_non_exhaustive()
    }
}

impl Default for LightArray {
    fn default() -> Self {
        Self::new()
    }
}

impl LightArray {
    /// An array of zeroes: no light anywhere in the section.
    #[must_use]
    pub fn new() -> Self {
        Self {
            nibbles: Box::new([0; BYTES]),
        }
    }

    /// An array where every cell holds `level`.
    ///
    /// # Panics
    ///
    /// If `level` exceeds 15. A level needs four bits, and masking an
    /// out-of-range level down would store darkness for a caller who asked
    /// for full brightness.
    #[must_use]
    pub fn filled(level: u8) -> Self {
        assert!(level < 16, "{level} does not fit in four bits");
        Self {
            nibbles: Box::new([level | level << 4; BYTES]),
        }
    }

    /// Rebuild from the 2048-byte array a chunk file holds.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, LightArrayError> {
        let found = bytes.len();
        let Ok(nibbles) = <&[u8; BYTES]>::try_from(bytes) else {
            return Err(LightArrayError::WrongLength {
                expected: BYTES,
                found,
            });
        };
        Ok(Self {
            nibbles: Box::new(*nibbles),
        })
    }

    /// The packed bytes, as a chunk file holds them.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; BYTES] {
        &self.nibbles
    }

    /// The packed bytes, taken.
    #[must_use]
    pub fn into_bytes(self) -> Box<[u8; BYTES]> {
        self.nibbles
    }

    /// The index of a cell. `y` varies slowest and `x` fastest, matching the
    /// block states of the same section.
    ///
    /// # Panics
    ///
    /// If any coordinate is 16 or more.
    #[must_use]
    pub const fn index(x: u32, y: u32, z: u32) -> usize {
        assert!(x < 16 && y < 16 && z < 16, "cell outside the section");
        ((y << 8) | (z << 4) | x) as usize
    }

    /// The level at a cell.
    ///
    /// # Panics
    ///
    /// If any coordinate is 16 or more.
    #[must_use]
    pub fn get(&self, x: u32, y: u32, z: u32) -> u8 {
        self.get_cell(Self::index(x, y, z))
    }

    /// Overwrite the level at a cell, returning what was there.
    ///
    /// # Panics
    ///
    /// If any coordinate is 16 or more, or `level` exceeds 15.
    pub fn set(&mut self, x: u32, y: u32, z: u32, level: u8) -> u8 {
        self.set_cell(Self::index(x, y, z), level)
    }

    /// The level at a packed cell index.
    ///
    /// # Panics
    ///
    /// If `index` is 4096 or more.
    #[must_use]
    pub fn get_cell(&self, index: usize) -> u8 {
        assert!(index < CELLS, "cell {index} is past the end of a section");
        let byte = self.nibbles[index / 2];
        if index % 2 == 0 {
            byte & 0x0f
        } else {
            byte >> 4
        }
    }

    /// Overwrite the level at a packed cell index, returning what was there.
    ///
    /// # Panics
    ///
    /// If `index` is 4096 or more, or `level` exceeds 15.
    pub fn set_cell(&mut self, index: usize, level: u8) -> u8 {
        assert!(index < CELLS, "cell {index} is past the end of a section");
        assert!(level < 16, "{level} does not fit in four bits");
        let byte = &mut self.nibbles[index / 2];
        if index % 2 == 0 {
            let previous = *byte & 0x0f;
            *byte = (*byte & 0xf0) | level;
            previous
        } else {
            let previous = *byte >> 4;
            *byte = (*byte & 0x0f) | level << 4;
            previous
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic level varied enough that two cells landing on each
    /// other's storage would be noticed.
    fn level(index: usize) -> u8 {
        ((index.wrapping_mul(2_654_435_761) >> 13) % 16) as u8
    }

    #[test]
    fn even_cells_live_in_the_low_nibble_and_odd_cells_in_the_high() {
        // Vanilla's rule, pinned against a hand-picked pair rather than
        // derived from the shift arithmetic in `get_cell`: cells 0 and 1
        // sharing a byte hold 0x05 and 0x0A, so that byte reads 0xA5. Getting
        // this backwards swaps the light of every neighbouring pair of blocks
        // and produces a section that is plausible and wrong.
        let mut written = LightArray::new();
        written.set_cell(0, 0x05);
        written.set_cell(1, 0x0a);
        written.set_cell(2, 0x00);
        written.set_cell(3, 0x03);
        assert_eq!(written.as_bytes()[0], 0xa5);

        let mut bytes = vec![0u8; BYTES];
        bytes[0] = 0xa5;
        bytes[1] = 0x30;
        let read = LightArray::from_bytes(&bytes).expect("2048 bytes");
        assert_eq!(read.get_cell(0), 0x05);
        assert_eq!(read.get_cell(1), 0x0a);
        assert_eq!(read.get_cell(2), 0x00);
        assert_eq!(read.get_cell(3), 0x03);
    }

    #[test]
    fn writing_one_cell_leaves_its_nibble_neighbour_alone() {
        // Both cells of a byte live in the same `u8`, so the bug this catches
        // is a mask built from the wrong parity — and it only shows when the
        // neighbour is not zero already.
        for start in [0usize, 1] {
            let mut array = LightArray::filled(9);
            for index in (start..CELLS).step_by(2) {
                array.set_cell(index, level(index));
                let before = array.get_cell(index ^ 1);
                assert_eq!(before, 9, "writing cell {index} disturbed its neighbour");
            }
            for index in (start..CELLS).step_by(2) {
                assert_eq!(array.get_cell(index), level(index));
            }
        }
    }

    #[test]
    fn coordinates_and_indices_reach_the_same_cell_in_the_states_order() {
        // y slowest, z middle, x fastest -- the same order as the section's
        // block states, asserted here against hand-computed numbers so the
        // convention cannot drift apart between the two arrays.
        assert_eq!(LightArray::index(0, 0, 0), 0);
        assert_eq!(LightArray::index(3, 0, 0), 3);
        assert_eq!(LightArray::index(0, 0, 1), 16);
        assert_eq!(LightArray::index(0, 1, 0), 256);
        assert_eq!(LightArray::index(3, 2, 1), 512 + 16 + 3);
        assert_eq!(LightArray::index(15, 15, 15), 4095);

        let mut array = LightArray::new();
        array.set(3, 2, 1, 7);
        assert_eq!(array.get_cell(LightArray::index(3, 2, 1)), 7);
        assert_eq!(
            array.get_cell(LightArray::index(1, 2, 3)),
            0,
            "not the transposed cell"
        );
        array.set_cell(561, 11);
        assert_eq!(array.get(1, 2, 3), 11);
    }

    #[test]
    fn every_cell_round_trips_through_the_packed_bytes() {
        let mut array = LightArray::new();
        for index in 0..CELLS {
            array.set_cell(index, level(index));
        }
        let bytes = array.as_bytes();
        assert_eq!(bytes.len(), BYTES);

        let read = LightArray::from_bytes(bytes).expect("its own output");
        assert_eq!(read, array);
        for index in 0..CELLS {
            assert_eq!(read.get_cell(index), level(index));
        }

        let taken = array.clone().into_bytes();
        let rebuilt = LightArray::from_bytes(&taken[..]).expect("2048 bytes");
        assert_eq!(rebuilt, array);
    }

    #[test]
    fn a_byte_run_of_any_other_length_is_named() {
        // 4097 cells would need another half byte; truncating silently would
        // shift every level after the cut and relight the section.
        let err = LightArray::from_bytes(&[0; BYTES - 1]).expect_err("one byte short");
        assert_eq!(
            err,
            LightArrayError::WrongLength {
                expected: 2048,
                found: 2047
            }
        );
        assert!(err.to_string().contains("2048"), "{err}");
        assert!(err.to_string().contains("4096"), "{err}");

        let err = LightArray::from_bytes(&[0; BYTES + 1]).expect_err("one byte over");
        assert_eq!(
            err,
            LightArrayError::WrongLength {
                expected: 2048,
                found: 2049
            }
        );
    }

    #[test]
    fn a_filled_array_holds_its_level_everywhere() {
        let array = LightArray::filled(15);
        assert!((0..CELLS).all(|i| array.get_cell(i) == 15));
        assert!(array.as_bytes().iter().all(|b| *b == 0xff));

        let dark = LightArray::filled(0);
        assert!(dark.as_bytes().iter().all(|b| *b == 0));

        let dim = LightArray::filled(5);
        assert_eq!(dim.as_bytes()[0], 0x55);
    }

    #[test]
    fn an_empty_array_is_dark_and_costs_two_kilobytes_of_nothing() {
        let array = LightArray::new();
        assert!((0..CELLS).all(|i| array.get_cell(i) == 0));
        assert_eq!(array.as_bytes().len(), BYTES);
    }

    #[test]
    fn setting_a_level_returns_what_was_there() {
        let mut array = LightArray::new();
        assert_eq!(array.set(4, 5, 6, 12), 0);
        assert_eq!(array.set(4, 5, 6, 3), 12);
        assert_eq!(array.get(4, 5, 6), 3);
        assert_eq!(array.set_cell(9, 8), 0, "the odd cell of byte 4 was empty");
    }

    #[test]
    #[should_panic(expected = "does not fit in four bits")]
    fn a_level_past_fifteen_panics_rather_than_being_masked() {
        let _ = LightArray::filled(16);
    }

    #[test]
    #[should_panic(expected = "past the end")]
    fn a_cell_index_past_the_section_panics() {
        let _ = LightArray::new().get_cell(CELLS);
    }

    #[test]
    #[should_panic(expected = "outside the section")]
    fn a_coordinate_past_the_edge_panics() {
        let _ = LightArray::new().get(16, 0, 0);
    }
}
