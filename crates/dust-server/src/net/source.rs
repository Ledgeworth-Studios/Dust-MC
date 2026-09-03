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
//! # What *is* cached is 256 integers per column
//!
//! Light does not stop at a chunk boundary, which means lighting a column
//! needs to know where the sky reaches in the four columns around it. That is
//! a [`SkyFloor`] — sixteen by sixteen integers, a kilobyte — and it is
//! nothing like a chunk. So those are cached even though the chunks are not:
//! at the default view distance of eight the whole working set is under three
//! hundred kilobytes,
//! and without it every column would read its four neighbours off disk to ask
//! them one question.
//!
//! The cache is cleared wholesale when it grows past its cap rather than
//! evicted a row at a time. The working set is the columns around the players,
//! so a cache that has passed the cap is one whose contents are mostly
//! nowhere anybody is standing, and the cost of being wrong is re-reading the
//! current view once.
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
use std::sync::{Arc, Mutex};

use dust_world::anvil::{self, Ids, Names};
use dust_world::chunk::Chunk;
use dust_world::column_light::{Skirt, SkyFloor};
use dust_world::coords::{ChunkPos, RegionPos};
use dust_world::heightmap::WorldHeight;
use dust_world::region::{FileStore, RegionFile};

use super::residency::Residency;
use super::generated::GeneratedWorld;
use super::world::FlatWorld;

/// Region files that have been opened, or found not to exist.
///
/// `None` is a region that is not there, remembered rather than retried: a
/// world is a disc and the columns off its edge are asked for constantly by a
/// player walking outward.
type OpenRegions = HashMap<(i32, i32), Option<RegionFile<FileStore>>>;

/// A column, borrowed when it can be, shared when the server is keeping one,
/// and built when neither.
///
/// The variants differ in size by about a megabyte, and that is the point
/// rather than an oversight: the borrowed one is what a flat world hands out
/// once per column per join without allocating — 289 times at the default view
/// distance — and boxing it to even the
/// variants out would put an allocation on exactly the path the borrow exists
/// to keep free. The value is a temporary — a caller sends the column and drops
/// it — so the large variant never sits in a collection.
///
/// [`Column::Resident`] is the one that is *not* a temporary, and it is an
/// `Arc` for that reason: it is a handle on a column the whole server is
/// keeping, and a caller may hold it for as long as it likes without stopping
/// the residency from retiring its own entry. See [`Residency`].
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Column<'a> {
    /// Every position shares this one. See [`FlatWorld`].
    Shared(&'a Chunk),
    /// One the server is keeping because a player is near it. See
    /// [`Residency`].
    Resident(Arc<Chunk>),
    /// Read from disk for this position.
    Built(Chunk),
}

impl Column<'_> {
    pub fn as_chunk(&self) -> &Chunk {
        match self {
            Self::Shared(chunk) => chunk,
            Self::Resident(chunk) => chunk,
            Self::Built(chunk) => chunk,
        }
    }
}

/// Where the world comes from.
///
/// Both arms are boxed, so the enum is a pointer and a tag. Neither world is
/// small — the Anvil one carries the open region files and the sky floors, the
/// flat one a template column's worth of bookkeeping — and an unboxed enum is
/// the size of its largest arm wherever it is stored. There is one of these
/// per server, reached through an `Arc`, so the allocation happens once at
/// boot and the indirection costs a pointer hop on a path that is about to
/// read a region file. `Column` below is the opposite case and keeps its large
/// variant unboxed for a stated reason.
pub enum Source {
    Flat(Box<FlatWorld>),
    /// Built from noise, for a server with no world file: every column, out
    /// to the edge of the coordinate space.
    Generated(Box<GeneratedWorld>),
    Anvil(Box<AnvilWorld>),
}

/// What a position a world file does not contain is served with.
///
/// A world file is a disc in an infinite plane and a player can walk off the
/// edge of it. The two answers are a plain that runs on, which is what Dust
/// served before there was a generator, and the terrain the world's own seed
/// says is there — which is the same terrain Minecraft would have generated,
/// so the edge is a seam in the *materials* and not in the shape.
///
/// Which one an operator gets is not a setting: it is whether the world's own
/// seed could be read. Generating the far side of the edge from a seed that is
/// not the world's own would put a cliff where the disc ends, and a wrong
/// answer that looks right is worse than an obviously artificial one.
#[derive(Debug)]
pub enum Fallback {
    Flat(Box<FlatWorld>),
    Generated(Box<GeneratedWorld>),
}

impl Fallback {
    /// The flat world underneath, which both arms carry: it owns the block
    /// palette and the world height everything else resolves against.
    pub fn flat(&self) -> &FlatWorld {
        match self {
            Self::Flat(flat) => flat,
            Self::Generated(world) => world.flat(),
        }
    }

    fn column(&self, pos: ChunkPos) -> Chunk {
        match self {
            Self::Flat(flat) => flat.column().clone(),
            Self::Generated(world) => world.column(pos),
        }
    }
}

impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Flat(_) => f.write_str("Flat"),
            Self::Generated(_) => f.write_str("Generated"),
            Self::Anvil(world) => write!(f, "Anvil({})", world.core.directory.display()),
        }
    }
}

impl Source {
    pub fn column(&self, pos: ChunkPos) -> Column<'_> {
        match self {
            Self::Flat(flat) => Column::Shared(flat.column()),
            // Nobody keeps a generated column yet. It is built on whatever
            // thread asked, which is what an Anvil column was before residency
            // existed; the two meet when residency stops being about a world
            // that has a file.
            Self::Generated(world) => Column::Built(world.column(pos)),
            Self::Anvil(world) => match world.residency.resident(pos) {
                Some(chunk) => Column::Resident(chunk),
                // Nobody is keeping this one. Built here, on whatever thread
                // asked, which is what every caller did before residency
                // existed: this path can only ever be as slow as it was.
                None => Column::Built(world.core.column(pos)),
            },
        }
    }

    /// Claim the columns around `centre` for one player.
    ///
    /// Refcounts only — see [`Residency::hold`]. Nothing here reads a file, so
    /// a session may call it from the task that just judged a movement packet;
    /// [`Source::warm`] is the half that must not be.
    ///
    /// A flat world has nothing to keep: it lends one template column to every
    /// position, so residency would be a refcount on a borrow.
    pub fn hold(&self, centre: ChunkPos) {
        if let Self::Anvil(world) = self {
            world.residency.hold(centre);
        }
    }

    /// Give up one player's claim on the columns around `centre`.
    pub fn release(&self, centre: ChunkPos) {
        if let Self::Anvil(world) = self {
            world.residency.release(centre);
        }
    }

    /// Claim named columns for one holder, for a caller whose working set is
    /// not a ring around a player. See [`Residency::hold_columns`].
    pub fn hold_columns(&self, columns: &[ChunkPos]) {
        if let Self::Anvil(world) = self {
            world.residency.hold_columns(columns);
        }
    }

    /// Give up a claim taken by [`Source::hold_columns`].
    pub fn release_columns(&self, columns: &[ChunkPos]) {
        if let Self::Anvil(world) = self {
            world.residency.release_columns(columns);
        }
    }

    /// Ask for these columns to be built, and carry on.
    ///
    /// **This is the call every caller on a hot path wants** and the only one
    /// that is safe from all of them: it hands a list to the world's own
    /// warming thread and returns. A session task and the tick loop are
    /// different threads with different rules about blocking, and neither of
    /// them may read a region file; this is the one door both can use.
    ///
    /// Nothing waits on the result. A caller that reaches a column before the
    /// thread does builds it, which is what every caller did before residency
    /// existed — the floor is the old behaviour, never a hole in the world.
    pub fn want(&self, columns: Vec<ChunkPos>) {
        if let Self::Anvil(world) = self {
            if let Some(wanted) = &world.wanted {
                // Fails only if the warming thread has gone, which happens
                // while the world is being dropped. There is nothing to warm
                // for a world that is going away.
                let _ = wanted.send(columns);
            }
        }
    }

    /// Ask for the ring around `centre` to be built, and carry on.
    pub fn want_ring(&self, centre: ChunkPos) {
        if let Self::Anvil(world) = self {
            self.want(world.residency.cold(centre));
        }
    }

    /// Build whatever around `centre` is claimed and not yet there, **on this
    /// thread**, and return how many columns that was.
    ///
    /// The blocking form, for the two callers that have a reason to wait: a
    /// join, which has no movement packet held up behind it and wants the
    /// ground under the player there before the loading screen ends, and a
    /// bench, which is measuring the cost itself. Everything else calls
    /// [`Source::want`].
    pub fn warm(&self, centre: ChunkPos) -> u32 {
        let Self::Anvil(world) = self else { return 0 };
        self.warm_columns(&world.residency.cold(centre))
    }

    /// The same, for a named set of columns rather than a ring.
    pub fn warm_columns(&self, columns: &[ChunkPos]) -> u32 {
        let Self::Anvil(world) = self else { return 0 };
        let mut built = 0;
        for pos in world.residency.cold_columns(columns) {
            // Built with no lock held, then offered. A player who walked away
            // in the meantime, or another thread that got there first, means
            // the column is dropped here rather than kept — see
            // [`Residency::fill`].
            let chunk = world.core.column(pos);
            world.residency.fill(pos, chunk);
            built += 1;
        }
        built
    }

    /// The server's resident set, for a caller that keeps a claim on it —
    /// `net::residency::Residence` for a session and
    /// `net::residency::ColumnClaim` for the item entities.
    ///
    /// `None` on a flat world, which lends one template column to every
    /// position and has nothing to keep. A claim on `None` does nothing and
    /// that is the right nothing: there is no column to hold.
    #[must_use]
    pub fn residency(&self) -> Option<Arc<Residency>> {
        match self {
            Self::Flat(_) | Self::Generated(_) => None,
            Self::Anvil(world) => Some(Arc::clone(&world.residency)),
        }
    }

    /// Where a claim sends the columns it has just taken, to be built off the
    /// caller's own thread. `None` where the world builds nothing or its
    /// warming thread would not start.
    #[must_use]
    pub fn warming(&self) -> Option<std::sync::mpsc::Sender<Vec<ChunkPos>>> {
        match self {
            Self::Flat(_) | Self::Generated(_) => None,
            Self::Anvil(world) => world.wanted.clone(),
        }
    }

    /// How many columns the server is keeping. Zero on a flat world.
    #[must_use]
    pub fn resident_columns(&self) -> usize {
        match self {
            Self::Flat(_) | Self::Generated(_) => 0,
            Self::Anvil(world) => world.residency.len(),
        }
    }

    /// The flat world underneath, which every source has: it is the fallback
    /// for a column a real world does not contain, and it owns the block
    /// palette everything else resolves against.
    pub fn flat(&self) -> &FlatWorld {
        match self {
            Self::Flat(flat) => flat,
            Self::Generated(world) => world.flat(),
            Self::Anvil(world) => world.core.fallback.flat(),
        }
    }
}

/// A world on disk, and the thread that reads it ahead of the players.
///
/// Two halves on purpose. [`AnvilCore`] is everything that answers a question
/// about the world, behind an `Arc` so that the warming thread can hold it; the
/// wrapper is the residency, the channel and the thread's own lifetime, which
/// belong to the world rather than to anything asking it for a column.
///
/// The thread is the answer to a question the server asks in two places and
/// cannot answer the same way in either. A session runs on a tokio worker and a
/// tick participant runs on the engine's own `std` thread; neither may block on
/// a region file, and `tokio::task::spawn_blocking` exists only for the first.
/// One thread here serves both, and neither caller has to know it is there.
pub struct AnvilWorld {
    core: Arc<AnvilCore>,
    /// The columns the server is keeping because players or items are near
    /// them. Shared with the warming thread; see [`Residency`] for what it is
    /// serialised against.
    residency: Arc<Residency>,
    /// Columns somebody has claimed and nobody has built yet. `None` where the
    /// thread could not be started, which is a world that warms nothing and
    /// still works: every caller builds its own column, exactly as they did
    /// before any of this.
    wanted: Option<std::sync::mpsc::Sender<Vec<ChunkPos>>>,
    warming: Option<std::thread::JoinHandle<()>>,
}

impl Drop for AnvilWorld {
    /// The sender goes first, which ends the thread's loop, and then the thread
    /// is waited for. Not detached: a warming thread still holding the region
    /// mutex while the process tears the world down is a shutdown that hangs
    /// on a lock nobody owns any more.
    fn drop(&mut self) {
        self.wanted = None;
        if let Some(thread) = self.warming.take() {
            let _ = thread.join();
        }
    }
}

/// Everything that answers a question about a world on disk.
struct AnvilCore {
    directory: PathBuf,
    /// Open region files, by the region they cover. Behind a mutex because a
    /// `RegionFile` seeks as it reads and every session's task asks it for
    /// columns.
    regions: Mutex<OpenRegions>,
    names: RegistryNames,
    height: WorldHeight,
    fallback: Fallback,
    /// How much light entering each block state costs. Built once at boot,
    /// because with a table in it this is 26,684 bytes and the alternative is
    /// building one per column served.
    opacity: dust_world::propagation::OpacityModel,
    /// What Minecraft says about a block state, or nothing. Held rather than
    /// folded into `opacity` because the heightmaps need it too and a second
    /// copy would be a second answer.
    constants: Option<std::sync::Arc<dust_registry::BlockConstants>>,
    /// What every block state gives off. Built once at boot beside `opacity`
    /// and for the same reason: with a table in it this is 26,684 bytes.
    emission: dust_world::propagation::EmissionModel,
    /// Where the sky reaches in each column read so far, for lighting the
    /// columns beside it. See the module note for why this is cached when the
    /// chunks are not.
    sky_floors: Mutex<HashMap<(i32, i32), SkyFloor>>,
}

/// How many columns' sky floors are kept before the cache is emptied.
///
/// A kilobyte each, so this is four megabytes — several times a view distance
/// of ten in every direction, which is the point: the cap is a bound on a
/// walk that never comes back, not a limit on how much of one view fits.
const SKY_FLOOR_CACHE_CAP: usize = 4096;

impl std::fmt::Debug for AnvilWorld {
    /// The open region files are file handles and seek positions, and the name
    /// tables are three hundred strings. What a reader wants is which world
    /// this is, how much of it is open and how much of it is being kept.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnvilWorld")
            .field("directory", &self.core.directory)
            .field(
                "regions_open",
                &self
                    .core
                    .regions
                    .lock()
                    .map(|open| open.len())
                    .unwrap_or_default(),
            )
            .field("resident_columns", &self.residency.len())
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

impl AnvilCore {
    /// `opacity` is what the columns are lit with — see
    /// [`world::opacity_of`](super::world::opacity_of), which is the one place
    /// that decides between Minecraft's own numbers and the air-only stand-in.
    ///
    /// The flat `fallback` keeps its own model and that is not an oversight: it
    /// is made of bedrock, dirt and grass, every one of which both models agree
    /// is a wall, so the two answers are the same answer.
    fn new(
        directory: PathBuf,
        names: RegistryNames,
        fallback: Fallback,
        opacity: dust_world::propagation::OpacityModel,
        constants: Option<std::sync::Arc<dust_registry::BlockConstants>>,
    ) -> Self {
        Self {
            directory,
            regions: Mutex::new(HashMap::new()),
            names,
            height: fallback.flat().height(),
            emission: super::world::emission_of(constants.as_deref()),
            fallback,
            opacity,
            constants,
            sky_floors: Mutex::new(HashMap::new()),
        }
    }
}

impl AnvilWorld {
    /// `opacity` is what the columns are lit with — see
    /// [`world::opacity_of`](super::world::opacity_of).
    pub fn new(
        directory: PathBuf,
        names: RegistryNames,
        fallback: FlatWorld,
        opacity: dust_world::propagation::OpacityModel,
        constants: Option<std::sync::Arc<dust_registry::BlockConstants>>,
    ) -> Self {
        Self::with_fallback(
            directory,
            names,
            Fallback::Flat(Box::new(fallback)),
            opacity,
            constants,
        )
    }

    /// The same world with the generator underneath it, for a save whose own
    /// seed this server could read.
    pub fn generating(
        directory: PathBuf,
        names: RegistryNames,
        fallback: GeneratedWorld,
        opacity: dust_world::propagation::OpacityModel,
        constants: Option<std::sync::Arc<dust_registry::BlockConstants>>,
    ) -> Self {
        Self::with_fallback(
            directory,
            names,
            Fallback::Generated(Box::new(fallback)),
            opacity,
            constants,
        )
    }

    fn with_fallback(
        directory: PathBuf,
        names: RegistryNames,
        fallback: Fallback,
        opacity: dust_world::propagation::OpacityModel,
        constants: Option<std::sync::Arc<dust_registry::BlockConstants>>,
    ) -> Self {
        let core = Arc::new(AnvilCore::new(
            directory, names, fallback, opacity, constants,
        ));
        let residency = Arc::new(Residency::new());
        let (wanted, requests) = std::sync::mpsc::channel::<Vec<ChunkPos>>();
        let warming = std::thread::Builder::new()
            .name("dust-warming".to_owned())
            .spawn({
                let core = Arc::clone(&core);
                let residency = Arc::clone(&residency);
                move || {
                    // Ends when the world drops its sender. Nothing here holds
                    // a lock across a build: `cold` takes a snapshot, the
                    // column is read with nothing held, and `fill` takes the
                    // write lock for one insert.
                    while let Ok(columns) = requests.recv() {
                        for pos in residency.cold_columns(&columns) {
                            residency.fill(pos, core.column(pos));
                        }
                    }
                }
            })
            .ok();
        Self {
            core,
            residency,
            // A thread that would not start leaves every caller building its
            // own columns, which is what they all did before this existed.
            wanted: warming.is_some().then_some(wanted),
            warming,
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
}

impl AnvilCore {
    /// Where the sky reaches in one column, read or remembered.
    ///
    /// A column the world does not contain answers with the flat fallback's
    /// floors, because that is what Dust serves there — the skirt has to
    /// describe the world a player will actually see, not the one on disk.
    fn sky_floor(&self, pos: ChunkPos) -> SkyFloor {
        if let Some(found) = self
            .sky_floors
            .lock()
            .expect("the sky-floor cache is never poisoned")
            .get(&(pos.x, pos.z))
        {
            return *found;
        }
        let floors = match self.read(pos) {
            Some(mut chunk) => {
                chunk.recompute_heightmaps(super::world::heightmap_predicate(
                    self.fallback.flat().palette().air,
                    self.constants.as_deref(),
                ));
                SkyFloor::of(&chunk)
            }
            None => SkyFloor::of(&self.fallback.column(pos)),
        };
        self.remember(pos, floors);
        floors
    }

    fn remember(&self, pos: ChunkPos, floors: SkyFloor) {
        let mut cache = self
            .sky_floors
            .lock()
            .expect("the sky-floor cache is never poisoned");
        if cache.len() >= SKY_FLOOR_CACHE_CAP {
            // Wholesale, not one row at a time. See the module note.
            cache.clear();
        }
        cache.insert((pos.x, pos.z), floors);
    }

    /// The four columns around `pos`, as a boundary condition for its light.
    fn skirt(&self, pos: ChunkPos) -> Skirt {
        Skirt {
            west: self.sky_floor(ChunkPos::new(pos.x - 1, pos.z)),
            east: self.sky_floor(ChunkPos::new(pos.x + 1, pos.z)),
            north: self.sky_floor(ChunkPos::new(pos.x, pos.z - 1)),
            south: self.sky_floor(ChunkPos::new(pos.x, pos.z + 1)),
        }
    }

    fn column(&self, pos: ChunkPos) -> Chunk {
        match self.read(pos) {
            Some(mut chunk) => {
                // The file's heightmaps are replaced with this server's, for
                // serving. `anvil::read` loads what the file carried — see its
                // docs for why it does not throw them away — and this is the
                // caller that has a reason to overwrite them: the client is
                // sent WORLD_SURFACE and MOTION_BLOCKING, and they have to
                // agree with the blocks in the packet beside them rather than
                // with whatever produced the file.
                //
                // The predicate is "not air" for all six, which is **not**
                // vanilla's rule for three of the four it writes. That is a
                // known approximation and the reason a save must not go
                // through here: `harness rewrite` reads and writes without
                // this step, so a round trip keeps the file's own answers.
                chunk.recompute_heightmaps(super::world::heightmap_predicate(
                    self.fallback.flat().palette().air,
                    self.constants.as_deref(),
                ));
                // Light is computed, not read. A chunk's stored light is a
                // cache of what an engine would produce, and this server has
                // its own engine; trusting the file would mean serving light
                // that no code here can reproduce.
                // This column's own floors go in the cache before its
                // neighbours are asked for theirs: a column is almost always
                // asked for beside the ones around it, so the pass that lights
                // it pays for the four that follow.
                self.remember(pos, SkyFloor::of(&chunk));
                let skirt = self.skirt(pos);
                let _ =
                    super::world::light_column(&mut chunk, &self.opacity, &self.emission, skirt);
                chunk
            }
            // Off the edge of what was generated. See the module note: a plain
            // running on beats an error or a hole.
            None => self.fallback.column(pos),
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
    /// The same table read the other way, for [`Ids`]. Built beside the
    /// forward one from the same iteration rather than searched on demand: a
    /// write walks every palette entry of every section, and a linear scan of
    /// sixty-four biomes per entry is a cost with no reason to exist.
    ///
    /// Two maps over one source is not two sources. What would be is building
    /// this from a second call to `synced::by_name`, which is why it is not.
    biome_names: Vec<&'static str>,
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
            biome_names: synced.entries.to_vec(),
        })
    }
}

impl Names for RegistryNames {
    /// Resolve a block *state*, not just a block.
    ///
    /// Start at the block's default state and apply each property the file
    /// named. `BlockState::with` returns `None` for a property this block does
    /// not have or a value it does not take, and that is **skipped rather than
    /// refused**: a world written by a newer Minecraft, or by a modded server,
    /// carries properties this build's table does not model, and refusing the
    /// whole chunk over one of them would make a world unopenable for a detail
    /// nobody can see. What is not done is returning the default and calling it
    /// the state — the properties that *are* understood are applied, so a
    /// staircase faces the way it was written even if some other field on it is
    /// unknown.
    fn block(&self, name: &str, properties: &[(&str, &str)]) -> Option<u32> {
        let mut state = dust_registry::Block::from_name(name)?.default_state();
        for (property, value) in properties {
            if let Some(next) = state.with(property, value) {
                state = next;
            }
        }
        Some(state.id())
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

impl Ids for RegistryNames {
    /// Name a block *state*, with every property that distinguishes it.
    ///
    /// The inverse of [`Names::block`], and deliberately not its exact mirror.
    /// The reader *skips* a property it does not understand, because a world
    /// written by a newer Minecraft carries fields this build cannot model and
    /// refusing a chunk over one would make a world unopenable for a detail
    /// nobody can see. The writer has no such case to be lenient about: every
    /// id it is handed came from this same table, so an id with no name is not
    /// a version gap, it is a caller holding a number that means nothing. That
    /// stops the write.
    ///
    /// A block with no properties returns an empty vector and the file omits
    /// the `Properties` compound, which is what vanilla writes and what the
    /// reader expects to find missing.
    fn block_name(&self, id: u32) -> Option<(&str, Vec<(&str, &str)>)> {
        let state = dust_registry::BlockState::from_id(id)?;
        Some((state.block().name(), state.properties()))
    }

    fn biome_name(&self, id: u32) -> Option<&str> {
        self.biome_names.get(id as usize).copied()
    }
}
