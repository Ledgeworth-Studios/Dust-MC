//! [`PalettedContainer`]: a cube of registry ids, stored as a palette plus a
//! packed index array.
//!
//! Two of these appear in every chunk section and they are not the same shape.
//! A section's block states are 16x16x16 with a four-bit floor on the storage
//! width; its biomes are 4x4x4 with a one-bit floor and no hashed tier at all.
//! Rather than write the container twice, the differences are gathered into
//! [`Strategy`], a value-level descriptor with one `const` per configuration.
//!
//! It is value-level rather than type-level on purpose. The other parameter a
//! container needs is the size of the registry it indexes, which decides the
//! width of the global palette and is only known at run time — so the type
//! would have carried half the configuration and a field the other half, which
//! is worse than putting both in the same place.

use crate::bits::{BitStorage, BitStorageError};
use crate::palette::{ceil_log2, Palette, PaletteKind};

/// The shape of a container and the palette thresholds it uses.
///
/// Every number here is from `PalettedContainer.Strategy` in the game. They are
/// not tuning: a container that promoted at a different size would write a
/// chunk section that a vanilla client decodes into different blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Strategy {
    /// Bits per axis. The container holds `1 << (3 * size_bits)` values.
    size_bits: u32,
    /// The narrowest storage the container will use once it has left the
    /// single-valued palette.
    min_storage_bits: u32,
    /// The widest palette that stays linear.
    linear_max_bits: u32,
    /// The widest palette that is hashed, or `None` where the container has no
    /// hashed tier and goes straight from linear to global.
    hashed_max_bits: Option<u32>,
}

impl Strategy {
    /// A chunk section's block states: 16x16x16.
    ///
    /// From `PalettedContainer.Strategy.SECTION_STATES`. Four bits per axis
    /// gives 4096 values. A palette needing 1 to 4 bits is linear and *stored
    /// at four bits regardless* — a section holding two block states still
    /// wastes three bits per entry, because the format says so. Five to eight
    /// bits is hashed; above that, global.
    pub const BLOCK_STATES: Self = Self {
        size_bits: 4,
        min_storage_bits: 4,
        linear_max_bits: 4,
        hashed_max_bits: Some(8),
    };

    /// A chunk section's biomes: 4x4x4.
    ///
    /// From `PalettedContainer.Strategy.SECTION_BIOMES`. Two bits per axis
    /// gives 64 values, one per 4x4x4 block cell. There is no four-bit floor
    /// here — a two-biome section really is stored one bit per entry — and
    /// there is no hashed tier: one, two or three bits is linear, and anything
    /// wider is the global biome palette. Both differences are real and both
    /// are places a container written once for block states gets biomes wrong.
    pub const BIOMES: Self = Self {
        size_bits: 2,
        min_storage_bits: 1,
        linear_max_bits: 3,
        hashed_max_bits: None,
    };

    /// How many values a container of this shape holds.
    #[must_use]
    pub const fn len(&self) -> usize {
        1usize << (3 * self.size_bits)
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        false
    }

    /// The length of one edge: 16 for block states, 4 for biomes.
    #[must_use]
    pub const fn edge(&self) -> u32 {
        1u32 << self.size_bits
    }

    /// The index of a cell, in the order the packed array uses.
    ///
    /// `y` varies slowest and `x` fastest, which is the opposite of the order
    /// the coordinates are written in. Getting this backwards produces a chunk
    /// that is a transposition of itself: every block present, none in the
    /// right place, and no error anywhere.
    ///
    /// # Panics
    ///
    /// If any coordinate is outside `0..edge()`.
    #[must_use]
    pub const fn index(&self, x: u32, y: u32, z: u32) -> usize {
        let edge = self.edge();
        assert!(
            x < edge && y < edge && z < edge,
            "coordinate outside the container"
        );
        ((y << (2 * self.size_bits)) | (z << self.size_bits) | x) as usize
    }

    /// The palette a container of this shape uses when it needs `bits` of
    /// index, and the storage width that goes with it.
    #[must_use]
    pub fn palette_for(&self, bits: u32, registry_size: u32) -> Palette {
        if bits == 0 {
            // Callers that reach here with a value in hand build the single
            // palette themselves; this is only for reconstruction from parts.
            return Palette::single(0);
        }
        if bits <= self.linear_max_bits {
            return Palette::linear(bits.max(self.min_storage_bits));
        }
        if let Some(max) = self.hashed_max_bits {
            if bits <= max {
                return Palette::hashed(bits.max(self.min_storage_bits));
            }
        }
        Palette::global(registry_size)
    }

    /// The storage width the *disk* format uses for a palette of `entries`
    /// values, which is not always the width the container uses in memory.
    ///
    /// From `Strategy.calculateBitsForSerialization`. The two agree for every
    /// tier except the global one, and there they cannot: on disk the indices
    /// point into the palette list that was written beside them, so they need
    /// `ceil_log2(entries)` bits, while in memory the same container indexes
    /// the whole registry and needs `ceil_log2(registry)`. A reader that used
    /// the in-memory width to unpack a file reads a large section as garbage;
    /// a writer that used it produces a file no vanilla server will open.
    #[must_use]
    pub fn disk_bits(&self, entries: usize, registry_size: u32) -> u32 {
        let bits = ceil_log2(entries as u32);
        let palette = self.palette_for(bits, registry_size);
        if palette.kind() == PaletteKind::Global {
            bits
        } else {
            palette.bits()
        }
    }
}

/// A value that is not an id of the registry this container indexes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotInRegistry {
    pub value: u32,
    pub registry_size: u32,
}

impl std::fmt::Display for NotInRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} is not an id of a registry with {} entries, so no palette can hold it",
            self.value, self.registry_size
        )
    }
}

impl std::error::Error for NotInRegistry {}

/// Something wrong with the palette-and-longs pair a container was rebuilt from.
///
/// Every variant here is a thing a real file can say and a thing this crate
/// refuses to guess at. The alternative to each of them is a section of
/// plausible wrong blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContainerError {
    /// A container with no palette entries maps nothing and cannot be read.
    EmptyPalette,
    /// More than one palette entry, and no packed array to index them with.
    MissingData { entries: usize },
    /// The packed array is the wrong length for the palette that came with it.
    Storage(BitStorageError),
    /// A packed index points past the end of the palette it came with.
    IndexNotInPalette {
        cell: usize,
        index: u32,
        entries: usize,
    },
    /// The palette lists the same value twice, so two indices mean one block
    /// and every index after the repeat is off by one.
    DuplicateEntry {
        value: u32,
        first: usize,
        again: usize,
    },
    /// A palette entry is not an id of the registry the container indexes.
    NotInRegistry {
        position: usize,
        value: u32,
        registry_size: u32,
    },
}

impl std::fmt::Display for ContainerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyPalette => f.write_str(
                "the palette is empty, so every index in the packed data refers to nothing",
            ),
            Self::MissingData { entries } => write!(
                f,
                "the palette has {entries} entries but no packed data came with it; only a \
                 single-entry palette may omit it"
            ),
            Self::Storage(e) => write!(f, "{e}"),
            Self::IndexNotInPalette {
                cell,
                index,
                entries,
            } => write!(
                f,
                "cell {cell} holds palette index {index}, and the palette has only {entries} \
                 entries"
            ),
            Self::DuplicateEntry {
                value,
                first,
                again,
            } => write!(
                f,
                "the palette lists {value} at both position {first} and position {again}, so \
                 the indices past {first} do not mean what the file says they mean"
            ),
            Self::NotInRegistry {
                position,
                value,
                registry_size,
            } => write!(
                f,
                "palette position {position} holds {value}, which is not an id of a registry \
                 with {registry_size} entries"
            ),
        }
    }
}

impl std::error::Error for ContainerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Storage(e) => Some(e),
            _ => None,
        }
    }
}

impl From<BitStorageError> for ContainerError {
    fn from(e: BitStorageError) -> Self {
        Self::Storage(e)
    }
}

/// A cube of registry ids: block states for a section, or biomes for one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PalettedContainer {
    strategy: Strategy,
    registry_size: u32,
    palette: Palette,
    storage: BitStorage,
}

impl PalettedContainer {
    /// A container of `strategy`'s shape, every cell holding `fill`.
    ///
    /// It starts in the single-valued palette with no packed array, which is
    /// what a freshly generated air section is and stays.
    ///
    /// # Panics
    ///
    /// If `fill` is not below `registry_size`.
    #[must_use]
    pub fn filled(strategy: Strategy, registry_size: u32, fill: u32) -> Self {
        assert!(
            fill < registry_size,
            "{fill} is not an id of a registry with {registry_size} entries"
        );
        Self {
            strategy,
            registry_size,
            palette: Palette::single(fill),
            storage: BitStorage::new(0, strategy.len()),
        }
    }

    /// Rebuild a container from the palette and packed array a chunk file
    /// holds.
    ///
    /// This is the shape the *disk* format uses: a list of values and an
    /// optional long array, with the tier inferred from the list's length. The
    /// network format sends an explicit bits-per-entry byte instead, and
    /// belongs to the protocol crate rather than here.
    ///
    /// Two things about the disk form are easy to miss and both are handled:
    ///
    /// * The indices are packed at [`Strategy::disk_bits`], which differs from
    ///   the in-memory width exactly when the tier is global.
    /// * When the tier is global, the indices in the file point into the
    ///   palette list that was written beside them, **not** at registry ids, so
    ///   every cell has to be translated. The container that comes out has no
    ///   palette list at all.
    ///
    /// A single-entry palette arriving with a packed array is accepted and the
    /// array ignored — every index in it can only be zero, so there is nothing
    /// to lose, and refusing would reject files other server software writes.
    pub fn from_parts(
        strategy: Strategy,
        registry_size: u32,
        entries: &[u32],
        data: Option<Vec<i64>>,
    ) -> Result<Self, ContainerError> {
        for (position, value) in entries.iter().enumerate() {
            if *value >= registry_size {
                return Err(ContainerError::NotInRegistry {
                    position,
                    value: *value,
                    registry_size,
                });
            }
            if let Some(first) = entries[..position].iter().position(|v| v == value) {
                return Err(ContainerError::DuplicateEntry {
                    value: *value,
                    first,
                    again: position,
                });
            }
        }

        match entries.len() {
            0 => return Err(ContainerError::EmptyPalette),
            1 => {
                return Ok(Self {
                    strategy,
                    registry_size,
                    palette: Palette::single(entries[0]),
                    storage: BitStorage::new(0, strategy.len()),
                })
            }
            _ => {}
        }
        let Some(data) = data else {
            return Err(ContainerError::MissingData {
                entries: entries.len(),
            });
        };

        let disk_bits = strategy.disk_bits(entries.len(), registry_size);
        let stored = BitStorage::from_longs(disk_bits, strategy.len(), data)?;

        // Every index in the file must name an entry. A palette of 5 entries is
        // packed at 3 bits and 3 bits can say 7, so this is not implied by the
        // long count and has to be looked at. Without it a malformed file turns
        // into a panic on the first read of the wrong cell, thousands of blocks
        // away from anything that names the file.
        for cell in 0..stored.len() {
            let index = stored.get(cell);
            if index as usize >= entries.len() {
                return Err(ContainerError::IndexNotInPalette {
                    cell,
                    index,
                    entries: entries.len(),
                });
            }
        }

        let bits = ceil_log2(entries.len() as u32);
        let mut palette = strategy.palette_for(bits, registry_size);
        if palette.kind() == PaletteKind::Global {
            let mut storage = BitStorage::new(palette.bits(), strategy.len());
            for cell in 0..stored.len() {
                storage.set(cell, entries[stored.get(cell) as usize]);
            }
            return Ok(Self {
                strategy,
                registry_size,
                palette,
                storage,
            });
        }

        for value in entries {
            palette
                .try_insert(*value)
                .expect("the palette was sized from this entry list");
        }
        Ok(Self {
            strategy,
            registry_size,
            palette,
            storage: stored,
        })
    }

    /// The palette list and packed array to write to a chunk file.
    ///
    /// This is not simply the container's own palette and storage. A container
    /// that reached the hashed tier and then had most of its blocks replaced
    /// still carries the entries it once held, and vanilla re-palettes on every
    /// write so that a file names only the values actually present. Dust does
    /// the same, because a file with dead palette entries is a file whose tier
    /// — and so whose bit width — differs from what a vanilla server writes for
    /// the same section, and the two would disagree about a chunk neither of
    /// them corrupted.
    ///
    /// `None` for the data means a single-valued section: one entry, no array.
    #[must_use]
    pub fn to_parts(&self) -> (Vec<u32>, Option<Vec<i64>>) {
        let mut entries: Vec<u32> = Vec::new();
        let mut cells: Vec<u32> = Vec::with_capacity(self.storage.len());
        for cell in 0..self.storage.len() {
            let value = self.get(cell);
            let index = match entries.iter().position(|v| *v == value) {
                Some(index) => index,
                None => {
                    entries.push(value);
                    entries.len() - 1
                }
            };
            cells.push(index as u32);
        }

        let bits = self.strategy.disk_bits(entries.len(), self.registry_size);
        if bits == 0 {
            return (entries, None);
        }
        let mut storage = BitStorage::new(bits, self.strategy.len());
        for (cell, index) in cells.into_iter().enumerate() {
            storage.set(cell, index);
        }
        (entries, Some(storage.into_longs()))
    }

    #[must_use]
    pub const fn strategy(&self) -> Strategy {
        self.strategy
    }

    /// How many ids the registry this container indexes has.
    ///
    /// The container never asks what a block or a biome *is*, but a chunk
    /// rebuilt from parts needs to answer "is the container I was handed
    /// indexing the same registry I am about to write it into" before it can
    /// accept block-entity handles and heightmap predicates against it. This
    /// is that question, not an invitation to reach for the registry itself.
    #[must_use]
    pub const fn registry_size(&self) -> u32 {
        self.registry_size
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.strategy.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        false
    }

    #[must_use]
    pub const fn palette(&self) -> &Palette {
        &self.palette
    }

    #[must_use]
    pub const fn storage(&self) -> &BitStorage {
        &self.storage
    }

    /// Which strategy the container is currently in.
    #[must_use]
    pub const fn palette_kind(&self) -> PaletteKind {
        self.palette.kind()
    }

    /// The value in one cell, by packed index.
    ///
    /// # Panics
    ///
    /// If `index` is past the end, or the packed array holds an index the
    /// palette does not map. The second cannot happen to a container this
    /// crate handed out: one it built keeps the two in step, and one rebuilt by
    /// [`PalettedContainer::from_parts`] had every cell checked against the
    /// palette length before it was returned. It is a panic rather than a
    /// `Result` because by the time a caller is reading cells the file has
    /// already been accepted, and a per-block `Result` on the hottest path in
    /// the server would be paid on every read to describe a state that was
    /// ruled out at the door.
    #[must_use]
    pub fn get(&self, index: usize) -> u32 {
        let stored = self.storage.get(index);
        self.palette.value(stored).unwrap_or_else(|| {
            panic!(
                "packed index {stored} at cell {index} is not in a {} palette of {} entries",
                self.palette.kind(),
                self.palette.len()
            )
        })
    }

    /// The value at a coordinate within the container.
    #[must_use]
    pub fn get_at(&self, x: u32, y: u32, z: u32) -> u32 {
        self.get(self.strategy.index(x, y, z))
    }

    /// Put `value` in one cell and return what was there.
    ///
    /// # Panics
    ///
    /// If `value` is not an id of the registry. Use
    /// [`PalettedContainer::try_set`] where the value came from a file.
    pub fn set(&mut self, index: usize, value: u32) -> u32 {
        self.try_set(index, value).unwrap_or_else(|e| panic!("{e}"))
    }

    /// [`PalettedContainer::set`], with the out-of-registry case named.
    pub fn try_set(&mut self, index: usize, value: u32) -> Result<u32, NotInRegistry> {
        if value >= self.registry_size {
            return Err(NotInRegistry {
                value,
                registry_size: self.registry_size,
            });
        }
        let previous = self.get(index);
        let id = match self.palette.try_insert(value) {
            Some(id) => id,
            None => self.promote_to_hold(value),
        };
        self.storage.set(index, id);
        Ok(previous)
    }

    /// The value at a coordinate, replaced.
    pub fn set_at(&mut self, x: u32, y: u32, z: u32, value: u32) -> u32 {
        self.set(self.strategy.index(x, y, z), value)
    }

    /// Move to the next palette up and re-index everything already stored.
    ///
    /// The re-index is the part that is easy to get wrong and impossible to
    /// see. The new palette does not continue the old one's numbering — a
    /// global palette's indices are registry ids and have nothing to do with
    /// the linear palette's positions — so every cell has to be decoded through
    /// the old palette and re-encoded through the new one. Copying the packed
    /// longs across and merely widening them would leave a section full of the
    /// wrong blocks, all of them valid, with no error raised anywhere.
    fn promote_to_hold(&mut self, value: u32) -> u32 {
        let needed = ceil_log2(self.palette.len() as u32 + 1);
        let mut fresh = self.strategy.palette_for(needed, self.registry_size);
        let mut storage = BitStorage::new(fresh.bits(), self.storage.len());

        for cell in 0..self.storage.len() {
            let old_index = self.storage.get(cell);
            let held = self
                .palette
                .value(old_index)
                .expect("a container's own storage only holds indices its palette maps");
            let new_index = fresh
                .try_insert(held)
                .expect("the new palette was sized to hold everything the old one held");
            storage.set(cell, new_index);
        }

        let id = fresh
            .try_insert(value)
            .expect("the new palette has room for the value that caused the promotion");
        self.palette = fresh;
        self.storage = storage;
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::PaletteKind::{Global, Hashed, Linear, Single};

    /// The number of block states on 1.21.1, from the extracted registry.
    ///
    /// Written down rather than imported. The container takes the registry size
    /// as a parameter precisely so that it does not depend on the block table,
    /// and a test that reached for the table would quietly remove the seam it
    /// is supposed to be checking.
    const BLOCK_STATES_1_21_1: u32 = 26_684;

    /// Biomes on 1.21.1. The exact number does not matter to the container; it
    /// matters that it is small enough that the global biome palette is narrow.
    const BIOMES_1_21_1: u32 = 64;

    #[test]
    fn a_fresh_container_is_one_value_and_no_longs() {
        let container = PalettedContainer::filled(Strategy::BLOCK_STATES, BLOCK_STATES_1_21_1, 0);
        assert_eq!(container.len(), 4096);
        assert_eq!(container.palette_kind(), Single);
        assert_eq!(container.storage().bits(), 0);
        assert!(container.storage().as_longs().is_empty());
        assert!((0..4096).all(|i| container.get(i) == 0));

        let biomes = PalettedContainer::filled(Strategy::BIOMES, BIOMES_1_21_1, 3);
        assert_eq!(biomes.len(), 64);
        assert_eq!(biomes.palette_kind(), Single);
        assert!((0..64).all(|i| biomes.get(i) == 3));
    }

    #[test]
    fn block_states_promote_at_the_boundaries_vanilla_documents() {
        // Distinct values are added one at a time and the tier is checked after
        // each. These are the numbers from `Strategy.SECTION_STATES`: linear
        // through 4 bits and stored at 4 regardless, hashed from 5 to 8,
        // global above.
        let mut container =
            PalettedContainer::filled(Strategy::BLOCK_STATES, BLOCK_STATES_1_21_1, 0);
        let expected = |distinct: u32| -> (crate::palette::PaletteKind, u32) {
            match distinct {
                1 => (Single, 0),
                2..=16 => (Linear, 4),
                17..=32 => (Hashed, 5),
                33..=64 => (Hashed, 6),
                65..=128 => (Hashed, 7),
                129..=256 => (Hashed, 8),
                _ => (Global, 15),
            }
        };

        for distinct in 1..=300u32 {
            // Value 0 is already in the palette from the fill, so the nth
            // distinct value is n - 1.
            container.set(distinct as usize - 1, distinct - 1);
            let (kind, bits) = expected(distinct);
            assert_eq!(
                container.palette_kind(),
                kind,
                "{distinct} distinct block states"
            );
            assert_eq!(
                container.storage().bits(),
                bits,
                "{distinct} distinct block states"
            );
        }
    }

    #[test]
    fn biomes_promote_at_their_own_boundaries_and_never_hash() {
        // Not the same numbers, and this is the test that fails if the
        // container was written once for block states and reused.
        let mut container = PalettedContainer::filled(Strategy::BIOMES, BIOMES_1_21_1, 0);
        let expected = |distinct: u32| match distinct {
            1 => (Single, 0),
            2 => (Linear, 1),
            3..=4 => (Linear, 2),
            5..=8 => (Linear, 3),
            _ => (Global, 6),
        };
        for distinct in 1..=16u32 {
            container.set(distinct as usize - 1, distinct - 1);
            let (kind, bits) = expected(distinct);
            assert_eq!(container.palette_kind(), kind, "{distinct} distinct biomes");
            assert_eq!(
                container.storage().bits(),
                bits,
                "{distinct} distinct biomes"
            );
            assert_ne!(
                container.palette_kind(),
                Hashed,
                "biomes have no hashed tier"
            );
        }
    }

    #[test]
    fn every_value_written_survives_every_promotion() {
        // The point of the whole exercise. A promotion rebuilds the palette and
        // repacks the storage at a new width, and the new palette does not
        // continue the old one's numbering -- so the check is not "the storage
        // is the right length" but "every cell still reads back the block that
        // was put in it", verified in full each time the tier changes.
        let mut container =
            PalettedContainer::filled(Strategy::BLOCK_STATES, BLOCK_STATES_1_21_1, 0);
        let mut written = vec![0u32; 4096];
        let mut tier = (container.palette_kind(), container.storage().bits());
        let mut promotions = 0;

        for cell in 0..4096usize {
            // Every cell gets a distinct value, so the container is driven all
            // the way from single-valued to global.
            let value = cell as u32 * 6 + 1;
            assert_eq!(container.set(cell, value), written[cell]);
            written[cell] = value;
            assert_eq!(container.get(cell), value);

            let now = (container.palette_kind(), container.storage().bits());
            if now != tier {
                promotions += 1;
                tier = now;
                for (index, expected) in written.iter().enumerate() {
                    assert_eq!(
                        container.get(index),
                        *expected,
                        "cell {index} changed when the palette became {} at {} bits",
                        now.0,
                        now.1
                    );
                }
                assert!(
                    container.storage().padding_is_zero(),
                    "a repacked storage is still written for a vanilla client"
                );
            }
        }

        // Single -> linear(4) -> hashed(5,6,7,8) -> global: six changes of tier.
        assert_eq!(
            promotions, 6,
            "the container did not pass through every tier"
        );
        assert_eq!(container.palette_kind(), Global);
        for (index, expected) in written.iter().enumerate() {
            assert_eq!(container.get(index), *expected, "cell {index} at the end");
        }
    }

    #[test]
    fn a_value_outside_the_registry_is_named_and_not_stored() {
        let mut container = PalettedContainer::filled(Strategy::BIOMES, 64, 0);
        let err = container.try_set(0, 64).expect_err("64 ids are 0..64");
        assert_eq!(
            err,
            NotInRegistry {
                value: 64,
                registry_size: 64
            }
        );
        assert!(err.to_string().contains("64 entries"), "{err}");
        assert_eq!(container.get(0), 0, "nothing was stored");
    }

    #[test]
    fn the_index_order_is_y_then_z_then_x_and_covers_every_cell() {
        for strategy in [Strategy::BLOCK_STATES, Strategy::BIOMES] {
            let edge = strategy.edge();
            let mut seen = vec![false; strategy.len()];
            for y in 0..edge {
                for z in 0..edge {
                    for x in 0..edge {
                        let index = strategy.index(x, y, z);
                        // Written out rather than derived, so a shift in the
                        // implementation does not move the expectation with it.
                        let expected = match edge {
                            16 => (y * 256 + z * 16 + x) as usize,
                            4 => (y * 16 + z * 4 + x) as usize,
                            _ => unreachable!("only two shapes exist"),
                        };
                        assert_eq!(index, expected, "({x}, {y}, {z}) in a {edge}-cube");
                        assert!(!seen[index], "two coordinates share index {index}");
                        seen[index] = true;
                    }
                }
            }
            assert!(seen.into_iter().all(|s| s), "some cell has no coordinate");
        }
    }

    #[test]
    fn coordinates_and_indices_reach_the_same_cell() {
        let mut container =
            PalettedContainer::filled(Strategy::BLOCK_STATES, BLOCK_STATES_1_21_1, 0);
        container.set_at(3, 7, 11, 42);
        assert_eq!(container.get_at(3, 7, 11), 42);
        assert_eq!(container.get(Strategy::BLOCK_STATES.index(3, 7, 11)), 42);
        assert_eq!(container.get_at(11, 7, 3), 0, "not the transposed cell");
    }

    #[test]
    fn the_disk_width_is_the_palette_width_until_the_container_goes_global() {
        let s = Strategy::BLOCK_STATES;
        // Below the global tier the two agree, and the four-bit floor applies.
        assert_eq!(s.disk_bits(2, BLOCK_STATES_1_21_1), 4);
        assert_eq!(s.disk_bits(16, BLOCK_STATES_1_21_1), 4);
        assert_eq!(s.disk_bits(17, BLOCK_STATES_1_21_1), 5);
        assert_eq!(s.disk_bits(256, BLOCK_STATES_1_21_1), 8);
        // Above it they must not: on disk the indices point into the palette
        // written beside them, so 257 entries are packed at 9 bits and not at
        // the global palette's 15.
        assert_eq!(s.disk_bits(257, BLOCK_STATES_1_21_1), 9);
        assert_eq!(s.disk_bits(4096, BLOCK_STATES_1_21_1), 12);
        assert_eq!(Strategy::BIOMES.disk_bits(9, BIOMES_1_21_1), 4);
    }

    #[test]
    fn parts_round_trip_through_every_tier() {
        for distinct in [1usize, 2, 16, 17, 256, 257, 1000, 4096] {
            let mut container =
                PalettedContainer::filled(Strategy::BLOCK_STATES, BLOCK_STATES_1_21_1, 0);
            let values: Vec<u32> = (0..4096)
                .map(|cell| (cell % distinct) as u32 * 5 + 1)
                .collect();
            for (cell, value) in values.iter().enumerate() {
                container.set(cell, *value);
            }

            let (entries, data) = container.to_parts();
            assert_eq!(entries.len(), distinct, "{distinct} distinct values");
            assert_eq!(data.is_none(), distinct == 1);
            if let Some(longs) = &data {
                assert_eq!(
                    longs.len(),
                    crate::bits::long_count(
                        4096,
                        Strategy::BLOCK_STATES.disk_bits(distinct, BLOCK_STATES_1_21_1)
                    ),
                    "{distinct} distinct values"
                );
            }

            let rebuilt = PalettedContainer::from_parts(
                Strategy::BLOCK_STATES,
                BLOCK_STATES_1_21_1,
                &entries,
                data,
            )
            .expect("its own output");
            for (cell, value) in values.iter().enumerate() {
                assert_eq!(
                    rebuilt.get(cell),
                    *value,
                    "{distinct} distinct, cell {cell}"
                );
            }
        }
    }

    #[test]
    fn a_global_tier_file_has_its_indices_translated_and_not_taken_literally() {
        // A 300-entry palette on disk holds 9-bit indices into that list. Read
        // as registry ids they would be a section of the first 300 block states
        // in the game, which is a perfectly plausible section of stone and
        // grass -- and wrong. This is the check that the translation happens.
        let entries: Vec<u32> = (0..300u32).map(|n| n * 77 + 5).collect();
        let bits = Strategy::BLOCK_STATES.disk_bits(entries.len(), BLOCK_STATES_1_21_1);
        assert_eq!(bits, 9);
        let mut packed = BitStorage::new(bits, 4096);
        for cell in 0..4096 {
            packed.set(cell, (cell % 300) as u32);
        }

        let container = PalettedContainer::from_parts(
            Strategy::BLOCK_STATES,
            BLOCK_STATES_1_21_1,
            &entries,
            Some(packed.into_longs()),
        )
        .expect("a well-formed global-tier section");

        assert_eq!(container.palette_kind(), Global);
        assert_eq!(container.storage().bits(), 15);
        for cell in 0..4096 {
            assert_eq!(container.get(cell), entries[cell % 300], "cell {cell}");
        }
    }

    #[test]
    fn a_malformed_section_is_named_rather_than_decoded() {
        let s = Strategy::BLOCK_STATES;
        let registry = BLOCK_STATES_1_21_1;

        assert_eq!(
            PalettedContainer::from_parts(s, registry, &[], None),
            Err(ContainerError::EmptyPalette)
        );

        assert_eq!(
            PalettedContainer::from_parts(s, registry, &[1, 2], None),
            Err(ContainerError::MissingData { entries: 2 })
        );

        // 4-bit data for a 2-entry palette is 256 longs, not 128.
        let err = PalettedContainer::from_parts(s, registry, &[1, 2], Some(vec![0; 128]))
            .expect_err("128 longs is the pre-1.16 length");
        assert!(matches!(err, ContainerError::Storage(_)), "{err}");
        assert!(err.to_string().contains("256 longs"), "{err}");

        // A 5-entry palette is packed at 4 bits, and 4 bits can say 15.
        let mut packed = BitStorage::new(4, 4096);
        packed.set(9, 7);
        let err =
            PalettedContainer::from_parts(s, registry, &[1, 2, 3, 4, 5], Some(packed.into_longs()))
                .expect_err("index 7 names nothing");
        assert_eq!(
            err,
            ContainerError::IndexNotInPalette {
                cell: 9,
                index: 7,
                entries: 5
            }
        );
        assert!(err.to_string().contains("cell 9"), "{err}");

        let err = PalettedContainer::from_parts(s, registry, &[1, 2, 1], Some(vec![0; 256]))
            .expect_err("1 appears twice");
        assert_eq!(
            err,
            ContainerError::DuplicateEntry {
                value: 1,
                first: 0,
                again: 2
            }
        );

        let err = PalettedContainer::from_parts(s, 10, &[1, 2, 99], Some(vec![0; 256]))
            .expect_err("99 is not an id of a 10-entry registry");
        assert_eq!(
            err,
            ContainerError::NotInRegistry {
                position: 2,
                value: 99,
                registry_size: 10
            }
        );
    }

    #[test]
    fn a_rewritten_section_names_only_the_values_still_in_it() {
        // A container that reached the hashed tier and was then filled with one
        // block writes a single-valued section, exactly as vanilla does. A
        // writer that dumped its own palette would write 200 dead entries and
        // an 8-bit array, which is a different file for the same section.
        let mut container =
            PalettedContainer::filled(Strategy::BLOCK_STATES, BLOCK_STATES_1_21_1, 0);
        for cell in 0..4096 {
            container.set(cell, (cell % 200) as u32);
        }
        assert_eq!(container.palette_kind(), Hashed);
        for cell in 0..4096 {
            container.set(cell, 12);
        }
        assert_eq!(
            container.palette_kind(),
            Hashed,
            "the container does not demote on its own"
        );

        let (entries, data) = container.to_parts();
        assert_eq!(entries, vec![12]);
        assert_eq!(data, None);
    }
}
