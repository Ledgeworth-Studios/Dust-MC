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
