//! Where a column comes from.
//!
//! Two answers so far, and the shape exists because there will be a third.
//! [`Source::Flat`] builds one column and shares it. [`Source::Anvil`] reads
//! one out of a world Minecraft wrote. Phase 6's generator is the third, and it
//! will be a variant here rather than a rewrite of everything that asks for a
//! column.
//!
//! # What an Anvil world costs to serve, and what is cached
//!
//! Region files are held open — a world is a few dozen of them and reopening
//! one per column would be a syscall storm — behind a mutex, because they are
//! read from every session's task and a `RegionFile` seeks as it reads. What is
//! **not** cached is the parsed column: a chunk is about a megabyte and a view
//! distance of ten is four hundred of them, so caching them all is a design
//! decision with a memory budget attached and not something to slip in.
//!
//! # A missing column is not an error
//!
//! A real world is a disc of generated chunks in an infinite plane, and a
//! player can walk off the edge of it. Vanilla generates what is missing; Dust
//! has no generator yet, so it falls back to the flat column. That is visible —
//! terrain stops and a plain runs on — which is the right kind of wrong: an
//! error would disconnect a player for walking, and a hole would look like a
//! bug in the chunk packet.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use dust_world::anvil::{self, Names};
use dust_world::chunk::Chunk;
use dust_world::coords::{ChunkPos, RegionPos};
use dust_world::heightmap::WorldHeight;
use dust_world::region::{FileStore, RegionFile};

use super::world::FlatWorld;

/// Region files that have been opened, or found not to exist.
///
/// `None` is a region that is not there, remembered rather than retried: a
/// world is a disc and the columns off its edge are asked for constantly by a
/// player walking outward.
type OpenRegions = HashMap<(i32, i32), Option<RegionFile<FileStore>>>;

/// A column, borrowed when it can be and built when it cannot.
///
/// The two variants differ in size by about a megabyte, and that is the point
/// rather than an oversight: the borrowed one is what a flat world hands out
/// twenty-five times per join without allocating, and boxing it to even the
/// variants out would put an allocation on exactly the path the borrow exists
/// to keep free. The value is a temporary — a caller sends the column and drops
/// it — so the large variant never sits in a collection.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Column<'a> {
    /// Every position shares this one. See [`FlatWorld`].
    Shared(&'a Chunk),
    /// Read from disk for this position.
    Built(Chunk),
}

impl Column<'_> {
    pub fn as_chunk(&self) -> &Chunk {
        match self {
            Self::Shared(chunk) => chunk,
            Self::Built(chunk) => chunk,
        }
    }
}

/// Where the world comes from.
pub enum Source {
    Flat(FlatWorld),
    Anvil(AnvilWorld),
}

impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Flat(_) => f.write_str("Flat"),
            Self::Anvil(world) => write!(f, "Anvil({})", world.directory.display()),
        }
    }
}

impl Source {
    pub fn column(&self, pos: ChunkPos) -> Column<'_> {
        match self {
            Self::Flat(flat) => Column::Shared(flat.column()),
            Self::Anvil(world) => Column::Built(world.column(pos)),
        }
    }

    /// The flat world underneath, which every source has: it is the fallback
    /// for a column a real world does not contain, and it owns the block
    /// palette everything else resolves against.
    pub fn flat(&self) -> &FlatWorld {
        match self {
            Self::Flat(flat) => flat,
            Self::Anvil(world) => &world.fallback,
        }
    }
}

/// A world on disk.
pub struct AnvilWorld {
    directory: PathBuf,
    /// Open region files, by the region they cover. Behind a mutex because a
    /// `RegionFile` seeks as it reads and every session's task asks it for
    /// columns.
    regions: Mutex<OpenRegions>,
    names: RegistryNames,
    height: WorldHeight,
    fallback: FlatWorld,
}

impl std::fmt::Debug for AnvilWorld {
    /// The open region files are file handles and seek positions, and the name
    /// tables are three hundred strings. What a reader wants is which world
    /// this is and how much of it is open.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnvilWorld")
            .field("directory", &self.directory)
            .field(
                "regions_open",
                &self
                    .regions
                    .lock()
                    .map(|open| open.len())
                    .unwrap_or_default(),
            )
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for RegistryNames {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistryNames")
            .field("biomes", &self.biomes.len())
            .finish()
    }
}

impl AnvilWorld {
    pub fn new(directory: PathBuf, names: RegistryNames, fallback: FlatWorld) -> Self {
        Self {
            directory,
            regions: Mutex::new(HashMap::new()),
            names,
            height: fallback.height(),
            fallback,
        }
    }

    /// Whether this looks like a world directory at all, checked at boot so an
    /// operator who mistyped a path is told then rather than by an empty world.
    pub fn is_region_directory(path: &Path) -> bool {
        std::fs::read_dir(path).is_ok_and(|entries| {
            entries.flatten().any(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "mca")
            })
        })
    }

    fn column(&self, pos: ChunkPos) -> Chunk {
        match self.read(pos) {
            Some(mut chunk) => {
                // Light is computed, not read. A chunk's stored light is a
                // cache of what an engine would produce, and this server has
                // its own engine; trusting the file would mean serving light
                // that no code here can reproduce.
                chunk.recompute_heightmaps(|_, state| state != self.fallback.palette().air);
                let opacity = dust_world::propagation::DefaultOpacity::transparent_only([self
                    .fallback
                    .palette()
                    .air]);
                let _ = dust_world::column_light::ColumnSkyLight::seed(
                    &mut chunk,
                    &opacity,
                    dust_world::propagation::Budget::new(4_000_000),
                );
                chunk
            }
            // Off the edge of what was generated. See the module note: a plain
            // running on beats an error or a hole.
            None => self.fallback.column().clone(),
        }
    }

    fn read(&self, pos: ChunkPos) -> Option<Chunk> {
        let region = RegionPos::new(pos.x >> 5, pos.z >> 5);
        let mut open = self
            .regions
            .lock()
            .expect("the region map is never poisoned");
        let slot = open.entry((region.x, region.z)).or_insert_with(|| {
            // A region file that is not there is the ordinary case at the edge
            // of a world, so the absence is remembered rather than retried on
            // every column.
            RegionFile::open_in(&self.directory, region).ok()
        });
        let file = slot.as_mut()?;
        let payload = file.read_chunk(pos).ok()??;
        let named = dust_nbt::read::from_bytes(payload.as_bytes()).ok()?;
        let dust_nbt::Tag::Compound(root) = &named.tag else {
            return None;
        };
        anvil::chunk(root, self.height, &self.names).ok()
    }
}

/// [`Names`] backed by the generated registries.
///
/// This is where `dust-world`'s deliberate ignorance of the registry is repaid:
/// the parser takes a lookup, and the assembly crate is the one that has both
/// the block table and the biome ids the *client* was told about. Those biome
/// ids are positions in the synced registry, not an internal numbering, and
/// using the wrong ones would render a plains as whatever is at that index.
pub struct RegistryNames {
    biomes: HashMap<&'static str, u32>,
}

impl RegistryNames {
    /// Build the tables, from the same synced registry the configuration state
    /// sends. One source for both, so a client and a chunk cannot disagree
    /// about which number is which biome.
    pub fn new() -> Option<Self> {
        let synced = dust_registry::synced::by_name("minecraft:worldgen/biome")?;
        Some(Self {
            biomes: synced
                .entries
                .iter()
                .enumerate()
                .map(|(id, name)| (*name, id as u32))
                .collect(),
        })
    }
}

impl Names for RegistryNames {
    fn block(&self, name: &str) -> Option<u32> {
        dust_registry::Block::from_name(name).map(|block| block.default_state().id())
    }

    fn biome(&self, name: &str) -> Option<u32> {
        self.biomes.get(name).copied()
    }

    fn block_registry_size(&self) -> u32 {
        dust_registry::STATE_COUNT
    }

    fn biome_registry_size(&self) -> u32 {
        self.biomes.len() as u32
    }
}
