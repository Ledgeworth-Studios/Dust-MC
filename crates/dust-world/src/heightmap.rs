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
//! **What this does not catch:** everything about the predicate. A heightmap
//! computed with the wrong test is a valid heightmap of the wrong numbers, and
//! nothing in this module can tell. The only check with teeth is comparing
//! against heightmaps a vanilla server wrote, which is what
//! `tests/vanilla_corpus.rs` does — and it can currently check the *shape* of
//! them, since there is no registry yet to check the values against.

use crate::bits::{BitStorage, BitStorageError};
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
}
