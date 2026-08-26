//! What the pieces of a chunk cost, measured with a stopwatch.
//!
//! # Why this is not criterion
//!
//! A benchmark framework brings statistics and a dozen dependencies to
//! answer "how fast", which for chunk storage is usually the wrong question.
//! The questions worth asking here are comparative and coarse: does the
//! palette ladder stay cheap enough that per-block edits never think about
//! it, does a region round trip cost what compression alone would suggest,
//! does one light walk over a whole column fit in a tick's budget. A fixed
//! workload timed with [`std::time::Instant`] answers those in one run of
//! `cargo bench -p dust-world` with nothing added to the lockfile.
//!
//! The workloads are shaped like real work rather than like noise — sections
//! driven through every palette tier, chunks dense enough to compress to
//! something, torch fields scattered through a full 16x384x16 column —
//! because speed on an empty section proves nothing.
//!
//! Run it: `cargo bench -p dust-world`.

use std::time::Instant;

use dust_world::light::LightArray;
use dust_world::propagation::{darken, raise, Budget, LightGraph};
use dust_world::region::{ChunkPayload, Compression, MemoryStore, RegionFile};
use dust_world::{
    BlockEntityHandle, BlockPos, Chunk, ChunkPos, HeightmapKind, NbtReader, NbtWriter,
    PalettedContainer, Strategy, WorldHeight,
};

/// Print one label and its per-round cost.
fn measure(label: &str, rounds: u32, mut f: impl FnMut(u32)) {
    // One untimed round to warm caches and fault the allocator's pages.
    f(0);
    let start = Instant::now();
    f(rounds);
    let elapsed = start.elapsed();
    println!(
        "{label:<28} {rounds:>3} rounds in {elapsed:.3?}  ({:.2} µs/round)",
        elapsed.as_secs_f64() / f64::from(rounds) * 1e6
    );
}

/// Drive one container from single-valued through linear, hashed and global,
/// exactly as a section that accumulates block kinds does. The re-index on
/// every promotion is the cost being watched: it is paid by whichever edit
/// crosses each boundary, and a regression here is invisible cell-by-cell.
fn promote_ladder() {
    let mut container = PalettedContainer::filled(Strategy::BLOCK_STATES, 26_684, 0);
    for distinct in 1..=300u32 {
        container.set(distinct as usize - 1, distinct - 1);
    }
    // At the top of the ladder the palette *is* the registry; the disk form
    // is what names the three hundred distinct values.
    assert_eq!(container.palette_kind(), dust_world::PaletteKind::Global);
    // And serialise at that tier, where the disk width differs from the
    // in-memory width and every index is rewritten.
    let (entries, data) = container.to_parts();
    assert_eq!(entries.len(), 300);
    assert!(data.is_some());
}

/// One chunk worth of plausible content: stone below dirt below air, a few
/// hundred varied blocks near the surface, light arrays with something in
/// them, heightmaps computed from what stands. Dense enough that the region
/// layer has real bytes to compress.
fn busy_chunk(pos: ChunkPos) -> Chunk {
    let mut chunk = Chunk::uniform(pos, WorldHeight::OVERWORLD, 26_684, 64, 0, 1);
    for y in -64..60 {
        for z in 0..16u32 {
            for x in 0..16u32 {
                let state = match y {
                    y if y < -60 => (x + z) % 3 + 1,
                    y if y < 40 => 1 + (y as u32 % 7),
                    _ => 0,
                };
                if state != 0 {
                    chunk.set_block(x, y, z, state);
                }
            }
        }
    }
    for section in 0..24usize {
        let base = WorldHeight::OVERWORLD.min_y() + (section * 16) as i32;
        let array = chunk.section_mut(base).sky_light_mut();
        for index in (0..4096usize).step_by(7) {
            array.set_cell(index, ((index * 13) % 16) as u8);
        }
    }
    chunk.recompute_heightmaps(|kind, state| match kind {
        HeightmapKind::WorldSurface | HeightmapKind::MotionBlocking => state != 0,
        _ => false,
    });
    chunk.insert_block_entity(BlockEntityHandle {
        position: BlockPos::new(pos.x * 16 + 4, -50, pos.z * 16 + 9),
        block_state: 43,
    });
    chunk.insert_block_entity(BlockEntityHandle {
        position: BlockPos::new(pos.x * 16 + 11, 20, pos.z * 16 + 2),
        block_state: 77,
    });
    chunk
}

/// The stand-in writer: flat big-endian fields, no tags -- this bench measures
/// the region store and the containers, not a serialiser (there is none on
/// this branch yet; see `chunk::NbtWriter`).
struct FlatFormat;

/// Big-endian field reader over an exact byte run.
struct Fields<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Fields<'a> {
    fn u32(&mut self) -> u32 {
        let end = self.at + 4;
        let value = u32::from_be_bytes(self.bytes[self.at..end].try_into().expect("four bytes"));
        self.at = end;
        value
    }

    fn i64(&mut self) -> i64 {
        let end = self.at + 8;
        let value = i64::from_be_bytes(self.bytes[self.at..end].try_into().expect("eight bytes"));
        self.at = end;
        value
    }
}

impl NbtWriter for FlatFormat {
    type Error = std::io::Error;

    fn write_chunk(&self, chunk: &Chunk) -> Result<Vec<u8>, Self::Error> {
        let mut out = Vec::new();
        out.extend_from_slice(&(chunk.section_count() as u32).to_be_bytes());
        for section in chunk.sections() {
            let (entries, data) = section.states().to_parts();
            out.extend_from_slice(&(entries.len() as u32).to_be_bytes());
            for entry in &entries {
                out.extend_from_slice(&entry.to_be_bytes());
            }
            match data {
                Some(longs) => {
                    out.extend_from_slice(&(longs.len() as u32).to_be_bytes());
                    for long in longs {
                        out.extend_from_slice(&long.to_be_bytes());
                    }
                }
                None => out.extend_from_slice(&0u32.to_be_bytes()),
            }
        }
        Ok(out)
    }
}

impl NbtReader for FlatFormat {
    type Error = std::io::Error;

    fn read_chunk(
        &self,
        pos: ChunkPos,
        world: WorldHeight,
        nbt: &[u8],
    ) -> Result<Chunk, Self::Error> {
        let mut fields = Fields { bytes: nbt, at: 0 };
        let count = fields.u32() as usize;
        let mut sections = Vec::with_capacity(count);
        for _ in 0..count {
            let entries = fields.u32() as usize;
            let palette: Vec<u32> = (0..entries).map(|_| fields.u32()).collect();
            let longs = fields.u32() as usize;
            let data = if longs == 0 {
                None
            } else {
                Some((0..longs).map(|_| fields.i64()).collect::<Vec<i64>>())
            };
            let biomes = PalettedContainer::filled(Strategy::BIOMES, 64, 1);
            let states =
                PalettedContainer::from_parts(Strategy::BLOCK_STATES, 26_684, &palette, data)
                    .expect("its own output");
            sections.push(dust_world::chunk::Section::new(
                states,
                biomes,
                LightArray::new(),
                LightArray::new(),
            ));
        }
        Ok(Chunk::from_parts(
            pos,
            world,
            26_684,
            64,
            sections,
            dust_world::HeightmapSet::new(world),
            Default::default(),
        ))
    }
}

/// A full column of cells backed by one `Vec`, opacity from the default
/// model. This is the shape any wiring will take: per-section arrays behind
/// arithmetic, the trait on top.
#[derive(Clone)]
struct Column {
    light: Vec<u8>,
}

const COLUMN_CELLS: usize = 16 * 384 * 16;

impl Column {
    fn new() -> Self {
        Self {
            light: vec![0; COLUMN_CELLS],
        }
    }

    fn offset(&self, x: i32, y: i32, z: i32) -> usize {
        // y runs from the overworld floor; shift it to zero-based first,
        // or a floor-level cell indexes from under the array.
        ((y + 64) as usize) * 256 + (z as usize) * 16 + x as usize
    }
}

impl LightGraph for Column {
    fn level(&self, x: i32, y: i32, z: i32) -> u8 {
        self.light[self.offset(x, y, z)]
    }

    fn set_level(&mut self, x: i32, y: i32, z: i32, level: u8) {
        let index = self.offset(x, y, z);
        self.light[index] = level;
    }

    fn opacity(&self, _x: i32, _y: i32, _z: i32) -> u8 {
        0
    }

    fn contains(&self, x: i32, y: i32, z: i32) -> bool {
        (0..16).contains(&x) && (-64..320).contains(&y) && (0..16).contains(&z)
    }
}

/// Torch positions scattered through the column, deterministic so runs are
/// comparable.
fn torches() -> Vec<(i32, i32, i32, u8)> {
    let mut seeds = Vec::new();
    let mut state = 0x5eed_beef_cafe_u64;
    let mut xorshift = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..64 {
        let x = (xorshift() % 16) as i32;
        let y = (xorshift() % 384) as i32 - 64;
        let z = (xorshift() % 16) as i32;
        seeds.push((x, y, z, 14));
    }
    seeds
}

fn main() {
    println!("dust-world workloads\n");

    measure("palette promote ladder", 400, |rounds| {
        for _ in 0..rounds {
            promote_ladder();
        }
    });

    const CHUNKS: u32 = 64;
    let chunks: Vec<Chunk> = (0..CHUNKS).map(|i| busy_chunk(slot_chunk(i))).collect();

    measure("region save+load round trip", 30, |rounds| {
        for _ in 0..rounds {
            let mut region =
                RegionFile::open(MemoryStore::new(), REGION).expect("a fresh region opens");
            for (index, chunk) in chunks.iter().enumerate() {
                let pos = slot_chunk(index as u32);
                let payload =
                    ChunkPayload::from_bytes(FlatFormat.write_chunk(chunk).expect("serialises"));
                region
                    .write_chunk(pos, &payload, Compression::Zlib, index as i32)
                    .expect("fits");
            }
            for index in 0..CHUNKS {
                let pos = slot_chunk(index);
                let payload = region.read_chunk(pos).expect("reads").expect("written");
                drop(FlatFormat.read_chunk(pos, WorldHeight::OVERWORLD, payload.as_bytes()));
            }
        }
    });

    let mut column = Column::new();
    let lights = torches();
    measure("light raise, 64 torches, full column", 20, |rounds| {
        for _ in 0..rounds {
            let mut fresh = Column::new();
            raise(&mut fresh, &lights, Budget::new(10_000_000)).expect("room");
            column = fresh;
        }
    });
    let half: Vec<(i32, i32, i32)> = lights
        .iter()
        .take(lights.len() / 2)
        .map(|&(x, y, z, _)| (x, y, z))
        .collect();
    let standing: Vec<(i32, i32, i32, u8)> =
        lights.iter().skip(lights.len() / 2).copied().collect();
    measure("light darken+relight, 32 of 64", 20, |rounds| {
        for _ in 0..rounds {
            let mut fresh = column.clone();
            darken(&mut fresh, &half, &standing, Budget::new(10_000_000)).expect("room");
        }
    });
}

const REGION: dust_world::RegionPos = dust_world::RegionPos::new(-2, 5);

/// The chunk position of slot `index`, eight per row across the region.
fn slot_chunk(index: u32) -> ChunkPos {
    REGION.chunk_at(index % 8, index / 8)
}
