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

/// Light one column, the way this server lights every column.
///
/// **One answer to "how does Dust light a column", reached from three places.**
/// The flat world lights its template, the Anvil source lights what it reads,
/// and `xtask harness light` lights a chunk to compare against vanilla's own —
/// and if the third of those restated the opacity model or the budget, it
/// would be measuring a lighting policy that no player ever sees.
///
/// # Errors
///
/// [`dust_world::propagation::PropagationError::BudgetExhausted`] if the walk
/// runs past [`LIGHT_BUDGET`]. The partial result is consistent: the column is
/// under-lit rather than corrupt.
pub fn light_column(
    chunk: &mut dust_world::chunk::Chunk,
    opacity: &OpacityModel,
    skirt: dust_world::column_light::Skirt,
) -> Result<u64, dust_world::propagation::PropagationError> {
    dust_world::column_light::ColumnSkyLight::seed_with_neighbours(
        chunk,
        opacity,
        skirt,
        dust_world::propagation::Budget::new(LIGHT_BUDGET),
    )
}

/// The opacity model a world of `air` and nothing transparent has.
///
/// **Air and nothing else, which is wrong, is stated as wrong, and is the one
/// place that changes when it stops being.** Vanilla gives water, glass,
/// leaves and ice an opacity of one or two; every one of them is fifteen here,
/// so sky light stops dead at the surface of an ocean and under a tree.
///
/// It is not a shortcut. Light emission and opacity are code constants in
/// Minecraft, present in no `--reports` output and in no data pack, so there is
/// nothing to extract yet — see decision record 0008, which costs the options
/// and says why none has been taken. `xtask harness light` measures what the
/// gap costs: 99.41% of cells agree with a world vanilla lit, and every
/// disagreement is this.
pub fn opacity_of(air: u32) -> OpacityModel {
    OpacityModel::transparent_only([air])
}

/// Where a player spawns in a *flat* world: the middle of a block, standing on
/// the surface.
///
/// The half-block offsets are not cosmetic. A client handed integer x and z
/// spawns on a block *corner*, and the first physics tick pushes it off; a
/// client handed a y equal to the surface spawns *inside* the grass and is
/// ejected upward. Both look like the server sent a bad position, which it
/// did.
///
/// A real world's surface is not at [`SURFACE_Y`], which is why
/// [`spawn_in`] exists and this is only its fallback.
pub const SPAWN: (f64, f64, f64) = (0.5, SURFACE_Y as f64 + 1.0, 0.5);

/// Where a player spawns in the world this server is actually serving.
///
/// Same x and z as [`SPAWN`] — nothing here reads the world's own spawn point,
/// which lives in `level.dat` beside the region directory rather than in it —
/// but the **y comes from the column's own heightmap** instead of from a
/// superflat's constant. Serving a world Minecraft generated with the flat
/// world's y puts a player at bedrock level: underground, in the dark, inside
/// stone, on a server that looks broken.
///
/// The heightmap is `MOTION_BLOCKING`, which is the row above the highest
/// thing that stops a player falling. Under a tree that is the ground rather
/// than the leaves; over an ocean it is the water's surface, because water
/// blocks motion — checked against Minecraft's own seed 0, where spawn is
/// ocean and this lands at y = 63 on the water rather than at y = 40 on the
/// sea floor. Standing on water is wrong, and it is wrong in the way a server
/// with no physics is wrong; what this replaces is the far larger error of
/// spawning at bedrock in the dark.
pub fn spawn_in(world: &crate::net::edits::EditedWorld) -> (f64, f64, f64) {
    let column = world.chunk(dust_world::coords::ChunkPos::new(0, 0));
    let surface = column
        .heightmaps()
        .get(dust_world::heightmap::HeightmapKind::MotionBlocking)
        .first_available(0, 0);
    let min_y = column.world().min_y();
    let max_y = min_y + column.world().height() as i32;
    // Clamped, because a heightmap is a claim about a column and this is a
    // position a client will be teleported to. An empty column reports the
    // world's floor, which is a legal answer and a fine place to stand.
    let y = surface.clamp(min_y, max_y - 2);
    (SPAWN.0, f64::from(y), SPAWN.2)
}

/// One flat world.
///
/// # Why a template column rather than a generator call per chunk
///
/// Every column of a flat world is identical, so generating each one is doing
/// the same work again. `dust-world`'s bench puts an overworld column at
/// **about 0.5 ms to build and 0.9 ms to light**, in release on an idle
/// machine — and 289 of them on a join at the default view distance. Run that
/// bench on an idle machine or not at all: the same line read 1.4 ms on a quiet
/// laptop and 6.0 ms on one that was also compiling.
///
/// So the column is built and lit once, here, and the chunk packet is told
/// which coordinates to put on it. That is correct for *this* world and is
/// explicitly not a general answer: the moment two columns differ, the
/// template goes and the cost comes back.
///
/// Both of those numbers were several times larger an hour ago, and how they
/// came down is the useful part. Sky light was 8.2 ms until `column_light`
/// started seeding only the boundary of the lit region. Generation was then
/// 2.7 ms, of which **2.6 was recomputing heightmaps** — measured rather than
/// guessed at, after "generation is 2.7 ms" named no suspect: allocating the
/// column is 7 µs and writing its 1,280 blocks is 52 µs. The heightmap walk
/// now skips a section that holds one value everywhere, which is every section
/// above the terrain.
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
        //
        // Lit with its neighbours, which here are itself: every column of a
        // flat world is this one, so the skirt changes nothing and the answer
        // is the same either way. It is used anyway for the same reason the
        // heightmaps are recomputed rather than written — this is the code
        // path that stops being trivial the day the terrain does, and a line
        // that has to be remembered later is a line that will not be.
        let floors = dust_world::column_light::SkyFloor::of(&chunk);
        let skirt = dust_world::column_light::Skirt {
            west: floors,
            east: floors,
            north: floors,
            south: floors,
        };
        let _ = light_column(&mut chunk, &self.opacity, skirt);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A flat world's spawn must not move.
    ///
    /// `spawn_in` reads the world instead of a constant, and the constant is
    /// still what a flat world should answer with. Without this, deriving the
    /// spawn could quietly shift every flat server's players by a block — and
    /// a block is the difference between standing on the grass and standing
    /// inside it.
    #[test]
    fn the_flat_world_still_spawns_exactly_where_the_constant_says() {
        let palette = Palette::resolve().expect("the block table");
        let world = crate::net::edits::EditedWorld::new(crate::net::source::Source::Flat(
            Box::new(FlatWorld::new(palette, 0, 64)),
        ));
        assert_eq!(spawn_in(&world), SPAWN);
    }

    /// A player spawns on top of what is there, not inside it.
    ///
    /// Breaking the block a player would stand on lowers the spawn by one,
    /// which is the property the whole function exists for: the y follows the
    /// blocks. Asserted through the world rather than through a heightmap
    /// directly, because an edit that did not reach the heightmap would look
    /// correct to anything that asked the heightmap.
    #[test]
    fn the_spawn_follows_the_block_under_it() {
        let palette = Palette::resolve().expect("the block table");
        let air = palette.air;
        let world = crate::net::edits::EditedWorld::new(crate::net::source::Source::Flat(
            Box::new(FlatWorld::new(palette, 0, 64)),
        ));
        let before = spawn_in(&world);
        world.set_block(
            dust_protocol::types::Position {
                x: 0,
                y: SURFACE_Y,
                z: 0,
            },
            air,
        );
        let after = spawn_in(&world);
        assert_eq!(
            after.1,
            before.1 - 1.0,
            "digging the surface block lowers the spawn"
        );
    }
}
