//! Sky light over one column, against a terrain whose answer is known.
//!
//! The value being checked is not "the walk ran". It is that the light follows
//! the *blocks* — that a cell under a solid block is dark and a cell above it
//! is not — because the failure this replaces was a constant fifteen
//! everywhere, which is a light array that looks perfectly well-formed and is
//! wrong in exactly the places anybody would notice.

use dust_world::chunk::Chunk;
use dust_world::column_light::ColumnSkyLight;
use dust_world::coords::ChunkPos;
use dust_world::heightmap::WorldHeight;
use dust_world::propagation::{Budget, DefaultOpacity};

const AIR: u32 = 0;
const STONE: u32 = 1;
const REGISTRY: u32 = 64;
const BIOMES: u32 = 4;

/// A column with a solid floor at `min_y` and a solid lid at `lid_y`, air
/// between and above.
fn column(lid_y: i32) -> Chunk {
    let world = WorldHeight::new(-64, 384);
    let mut chunk = Chunk::uniform(ChunkPos::new(0, 0), world, REGISTRY, BIOMES, AIR, 0);
    for x in 0..16 {
        for z in 0..16 {
            chunk.set_block(x, world.min_y(), z, STONE);
            chunk.set_block(x, lid_y, z, STONE);
        }
    }
    chunk.recompute_heightmaps(|_, state| state != AIR);
    chunk
}

fn seed(chunk: &mut Chunk) {
    let opacity = DefaultOpacity::transparent_only([AIR]);
    ColumnSkyLight::seed(chunk, &opacity, Budget::new(4_000_000)).expect("within budget");
}

/// Read the sky level at a world y.
fn sky(chunk: &Chunk, x: u32, y: i32, z: u32) -> u8 {
    let row = (y - chunk.world().min_y()) as u32 % 16;
    chunk.section(y).sky_light().get(x, row, z)
}

#[test]
fn open_sky_is_full_and_under_a_lid_is_dark() {
    let lid_y = 0;
    let mut chunk = column(lid_y);
    seed(&mut chunk);

    // Above the lid: open sky, all the way up.
    assert_eq!(sky(&chunk, 8, lid_y + 1, 8), 15, "just above the lid");
    assert_eq!(sky(&chunk, 8, 200, 8), 15, "far above the lid");

    // The lid itself and everything under it: sealed, so no sky reaches. This
    // is the assertion a constant-fifteen light array fails and nothing else
    // does — the room under the lid is the cave, and lighting it like a meadow
    // is the bug.
    assert_eq!(sky(&chunk, 8, lid_y, 8), 0, "inside the lid");
    assert_eq!(sky(&chunk, 8, lid_y - 1, 8), 0, "the sealed room below");
    assert_eq!(sky(&chunk, 8, -60, 8), 0, "near the floor");
}

#[test]
fn light_attenuates_sideways_under_an_overhang_rather_than_stopping_dead() {
    // A lid over half the column: the open half is lit, the covered half is
    // lit *from the side*, fading with distance. That fade is the whole reason
    // for a propagation walk rather than a per-column height lookup — a
    // heightmap alone would make the covered half uniformly black.
    let world = WorldHeight::new(-64, 384);
    let mut chunk = Chunk::uniform(ChunkPos::new(0, 0), world, REGISTRY, BIOMES, AIR, 0);
    for x in 0..16 {
        for z in 0..16 {
            chunk.set_block(x, world.min_y(), z, STONE);
        }
    }
    for x in 0..8 {
        for z in 0..16 {
            chunk.set_block(x, 0, z, STONE);
        }
    }
    chunk.recompute_heightmaps(|_, state| state != AIR);
    seed(&mut chunk);

    // Under the open half: full sky.
    assert_eq!(sky(&chunk, 12, -1, 8), 15);

    // Under the overhang, walking away from the edge: strictly darker each
    // step, and never brighter than the step before.
    let mut previous = 16u8;
    for x in (0..8).rev() {
        let level = sky(&chunk, x, -1, 8);
        assert!(
            level < previous,
            "sky at x={x} is {level}, not less than {previous} — light under an \
             overhang must fade with distance from the edge"
        );
        previous = level;
    }
    assert_eq!(sky(&chunk, 0, -1, 8), 7, "eight steps in from the edge");
}

#[test]
fn seeding_twice_changes_nothing() {
    // The walk is idempotent over an unchanged column, which is what makes it
    // safe to run on every chunk send rather than only on generation.
    let mut chunk = column(0);
    seed(&mut chunk);
    let before: Vec<u8> = chunk
        .sections()
        .iter()
        .flat_map(|s| s.sky_light().as_bytes().to_vec())
        .collect();
    seed(&mut chunk);
    let after: Vec<u8> = chunk
        .sections()
        .iter()
        .flat_map(|s| s.sky_light().as_bytes().to_vec())
        .collect();
    assert_eq!(before, after);
}

#[test]
fn light_stops_at_the_column_edge_rather_than_leaking_out_of_the_world() {
    // A stated limitation, asserted so it is a decision rather than a
    // discovery: one column knows nothing about its neighbours, so a cell at
    // x = 15 is at the edge of the volume. The walk must stop there rather
    // than index past the chunk.
    let mut chunk = column(0);
    seed(&mut chunk);
    assert_eq!(sky(&chunk, 15, 5, 15), 15, "the far corner is still lit");
    assert_eq!(sky(&chunk, 0, 5, 0), 15, "and so is the near one");
}
