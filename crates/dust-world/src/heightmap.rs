//! Heightmaps: for each of a chunk's 256 columns, the lowest y with nothing
//! interesting above it.
//!
//! A heightmap is 256 values in a [`BitStorage`] whose width comes from the
//! world's height rather than from a palette. The stored number is the first
//! *available* y expressed relative to the world's floor: `y - min_y`, so zero
//! means "the column is empty all the way down" and `height` means "the column
//! is full to the ceiling". The block at the top of the column is therefore at
//! [`Heightmap::highest_taken`], one below what is stored, and confusing the
//! two is an off-by-one that puts mobs inside the floor.
//!
//! # The seam: what counts is not decided here
//!
//! There are six heightmaps and they differ *only* in which block states they
//! consider interesting. Answering that needs the block registry — whether a
//! state blocks motion, whether it is a fluid, whether its block is a leaves
//! block — and this crate does not depend on the registry, deliberately: a
//! heightmap is 256 small integers and knows nothing about blocks.
//!
//! So the predicate is a parameter. [`Heightmap::recompute_column`] takes the
//! column's states from the top down and a `FnMut(u32) -> bool`, and the caller
//! that owns the registry supplies the meaning. When the block-behaviour data
//! lands, the six predicates become six functions in the crate that owns it,
//! and nothing here changes.
//!
//! The same seam runs through the *incremental* path,
//! [`Heightmap::update_on_set_block`]: a single block edit folds into the
//! map in place of a column recompute, and the one case it cannot settle on
//! its own — the surface sinking because its top block stopped counting —
//! takes the states below as a lazy parameter rather than a back-reference
//! to a chunk this type deliberately does not hold.
//!
//! **What this does not catch:** everything about the predicate. A heightmap
//! computed with the wrong test is a valid heightmap of the wrong numbers, and
//! nothing in this module can tell. The only check with teeth is comparing
//! against heightmaps a vanilla server wrote, which is what
//! `tests/vanilla_corpus.rs` does — and it can currently check the *shape* of
//! them, since there is no registry yet to check the values against.

use crate::bits::{BitStorage, BitStorageError};
use crate::container::{PalettedContainer, Strategy};
use crate::palette::ceil_log2;

/// The number of columns in a chunk: 16 x 16.
pub const COLUMNS: usize = 256;

/// A world's vertical extent.
///
/// These numbers belong to a dimension type and will move to whichever crate
/// ends up owning the dimension registry. They are here for now because a
/// heightmap cannot be built without them and this crate should not have to
/// wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorldHeight {
    min_y: i32,
    height: u32,
}

impl WorldHeight {
    /// The 1.21.1 overworld: y from -64 to 319 inclusive.
    pub const OVERWORLD: Self = Self {
        min_y: -64,
        height: 384,
    };

    /// The 1.21.1 nether and end: y from 0 to 255 inclusive.
    pub const NETHER: Self = Self {
        min_y: 0,
        height: 256,
    };

    #[must_use]
    pub const fn new(min_y: i32, height: u32) -> Self {
        Self { min_y, height }
    }

    #[must_use]
    pub const fn min_y(&self) -> i32 {
        self.min_y
    }

    /// The number of blocks from floor to ceiling.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// One past the highest block position.
    #[must_use]
    pub const fn max_y_exclusive(&self) -> i32 {
        self.min_y + self.height as i32
    }

    /// The storage width a heightmap of this world uses.
    ///
    /// `ceil_log2(height + 1)`, and the `+ 1` is load-bearing: the stored value
    /// ranges over `0..=height`, which is `height + 1` distinct numbers, so a
    /// 384-block world needs nine bits and not eight. Vanilla computes it the
    /// same way in `Heightmap`'s constructor. Nine bits is also the width that
    /// makes the packing convention visible — 256 nine-bit values are 37 longs
    /// under the modern format and 36 under the old one — so the overworld
    /// heightmap is the best evidence in a region file about which packing a
    /// reader is using.
    #[must_use]
    pub const fn heightmap_bits(&self) -> u32 {
        ceil_log2(self.height + 1)
    }
}

/// Which of the six heightmaps this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HeightmapKind {
    /// Highest non-air block. Worldgen only.
    WorldSurfaceWg,
    /// Highest non-air block.
    WorldSurface,
    /// Highest block that blocks motion or holds fluid. Worldgen only.
    OceanFloorWg,
    /// Highest block that blocks motion or holds fluid.
    OceanFloor,
    /// Highest block that blocks motion or holds fluid, as the client needs it.
    MotionBlocking,
    /// The same, but leaves do not count.
    MotionBlockingNoLeaves,
}

impl HeightmapKind {
    /// All six, in the order vanilla declares them.
    pub const ALL: [Self; 6] = [
        Self::WorldSurfaceWg,
        Self::WorldSurface,
        Self::OceanFloorWg,
        Self::OceanFloor,
        Self::MotionBlocking,
        Self::MotionBlockingNoLeaves,
    ];

    /// The key this heightmap has in a chunk's NBT.
    #[must_use]
    pub const fn nbt_key(self) -> &'static str {
        match self {
            Self::WorldSurfaceWg => "WORLD_SURFACE_WG",
            Self::WorldSurface => "WORLD_SURFACE",
            Self::OceanFloorWg => "OCEAN_FLOOR_WG",
            Self::OceanFloor => "OCEAN_FLOOR",
            Self::MotionBlocking => "MOTION_BLOCKING",
            Self::MotionBlockingNoLeaves => "MOTION_BLOCKING_NO_LEAVES",
        }
    }

    /// Whether a fully generated chunk stores this one on disk.
    ///
    /// The two `_WG` maps exist so that world generation can ask about a chunk
    /// it has not finished building; once the chunk is finished they are dead
    /// and vanilla does not write them. A reader that expected six keys in a
    /// chunk's `Heightmaps` compound and found four would conclude the file was
    /// damaged, so this is the difference being written down rather than
    /// discovered. `tests/vanilla_corpus.rs` checks it against real chunks.
    #[must_use]
    pub const fn persisted(self) -> bool {
        !matches!(self, Self::WorldSurfaceWg | Self::OceanFloorWg)
    }

    /// Whether the client is sent this one.
    #[must_use]
    pub const fn sent_to_client(self) -> bool {
        matches!(self, Self::WorldSurface | Self::MotionBlocking)
    }

    /// The kind with this NBT key.
    #[must_use]
    pub fn from_nbt_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.nbt_key() == key)
    }
}

/// One heightmap: 256 columns of a chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heightmap {
    kind: HeightmapKind,
    world: WorldHeight,
    storage: BitStorage,
}

impl Heightmap {
    /// An empty heightmap: every column reads as its world's floor.
    #[must_use]
    pub fn new(kind: HeightmapKind, world: WorldHeight) -> Self {
        Self {
            kind,
            world,
            storage: BitStorage::new(world.heightmap_bits(), COLUMNS),
        }
    }

    /// A heightmap read from the long array in a chunk file.
    pub fn from_longs(
        kind: HeightmapKind,
        world: WorldHeight,
        data: Vec<i64>,
    ) -> Result<Self, BitStorageError> {
        Ok(Self {
            kind,
            world,
            storage: BitStorage::from_longs(world.heightmap_bits(), COLUMNS, data)?,
        })
    }

    #[must_use]
    pub const fn kind(&self) -> HeightmapKind {
        self.kind
    }

    #[must_use]
    pub const fn world(&self) -> WorldHeight {
        self.world
    }

    #[must_use]
    pub const fn storage(&self) -> &BitStorage {
        &self.storage
    }

    /// The packed longs, as a chunk file holds them.
    #[must_use]
    pub fn as_longs(&self) -> &[i64] {
        self.storage.as_longs()
    }

    /// The index of a column. `x` fastest, matching vanilla's `getIndex`.
    ///
    /// # Panics
    ///
    /// If either coordinate is 16 or more.
    #[must_use]
    pub const fn index(x: u32, z: u32) -> usize {
        assert!(x < 16 && z < 16, "column outside the chunk");
        (x + z * 16) as usize
    }

    /// The lowest y in this column with nothing this heightmap counts above it.
    ///
    /// For an empty column this is the world's floor, which is a y at which
    /// there is no block — the value is a boundary, not a position.
    #[must_use]
    pub fn first_available(&self, x: u32, z: u32) -> i32 {
        self.world.min_y + self.storage.get(Self::index(x, z)) as i32
    }

    /// The y of the highest block this heightmap counts, or `None` if the
    /// column holds none.
    #[must_use]
    pub fn highest_taken(&self, x: u32, z: u32) -> Option<i32> {
        let stored = self.storage.get(Self::index(x, z));
        (stored > 0).then(|| self.world.min_y + stored as i32 - 1)
    }

    /// Record that `y` is the first available position in this column.
    ///
    /// # Panics
    ///
    /// If `y` is outside `min_y..=max_y_exclusive`. The inclusive upper end is
    /// deliberate: a column full to the ceiling has its first available
    /// position one above the highest block, which is one past the top of the
    /// world and is exactly the value the extra bit of width exists for.
    pub fn set_first_available(&mut self, x: u32, z: u32, y: i32) {
        assert!(
            y >= self.world.min_y && y <= self.world.max_y_exclusive(),
            "{y} is outside a world running from {} to {}",
            self.world.min_y,
            self.world.max_y_exclusive()
        );
        let stored = (y - self.world.min_y) as u32;
        self.storage.set(Self::index(x, z), stored);
    }

    /// Fold one block change into the map instead of recomputing the column.
    ///
    /// The arguments are what a block edit already knows: where it happened,
    /// whether the previous state counted (`was_opaque`) and whether the new
    /// one does (`is_opaque`). Three cases need work:
    ///
    /// * A counting block placed at or above the current surface raises it to
    ///   `y + 1`, whatever stood there before.
    /// * A counting block removed from the surface itself — `y` exactly one
    ///   below the first available position — sinks it, and where it lands
    ///   depends on what is *under* that block, which a heightmap cannot see.
    ///   The caller supplies the view: `below` yields `(y, counted)` pairs for
    ///   the rest of the column, top-down from `y - 1`, and the walk stops at
    ///   the first counted row. Building that iterator costs the caller
    ///   almost nothing precisely because it is lazy — in every other case it
    ///   is never consumed.
    /// * Everything else — interior edits, air into air, a counted block
    ///   replaced by another — moves nothing, because some counted block
    ///   still stands above `y`.
    ///
    /// This is exact by construction, not approximately right: the result is
    /// identical to [`Heightmap::recompute_column`] over the same column,
    /// which `tests/heightmap_incremental.rs` holds against hundreds of
    /// random edit schedules.
    ///
    /// # Panics
    ///
    /// As [`Heightmap::set_first_available`], if a raised surface would land
    /// past the ceiling — impossible for a block inside this world — or if
    /// `below` stops descending. A lazy check keeps the contract visible
    /// without charging the hot path for it.
    pub fn update_on_set_block<I>(
        &mut self,
        x: u32,
        y: i32,
        z: u32,
        was_opaque: bool,
        is_opaque: bool,
        below: I,
    ) where
        I: IntoIterator<Item = (i32, bool)>,
    {
        let first_available = self.first_available(x, z);
        if is_opaque {
            if y >= first_available {
                self.set_first_available(x, z, y + 1);
            }
        } else if was_opaque && y == first_available - 1 {
            let mut sunk_to = self.world.min_y;
            let mut previous = y;
            for (below_y, counted) in below {
                debug_assert!(
                    below_y < previous,
                    "the column below the edit must arrive top-down"
                );
                previous = below_y;
                if counted {
                    sunk_to = below_y + 1;
                    break;
                }
            }
            self.set_first_available(x, z, sunk_to);
        }
    }

    /// Recompute one column from the states in it.
    ///
    /// `top_down` yields `(y, state)` from the top of the world downwards, and
    /// may stop early — this returns as soon as `matches` accepts one, so a
    /// caller may hand it a lazy iterator over sections and pay for only the
    /// sections it had to look at.
    ///
    /// `matches` is the seam described in the module documentation: this
    /// function does not know and must not know which states count.
    pub fn recompute_column<I, F>(&mut self, x: u32, z: u32, top_down: I, mut matches: F)
    where
        I: IntoIterator<Item = (i32, u32)>,
        F: FnMut(u32) -> bool,
    {
        let mut first_available = self.world.min_y;
        for (y, state) in top_down {
            if matches(state) {
                first_available = y + 1;
                break;
            }
        }
        self.set_first_available(x, z, first_available);
    }

    /// Recompute every column from a chunk's sections.
    ///
    /// `sections` are the chunk's block-state containers in bottom-up order,
    /// one per sixteen rows of the world. The scan walks each column from the
    /// top of the world down — top section first, row fifteen before row zero
    /// — and stops at the first state `matches` accepts, so a column whose
    /// interesting block is near the surface never reads the stone below it.
    /// This is [`Heightmap::recompute_column`] with the column iterator built
    /// from sections, which is what a chunk actually has; the predicate stays
    /// a parameter for the same reason it is there.
    ///
    /// # Panics
    ///
    /// If the section list does not tile the world exactly — one container per
    /// sixteen rows — or if any container is not a block-state container. A
    /// biome container passed by mistake would answer every question with a
    /// plausible biome id and compute a heightmap of them; that is a caller
    /// bug, and it is named here rather than buried in the numbers.
    ///
    /// # Panics (from the predicate path)
    ///
    /// [`Heightmap::set_first_available`] panics if a matching state sits at a
    /// y past the ceiling, which cannot happen for containers of this world's
    /// own shape.
    pub fn recompute_from_sections<F>(&mut self, sections: &[&PalettedContainer], mut matches: F)
    where
        F: FnMut(u32) -> bool,
    {
        assert_eq!(
            sections.len() * 16,
            self.world.height as usize,
            "a world {} rows tall needs {} sections, and {} were supplied",
            self.world.height,
            self.world.height / 16,
            sections.len()
        );
        for section in sections {
            assert_eq!(
                section.strategy(),
                Strategy::BLOCK_STATES,
                "a heightmap is recomputed from block-state sections of {} cells, and one \
                 container here holds {} cells instead",
                Strategy::BLOCK_STATES.len(),
                section.len()
            );
        }

        let min_y = self.world.min_y;
        for z in 0..16u32 {
            for x in 0..16u32 {
                let top_down =
                    sections
                        .iter()
                        .enumerate()
                        .rev()
                        .flat_map(move |(index, section)| {
                            let base = min_y + (index * 16) as i32;
                            (0..16u32)
                                .rev()
                                .map(move |row| (base + row as i32, section.get_at(x, row, z)))
                        });
                self.recompute_column(x, z, top_down, &mut matches);
            }
        }
    }
}

/// The six heightmaps of one chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeightmapSet {
    maps: [Heightmap; 6],
}

impl HeightmapSet {
    #[must_use]
    pub fn new(world: WorldHeight) -> Self {
        Self {
            maps: HeightmapKind::ALL.map(|kind| Heightmap::new(kind, world)),
        }
    }

    #[must_use]
    pub fn get(&self, kind: HeightmapKind) -> &Heightmap {
        &self.maps[Self::slot(kind)]
    }

    pub fn get_mut(&mut self, kind: HeightmapKind) -> &mut Heightmap {
        &mut self.maps[Self::slot(kind)]
    }

    /// Every heightmap, in the order vanilla declares them.
    pub fn iter(&self) -> impl Iterator<Item = &Heightmap> {
        self.maps.iter()
    }

    /// The ones a fully generated chunk writes to disk.
    pub fn persisted(&self) -> impl Iterator<Item = &Heightmap> {
        self.maps.iter().filter(|m| m.kind().persisted())
    }

    /// Recompute all six heightmaps from a chunk's sections in one call.
    ///
    /// The six maps disagree about which states count, and this crate cannot
    /// know any of their answers, so `matches` is asked per kind: it receives
    /// the [`HeightmapKind`] and a block state, and says whether that map
    /// counts it. Each map is still walked lazily on its own, so a predicate
    /// that is expensive for one kind does not slow the others.
    pub fn recompute_from_sections<F>(&mut self, sections: &[&PalettedContainer], mut matches: F)
    where
        F: FnMut(HeightmapKind, u32) -> bool,
    {
        for slot in 0..self.maps.len() {
            let kind = self.maps[slot].kind();
            self.maps[slot].recompute_from_sections(sections, |state| matches(kind, state));
        }
    }

    fn slot(kind: HeightmapKind) -> usize {
        HeightmapKind::ALL
            .iter()
            .position(|k| *k == kind)
            .expect("ALL lists every kind")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plus_one_in_the_width_is_a_whole_bit_at_a_power_of_two() {
        // 384 blocks of world is 385 distinct stored values, which is nine
        // bits. Eight would be enough for every position and one short of the
        // "full to the ceiling" value, and the column that reached the top
        // would wrap to the floor.
        //
        // The nether is the case that makes the point: it is exactly 256 tall,
        // which is eight bits of position, and it still needs nine -- because
        // the value that says "full" is 256 and does not fit. A width worked
        // out from the height alone is wrong for every world whose height is a
        // power of two, and both of Minecraft's other dimensions are.
        assert_eq!(WorldHeight::OVERWORLD.heightmap_bits(), 9);
        assert_eq!(WorldHeight::NETHER.heightmap_bits(), 9);
        assert_eq!(WorldHeight::new(0, 255).heightmap_bits(), 8);
        assert_eq!(WorldHeight::new(0, 256).heightmap_bits(), 9);
    }

    #[test]
    fn a_heightmap_is_two_hundred_and_fifty_six_values_and_thirty_seven_longs() {
        let map = Heightmap::new(HeightmapKind::MotionBlocking, WorldHeight::OVERWORLD);
        assert_eq!(map.storage().len(), COLUMNS);
        assert_eq!(map.storage().bits(), 9);
        assert_eq!(
            map.as_longs().len(),
            37,
            "36 would be the pre-1.16 packing of the same values"
        );
    }

    #[test]
    fn an_empty_column_reads_as_the_floor_and_holds_no_block() {
        let map = Heightmap::new(HeightmapKind::WorldSurface, WorldHeight::OVERWORLD);
        assert_eq!(map.first_available(0, 0), -64);
        assert_eq!(map.highest_taken(0, 0), None);
    }

    #[test]
    fn the_highest_block_is_one_below_the_first_available_position() {
        // The off-by-one that puts a mob inside the floor.
        let mut map = Heightmap::new(HeightmapKind::MotionBlocking, WorldHeight::OVERWORLD);
        map.set_first_available(4, 9, 64);
        assert_eq!(map.first_available(4, 9), 64);
        assert_eq!(map.highest_taken(4, 9), Some(63));
    }

    #[test]
    fn a_column_full_to_the_ceiling_stores_the_value_past_the_top() {
        let mut map = Heightmap::new(HeightmapKind::WorldSurface, WorldHeight::OVERWORLD);
        let top = WorldHeight::OVERWORLD.max_y_exclusive();
        assert_eq!(top, 320);
        map.set_first_available(0, 0, top);
        assert_eq!(map.first_available(0, 0), 320);
        assert_eq!(map.highest_taken(0, 0), Some(319));
    }

    #[test]
    fn every_column_is_its_own_and_survives_the_round_trip_through_longs() {
        let mut map = Heightmap::new(HeightmapKind::OceanFloor, WorldHeight::OVERWORLD);
        for z in 0..16 {
            for x in 0..16 {
                map.set_first_available(x, z, -64 + (x * 16 + z) as i32);
            }
        }
        let longs = map.as_longs().to_vec();
        assert!(map.storage().padding_is_zero());
        let read = Heightmap::from_longs(HeightmapKind::OceanFloor, WorldHeight::OVERWORLD, longs)
            .expect("its own output");
        for z in 0..16 {
            for x in 0..16 {
                assert_eq!(
                    read.first_available(x, z),
                    -64 + (x * 16 + z) as i32,
                    "({x}, {z})"
                );
            }
        }
        assert_eq!(read, map);
    }

    #[test]
    fn an_incremental_raise_lands_the_surface_above_the_new_block() {
        let mut map = Heightmap::new(HeightmapKind::WorldSurface, WorldHeight::OVERWORLD);
        // Placing counted blocks onto an empty column: each raise lands at
        // y + 1, and the lazy `below` argument is never touched -- which is
        // what lets callers build it from an iterator they already have.
        let mut consumed = false;
        let below = std::iter::from_fn(|| {
            consumed = true;
            None::<(i32, bool)>
        });
        map.update_on_set_block(3, 5, 4, false, true, below);
        assert!(!consumed, "a raise has no need of the column beneath");
        assert_eq!(map.first_available(3, 4), 6);
        assert_eq!(map.highest_taken(3, 4), Some(5));

        // A second block higher still moves the surface past itself; one
        // lower than the surface moves nothing.
        map.update_on_set_block(3, 90, 4, false, true, std::iter::empty());
        assert_eq!(map.first_available(3, 4), 91);
        map.update_on_set_block(3, 40, 4, false, true, std::iter::empty());
        assert_eq!(
            map.first_available(3, 4),
            91,
            "an interior placement does not lower or double-count"
        );
    }

    #[test]
    fn an_incremental_lower_walks_the_column_only_when_the_top_left() {
        let mut map = Heightmap::new(HeightmapKind::WorldSurface, WorldHeight::new(0, 32));
        // Column with counted blocks at y = 20 and y = 7: surface at 21.
        map.set_first_available(1, 2, 21);

        // Removing an interior counted block leaves the surface alone --
        // something counted still stands above it.
        map.update_on_set_block(1, 7, 2, true, false, [(6, true)]);
        assert_eq!(map.first_available(1, 2), 21);

        // Removing the top sinks it to the next counted row below.
        let mut asked = Vec::new();
        let below = (0..20i32).rev().map(|row| {
            let counted = row == 7;
            asked.push((row, counted));
            (row, counted)
        });
        map.update_on_set_block(1, 20, 2, true, false, below);
        assert_eq!(map.first_available(1, 2), 8);
        // The walk stopped at the first counted row; nothing below was read.
        assert_eq!(asked.len(), 13, "rows 19..=7 were looked at, no further");
        assert_eq!(asked.last().copied(), Some((7, true)));

        // And removing the last counted block drops the column to the floor.
        map.update_on_set_block(1, 7, 2, true, false, (0..7).rev().map(|y| (y, false)));
        assert_eq!(map.first_available(1, 2), 0);
        assert_eq!(map.highest_taken(1, 2), None);
    }

    #[test]
    fn replacing_a_counted_block_with_another_moves_nothing() {
        let mut map = Heightmap::new(HeightmapKind::WorldSurface, WorldHeight::new(0, 32));
        map.set_first_available(9, 9, 14);
        map.update_on_set_block(9, 13, 9, true, true, std::iter::empty());
        assert_eq!(map.first_available(9, 9), 14);
        // Placing a counted block above the surface is a different story:
        // that raises regardless of what the previous state was.
        map.update_on_set_block(9, 30, 9, false, true, std::iter::empty());
        assert_eq!(map.first_available(9, 9), 31);
    }

    #[test]
    fn raising_and_lowering_across_word_boundaries_leaves_the_neighbours_alone() {
        // Nine-bit columns pack seven to a long; columns 6 and 7 share a word
        // boundary, so edits that push their stored values around are exactly
        // the writes most likely to smear into a neighbour if the packing is
        // wrong. Both directions, repeatedly, with the padding checked after.
        for world in [
            WorldHeight::OVERWORLD,
            WorldHeight::NETHER,
            WorldHeight::new(0, 255),
        ] {
            let bits = world.heightmap_bits();
            let per_long = crate::bits::values_per_long(bits);
            let left_index = per_long - 1;
            let right_index = per_long;
            if right_index >= COLUMNS {
                continue;
            }
            let (left_x, left_z) = ((left_index % 16) as u32, (left_index / 16) as u32);
            let (right_x, right_z) = ((right_index % 16) as u32, (right_index / 16) as u32);
            let mut map = Heightmap::new(HeightmapKind::MotionBlocking, world);
            let floor = world.min_y();
            let ceiling = world.max_y_exclusive();

            for step in 0..12usize {
                // Interleaved placements at unrelated heights -- one high and
                // sinking, one low and rising -- so neither column's stored
                // value sits still while the other is rewritten.
                let left_y = ceiling - 2 - step as i32;
                let right_y = ceiling - 40 + step as i32 * 3;
                map.update_on_set_block(left_x, left_y, left_z, false, true, std::iter::empty());
                assert_eq!(
                    map.first_available(left_x, left_z),
                    left_y + 1,
                    "{world:?} raise step {step}"
                );
                map.update_on_set_block(right_x, right_y, right_z, false, true, std::iter::empty());
                assert_eq!(
                    map.first_available(right_x, right_z),
                    right_y + 1,
                    "{world:?} raise step {step}"
                );
                // And the exact inverse: removing each placed top sinks its
                // column back to the floor through the lazy walk.
                map.update_on_set_block(
                    left_x,
                    left_y,
                    left_z,
                    true,
                    false,
                    (floor..left_y).rev().map(|y| (y, false)),
                );
                assert_eq!(
                    map.first_available(left_x, left_z),
                    floor,
                    "{world:?} sink step {step}"
                );
                map.update_on_set_block(
                    right_x,
                    right_y,
                    right_z,
                    true,
                    false,
                    (floor..right_y).rev().map(|y| (y, false)),
                );
                assert_eq!(
                    map.first_available(right_x, right_z),
                    floor,
                    "{world:?} sink step {step}"
                );
                assert!(map.storage().padding_is_zero(), "{world:?} step {step}");
            }
        }
    }

    #[test]
    fn recomputing_a_column_stops_at_the_first_state_the_predicate_accepts() {
        // The seam. The predicate here counts odd state ids, which is not what
        // any real heightmap does and is exactly the point: this module must
        // work for a predicate it knows nothing about.
        let mut map = Heightmap::new(HeightmapKind::MotionBlocking, WorldHeight::OVERWORLD);
        let column: Vec<(i32, u32)> = (-64..320)
            .rev()
            .map(|y| (y, if y == 70 { 3 } else { 0 }))
            .collect();
        let mut asked = 0;
        map.recompute_column(2, 3, column, |state| {
            asked += 1;
            state % 2 == 1
        });
        assert_eq!(map.first_available(2, 3), 71);
        assert_eq!(map.highest_taken(2, 3), Some(70));
        assert_eq!(asked, 320 - 70, "it kept asking after it had an answer");
    }

    #[test]
    fn a_column_the_predicate_never_accepts_is_empty() {
        let mut map = Heightmap::new(HeightmapKind::MotionBlocking, WorldHeight::OVERWORLD);
        map.recompute_column(0, 0, (-64..320).rev().map(|y| (y, 0u32)), |_| false);
        assert_eq!(map.first_available(0, 0), -64);
        assert_eq!(map.highest_taken(0, 0), None);
    }

    #[test]
    fn four_of_the_six_are_written_to_disk() {
        // A reader that expected six keys in a chunk's Heightmaps compound and
        // found four would call an intact file damaged.
        let persisted: Vec<&str> = HeightmapKind::ALL
            .into_iter()
            .filter(|k| k.persisted())
            .map(HeightmapKind::nbt_key)
            .collect();
        assert_eq!(
            persisted,
            vec![
                "WORLD_SURFACE",
                "OCEAN_FLOOR",
                "MOTION_BLOCKING",
                "MOTION_BLOCKING_NO_LEAVES"
            ]
        );
        assert!(!HeightmapKind::WorldSurfaceWg.persisted());
        assert!(!HeightmapKind::OceanFloorWg.persisted());
    }

    #[test]
    fn keys_round_trip_and_nothing_else_parses() {
        for kind in HeightmapKind::ALL {
            assert_eq!(HeightmapKind::from_nbt_key(kind.nbt_key()), Some(kind));
        }
        assert_eq!(HeightmapKind::from_nbt_key("MOTION_BLOCKING_LEAVES"), None);
        assert_eq!(HeightmapKind::from_nbt_key("world_surface"), None);
    }

    #[test]
    fn a_set_holds_one_of_each_and_hands_back_the_one_asked_for() {
        let mut set = HeightmapSet::new(WorldHeight::OVERWORLD);
        assert_eq!(set.iter().count(), 6);
        assert_eq!(set.persisted().count(), 4);
        for kind in HeightmapKind::ALL {
            assert_eq!(set.get(kind).kind(), kind);
            set.get_mut(kind).set_first_available(1, 1, kind as i32);
        }
        for kind in HeightmapKind::ALL {
            assert_eq!(
                set.get(kind).first_available(1, 1),
                kind as i32,
                "{}",
                kind.nbt_key()
            );
        }
    }

    /// A stored height varied enough that two columns landing in the wrong
    /// slots of each other would be noticed, and deterministic, because a
    /// hand-checked vector must stay hand-checkable.
    fn stored(index: usize) -> u32 {
        ((index * 37 + 11) % 385) as u32
    }

    #[test]
    fn the_nine_bit_words_hold_seven_columns_at_the_vanilla_offsets() {
        // The layout, written out rather than derived from any loop in this
        // module: seven nine-bit columns share each long at bit offsets 0, 9,
        // 18, 27, 36, 45 and 54, leaving bit 63 of every long as padding. The
        // expectations below are that arithmetic done once by hand, against
        // concrete values -- not a second implementation of the packing.
        let mut map = Heightmap::new(HeightmapKind::MotionBlocking, WorldHeight::OVERWORLD);
        for column in 0..14usize {
            // Column n sits at x = n % 16, z = n / 16; all fourteen fit in the
            // z == 0 edge of the chunk.
            let (x, z) = ((column % 16) as u32, (column / 16) as u32);
            let value = stored(column);
            map.set_first_available(x, z, WorldHeight::OVERWORLD.min_y() + value as i32);
        }

        let longs = map.as_longs();
        assert_eq!(longs.len(), 37);
        assert_eq!(
            longs[0],
            stored(0) as i64
                | ((stored(1) as i64) << 9)
                | ((stored(2) as i64) << 18)
                | ((stored(3) as i64) << 27)
                | ((stored(4) as i64) << 36)
                | ((stored(5) as i64) << 45)
                | ((stored(6) as i64) << 54),
            "columns 0..7 pack into the first long"
        );
        assert_eq!(
            longs[1],
            stored(7) as i64
                | ((stored(8) as i64) << 9)
                | ((stored(9) as i64) << 18)
                | ((stored(10) as i64) << 27)
                | ((stored(11) as i64) << 36)
                | ((stored(12) as i64) << 45)
                | ((stored(13) as i64) << 54),
            "columns 7..13 pack into the second long"
        );
        assert!(
            longs[2..].iter().all(|l| *l == 0),
            "every column past 13 was left empty"
        );
        assert!(map.storage().padding_is_zero());
    }

    #[test]
    fn reading_the_words_back_yields_the_columns_they_were_written_from() {
        // The same layout read backwards through the public door: the longs
        // above, decoded by vanilla's rule -- column c lives in long c / 7 at
        // shift (c % 7) * 9 -- must come back as the values put in.
        let mut map = Heightmap::new(HeightmapKind::WorldSurface, WorldHeight::OVERWORLD);
        for column in 0..COLUMNS {
            let (x, z) = ((column % 16) as u32, (column / 16) as u32);
            map.set_first_available(x, z, WorldHeight::OVERWORLD.min_y() + stored(column) as i32);
        }
        let longs = map.as_longs().to_vec();

        let mut expected = vec![0i64; 37];
        for column in 0..COLUMNS {
            expected[column / 7] |= (stored(column) as i64) << ((column % 7) * 9);
        }
        assert_eq!(longs, expected);
        assert!(map.storage().padding_is_zero());

        let read =
            Heightmap::from_longs(HeightmapKind::WorldSurface, WorldHeight::OVERWORLD, longs)
                .expect("its own output");
        for column in 0..COLUMNS {
            let (x, z) = ((column % 16) as u32, (column / 16) as u32);
            assert_eq!(
                read.first_available(x, z),
                WorldHeight::OVERWORLD.min_y() + stored(column) as i32,
                "column {column}"
            );
        }
    }

    #[test]
    fn columns_either_side_of_every_word_boundary_move_independently() {
        // The pair that shares a word boundary is the pair an off-by-one in
        // the shift arithmetic corrupts: write the last column of one long and
        // the first of the next, then swap them, at every boundary there is.
        // Three worlds cover both widths a heightmap actually uses: 384 rows
        // and 256 rows are nine bits, 255 rows is eight.
        for world in [
            WorldHeight::OVERWORLD,
            WorldHeight::NETHER,
            WorldHeight::new(0, 255),
        ] {
            let bits = world.heightmap_bits();
            let per_long = crate::bits::values_per_long(bits);
            let longs = crate::bits::long_count(COLUMNS, bits);
            let mut map = Heightmap::new(HeightmapKind::OceanFloor, world);

            for boundary in 0..longs.saturating_sub(1) {
                let left = boundary * per_long + per_long - 1;
                let right = left + 1;
                if right >= COLUMNS {
                    break;
                }
                let (lx, lz) = ((left % 16) as u32, (left / 16) as u32);
                let (rx, rz) = ((right % 16) as u32, (right / 16) as u32);
                let high = world.max_y_exclusive();
                let low = world.min_y() + 1;

                for (first, second) in [(high, low), (low, high)] {
                    map.set_first_available(lx, lz, first);
                    map.set_first_available(rx, rz, second);
                    assert_eq!(
                        map.first_available(lx, lz),
                        first,
                        "{world:?} long {boundary}"
                    );
                    assert_eq!(
                        map.first_available(rx, rz),
                        second,
                        "{world:?} long {boundary}"
                    );
                }
            }
            assert!(map.storage().padding_is_zero(), "{world:?}");
        }
    }

    #[test]
    fn recomputing_from_sections_walks_each_column_from_the_top_of_the_world() {
        use crate::container::PalettedContainer;

        // A two-section world, small enough that every number in the test can
        // be checked by eye: y runs 0..32, the bottom section holds rows 0..16
        // and the top section rows 16..32.
        let registry = 26_684;
        let mut bottom = PalettedContainer::filled(Strategy::BLOCK_STATES, registry, 0);
        let mut top = PalettedContainer::filled(Strategy::BLOCK_STATES, registry, 0);

        // Column (3, 4): one counted state at y = 20, nothing else anywhere.
        top.set_at(3, 4, 4, 9);
        // Column (15, 15): one counted state down in the bottom section, at
        // y = 5, under air all the way up.
        bottom.set_at(15, 5, 15, 7);

        let sections = [&bottom, &top];
        let mut map = Heightmap::new(HeightmapKind::WorldSurface, WorldHeight::new(0, 32));
        map.recompute_from_sections(&sections, |state| state != 0);

        assert_eq!(map.first_available(3, 4), 21, "the block at 20 is counted");
        assert_eq!(map.highest_taken(3, 4), Some(20));
        assert_eq!(
            map.first_available(15, 15),
            6,
            "a deep block is found through sixteen rows of air"
        );
        assert_eq!(map.first_available(0, 0), 0, "an untouched column is floor");
        for z in 0..16u32 {
            for x in 0..16u32 {
                if (x, z) == (3, 4) || (x, z) == (15, 15) {
                    continue;
                }
                assert_eq!(
                    map.first_available(x, z),
                    0,
                    "column ({x}, {z}) should be empty"
                );
            }
        }
    }

    #[test]
    fn the_set_recompute_feeds_each_map_its_own_predicate() {
        use crate::container::PalettedContainer;

        // Six maps, six answers about the same two columns. The predicate
        // counts a state only for the kind whose ordinal matches it minus one,
        // which makes the expected heightmap of every kind different and
        // hand-checkable: kind 0 counts the state 1 pillar, kind 1 the state 2
        // pillar above it, kind 3 the state 4 block down below, and kinds 2,
        // 4 and 5 count nothing at all.
        let registry = 26_684;
        let mut bottom = PalettedContainer::filled(Strategy::BLOCK_STATES, registry, 0);
        let mut top = PalettedContainer::filled(Strategy::BLOCK_STATES, registry, 0);
        top.set_at(2, 7, 2, 1); // y = 23, column (2, 2)
        top.set_at(5, 3, 5, 2); // y = 19, column (5, 5)
        bottom.set_at(5, 9, 5, 4); // y = 9, column (5, 5)

        let sections = [&bottom, &top];
        let mut set = HeightmapSet::new(WorldHeight::new(0, 32));
        set.recompute_from_sections(&sections, |kind, state| state == kind as u32 + 1);

        let floor = 0;
        assert_eq!(
            set.get(HeightmapKind::WorldSurfaceWg).first_available(2, 2),
            24
        );
        assert_eq!(
            set.get(HeightmapKind::WorldSurfaceWg).first_available(5, 5),
            floor
        );

        assert_eq!(
            set.get(HeightmapKind::WorldSurface).first_available(5, 5),
            20
        );
        assert_eq!(
            set.get(HeightmapKind::WorldSurface).first_available(2, 2),
            floor
        );

        assert_eq!(
            set.get(HeightmapKind::OceanFloorWg).first_available(2, 2),
            floor
        );
        assert_eq!(
            set.get(HeightmapKind::OceanFloorWg).first_available(5, 5),
            floor
        );

        assert_eq!(set.get(HeightmapKind::OceanFloor).first_available(5, 5), 10);
        assert_eq!(
            set.get(HeightmapKind::MotionBlocking).first_available(2, 2),
            floor
        );
        assert_eq!(
            set.get(HeightmapKind::MotionBlockingNoLeaves)
                .first_available(5, 5),
            floor
        );
    }

    #[test]
    fn recomputing_from_a_section_list_that_does_not_tile_the_world_is_named() {
        use crate::container::PalettedContainer;

        let registry = 26_684;
        let section = PalettedContainer::filled(Strategy::BLOCK_STATES, registry, 0);

        // One section cannot cover thirty-two rows; the missing sixteen would
        // silently read as air and flatten every column.
        let one = [&section];
        let mut map = Heightmap::new(HeightmapKind::WorldSurface, WorldHeight::new(0, 32));
        let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            map.recompute_from_sections(&one, |_| true);
        }))
        .expect_err("the section count is wrong");
        let message = err.downcast_ref::<String>().cloned().unwrap_or_default();
        assert!(message.contains("needs 2 sections"), "{message}");

        // And a biome container would answer every question plausibly and
        // wrongly, so the shape is checked too.
        let biomes = PalettedContainer::filled(Strategy::BIOMES, 64, 0);
        let wrong_shape = [&biomes, &biomes];
        let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            map.recompute_from_sections(&wrong_shape, |_| true);
        }))
        .expect_err("biomes are not block states");
        let message = err.downcast_ref::<String>().cloned().unwrap_or_default();
        assert!(message.contains("holds 64 cells"), "{message}");
    }

    #[test]
    fn all_six_maps_round_trip_through_longs_at_once() {
        let mut set = HeightmapSet::new(WorldHeight::OVERWORLD);
        for kind in HeightmapKind::ALL {
            let map = set.get_mut(kind);
            for column in 0..COLUMNS {
                let value = (column * 31 + kind as usize * 97 + 5) % 385;
                let (x, z) = ((column % 16) as u32, (column / 16) as u32);
                map.set_first_available(x, z, -64 + value as i32);
            }
        }

        for kind in HeightmapKind::ALL {
            let longs = set.get(kind).as_longs().to_vec();
            assert_eq!(longs.len(), 37, "{}", kind.nbt_key());
            let read = Heightmap::from_longs(kind, WorldHeight::OVERWORLD, longs).expect("its own");
            assert_eq!(*set.get(kind), read, "{}", kind.nbt_key());
        }
    }
}
