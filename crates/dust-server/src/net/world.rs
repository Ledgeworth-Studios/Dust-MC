//! The world a joining player is put into.
//!
//! # What this is, honestly
//!
//! A superflat: bedrock, three rows of dirt, one of grass, air above, one
//! biome everywhere. Every chunk is identical and every chunk is generated on
//! demand from the same six lines.
//!
//! It is not worldgen and it is not pretending to be. Phase 6 builds the
//! generator; what this exists for is to be a *world* — a real column of real
//! block states at real coordinates, encoded through the real section codec,
//! streamed through the real chunk packet — so that everything between the
//! socket and the block table gets exercised by something a player can stand
//! on. A join that sends no chunks is a join nobody can tell is broken.
//!
//! # Why the block ids come from the registry and not from constants
//!
//! `minecraft:grass_block` is not a number this file may know. Its state id
//! depends on the block table the extractor produced, which depends on the
//! Minecraft version, and a constant here would be right until a version bump
//! and then silently place something else. Looking them up costs a scan at
//! boot and buys a failure that names the block.

use dust_registry::Block;
use dust_world::chunk::Chunk;
use dust_world::coords::ChunkPos;
use dust_world::heightmap::{HeightmapKind, WorldHeight};
use dust_world::propagation::DefaultOpacity as OpacityModel;

/// The block states one flat world is built from, resolved once.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub air: u32,
    pub bedrock: u32,
    pub dirt: u32,
    pub grass: u32,
}

impl Palette {
    /// Resolve every block this world needs, or name the one that is missing.
    ///
    /// Called during boot rather than per chunk, and fallible rather than
    /// unwrapping: the block table is generated, and a generated table that
    /// stopped containing `minecraft:bedrock` is a thing an operator has to be
    /// told about at start-up rather than the first time somebody joins.
    pub fn resolve() -> Result<Self, MissingBlock> {
        let state = |name: &'static str| {
            Block::from_name(name)
                .map(|block| block.default_state().id())
                .ok_or(MissingBlock { name })
        };
        Ok(Self {
            air: state("minecraft:air")?,
            bedrock: state("minecraft:bedrock")?,
            dirt: state("minecraft:dirt")?,
            grass: state("minecraft:grass_block")?,
        })
    }
}

/// A block the generated table does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MissingBlock {
    pub name: &'static str,
}

impl std::fmt::Display for MissingBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the generated block table has no {}; it is not the table this \
             server was built against",
            self.name
        )
    }
}

impl std::error::Error for MissingBlock {}

/// How much work one column's sky-light walk may do.
///
/// Counted in edge examinations. A column is 16x16x384 cells with six edges
/// each, so a full rewrite of every one is under a million; four is generous
/// enough that no honest column reaches it and finite enough that a walk which
/// somehow cannot converge stops instead of holding a thread.
const LIGHT_BUDGET: u64 = 4_000_000;

/// The y of the topmost solid row. One block of grass at the world's floor
/// plus four, which is where a vanilla superflat puts it.
pub const SURFACE_Y: i32 = -60;

/// Where a player spawns: the middle of a block, standing on the surface.
///
/// The half-block offsets are not cosmetic. A client handed integer x and z
/// spawns on a block *corner*, and the first physics tick pushes it off; a
/// client handed a y equal to the surface spawns *inside* the grass and is
/// ejected upward. Both look like the server sent a bad position, which it
/// did.
pub const SPAWN: (f64, f64, f64) = (0.5, SURFACE_Y as f64 + 1.0, 0.5);

/// One flat world.
///
/// # Why a template column rather than a generator call per chunk
///
/// Every column of a flat world is identical, so generating each one is doing
/// the same work again — and that work is not free. `dust-world`'s bench puts
/// an overworld column at **2.7 ms to generate and 0.57 ms to light**, in
/// release. Twenty-five of them is eighty milliseconds, which is more than a
/// tick.
///
/// So the column is built and lit once, here, and the chunk packet is told
/// which coordinates to put on it. That is correct for *this* world and is
/// explicitly not a general answer: the moment two columns differ, the
/// template goes and the cost comes back.
///
/// When it does, the number to beat is the 2.7 ms, not the lighting. That
/// split is recent: whole-region sky-light seeding cost 8.2 ms until
/// `column_light` started seeding only the boundary of the lit region, and the
/// bottleneck moved from the light engine to putting blocks in the container
/// one at a time.
#[derive(Debug, Clone)]
pub struct FlatWorld {
    palette: Palette,
    /// What passes light. Air only, which is exactly right for a world made of
    /// bedrock, dirt and grass — and the model the engine takes, so the day
    /// this world has glass in it there is a place to say so.
    opacity: OpacityModel,
    height: WorldHeight,
    biome: u32,
    block_registry_size: u32,
    biome_registry_size: u32,
    /// Built and lit once at construction. See the type's own note.
    template: Chunk,
}

impl FlatWorld {
    /// Build the world description. `biome` is an id into the biome registry
    /// as it was synced during configuration — the *same* order, because the
    /// client built its mapping from the packet this server sent it and a
    /// second ordering here would name a different biome.
    pub fn new(palette: Palette, biome: u32, biome_registry_size: u32) -> Self {
        let mut world = Self {
            opacity: OpacityModel::transparent_only([palette.air]),
            palette,
            height: WorldHeight::OVERWORLD,
            biome,
            block_registry_size: dust_registry::STATE_COUNT,
            biome_registry_size,
            // Replaced immediately below. An empty placeholder rather than an
            // Option, because a world without its column is not a state any
            // caller should be able to observe.
            template: Chunk::uniform(
                ChunkPos::new(0, 0),
                WorldHeight::OVERWORLD,
                dust_registry::STATE_COUNT,
                biome_registry_size,
                palette.air,
                biome,
            ),
        };
        world.template = world.generate(ChunkPos::new(0, 0));
        world
    }

    /// The block states this world is built from.
    pub fn palette(&self) -> Palette {
        self.palette
    }

    pub fn height(&self) -> WorldHeight {
        self.height
    }

    /// The column every position in this world holds.
    ///
    /// A reference, not a copy: the caller sends it and does not keep it, and
    /// the coordinates that make it a particular column live on the packet.
    pub fn column(&self) -> &Chunk {
        &self.template
    }

    /// Build the column this world is made of.
    fn generate(&self, pos: ChunkPos) -> Chunk {
        let mut chunk = Chunk::uniform(
            pos,
            self.height,
            self.block_registry_size,
            self.biome_registry_size,
            self.palette.air,
            self.biome,
        );
        let floor = self.height.min_y();
        for x in 0..16 {
            for z in 0..16 {
                chunk.set_block(x, floor, z, self.palette.bedrock);
                for y in (floor + 1)..SURFACE_Y {
                    chunk.set_block(x, y, z, self.palette.dirt);
                }
                chunk.set_block(x, SURFACE_Y, z, self.palette.grass);
            }
        }
        // The heightmaps travel in the chunk packet and the client uses them
        // for lighting and for where rain lands. Recomputed from the sections
        // rather than written by hand, so that the day this stops being flat
        // the heights follow the blocks instead of a constant.
        let air = self.palette.air;
        // Every kind counts the same blocks here, because a flat world has no
        // leaves, no water and no non-solid surface — the four maps only
        // diverge where those exist. The predicate still takes the kind, so
        // the day this world grows one, the divergence has somewhere to go.
        chunk.recompute_heightmaps(|_kind, state| state != air);

        // Sky light, from the real propagation engine rather than a constant.
        // A flat world's answer happens to be simple — fifteen above the
        // grass, nothing below it — and computing it anyway is the point: the
        // day the terrain is not flat, the light follows without this line
        // changing.
        //
        // A budget failure here would leave the column under-lit rather than
        // corrupt, and the budget is far above what a column can need, so it
        // is reported to the caller rather than swallowed.
        let _ = dust_world::column_light::ColumnSkyLight::seed(
            &mut chunk,
            &self.opacity,
            dust_world::propagation::Budget::new(LIGHT_BUDGET),
        );
        chunk
    }

    /// Which heightmaps a 1.21.1 client is sent.
    ///
    /// Two of the four: the other two are server-side bookkeeping that vanilla
    /// keeps out of the packet, and sending them would be sending the client
    /// something it has no use for and no code to read.
    pub const NETWORK_HEIGHTMAPS: [HeightmapKind; 2] =
        [HeightmapKind::MotionBlocking, HeightmapKind::WorldSurface];
}
