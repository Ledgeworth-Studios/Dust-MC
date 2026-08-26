//! The chunk: everything one 16 x 384 x 16 slice of a world carries, and the
//! seam where serialisation plugs in.
//!
//! A chunk is sections of block states and biomes, two light arrays per
//! section, six heightmaps and a list of block entities. Every part of that
//! already existed in this crate as arithmetic over integers; this module is
//! the part that says *these forty-odd pieces belong to one chunk* and gives
//! the whole thing an identity — which chunk of which world, how many
//! sections, which registries its ids name into.
//!
//! # The NBT seam, stated plainly for whoever wires it
//!
//! A chunk's bytes inside a region payload are compressed NBT, and there is
//! no NBT crate on this branch: `dust-nbt` forked away from this base and
//! will be merged later. Rather than reach across branches or grow a second,
//! worse NBT writer here, the boundary is two traits — [`NbtWriter`] and
//! [`NbtReader`] — and nothing else. They carry a [`Chunk`] to and from the
//! decompressed byte run that [`region`](crate::region) already moves in and
//! out of sector storage. When `dust-nbt` lands, its author implements these
//! traits over real NBT with Anvil-compatible field names and deletes any
//! stand-in; no call site in this crate changes, because nothing here parses
//! or emits a single tag.
//!
//! Two consequences are deliberate. First, the field names a vanilla server
//! expects (`sections`, `block_states`, `data`, `palette`, `Heightmaps`, and
//! so on) appear nowhere in this module: they are properties of the *writer*
//! that will be plugged in, not of the chunk in memory. Second, the tests
//! that prove a chunk survives the trip through a region file use a flat
//! binary stand-in format defined next to them, and say so. Those tests
//! check the plumbing — that the seam carries a whole chunk, both ways,
//! through a real region store — and they cannot check NBT conformance,
//! because there is no NBT here yet.
//!
//! # Decisions this skeleton makes, so nobody has to make them twice
//!
//! * **Setting a block does not touch the heightmaps or the light.** Vanilla
//!   updates both as blocks change; doing that correctly needs the block
//!   registry (which states count for each map) and a cross-section light
//!   engine, neither of which lives on this branch. Until then the caller
//!   recomputes explicitly with [`Chunk::recompute_heightmaps`], and a stale
//!   heightmap is visible rather than half-maintained.
//! * **Registry sizes ride along.** The chunk stores how many block states
//!   and biomes its registries hold, because a paletted container cannot
//!   answer "is this id in range" without that number and this crate does
//!   not depend on the extracted tables. Whoever builds chunks supplies the
//!   numbers once, at construction.
//! * **Block entities live in a generational slab; positions are the saved
//!   identity.** A record is addressed at run time by a key that survives
//!   other records coming and going — [`slab::SlabKey`] pairs a slot with a
//!   generation, so a key whose record was removed reports itself dead
//!   instead of reading whoever moved into its slot. The position stands on
//!   the record itself: it is what serialisation writes, in position order,
//!   and it is what the next load turns back into keys. Keys never cross
//!   the file boundary because nothing about them outlives the process.
//!
//! **What this module does not catch:** whether the ids mean anything. A
//! chunk full of state ids past the end of the real block table is built
//! happily if the caller claims a large enough registry, and only the layer
//! above — which owns the tables — can disagree.

use crate::container::{NotInRegistry, PalettedContainer, Strategy};
use crate::coords::{BlockPos, ChunkPos};
use crate::heightmap::{HeightmapKind, HeightmapSet, WorldHeight};
use crate::light::LightArray;
use crate::slab::{Slab, SlabError, SlabKey};

/// One sixteen-cubed slice of a chunk: block states, biomes, and the two
/// light arrays.
///
/// This mirrors what a vanilla chunk section carries, minus the Y coordinate
/// itself — the chunk owns the vertical layout, so a section does not need to
/// remember where it hangs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    states: PalettedContainer,
    biomes: PalettedContainer,
    sky_light: LightArray,
    block_light: LightArray,
}

impl Section {
    /// Assemble a section from its parts.
    ///
    /// # Panics
    ///
    /// If either container is the wrong shape: block states must be the
    /// sixteen-cubed container and biomes the four-cubed one. Both mistakes
    /// produce sections whose coordinates mean something else than they
    /// claim, which is a caller bug worth stopping at the door.
    #[must_use]
    pub fn new(
        states: PalettedContainer,
        biomes: PalettedContainer,
        sky_light: LightArray,
        block_light: LightArray,
    ) -> Self {
        assert_eq!(
            states.strategy(),
            Strategy::BLOCK_STATES,
            "a section's block states are a 4096-cell container, and this one holds {}",
            states.len()
        );
        assert_eq!(
            biomes.strategy(),
            Strategy::BIOMES,
            "a section's biomes are a 64-cell container, and this one holds {}",
            biomes.len()
        );
        Self {
            states,
            biomes,
            sky_light,
            block_light,
        }
    }

    #[must_use]
    pub fn states(&self) -> &PalettedContainer {
        &self.states
    }

    #[must_use]
    pub fn states_mut(&mut self) -> &mut PalettedContainer {
        &mut self.states
    }

    #[must_use]
    pub fn biomes(&self) -> &PalettedContainer {
        &self.biomes
    }

    #[must_use]
    pub fn biomes_mut(&mut self) -> &mut PalettedContainer {
        &mut self.biomes
    }

    #[must_use]
    pub fn sky_light(&self) -> &LightArray {
        &self.sky_light
    }

    #[must_use]
    pub fn sky_light_mut(&mut self) -> &mut LightArray {
        &mut self.sky_light
    }

    #[must_use]
    pub fn block_light(&self) -> &LightArray {
        &self.block_light
    }

    #[must_use]
    pub fn block_light_mut(&mut self) -> &mut LightArray {
        &mut self.block_light
    }
}

/// A record attached to one block position — a chest's contents, a sign's
/// text, a spawner's configuration.
///
/// **This is the placeholder promised in the module documentation.** The real
/// record is tagged NBT whose shape depends on the block, and parsing it is
/// the NBT layer's job. What is settled now is where such records live — in
/// the owning chunk's generational slab, addressed by [`SlabKey`] at run
/// time — and what identifies them across a save: the position on the
/// record, which serialisation writes in sorted order and nothing else
/// about the key survives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockEntityHandle {
    /// The block this record belongs to. Part of the record rather than the
    /// slab's bookkeeping because it is the identity that crosses files:
    /// keys are run-time handles and mean nothing after a load.
    pub position: BlockPos,
    /// The block state id of the block that owns this entity. The owner's
    /// identity decides how the eventual NBT payload is interpreted, so it
    /// rides along even before the payload itself does.
    pub block_state: u32,
}

/// Why a block-entity operation did not happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockEntityError {
    /// Another record already stands at that block. One block carries one
    /// block entity; replacing one is remove-then-insert, so that both the
    /// old key's death and the new key's birth are visible to whoever held
    /// either.
    PositionOccupied { position: BlockPos },
    /// The key does not name a live record: its slot never existed here, or
    /// its occupancy has been replaced since the key was issued.
    UnknownKey(SlabError),
}

impl std::fmt::Display for BlockEntityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PositionOccupied { position } => write!(
                f,
                "{position} already holds a block entity; remove it before placing another"
            ),
            Self::UnknownKey(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for BlockEntityError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::UnknownKey(e) => Some(e),
            _ => None,
        }
    }
}

impl From<SlabError> for BlockEntityError {
    fn from(e: SlabError) -> Self {
        Self::UnknownKey(e)
    }
}

/// Everything one chunk column of a world carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pos: ChunkPos,
    world: WorldHeight,
    block_registry_size: u32,
    biome_registry_size: u32,
    sections: Box<[Section]>,
    heightmaps: HeightmapSet,
    block_entities: Slab<BlockEntityHandle>,
}

impl Chunk {
    /// A chunk where every block is `fill_state` and every biome cell is
    /// `fill_biome`.
    ///
    /// # Panics
    ///
    /// If the world's height is not a multiple of sixteen, or either fill
    /// value is outside its registry. Both are caller bugs: a world whose
    /// height does not tile into sections has no chunk format, and a fill id
    /// outside the registry would make every subsequent question about the
    /// chunk a lie.
    #[must_use]
    pub fn uniform(
        pos: ChunkPos,
        world: WorldHeight,
        block_registry_size: u32,
        biome_registry_size: u32,
        fill_state: u32,
        fill_biome: u32,
    ) -> Self {
        assert!(
            world.height() >= 16 && world.height() % 16 == 0,
            "a world {} rows tall does not divide into sixteen-row sections",
            world.height()
        );
        let count = world.height() / 16;
        Self {
            pos,
            world,
            block_registry_size,
            biome_registry_size,
            sections: (0..count)
                .map(|_| {
                    Section::new(
                        PalettedContainer::filled(
                            Strategy::BLOCK_STATES,
                            block_registry_size,
                            fill_state,
                        ),
                        PalettedContainer::filled(
                            Strategy::BIOMES,
                            biome_registry_size,
                            fill_biome,
                        ),
                        LightArray::new(),
                        LightArray::new(),
                    )
                })
                .collect(),
            heightmaps: HeightmapSet::new(world),
            block_entities: Slab::new(),
        }
    }

    /// Reassemble a chunk that was taken apart, as a reader does.
    ///
    /// This is the entry point [`NbtReader`] implementations call once they
    /// have turned a file's tags back into containers, maps and longs. It
    /// re-checks the structural facts a file could contradict — the section
    /// count against the world height, each container's shape and registry —
    /// because those facts are what keep every later accessor's arithmetic
    /// honest, and a reader that skipped them would trade a clear error at
    /// the door for a panic somewhere inside a hot loop.
    ///
    /// # Panics
    ///
    /// If the parts disagree with each other: the wrong number of sections,
    /// a container of the wrong shape, a container indexing a different
    /// registry than the one named, or a heightmap set built for another
    /// world. Each names what it found.
    #[must_use]
    pub fn from_parts(
        pos: ChunkPos,
        world: WorldHeight,
        block_registry_size: u32,
        biome_registry_size: u32,
        sections: Vec<Section>,
        heightmaps: HeightmapSet,
        block_entities: Slab<BlockEntityHandle>,
    ) -> Self {
        let expected = usize::try_from(world.height() / 16).expect("a sane world height");
        assert_eq!(
            sections.len(),
            expected,
            "{expected} sections tile a world {} rows tall, and {} were supplied",
            world.height(),
            sections.len()
        );
        for (index, section) in sections.iter().enumerate() {
            assert_eq!(
                section.states().strategy(),
                Strategy::BLOCK_STATES,
                "section {index} does not hold block states"
            );
            assert_eq!(
                section.biomes().strategy(),
                Strategy::BIOMES,
                "section {index} does not hold biomes"
            );
            assert_eq!(
                section.states().registry_size(),
                block_registry_size,
                "section {index} indexes a block registry of {}, but the chunk names {}",
                section.states().registry_size(),
                block_registry_size
            );
            assert_eq!(
                section.biomes().registry_size(),
                biome_registry_size,
                "section {index} indexes a biome registry of {}, but the chunk names {}",
                section.biomes().registry_size(),
                biome_registry_size
            );
        }
        for map in heightmaps.iter() {
            assert_eq!(
                map.world(),
                world,
                "{} was built for a {:?} world, not this one",
                map.kind().nbt_key(),
                map.world()
            );
        }
        Self {
            pos,
            world,
            block_registry_size,
            biome_registry_size,
            sections: sections.into_boxed_slice(),
            heightmaps,
            block_entities,
        }
    }

    #[must_use]
    pub const fn pos(&self) -> ChunkPos {
        self.pos
    }

    #[must_use]
    pub const fn world(&self) -> WorldHeight {
        self.world
    }

    /// How many block state ids the registry this chunk indexes holds.
    #[must_use]
    pub const fn block_registry_size(&self) -> u32 {
        self.block_registry_size
    }

    /// How many biome ids the registry this chunk indexes holds.
    #[must_use]
    pub const fn biome_registry_size(&self) -> u32 {
        self.biome_registry_size
    }

    /// How many sections the chunk is built from.
    #[must_use]
    pub const fn section_count(&self) -> usize {
        self.sections.len()
    }

    /// Which section a world y falls in.
    ///
    /// # Panics
    ///
    /// If `y` is outside the world.
    #[must_use]
    pub fn section_index(&self, y: i32) -> usize {
        assert!(
            y >= self.world.min_y() && y < self.world.max_y_exclusive(),
            "{y} is outside a world running from {} to {}",
            self.world.min_y(),
            self.world.max_y_exclusive()
        );
        (y - self.world.min_y()) as usize / 16
    }

    /// The section holding world y.
    ///
    /// # Panics
    ///
    /// If `y` is outside the world.
    #[must_use]
    pub fn section(&self, y: i32) -> &Section {
        &self.sections[self.section_index(y)]
    }

    /// The section holding world y, for writing.
    ///
    /// # Panics
    ///
    /// If `y` is outside the world.
    #[must_use]
    pub fn section_mut(&mut self, y: i32) -> &mut Section {
        let index = self.section_index(y);
        &mut self.sections[index]
    }

    /// Every section, bottom-up.
    #[must_use]
    pub fn sections(&self) -> &[Section] {
        &self.sections
    }

    /// The block state at world coordinates.
    ///
    /// # Panics
    ///
    /// If `x` or `z` is 16 or more, or `y` is outside the world.
    #[must_use]
    pub fn get_block(&self, x: u32, y: i32, z: u32) -> u32 {
        assert!(x < 16 && z < 16, "({x}, {z}) is outside a chunk column");
        let row = (y - self.world.min_y()) as u32;
        self.section(y).states().get_at(x, row % 16, z)
    }

    /// Put a block state at world coordinates, returning what was there.
    ///
    /// The heightmaps and the light arrays are deliberately untouched; see
    /// the decisions in the module documentation.
    ///
    /// # Panics
    ///
    /// If the coordinates are outside the chunk, or `state` is outside the
    /// block registry. Use [`Chunk::try_set_block`] where the id came from a
    /// file.
    pub fn set_block(&mut self, x: u32, y: i32, z: u32, state: u32) -> u32 {
        self.try_set_block(x, y, z, state)
            .unwrap_or_else(|e| panic!("{e}"))
    }

    /// [`Chunk::set_block`], with the out-of-registry case named.
    ///
    /// # Panics
    ///
    /// If the coordinates are outside the chunk.
    pub fn try_set_block(
        &mut self,
        x: u32,
        y: i32,
        z: u32,
        state: u32,
    ) -> Result<u32, NotInRegistry> {
        assert!(x < 16 && z < 16, "({x}, {z}) is outside a chunk column");
        let row = (y - self.world.min_y()) as u32;
        self.section_mut(y)
            .states_mut()
            .try_set(Strategy::BLOCK_STATES.index(x, row % 16, z), state)
    }

    /// The biome governing world coordinates.
    ///
    /// Biome cells are four blocks wide on every axis, so this is a lookup
    /// through `(x >> 2, y >> 2, z >> 2)` in the section's sixty-four cells.
    ///
    /// # Panics
    ///
    /// If `x` or `z` is 16 or more, or `y` is outside the world.
    #[must_use]
    pub fn get_biome(&self, x: u32, y: i32, z: u32) -> u32 {
        assert!(x < 16 && z < 16, "({x}, {z}) is outside a chunk column");
        let row = (y - self.world.min_y()) as u32;
        self.section(y)
            .biomes()
            .get_at(x >> 2, (row % 16) >> 2, z >> 2)
    }

    /// Name the biome governing world coordinates, returning what was there.
    ///
    /// # Panics
    ///
    /// If the coordinates are outside the chunk, or `biome` is outside the
    /// biome registry.
    pub fn set_biome(&mut self, x: u32, y: i32, z: u32, biome: u32) -> u32 {
        assert!(x < 16 && z < 16, "({x}, {z}) is outside a chunk column");
        let row = (y - self.world.min_y()) as u32;
        self.section_mut(y)
            .biomes_mut()
            .set_at(x >> 2, (row % 16) >> 2, z >> 2, biome)
    }

    #[must_use]
    pub const fn heightmaps(&self) -> &HeightmapSet {
        &self.heightmaps
    }

    #[must_use]
    pub const fn heightmaps_mut(&mut self) -> &mut HeightmapSet {
        &mut self.heightmaps
    }

    /// Recompute all six heightmaps from this chunk's own sections.
    ///
    /// The convenience wrapper around
    /// [`HeightmapSet::recompute_from_sections`](crate::heightmap::HeightmapSet::recompute_from_sections):
    /// the sections never have to be borrowed out by hand, and the predicate
    /// keeps the same per-kind shape as there.
    pub fn recompute_heightmaps<F>(&mut self, matches: F)
    where
        F: FnMut(HeightmapKind, u32) -> bool,
    {
        let sections: Vec<&PalettedContainer> = self.sections.iter().map(|s| s.states()).collect();
        self.heightmaps.recompute_from_sections(&sections, matches);
    }

    /// How many block entities this chunk carries.
    #[must_use]
    pub fn block_entity_count(&self) -> usize {
        self.block_entities.len()
    }

    /// Attach a block entity to a block in this chunk, returning the key it
    /// lives under.
    ///
    /// # Panics
    ///
    /// If `handle.position` is not a block of this chunk, or already holds
    /// another record. A record stored under a position this chunk does not
    /// own would be written into whichever region file happened to hold this
    /// chunk and silently vanish from the one that owns the block; two
    /// records on one block have no format meaning. Both are caller bugs,
    /// and [`Chunk::try_insert_block_entity`] names the second for paths
    /// where data, not code, decides.
    pub fn insert_block_entity(&mut self, handle: BlockEntityHandle) -> SlabKey {
        match self.try_insert_block_entity(handle) {
            Ok(key) => key,
            Err(e) => panic!("{e}"),
        }
    }

    /// [`Chunk::insert_block_entity`], with the occupied-position case named.
    ///
    /// # Panics
    ///
    /// If `handle.position` is not a block of this chunk. That check guards
    /// every later write of the chunk, so refusing here is refusing before
    /// the mistake can hide.
    pub fn try_insert_block_entity(
        &mut self,
        handle: BlockEntityHandle,
    ) -> Result<SlabKey, BlockEntityError> {
        self.require_contains(handle.position);
        if self.block_entity_at(handle.position).is_some() {
            return Err(BlockEntityError::PositionOccupied {
                position: handle.position,
            });
        }
        Ok(self.block_entities.insert(handle))
    }

    /// Take a block entity's record away, returning it.
    ///
    /// The returned record is detached from its key: the key dies with the
    /// removal, and any other holder of it learns so through
    /// [`BlockEntityError::UnknownKey`].
    pub fn remove_block_entity(
        &mut self,
        key: SlabKey,
    ) -> Result<BlockEntityHandle, BlockEntityError> {
        self.block_entities.remove(key).map_err(From::from)
    }

    /// A block entity's record, while its key is live.
    pub fn block_entity(&self, key: SlabKey) -> Result<&BlockEntityHandle, BlockEntityError> {
        self.block_entities.get(key).map_err(From::from)
    }

    /// The record standing at a block, with the key that addresses it.
    ///
    /// A scan over the slab's live records, which is the right shape for a
    /// collection a handful long. Code walking many positions wants to keep
    /// the keys it was handed at insert time instead.
    #[must_use]
    pub fn block_entity_at(&self, position: BlockPos) -> Option<(SlabKey, &BlockEntityHandle)> {
        self.block_entities
            .iter()
            .find(|(_, handle)| handle.position == position)
    }

    /// Every record, in ascending slot order.
    ///
    /// Deterministic for a given history of edits, and stable across a
    /// borrow — but not the order files are written in, because slot order
    /// remembers which records were removed along the way. Serialisation
    /// walks [`Chunk::block_entities_by_position`] instead, whose order is a
    /// function of the contents alone.
    pub fn block_entities(&self) -> impl Iterator<Item = (SlabKey, &BlockEntityHandle)> + '_ {
        self.block_entities.iter()
    }

    /// Every record, ordered by position: the order a file names them in.
    ///
    /// Saved bytes depend on this order being a pure function of the chunk's
    /// records — two chunks holding the same entities must save identically
    /// whatever order they were built in, and however many were added and
    /// removed since.
    pub fn block_entities_by_position(&self) -> impl Iterator<Item = &BlockEntityHandle> {
        let mut handles: Vec<&BlockEntityHandle> = self.block_entities.values().collect();
        handles.sort_unstable_by_key(|a| a.position);
        handles.into_iter()
    }

    fn require_contains(&self, pos: BlockPos) {
        assert!(
            pos.chunk() == self.pos,
            "{} belongs to chunk ({}, {}), not to ({}, {})",
            pos,
            pos.chunk().x,
            pos.chunk().z,
            self.pos.x,
            self.pos.z
        );
        assert!(
            pos.y >= self.world.min_y() && pos.y < self.world.max_y_exclusive(),
            "{pos} is outside a world running from {} to {}",
            self.world.min_y(),
            self.world.max_y_exclusive()
        );
    }
}

/// Write a chunk's serialised form.
///
/// **The seam.** There is no NBT on this branch; see the module
/// documentation for why, and for what the integrator owes this trait: an
/// implementation over `dust-nbt` producing Anvil-compatible field names,
/// replacing whatever stand-in the tests carry. The return value is the
/// decompressed payload exactly as [`crate::region::RegionFile::write_chunk`]
/// wants it — compression is the region layer's job and happens after.
pub trait NbtWriter {
    /// Why the chunk could not be written.
    type Error: std::error::Error;

    /// Serialise a chunk to the bytes a region payload holds.
    fn write_chunk(&self, chunk: &Chunk) -> Result<Vec<u8>, Self::Error>;
}

/// Read a chunk's serialised form.
///
/// The other half of [`NbtWriter`]'s seam. `pos` and `world` are supplied by
/// the caller rather than trusted to the bytes: a file's root compound is
/// somebody else's data, and the chunk's identity — which column, which
/// dimension shape — is decided by whoever opened the region file, not by
/// what the payload wishes it were. An implementation that finds the bytes
/// disagreeing with them reports that through its error rather than building
/// a chunk that lies about where it is.
pub trait NbtReader {
    /// Why the bytes are not a chunk.
    type Error: std::error::Error;

    /// Parse a chunk from the decompressed payload of a region file.
    fn read_chunk(
        &self,
        pos: ChunkPos,
        world: WorldHeight,
        nbt: &[u8],
    ) -> Result<Chunk, Self::Error>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small world: two sections, y from 0 to 32, every number in these
    /// tests checkable by eye.
    fn small_world() -> WorldHeight {
        WorldHeight::new(0, 32)
    }

    fn empty_chunk() -> Chunk {
        Chunk::uniform(ChunkPos::new(-3, 7), small_world(), 26_684, 64, 0, 1)
    }

    #[test]
    fn an_overworld_chunk_has_twenty_four_sections_and_knows_its_column() {
        let chunk = Chunk::uniform(
            ChunkPos::new(-1, 2),
            WorldHeight::OVERWORLD,
            26_684,
            64,
            0,
            1,
        );
        assert_eq!(chunk.pos(), ChunkPos::new(-1, 2));
        assert_eq!(chunk.world(), WorldHeight::OVERWORLD);
        assert_eq!(chunk.section_count(), 24);
        assert_eq!(chunk.block_entity_count(), 0);
        // Every section answers for its own sixteen rows, top to bottom.
        for index in 0..24usize {
            let y = chunk.world().min_y() + index as i32 * 16;
            assert_eq!(chunk.section_index(y), index);
            assert_eq!(chunk.section_index(y + 15), index);
        }
        assert!((0..4096).all(|i| chunk.sections()[12].states().get(i) == 0));
        assert!((0..64).all(|i| chunk.sections()[12].biomes().get(i) == 1));
    }

    #[test]
    fn blocks_land_in_the_section_their_y_names() {
        let mut chunk = empty_chunk();
        // One block at the very bottom, one straddling the section border,
        // one at the ceiling.
        assert_eq!(chunk.set_block(3, 0, 4, 42), 0);
        assert_eq!(chunk.set_block(5, 15, 6, 43), 0);
        assert_eq!(chunk.set_block(5, 16, 6, 44), 0);
        assert_eq!(chunk.set_block(7, 31, 8, 45), 0);

        assert_eq!(chunk.get_block(3, 0, 4), 42);
        assert_eq!(chunk.get_block(5, 15, 6), 43);
        assert_eq!(chunk.get_block(5, 16, 6), 44);
        assert_eq!(chunk.get_block(7, 31, 8), 45);

        // The same local coordinates one row up or down name other blocks.
        assert_eq!(chunk.get_block(5, 14, 6), 0);
        assert_eq!(chunk.get_block(5, 17, 6), 0);

        // And the sections hold them where the container arithmetic says.
        assert_eq!(chunk.section(0).states().get_at(3, 0, 4), 42);
        assert_eq!(chunk.section(15).states().get_at(5, 15, 6), 43);
        assert_eq!(chunk.section(16).states().get_at(5, 0, 6), 44);
        assert_eq!(chunk.section(31).states().get_at(7, 15, 8), 45);

        assert_eq!(
            chunk.set_block(3, 0, 4, 9),
            42,
            "the previous state comes back"
        );
    }

    #[test]
    fn biome_cells_are_four_blocks_wide_on_every_axis() {
        let mut chunk = empty_chunk();
        // Block (5, 17, 9): section-local row 1, so the cell is
        // ((5>>2), (1>>2), (9>>2)) = (1, 0, 2) in the second section.
        assert_eq!(chunk.set_biome(5, 17, 9, 33), 1);
        assert_eq!(chunk.get_biome(5, 17, 9), 33);
        assert_eq!(
            chunk.section(17).biomes().get_at(1, 0, 2),
            33,
            "the cell arithmetic is visible in the section"
        );
        // Three neighbours inside the same four-block cube share its biome.
        assert_eq!(chunk.get_biome(4, 16, 8), 33);
        assert_eq!(chunk.get_biome(7, 19, 11), 33);
        // The next cube over does not.
        assert_eq!(chunk.get_biome(8, 16, 8), 1);
        assert_eq!(chunk.get_biome(4, 20, 8), 1);
        assert_eq!(chunk.get_biome(4, 16, 12), 1);
    }

    #[test]
    fn a_block_entity_belongs_to_this_chunk_or_it_is_refused() {
        let mut chunk = empty_chunk();
        let here = BlockPos::new(-48 + 5, 10, 112 + 3);
        assert_eq!(here.chunk(), ChunkPos::new(-3, 7));

        let key = chunk.insert_block_entity(BlockEntityHandle {
            position: here,
            block_state: 91,
        });
        assert_eq!(chunk.block_entity(key).map(|h| h.block_state), Ok(91));
        assert_eq!(
            chunk
                .block_entities_by_position()
                .map(|h| h.position)
                .collect::<Vec<_>>(),
            vec![here],
            "the record stands where it was put"
        );

        let err = chunk
            .try_insert_block_entity(BlockEntityHandle {
                position: here,
                block_state: 92,
            })
            .expect_err("one block, one record");
        assert_eq!(err, BlockEntityError::PositionOccupied { position: here });
        assert_eq!(chunk.block_entity_count(), 1, "the refusal stored nothing");
        assert_eq!(
            chunk.remove_block_entity(key).map(|h| h.block_state),
            Ok(91)
        );
        // The removal retired the slot's occupancy, and the dead key says so
        // by name rather than answering with whoever comes next.
        assert_eq!(
            chunk.block_entity(key),
            Err(BlockEntityError::UnknownKey(SlabError::StaleGeneration {
                slot: key.slot(),
                generation: key.generation(),
                current_generation: key.generation() + 1,
            }))
        );

        // Another chunk's column, and a y outside this world.
        let elsewhere = BlockPos::new(-32, 10, 115);
        assert_ne!(elsewhere.chunk(), ChunkPos::new(-3, 7));
        let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            chunk.insert_block_entity(BlockEntityHandle {
                position: elsewhere,
                block_state: 1,
            });
        }))
        .expect_err("the position belongs to another chunk");
        let message = err.downcast_ref::<String>().cloned().unwrap_or_default();
        assert!(message.contains("belongs to"), "{message}");

        let above = BlockPos::new(-43, 32, 115);
        let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            chunk.insert_block_entity(BlockEntityHandle {
                position: above,
                block_state: 1,
            });
        }))
        .expect_err("y == max is outside this world");
        let message = err.downcast_ref::<String>().cloned().unwrap_or_default();
        assert!(message.contains("outside a world"), "{message}");
    }

    #[test]
    fn records_leave_in_position_order_and_keys_survive_each_other() {
        // Saved bytes depend on the position order; the slab's slot order is
        // a different thing entirely and must not leak into it.
        let mut chunk = empty_chunk();
        let scattered = [
            BlockPos::new(-46, 20, 118),
            BlockPos::new(-48, 5, 113),
            BlockPos::new(-44, 31, 112),
            BlockPos::new(-48, 5, 112),
        ];
        for (n, pos) in scattered.iter().enumerate() {
            chunk.insert_block_entity(BlockEntityHandle {
                position: *pos,
                block_state: 100 + n as u32,
            });
        }

        let positions: Vec<BlockPos> = chunk
            .block_entities_by_position()
            .map(|h| h.position)
            .collect();
        let mut sorted = positions.clone();
        sorted.sort_unstable();
        assert_eq!(positions, sorted);
        assert_eq!(positions[0], BlockPos::new(-48, 5, 112), "lowest x first");

        // Remove the lowest-position record and add another; the save order
        // follows contents, not history.
        let lowest = BlockPos::new(-48, 5, 112);
        let (key, handle) = chunk.block_entity_at(lowest).expect("still standing");
        let state = handle.block_state;
        chunk.remove_block_entity(key).expect("live");
        chunk.insert_block_entity(BlockEntityHandle {
            position: BlockPos::new(-47, 8, 114),
            block_state: state + 50,
        });

        let positions: Vec<BlockPos> = chunk
            .block_entities_by_position()
            .map(|h| h.position)
            .collect();
        assert_eq!(
            positions,
            vec![
                BlockPos::new(-48, 5, 113),
                BlockPos::new(-47, 8, 114),
                BlockPos::new(-46, 20, 118),
                BlockPos::new(-44, 31, 112),
            ],
            "position order over what remains"
        );
    }

    #[test]
    fn recomputing_heightmaps_reads_the_chunks_own_sections() {
        let mut chunk = empty_chunk();
        chunk.set_block(3, 7, 4, 100);
        chunk.set_block(9, 25, 2, 101);
        chunk.recompute_heightmaps(|kind, state| match kind {
            HeightmapKind::WorldSurface | HeightmapKind::MotionBlocking => state != 0,
            _ => false,
        });

        assert_eq!(
            chunk
                .heightmaps()
                .get(HeightmapKind::WorldSurface)
                .first_available(3, 4),
            8
        );
        assert_eq!(
            chunk
                .heightmaps()
                .get(HeightmapKind::MotionBlocking)
                .first_available(9, 2),
            26
        );
        assert_eq!(
            chunk
                .heightmaps()
                .get(HeightmapKind::OceanFloor)
                .first_available(3, 4),
            0,
            "kinds whose predicate counts nothing stay at the floor"
        );
    }

    #[test]
    fn coordinates_outside_the_chunk_are_named_rather_than_wrapped() {
        let mut chunk = empty_chunk();

        // Masking an out-of-range coordinate into range would silently write
        // a different block, so every accessor refuses by name instead.
        fn refused(mut act: impl FnMut(), what: &str) -> String {
            let message =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(&mut act)).expect_err(what);
            message
                .downcast_ref::<String>()
                .cloned()
                .unwrap_or_default()
        }

        let message = refused(
            || {
                let _ = chunk.set_block(16, 0, 0, 1);
            },
            "x == 16 is outside",
        );
        assert!(message.contains("(16, 0)"), "{message}");
        let message = refused(
            || {
                let _ = chunk.get_block(0, 31, 16);
            },
            "z == 16 is outside",
        );
        assert!(message.contains("(0, 16)"), "{message}");
        let message = refused(
            || {
                let _ = chunk.set_block(0, 32, 0, 1);
            },
            "y past the ceiling is outside",
        );
        assert!(message.contains("32"), "{message}");
        let message = refused(
            || {
                let _ = chunk.get_block(0, -1, 0);
            },
            "y under the floor",
        );
        assert!(message.contains("-1"), "{message}");
        let message = refused(
            || {
                let _ = chunk.set_biome(16, 0, 0, 2);
            },
            "biomes too",
        );
        assert!(message.contains("(16, 0)"), "{message}");
        let message = refused(
            || {
                let _ = chunk.section_index(-1);
            },
            "the section lookup too",
        );
        assert!(message.contains("-1"), "{message}");
    }

    #[test]
    fn reassembling_from_parts_that_disagree_with_each_other_is_named() {
        let world = small_world();
        let good = Section::new(
            PalettedContainer::filled(Strategy::BLOCK_STATES, 26_684, 0),
            PalettedContainer::filled(Strategy::BIOMES, 64, 1),
            LightArray::new(),
            LightArray::new(),
        );
        let wrong_registry = Section::new(
            PalettedContainer::filled(Strategy::BLOCK_STATES, 999, 0),
            PalettedContainer::filled(Strategy::BIOMES, 64, 1),
            LightArray::new(),
            LightArray::new(),
        );

        // Two sections tile this world; one does not.
        let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = Chunk::from_parts(
                ChunkPos::new(0, 0),
                world,
                26_684,
                64,
                vec![good.clone()],
                HeightmapSet::new(world),
                Slab::new(),
            );
        }))
        .expect_err("one section cannot tile thirty-two rows");
        let message = err.downcast_ref::<String>().cloned().unwrap_or_default();
        assert!(message.contains("2 sections"), "{message}");

        let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = Chunk::from_parts(
                ChunkPos::new(0, 0),
                world,
                26_684,
                64,
                vec![wrong_registry, good.clone()],
                HeightmapSet::new(world),
                Slab::new(),
            );
        }))
        .expect_err("the section indexes another registry");
        let message = err.downcast_ref::<String>().cloned().unwrap_or_default();
        assert!(message.contains("999"), "{message}");

        // A heightmap set built for another world would answer questions
        // about ys this chunk does not have.
        let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = Chunk::from_parts(
                ChunkPos::new(0, 0),
                world,
                26_684,
                64,
                vec![good.clone(), good.clone()],
                HeightmapSet::new(WorldHeight::OVERWORLD),
                Slab::new(),
            );
        }))
        .expect_err("the heightmaps are for another world");
        let message = err.downcast_ref::<String>().cloned().unwrap_or_default();
        assert!(message.contains("WORLD_SURFACE_WG"), "{message}");
        assert!(message.contains("not this one"), "{message}");
    }
}
