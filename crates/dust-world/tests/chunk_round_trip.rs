//! A chunk's journey: memory, through the NBT seam, into a region file, back
//! out, and equal all the way.
//!
//! The chunk exists; the region layer exists; what joins them is a byte run
//! that is, in the real server, compressed NBT. There is no NBT on this
//! branch -- `dust-nbt` will be merged by someone else's commit -- so these
//! tests carry chunks across that seam with a stand-in writer and reader
//! defined below. The stand-in proves everything that can be proved without
//! real tags:
//!
//! * a whole chunk survives the trip both ways, through an in-memory store
//!   *and* a file on disk;
//! * saving is deterministic: the same chunk saved twice writes identical
//!   bytes, and two chunks with identical contents built by different routes
//!   write identical bytes too -- which is where hash-map iteration order
//!   would leak into saved worlds if the writer were careless;
//! * a reader refuses bytes that are not its own rather than inventing a
//!   chunk.
//!
//! **What the stand-in cannot prove:** that Dust's files are Anvil files.
//! That needs field names, tag types and vanilla's exact compound layout,
//! all of which belong to the `dust-nbt` implementation of
//! [`NbtWriter`](dust_world::NbtWriter) and [`NbtReader`](dust_world::NbtReader).
//! When it lands, it replaces `DirectFormat` below and these tests keep
//! running unchanged against it.
//!
//! One consequence shows up as a helper rather than an `==`:
//! `Chunk`'s derived equality includes each container's *in-memory* palette
//! shape, and a serialised chunk deliberately does not carry that shape --
//! vanilla re-palettes on every write, entries in first-appearance order
//! over the cells. So a chunk that went through a file and came back holds
//! the same blocks, biomes, light, heightmaps and records with its palettes
//! rebuilt canonically. The tests call that equivalent, and check it cell by
//! cell; the determinism tests separately pin that the canonical form is a
//! pure function of those contents.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use dust_world::chunk::Section;
use dust_world::heightmap::{HeightmapKind, HeightmapSet};
use dust_world::light::LightArray;
use dust_world::region::{Compression, MemoryStore, RegionFile};
use dust_world::{
    BlockEntityHandle, BlockPos, Chunk, ChunkPos, NbtReader, NbtWriter, PalettedContainer,
    RegionPos, Strategy, WorldHeight,
};

const REGION: RegionPos = RegionPos::new(-4, 5);
const BLOCK_REGISTRY: u32 = 26_684;
const BIOME_REGISTRY: u32 = 64;

/// A small world with three sections, y from -32 to 16.
fn world() -> WorldHeight {
    WorldHeight::new(-32, 48)
}

// ---------------------------------------------------------------------------
// DirectFormat: the stand-in named in the module documentation.
//
// It is flat binary, not NBT, on purpose. Every field it writes comes off the
// public Chunk API in a fixed order, which is exactly what makes the
// determinism tests meaningful: if any ordering inside Chunk were unstable --
// a hash map visited in insertion order, say -- two logically identical
// chunks would encode differently and the tests would catch it here, before
// a saved world ever depended on it.
// ---------------------------------------------------------------------------

struct DirectFormat;

#[derive(Debug)]
enum CodecError {
    NotOurs(&'static str),
    Truncated,
    Container(dust_world::ContainerError),
    Bits(dust_world::BitStorageError),
    Light(dust_world::LightArrayError),
}

impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotOurs(what) => write!(f, "these bytes are not a DirectFormat chunk: {what}"),
            Self::Truncated => f.write_str("the bytes end inside a chunk"),
            Self::Container(e) => write!(f, "{e}"),
            Self::Bits(e) => write!(f, "{e}"),
            Self::Light(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for CodecError {}

impl From<dust_world::ContainerError> for CodecError {
    fn from(e: dust_world::ContainerError) -> Self {
        Self::Container(e)
    }
}

impl From<dust_world::BitStorageError> for CodecError {
    fn from(e: dust_world::BitStorageError) -> Self {
        Self::Bits(e)
    }
}

impl From<dust_world::LightArrayError> for CodecError {
    fn from(e: dust_world::LightArrayError) -> Self {
        Self::Light(e)
    }
}

const MAGIC: [u8; 4] = *b"DWCK";
const VERSION: u32 = 1;

/// One container as the file holds it: entries beside packed indices.
fn put_container(out: &mut Vec<u8>, container: &PalettedContainer) {
    let (entries, data) = container.to_parts();
    out.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    for entry in &entries {
        out.extend_from_slice(&entry.to_be_bytes());
    }
    match &data {
        None => out.extend_from_slice(&0u32.to_be_bytes()),
        Some(longs) => {
            out.extend_from_slice(&(longs.len() as u32).to_be_bytes());
            for long in longs {
                out.extend_from_slice(&long.to_be_bytes());
            }
        }
    }
}

fn get_container(
    cursor: &mut Cursor,
    strategy: Strategy,
    registry: u32,
) -> Result<PalettedContainer, CodecError> {
    let count = cursor.u32()? as usize;
    if count > 4096 {
        return Err(CodecError::NotOurs("an impossible palette length"));
    }
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        entries.push(cursor.u32()?);
    }
    let data = match cursor.u32()? {
        0 => None,
        n => Some(
            (0..n)
                .map(|_| cursor.i64())
                .collect::<Result<Vec<_>, _>>()?,
        ),
    };
    PalettedContainer::from_parts(strategy, registry, &entries, data).map_err(CodecError::from)
}

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Cursor<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], CodecError> {
        let end = self.at.checked_add(n).ok_or(CodecError::Truncated)?;
        if end > self.bytes.len() {
            return Err(CodecError::Truncated);
        }
        let slice = &self.bytes[self.at..end];
        self.at = end;
        Ok(slice)
    }

    fn u32(&mut self) -> Result<u32, CodecError> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn i32(&mut self) -> Result<i32, CodecError> {
        Ok(self.u32()? as i32)
    }

    fn i64(&mut self) -> Result<i64, CodecError> {
        let b = self.take(8)?;
        Ok(i64::from_be_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    fn long_array(&mut self) -> Result<Vec<i64>, CodecError> {
        let count = self.u32()? as usize;
        if count > 1024 {
            return Err(CodecError::NotOurs("an impossible array length"));
        }
        (0..count).map(|_| self.i64()).collect()
    }
}

fn put_long_array(out: &mut Vec<u8>, longs: &[i64]) {
    out.extend_from_slice(&(longs.len() as u32).to_be_bytes());
    for long in longs {
        out.extend_from_slice(&long.to_be_bytes());
    }
}

impl NbtWriter for DirectFormat {
    type Error = CodecError;

    fn write_chunk(&self, chunk: &Chunk) -> Result<Vec<u8>, Self::Error> {
        let mut out = Vec::new();
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&VERSION.to_be_bytes());

        // Identity first, because a reader needs to know where it landed
        // before it can mean anything else.
        out.extend_from_slice(&chunk.pos().x.to_be_bytes());
        out.extend_from_slice(&chunk.pos().z.to_be_bytes());
        out.extend_from_slice(&chunk.world().min_y().to_be_bytes());
        out.extend_from_slice(&chunk.world().height().to_be_bytes());

        // Registries, since ids are meaningless without their tables' size.
        out.extend_from_slice(&chunk.block_registry_size().to_be_bytes());
        out.extend_from_slice(&chunk.biome_registry_size().to_be_bytes());

        // Sections, bottom-up: states, biomes, sky light, block light.
        out.extend_from_slice(&(chunk.section_count() as u32).to_be_bytes());
        for section in chunk.sections() {
            put_container(&mut out, section.states());
            put_container(&mut out, section.biomes());
            out.extend_from_slice(section.sky_light().as_bytes());
            out.extend_from_slice(section.block_light().as_bytes());
        }

        // Heightmaps in the crate's declared order, whatever that order is:
        // the reader walks the same list, so no names are needed.
        for map in chunk.heightmaps().iter() {
            put_long_array(&mut out, map.as_longs());
        }

        // Block entities in key order, which BTreeMap guarantees.
        out.extend_from_slice(&(chunk.block_entities().len() as u32).to_be_bytes());
        for (pos, handle) in chunk.block_entities() {
            out.extend_from_slice(&pos.x.to_be_bytes());
            out.extend_from_slice(&pos.y.to_be_bytes());
            out.extend_from_slice(&pos.z.to_be_bytes());
            out.extend_from_slice(&handle.block_state.to_be_bytes());
        }
        Ok(out)
    }
}

impl NbtReader for DirectFormat {
    type Error = CodecError;

    fn read_chunk(
        &self,
        pos: ChunkPos,
        world: WorldHeight,
        nbt: &[u8],
    ) -> Result<Chunk, Self::Error> {
        let mut cursor = Cursor { bytes: nbt, at: 0 };
        if cursor.take(4)? != MAGIC {
            return Err(CodecError::NotOurs("wrong magic"));
        }
        if cursor.u32()? != VERSION {
            return Err(CodecError::NotOurs("unknown version"));
        }
        let file_x = cursor.i32()?;
        let file_z = cursor.i32()?;
        if (file_x, file_z) != (pos.x, pos.z) {
            return Err(CodecError::NotOurs("the payload names another chunk"));
        }
        let min_y = cursor.i32()?;
        let height = cursor.u32()?;
        if min_y != world.min_y() || height != world.height() {
            return Err(CodecError::NotOurs(
                "the payload was written for another world",
            ));
        }
        let block_registry = cursor.u32()?;
        let biome_registry = cursor.u32()?;

        let section_count = cursor.u32()? as usize;
        let mut sections = Vec::with_capacity(section_count);
        for _ in 0..section_count {
            let states = get_container(&mut cursor, Strategy::BLOCK_STATES, block_registry)?;
            let biomes = get_container(&mut cursor, Strategy::BIOMES, biome_registry)?;
            let sky = LightArray::from_bytes(cursor.take(dust_world::light::BYTES)?)?;
            let block = LightArray::from_bytes(cursor.take(dust_world::light::BYTES)?)?;
            sections.push(Section::new(states, biomes, sky, block));
        }

        let mut heightmaps = HeightmapSet::new(world);
        for kind in HeightmapKind::ALL {
            let longs = cursor.long_array()?;
            let map =
                dust_world::Heightmap::from_longs(kind, world, longs).map_err(CodecError::from)?;
            *heightmaps.get_mut(kind) = map;
        }

        let entity_count = cursor.u32()? as usize;
        let mut entities = BTreeMap::new();
        for _ in 0..entity_count {
            let x = cursor.i32()?;
            let y = cursor.i32()?;
            let z = cursor.i32()?;
            let state = cursor.u32()?;
            entities.insert(
                BlockPos::new(x, y, z),
                BlockEntityHandle { block_state: state },
            );
        }

        Ok(Chunk::from_parts(
            pos,
            world,
            block_registry,
            biome_registry,
            sections,
            heightmaps,
            entities,
        ))
    }
}

// ---------------------------------------------------------------------------
// Fixtures.
// ---------------------------------------------------------------------------

/// An interesting chunk: every section doing something different, heightmaps
/// recomputed from its own blocks, light set by hand, block entities in it.
///
/// Air is id 0 and the fill; everything else placed here is deliberate.
fn interesting_chunk(pos: ChunkPos) -> Chunk {
    let mut chunk = Chunk::uniform(pos, world(), BLOCK_REGISTRY, BIOME_REGISTRY, 0, 2);

    // Bottom section: a bedrock floor and a stone pillar, still linear-tier.
    for x in 0..16u32 {
        for z in 0..16u32 {
            chunk.set_block(x, -32, z, 10 + (x + z) % 3);
        }
    }
    chunk.set_block(7, -31, 7, 42);
    chunk.set_block(7, -30, 7, 43);

    // Middle section: enough distinct states to cross into the hashed tier,
    // so the round trip exercises a multi-entry palette and its packing.
    let mid = chunk.section_mut(-16).states_mut();
    for cell in 0..300usize {
        mid.set(cell, ((cell * 37) % 250) as u32 * 6 + 20);
    }

    // Top section: one block at the very top row.
    chunk.set_block(15, 15, 0, 77);

    // Biomes: four distinct across the middle section's cells.
    let biomes = chunk.section_mut(-16).biomes_mut();
    biomes.set_at(0, 0, 0, 9);
    biomes.set_at(3, 3, 3, 17);
    biomes.set_at(1, 2, 2, 33);

    // Light: a hand-set patch in each array of each section.
    for index in 0..chunk.section_count() {
        let base = chunk.world().min_y() + index as i32 * 16;
        let section = chunk.section_mut(base);
        section.sky_light_mut().set(3, 4, 5, 15);
        section.sky_light_mut().set(3, 4, 6, 14);
        section.block_light_mut().set(8, 9, 8, 7);
    }

    // Heightmaps from the blocks just placed, per-kind predicates. The
    // ocean-floor kinds count only the pillar's base block, deliberately
    // narrower than "any solid thing", so different maps demonstrably end up
    // with different numbers over the same ground.
    chunk.recompute_heightmaps(|kind, state| match kind {
        HeightmapKind::WorldSurfaceWg | HeightmapKind::WorldSurface => state != 0,
        HeightmapKind::OceanFloorWg | HeightmapKind::OceanFloor => state == 42,
        HeightmapKind::MotionBlocking => state != 0 && state != 43,
        HeightmapKind::MotionBlockingNoLeaves => state != 0,
    });

    chunk.insert_block_entity(
        BlockPos::new(pos.x * 16 + 7, -30, pos.z * 16 + 7),
        BlockEntityHandle { block_state: 43 },
    );
    chunk.insert_block_entity(
        BlockPos::new(pos.x * 16 + 15, 15, pos.z * 16),
        BlockEntityHandle { block_state: 77 },
    );
    chunk
}

fn encode(chunk: &Chunk) -> Vec<u8> {
    DirectFormat
        .write_chunk(chunk)
        .expect("a sound chunk encodes")
}

/// The equivalence a round trip can honestly promise: identical identity,
/// identical contents cell by cell, identical heightmaps, light and records.
/// Deliberately *not* `==`, which also compares each container's in-memory
/// palette -- the thing serialisation normalises away. See the module
/// documentation.
fn assert_chunks_equivalent(left: &Chunk, right: &Chunk) {
    assert_eq!(left.pos(), right.pos());
    assert_eq!(left.world(), right.world());
    assert_eq!(left.block_registry_size(), right.block_registry_size());
    assert_eq!(left.biome_registry_size(), right.biome_registry_size());
    assert_eq!(left.section_count(), right.section_count());
    for index in 0..left.section_count() {
        let (a, b) = (&left.sections()[index], &right.sections()[index]);
        assert_eq!(a.states().len(), b.states().len());
        for cell in 0..a.states().len() {
            assert_eq!(
                a.states().get(cell),
                b.states().get(cell),
                "section {index}, block cell {cell}"
            );
        }
        for cell in 0..a.biomes().len() {
            assert_eq!(
                a.biomes().get(cell),
                b.biomes().get(cell),
                "section {index}, biome cell {cell}"
            );
        }
        assert_eq!(a.sky_light(), b.sky_light(), "section {index} sky light");
        assert_eq!(
            a.block_light(),
            b.block_light(),
            "section {index} block light"
        );
    }
    for kind in HeightmapKind::ALL {
        assert_eq!(
            left.heightmaps().get(kind).as_longs(),
            right.heightmaps().get(kind).as_longs(),
            "{}",
            kind.nbt_key()
        );
    }
    assert_eq!(left.block_entities(), right.block_entities());
}

fn cycle_through_memory(chunk: &Chunk, timestamp: i32) -> Chunk {
    let bytes = encode(chunk);
    let mut file = RegionFile::open(MemoryStore::new(), REGION).expect("opens");
    file.write_chunk(
        chunk.pos(),
        &dust_world::ChunkPayload::from_bytes(bytes),
        Compression::Zlib,
        timestamp,
    )
    .expect("writes");
    let raw = file.into_store();
    let mut reopened = RegionFile::open(raw, REGION).expect("reopens");
    let stored = reopened
        .read_chunk(chunk.pos())
        .expect("reads")
        .expect("present");
    DirectFormat
        .read_chunk(chunk.pos(), chunk.world(), stored.as_bytes())
        .expect("decodes")
}

#[test]
fn a_chunk_survives_an_in_memory_round_trip_through_the_region_store() {
    let pos = REGION.chunk_at(3, 11);
    let original = interesting_chunk(pos);
    let restored = cycle_through_memory(&original, 1_234_567_890);

    assert_chunks_equivalent(&restored, &original);

    // Spot-check the parts that matter most, in case the helper ever grows a
    // blind spot. The middle section's junk layer tops out at y = -16 in
    // every column (its cells cover row 0 of that section), so maps that
    // count any non-air block read -15; the ocean-floor predicate matches
    // only the pillar's base at -31, so it reads -30 -- and proves a deep
    // block is found through air above it.
    assert_eq!(
        restored.section(-16).states().get(299),
        original.section(-16).states().get(299)
    );
    for kind in [HeightmapKind::WorldSurface, HeightmapKind::MotionBlocking] {
        assert_eq!(
            restored.heightmaps().get(kind).first_available(7, 7),
            -15,
            "{}: the junk layer at -16 is what counts",
            kind.nbt_key()
        );
    }
    for kind in [HeightmapKind::OceanFloorWg, HeightmapKind::OceanFloor] {
        assert_eq!(
            restored.heightmaps().get(kind).first_available(7, 7),
            -30,
            "{}: the counted block is at -31",
            kind.nbt_key()
        );
    }
    assert_eq!(restored.block_entities().len(), 2);
}

#[test]
fn a_chunk_survives_a_round_trip_through_a_file_on_disk() {
    struct Scratch(PathBuf);
    impl Scratch {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "dust-world-chunk-{name}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("scratch directory");
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    let scratch = Scratch::new("disk");
    let pos = REGION.chunk_at(31, 31);
    let original = interesting_chunk(pos);

    {
        let bytes = encode(&original);
        let mut file = RegionFile::open_in(scratch.path(), REGION).expect("creates");
        file.write_chunk(
            pos,
            &dust_world::ChunkPayload::from_bytes(bytes),
            Compression::Zlib,
            77,
        )
        .expect("writes");
    }

    let mut reopened = RegionFile::open_in(scratch.path(), REGION).expect("reopens");
    let stored = reopened.read_chunk(pos).expect("reads").expect("present");
    let restored = DirectFormat
        .read_chunk(pos, world(), stored.as_bytes())
        .expect("decodes");
    assert_chunks_equivalent(&restored, &original);
}

#[test]
fn saving_the_same_chunk_twice_writes_identical_files_and_identical_payloads() {
    let chunk = interesting_chunk(REGION.chunk_at(9, 9));

    let first = encode(&chunk);
    let second = encode(&chunk);
    assert_eq!(first, second, "encoding is a pure function of the chunk");

    let save = || -> Vec<u8> {
        let mut file = RegionFile::open(MemoryStore::new(), REGION).expect("opens");
        file.write_chunk(
            chunk.pos(),
            &dust_world::ChunkPayload::from_bytes(encode(&chunk)),
            Compression::Zlib,
            42,
        )
        .expect("writes");
        file.into_store().into_bytes()
    };
    assert_eq!(save(), save(), "two saves of one chunk are one file");
}

#[test]
fn chunks_with_identical_contents_but_different_histories_save_identically() {
    // The determinism test with teeth. Both chunks below hold exactly the
    // same blocks in the middle section; one was written straight there, the
    // other was dragged up past the hashed tier with junk and then rewritten.
    // Their palettes in memory remember none of that -- and their serialised
    // form must not either.
    let pos = REGION.chunk_at(12, 2);
    let pattern: Vec<u32> = (0..4096)
        .map(|cell| (cell * 53 % 400) as u32 * 9 + 21)
        .collect();

    let mut straight = Chunk::uniform(pos, world(), BLOCK_REGISTRY, BIOME_REGISTRY, 1, 2);
    {
        let states = straight.section_mut(-16).states_mut();
        for (cell, value) in pattern.iter().enumerate() {
            states.set(cell, *value);
        }
    }

    let mut scenic = Chunk::uniform(pos, world(), BLOCK_REGISTRY, BIOME_REGISTRY, 1, 2);
    {
        let states = scenic.section_mut(-16).states_mut();
        // Three hundred junk states force the global palette...
        for cell in 0..300usize {
            states.set(cell, cell as u32 * 61 + 900);
        }
        // ...then every cell is rewritten to the target pattern, scanning
        // cells in a different order than `straight` used.
        for step in 0..4096usize {
            let cell = (step * 2531 + 7) % 4096;
            states.set(cell, pattern[cell]);
        }
    }

    // Same treatment for the block-entity list: inserted in opposite orders,
    // which only matters if the carrying structure's iteration is stable.
    let spots = [
        BlockPos::new(pos.x * 16 + 1, -20, pos.z * 16 + 2),
        BlockPos::new(pos.x * 16 + 3, -10, pos.z * 16 + 4),
        BlockPos::new(pos.x * 16 + 5, 0, pos.z * 16 + 6),
    ];
    for spot in spots {
        straight.insert_block_entity(spot, BlockEntityHandle { block_state: 5 });
    }
    for spot in spots.iter().rev() {
        scenic.insert_block_entity(*spot, BlockEntityHandle { block_state: 5 });
    }

    assert_eq!(
        encode(&straight),
        encode(&scenic),
        "same contents, same bytes"
    );
}

#[test]
fn many_chunks_across_a_region_round_trip_together() {
    let positions = [
        REGION.chunk_at(0, 0),
        REGION.chunk_at(31, 0),
        REGION.chunk_at(0, 31),
        REGION.chunk_at(31, 31),
        REGION.chunk_at(17, 8),
        REGION.chunk_at(5, 22),
    ];

    let mut file = RegionFile::open(MemoryStore::new(), REGION).expect("opens");
    for (index, pos) in positions.iter().enumerate() {
        let mut chunk = interesting_chunk(*pos);
        // Make each chunk differ beyond its coordinates: a distinct extra
        // block per index, so a header pointing at the wrong sectors yields
        // the wrong chunk rather than a plausible twin.
        chunk.set_block(
            index as u32 % 16,
            0,
            (index * 7) as u32 % 16,
            500 + index as u32,
        );
        let bytes = encode(&chunk);
        file.write_chunk(
            *pos,
            &dust_world::ChunkPayload::from_bytes(bytes),
            Compression::Zlib,
            index as i32,
        )
        .expect("writes");
    }
    assert_eq!(file.chunk_count(), positions.len());

    let mut reopened = RegionFile::open(file.into_store(), REGION).expect("reopens");
    for (index, pos) in positions.iter().enumerate() {
        let mut expected = interesting_chunk(*pos);
        expected.set_block(
            index as u32 % 16,
            0,
            (index * 7) as u32 % 16,
            500 + index as u32,
        );
        let stored = reopened.read_chunk(*pos).expect("reads").expect("present");
        let restored = DirectFormat
            .read_chunk(*pos, world(), stored.as_bytes())
            .expect("decodes");
        assert_chunks_equivalent(&restored, &expected);
        assert_eq!(reopened.timestamp(*pos), Some(index as i32));
    }
}

#[test]
fn an_empty_chunk_round_trips_as_nothing_but_structure() {
    // All air, all one biome, dark everywhere: the overwhelmingly common
    // section shape. Single-valued containers pack no arrays at all, so what
    // the serialised chunk costs is almost exactly its light arrays -- and it
    // comes back as exactly itself.
    let pos = REGION.chunk_at(1, 1);
    let chunk = Chunk::uniform(pos, world(), BLOCK_REGISTRY, BIOME_REGISTRY, 0, 2);
    let bytes = encode(&chunk);
    assert!(
        bytes.len() < 15_000,
        "an empty chunk carries {} bytes; three sections of light and no data",
        bytes.len()
    );

    let restored = cycle_through_memory(&chunk, 5);
    assert_eq!(restored, chunk);
    for index in 0..restored.section_count() {
        assert_eq!(
            restored.sections()[index].states().palette_kind(),
            dust_world::PaletteKind::Single
        );
        assert!(restored.sections()[index]
            .states()
            .storage()
            .as_longs()
            .is_empty());
    }
}

#[test]
fn the_reader_refuses_bytes_that_are_not_its_own_rather_than_guessing() {
    let pos = REGION.chunk_at(2, 3);
    let good = encode(&interesting_chunk(pos));

    // Wrong magic entirely.
    let mut garbage = good.clone();
    garbage[0] ^= 0xff;
    let err = DirectFormat
        .read_chunk(pos, world(), &garbage)
        .expect_err("not our magic");
    assert!(err.to_string().contains("magic"), "{err}");

    // Truncated mid-payload.
    let err = DirectFormat
        .read_chunk(pos, world(), &good[..good.len() / 2])
        .expect_err("ends early");
    assert!(matches!(err, CodecError::Truncated), "{err}");

    // A payload naming a different chunk than the slot it sits in: the
    // caller decides where the chunk is, and the bytes must agree.
    let elsewhere = REGION.chunk_at(2, 4);
    let err = DirectFormat
        .read_chunk(elsewhere, world(), &good)
        .expect_err("the payload names another position");
    assert!(err.to_string().contains("another chunk"), "{err}");

    // And another dimension shape entirely.
    let err = DirectFormat
        .read_chunk(pos, WorldHeight::OVERWORLD, &good)
        .expect_err("written for another world");
    assert!(err.to_string().contains("another world"), "{err}");
}
