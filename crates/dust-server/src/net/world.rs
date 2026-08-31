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
use dust_world::propagation::OpacityModel;

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
/// Both kinds, in the order they cost: sky light always, and block light only
/// where something in the column gives light off. The two do not interact —
/// they are separate arrays walked separately, exactly as vanilla keeps them —
/// so the block pass is skipped entirely for a column with no torch in it,
/// which is nearly every column of a real world.
///
/// # Errors
///
/// [`dust_world::propagation::PropagationError::BudgetExhausted`] if either
/// walk runs past [`LIGHT_BUDGET`]. The partial result is consistent: the
/// column is under-lit rather than corrupt.
pub fn light_column(
    chunk: &mut dust_world::chunk::Chunk,
    opacity: &OpacityModel,
    emission: &dust_world::propagation::EmissionModel,
    skirt: dust_world::column_light::Skirt,
) -> Result<u64, dust_world::propagation::PropagationError> {
    let sky = dust_world::column_light::ColumnSkyLight::seed_with_neighbours(
        chunk,
        opacity,
        skirt,
        dust_world::propagation::Budget::new(LIGHT_BUDGET),
    )?;
    let block = dust_world::column_light::ColumnBlockLight::seed(
        chunk,
        opacity,
        emission,
        dust_world::propagation::Budget::new(LIGHT_BUDGET),
    )?;
    Ok(sky + block)
}

/// What every block state gives off, given whatever constants there are.
///
/// **This is the one place that answer is decided**, beside [`opacity_of`] and
/// [`heightmap_predicate`], and unlike those two its no-table case is not an
/// approximation of anything. A server with no constants table says nothing
/// emits, and that is a refusal to invent a number rather than a stand-in for
/// one: there is no defensible guess at how bright a torch is.
///
/// On 1.21.1 the table says 1,588 of 26,684 states emit.
pub fn emission_of(
    constants: Option<&dust_registry::BlockConstants>,
) -> dust_world::propagation::EmissionModel {
    match constants {
        Some(table) => dust_world::propagation::EmissionModel::per_state(
            (0..table.len() as u32).map(|state| table.emission(state)),
        ),
        None => dust_world::propagation::EmissionModel::nothing(),
    }
}

/// The opacity model to light with, given whatever light table there is.
///
/// **This is the one place the answer is decided**, and it has two of them.
///
/// With a table — Minecraft's own `getLightBlock` for every block state, read
/// out of the operator's own jar by `cargo xtask extract --only constants` — every
/// state carries the number Minecraft carries. On 1.21.1 that is three values
/// and nothing else: 14,616 states cost nothing, 9,552 cost one, 2,516 are
/// walls.
///
/// Without one, `air` passes light and every other block is a wall. That is
/// wrong and is stated as wrong: vanilla gives water, glass, leaves and ice an
/// opacity of one, so sky light stops dead at the surface of an ocean and under
/// a tree. It is not a shortcut — opacity is a code constant in Minecraft,
/// present in no `--reports` output and in no data pack, which is decision
/// record 0008 — and `xtask harness light` measures what it costs, both ways,
/// in one run.
pub fn opacity_of(air: u32, light: Option<&dust_registry::BlockConstants>) -> OpacityModel {
    match light {
        // Dense and in id order, which is the order the table is keyed by:
        // the oracle reads Minecraft's own `IdMapper`, so no name is matched
        // anywhere along this path and there is no place for a mismatch to
        // hide.
        Some(table) => {
            OpacityModel::per_state((0..table.len() as u32).map(|state| table.opacity(state)))
        }
        None => OpacityModel::transparent_only([air]),
    }
}

/// The predicate `recompute_heightmaps` wants, given whatever constants there
/// are.
///
/// **This is the one place that answer is decided**, and like
/// [`opacity_of`] it has two of them.
///
/// With a table, each of the six heightmaps is asked its own question and the
/// answer is the one Minecraft's own `Heightmap$Types` predicate gives —
/// resolved to a column once, here, rather than by name in a loop that runs
/// six times for every one of a chunk's 98,304 cells.
///
/// Without one, every heightmap gets `state != air`. That is exactly right for
/// `WORLD_SURFACE` and wrong for the other five, and the way it is wrong is
/// visible in a player's sky light: `MOTION_BLOCKING` is where Dust's sky
/// starts, vanilla does not count short grass or a flower in it, and a cell
/// standing in one comes out a level darker than Minecraft makes it.
/// `cargo xtask harness light` measures that at 179 cells of 2.4 million on
/// seed 0 — the last of the four inputs, and the only one that was still
/// Dust's own invention.
///
/// It is not even right about air: `state != air` compares against
/// `minecraft:air` alone, and `cave_air` and `void_air` are two more states
/// vanilla's `isAir` says yes to.
pub fn heightmap_predicate(
    air: u32,
    constants: Option<&dust_registry::BlockConstants>,
) -> impl FnMut(dust_world::heightmap::HeightmapKind, u32) -> bool + '_ {
    // One lookup per heightmap, done now. A `Flag` is a resolved column, so
    // the closure below does an array index where a name would do a string
    // comparison against six candidates.
    let columns: [Option<dust_registry::constants::Flag>; 6] =
        dust_world::heightmap::HeightmapKind::ALL
            .map(|kind| constants.and_then(|table| table.flag(kind.nbt_key())));
    move |kind, state| match (constants, columns[kind as usize]) {
        (Some(table), Some(column)) => table.is_set(column, state),
        // No table, or a table written before this heightmap had a column.
        // The second case is why `flag` answers `None` rather than `false`: a
        // column that is absent means "fall back", and one that is present and
        // zero means "Minecraft says no".
        _ => state != air,
    }
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

/// Where a player spawns at the world's origin.
///
/// The x and z of [`SPAWN`], which is right for a flat world and is the
/// fallback for a real one with no `level.dat` beside it to say otherwise.
pub fn spawn_in(world: &crate::net::edits::EditedWorld) -> (f64, f64, f64) {
    spawn_at(world, 0, 0)
}

/// Where a player spawns in the column at `x`, `z`.
///
/// **The x and z come from the world and the y comes from the blocks**, and
/// those are two different sources on purpose.
///
/// The column is the world's own spawn point, read out of `level.dat` beside
/// the region directory by [`level::spawn_beside`](crate::net::level::spawn_beside)
/// — Minecraft's seed 1 spawns at x 112, z 176, and serving that world from x
/// 0, z 0 puts a joining player 176 blocks out in open ocean looking at a
/// world they were never meant to see first.
///
/// The y is **not** `level.dat`'s `SpawnY`, and this is the part worth stating.
/// A stored y is a claim about what the world was when it was written; the
/// column may have been dug out since, and a player teleported into stone is
/// ejected by the client in a direction nobody chose. The heightmap is a fact
/// about the world being served right now. Serving a real world with the
/// *flat* world's y is worse again: bedrock level, underground, in the dark,
/// on a server that looks broken.
///
/// The heightmap is `MOTION_BLOCKING`, which is the row above the highest
/// thing that stops a player falling. Under a tree that is the ground rather
/// than the leaves; over an ocean it is the water's surface, because water
/// blocks motion — checked against Minecraft's own seed 0, where this lands at
/// y = 63 on the water rather than at y = 40 on the sea floor. Standing on
/// water is wrong, and it is wrong in the way a server with no physics is
/// wrong; what it replaces is the far larger error of standing in bedrock.
pub fn spawn_at(world: &crate::net::edits::EditedWorld, x: i32, z: i32) -> (f64, f64, f64) {
    let column = world.chunk(dust_world::coords::ChunkPos::new(x >> 4, z >> 4));
    // The column's own 16x16 grid. `rem_euclid` and not `& 15` written out,
    // because a negative x is the common case here — Minecraft's seed 0 spawns
    // at x -32 — and the two agree only because the block is a power of two.
    // No sign-loss suppression, and none is needed: clippy follows
    // `rem_euclid(16)` to 0..16 and does not fire. An `#[expect]` here was an
    // error rather than dead weight, which is the lint config working.
    let (local_x, local_z) = (x.rem_euclid(16) as u32, z.rem_euclid(16) as u32);
    let surface = column
        .heightmaps()
        .get(dust_world::heightmap::HeightmapKind::MotionBlocking)
        .first_available(local_x, local_z);
    let min_y = column.world().min_y();
    let max_y = min_y + column.world().height() as i32;
    // Clamped, because a heightmap is a claim about a column and this is a
    // position a client will be teleported to. An empty column reports the
    // world's floor, which is a legal answer and a fine place to stand.
    let y = surface.clamp(min_y, max_y - 2);
    // The half-block offsets, for the reason [`SPAWN`] gives: an integer x and
    // z is a block corner, and the first physics tick pushes a player off it.
    (f64::from(x) + 0.5, f64::from(y), f64::from(z) + 0.5)
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
        // Nothing in a superflat emits: bedrock, dirt and grass. The model is
        // passed rather than assumed for the same reason the skirt is.
        let _ = light_column(
            &mut chunk,
            &self.opacity,
            &dust_world::propagation::EmissionModel::nothing(),
            skirt,
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

    /// A spawn column that is not the origin lands in the middle of that
    /// column, and the local coordinates are worked out the right way round.
    ///
    /// Both halves matter and only one of them is obvious. The half-block
    /// offsets have to follow the column rather than stay at 0.5, or every
    /// world with a spawn point puts its players on a block corner a thousand
    /// blocks from where they belong. And **a negative x is the common case,
    /// not the edge case**: Minecraft's own seed 0 spawns at x -32. `-33 >> 4`
    /// is -3 and `(-33).rem_euclid(16)` is 15, which is the column to the west
    /// and its last cell; a `/ 16` and a `% 16` would give -2 and -1, and -1
    /// is not a cell.
    #[test]
    fn a_spawn_column_that_is_not_the_origin_is_found_and_centred() {
        let palette = Palette::resolve().expect("the block table");
        let world = crate::net::edits::EditedWorld::new(crate::net::source::Source::Flat(
            Box::new(FlatWorld::new(palette, 0, 64)),
        ));
        let y = SPAWN.1;
        assert_eq!(spawn_at(&world, 112, 176), (112.5, y, 176.5));
        assert_eq!(spawn_at(&world, -32, 0), (-31.5, y, 0.5));
        assert_eq!(spawn_at(&world, -33, -1), (-32.5, y, -0.5));
        // And the origin still answers what the constant says, which is what
        // `spawn_in` is now written in terms of.
        assert_eq!(spawn_at(&world, 0, 0), SPAWN);
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
