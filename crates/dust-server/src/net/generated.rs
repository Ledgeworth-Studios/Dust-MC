//! A column built from noise, which is what a player walks on when there is
//! no world file to read.
//!
//! [`super::world::FlatWorld`] served every column of every world until this
//! existed, and its own note said what it was: bedrock, three rows of dirt and
//! grass at y -60, everywhere, forever. Decision record 0012 measured what
//! that costs a player — 20,736 of 20,736 columns at the wrong height on both
//! seeds it looked at — and put the terrain second in the order of the work.
//! This is that terrain, wired to the socket.
//!
//! # What a player gets, and what they do not
//!
//! `dust_gen::terrain` is vanilla's **noise stage** and nothing past it: the
//! dimension's default block where `final_density` is positive, its default
//! fluid below the sea level its settings name, air above. So the ground is
//! stone rather than grass and there are no trees, no ores, no carved caves
//! and no villages. Mountains, valleys, overhangs, coastlines, oceans and
//! sea floors are all there and all in the right place, and the biome a cell
//! holds is the biome Minecraft would have put there.
//!
//! Surface rules are the next stage and they are the reason this is stone.
//! Inventing a rule for the top block — grass above the sea, sand beside it —
//! would be right most of the time and wrong in a way no test here could name,
//! and it would poison the measurement that is supposed to replace it.
//!
//! # Why the light needs the four columns around it
//!
//! Sky light does not stop at a chunk boundary. A flat world could be lit with
//! its own floors on all four sides because every column of it is the same
//! column; a real one cannot, and a cliff at x 16 lit as though the next chunk
//! were the same shape is a seam a player sees. So a column's neighbours are
//! generated for their sky floors — terrain only, no biomes and no light — and
//! remembered, exactly as [`super::source::AnvilWorld`] remembers the floors it
//! reads. Each position's floor is then computed once: a view distance of
//! eight is 289 columns and 72 more around its edge, not 289 times four.

use std::collections::HashMap;
use std::sync::Mutex;

use dust_world::chunk::Chunk;
use dust_world::column_light::{Skirt, SkyFloor};
use dust_world::coords::ChunkPos;
use dust_world::heightmap::WorldHeight;
use dust_world::propagation::{EmissionModel, OpacityModel};

use dust_gen::terrain::{Generator, Material};

use super::world::{FlatWorld, Palette};

/// Sky floors kept for columns that have been generated, capped and cleared
/// wholesale for the reason [`super::source`]'s own cache is.
const SKY_FLOOR_CACHE_CAP: usize = 4096;

/// What the noise stage cannot build without.
#[derive(Debug)]
pub struct MissingBlock {
    pub name: String,
}

impl std::fmt::Display for MissingBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the dimension's settings name {} and the block registry has no such state",
            self.name
        )
    }
}

impl std::error::Error for MissingBlock {}

/// A world generated from noise.
#[derive(Debug)]
pub struct GeneratedWorld {
    generator: Generator,
    /// The plain underneath, kept for the two things every source is asked
    /// for — the block palette and the world height — and for nothing else.
    /// It is a template column's worth of bookkeeping and no position is ever
    /// served from it.
    flat: FlatWorld,
    palette: Palette,
    opacity: OpacityModel,
    emission: EmissionModel,
    height: WorldHeight,
    constants: Option<std::sync::Arc<dust_registry::BlockConstants>>,
    /// The dimension's own two blocks, resolved once at boot.
    solid: u32,
    fluid: u32,
    /// What a cell gets when the biome source has no answer for it, which is
    /// the same biome the flat world serves.
    default_biome: u32,
    biome_registry_size: u32,
    floors: Mutex<HashMap<(i32, i32), SkyFloor>>,
}

impl GeneratedWorld {
    pub fn new(
        generator: Generator,
        flat: FlatWorld,
        opacity: OpacityModel,
        default_biome: u32,
        biome_registry_size: u32,
        constants: Option<std::sync::Arc<dust_registry::BlockConstants>>,
    ) -> Result<Self, MissingBlock> {
        let settings = generator.settings();
        let solid = state_of(&settings.default_block)?;
        let fluid = state_of(&settings.default_fluid)?;
        Ok(Self {
            emission: super::world::emission_of(constants.as_deref()),
            height: flat.height(),
            palette: flat.palette(),
            generator,
            flat,
            opacity,
            constants,
            solid,
            fluid,
            default_biome,
            biome_registry_size,
            floors: Mutex::new(HashMap::new()),
        })
    }

    pub fn palette(&self) -> Palette {
        self.palette
    }

    pub fn flat(&self) -> &FlatWorld {
        &self.flat
    }

    pub fn height(&self) -> WorldHeight {
        self.height
    }

    pub fn settings(&self) -> &dust_gen::noise::build::NoiseSettings {
        self.generator.settings()
    }

    /// One column, blocks, biomes, heightmaps and light.
    pub fn column(&self, pos: ChunkPos) -> Chunk {
        let mut chunk = self.build(pos, true);
        // This column's own floors go in before its neighbours are asked for
        // theirs, so a scan pays for each position once.
        self.remember(pos, SkyFloor::of(&chunk));
        let skirt = Skirt {
            west: self.sky_floor(ChunkPos::new(pos.x - 1, pos.z)),
            east: self.sky_floor(ChunkPos::new(pos.x + 1, pos.z)),
            north: self.sky_floor(ChunkPos::new(pos.x, pos.z - 1)),
            south: self.sky_floor(ChunkPos::new(pos.x, pos.z + 1)),
        };
        let _ = super::world::light_column(&mut chunk, &self.opacity, &self.emission, skirt);
        chunk
    }

    /// Blocks, and biomes only when the caller is going to serve them.
    ///
    /// A neighbour is generated for one thing — where its sky reaches — and
    /// its biomes are 2.4 ms of climate nobody would read.
    fn build(&self, pos: ChunkPos, with_biomes: bool) -> Chunk {
        let mut chunk = Chunk::uniform(
            pos,
            self.height,
            dust_registry::STATE_COUNT,
            self.biome_registry_size,
            self.palette.air,
            self.default_biome,
        );
        let mut columns = self.generator.columns();
        let min_y = self.height.min_y();
        let top = min_y + self.height.height() as i32;
        {
            let materials = columns.terrain(pos.x, pos.z);
            for y in min_y..top {
                let row = (y - min_y) as usize * 256;
                for z in 0..16u32 {
                    for x in 0..16u32 {
                        // The world's own floor is bedrock. Vanilla writes its
                        // bottom five rows with a die, which is a surface rule
                        // and is not here; one row is not that rule, it is the
                        // floor, and without it a player digs into the void.
                        let state = if y == min_y {
                            self.palette.bedrock
                        } else {
                            match Material::from_code(materials[row + (z * 16 + x) as usize]) {
                                Material::Air => self.palette.air,
                                Material::Solid => self.solid,
                                Material::Fluid => self.fluid,
                            }
                        };
                        if state != self.palette.air {
                            chunk.set_block(x, y, z, state);
                        }
                    }
                }
            }
        }
        if with_biomes {
            // Column outermost and y innermost: four of the six climate
            // functions do not depend on y and the sampler holds them for as
            // long as the column does not move.
            let base_x = pos.x * 4;
            let base_z = pos.z * 4;
            for z in (0..16u32).step_by(4) {
                for x in (0..16u32).step_by(4) {
                    let quart_x = base_x + (x as i32 >> 2);
                    let quart_z = base_z + (z as i32 >> 2);
                    for y in (min_y..top).step_by(4) {
                        if let Some(biome) = columns.biomes().biome(quart_x, y >> 2, quart_z) {
                            chunk.set_biome(x, y, z, biome);
                        }
                    }
                }
            }
        }
        chunk.recompute_heightmaps(super::world::heightmap_predicate(
            self.palette.air,
            self.constants.as_deref(),
        ));
        chunk
    }

    fn sky_floor(&self, pos: ChunkPos) -> SkyFloor {
        if let Some(held) = self
            .floors
            .lock()
            .expect("the floor map is never poisoned")
            .get(&(pos.x, pos.z))
        {
            return *held;
        }
        let floors = SkyFloor::of(&self.build(pos, false));
        self.remember(pos, floors);
        floors
    }

    fn remember(&self, pos: ChunkPos, floors: SkyFloor) {
        let mut cache = self.floors.lock().expect("the floor map is never poisoned");
        if cache.len() >= SKY_FLOOR_CACHE_CAP {
            cache.clear();
        }
        cache.insert((pos.x, pos.z), floors);
    }
}

/// Resolve a block a noise-settings file named, properties and all.
fn state_of(spec: &dust_gen::noise::build::BlockSpec) -> Result<u32, MissingBlock> {
    let missing = || MissingBlock {
        name: spec.name.clone(),
    };
    let block = dust_registry::Block::from_name(&spec.name).ok_or_else(missing)?;
    let mut state = block.default_state();
    for (property, value) in &spec.properties {
        state = state.with(property, value).ok_or_else(missing)?;
    }
    Ok(state.id())
}

/// Build a generated world out of whatever is under `[data] path`, or say why
/// there is none.
///
/// `Ok(None)` when the operator has not extracted the biome parameter list —
/// which is the ordinary case for a server that has only ever run flat, and is
/// not an error. `Err` when the files are there and do not answer: a data
/// directory that is half a world is a mistake an operator should be told
/// about at boot rather than by walking into it.
///
/// Nothing here is Mojang's. The density functions, the noise parameters and
/// the sea level come from the operator's own unpacked data pack, and the
/// biome parameter list from the table `cargo xtask extract --only worldgen`
/// writes out of their own server jar. Decision records 0006, 0007 and 0008.
#[allow(clippy::too_many_arguments)]
pub fn beside(
    data_path: &std::path::Path,
    seed: i64,
    flat: FlatWorld,
    opacity: OpacityModel,
    default_biome: u32,
    biome_registry_size: u32,
    constants: Option<std::sync::Arc<dust_registry::BlockConstants>>,
) -> Result<Option<(GeneratedWorld, Report)>, String> {
    let table = data_path.join(dust_gen::biome::FILE);
    let text = match std::fs::read_to_string(&table) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{} could not be read: {e}", table.display())),
    };
    let mut parameters = dust_gen::biome::BiomeParameters::parse(&text)
        .map_err(|e| format!("{}: {e}", table.display()))?;

    // The table carries the id its extraction saw beside the biome's name, and
    // the name is what is checked here against the registry this server will
    // actually send. A version that renumbered a biome is then caught on the
    // row it renumbered rather than by a player standing in the wrong forest.
    use dust_world::anvil::Names as _;
    let names = super::source::RegistryNames::new()
        .ok_or_else(|| "the synced registries have no biome registry".to_owned())?;
    let moved = parameters.rebind(|name| names.biome(name));
    let regions = parameters.len();
    let biomes = parameters.distinct_biomes();

    let generator = dust_gen::terrain::Generator::new(data_path, "overworld", seed, parameters)
        .map_err(|e| {
            format!(
                "{} has {} beside it but no overworld to generate: {e}",
                data_path.display(),
                dust_gen::biome::FILE
            )
        })?;
    let settings = generator.settings().clone();
    let world = GeneratedWorld::new(
        generator,
        flat,
        opacity,
        default_biome,
        biome_registry_size,
        constants,
    )
    .map_err(|e| e.to_string())?;
    Ok(Some((
        world,
        Report {
            regions,
            biomes,
            moved: moved.into_iter().map(|entry| entry.name).collect(),
            sea_level: settings.sea_level,
            default_block: settings.default_block.name,
            default_fluid: settings.default_fluid.name,
        },
    )))
}

/// What [`beside`] found, for the one line a server says about it at boot.
#[derive(Debug)]
pub struct Report {
    pub regions: usize,
    pub biomes: usize,
    /// Biomes whose id in the table is not the id this build's registry has.
    pub moved: Vec<String>,
    pub sea_level: i32,
    pub default_block: String,
    pub default_fluid: String,
}

impl Report {
    pub fn summary(&self, seed: i64) -> String {
        format!(
            "generating from seed {seed}: {} climate region(s) over {} biome(s), \
             {} above sea level {}, no surface rules yet so the ground is the \
             default block{}",
            self.regions,
            self.biomes,
            self.default_fluid,
            self.sea_level,
            if self.moved.is_empty() {
                String::new()
            } else {
                format!(
                    " — and {} biome(s) have moved since the table was written: {}",
                    self.moved.len(),
                    self.moved.join(", ")
                )
            }
        )
    }
}
