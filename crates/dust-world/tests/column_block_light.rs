//! Block light over one column: a torch lights a room, and nothing else does.
//!
//! What these check is that the light follows the *emitters* — that a cell
//! beside a glowing block is bright, that the brightness falls off a level a
//! step, that a wall stops it, and that a column with nothing glowing in it
//! comes back completely dark. The last one is not a formality: block light
//! shares its walk with sky light, and a pass that seeded from the sky floor by
//! mistake would fill an empty column with fifteens and every structural check
//! would still pass.
//!
//! **What they cannot check is whether Minecraft agrees**, because nothing in
//! this repository holds Minecraft's emission values to compare against. That
//! is `cargo xtask harness light`, against block light a real server computed.

use dust_world::chunk::Chunk;
use dust_world::column_light::ColumnBlockLight;
use dust_world::coords::ChunkPos;
use dust_world::heightmap::WorldHeight;
use dust_world::propagation::{Budget, EmissionModel, OpacityModel};

const AIR: u32 = 0;
const STONE: u32 = 1;
/// A block that gives off fourteen, which is what a torch gives off.
const TORCH: u32 = 2;
const REGISTRY: u32 = 64;
const BIOMES: u32 = 4;

fn world() -> WorldHeight {
    WorldHeight::new(-64, 384)
}

fn air_column() -> Chunk {
    Chunk::uniform(ChunkPos::new(0, 0), world(), REGISTRY, BIOMES, AIR, 0)
}

/// Air passes light, stone and the torch itself are walls — which is what
/// Minecraft says about a torch too: `canOcclude` is false but `getLightBlock`
/// is zero, so it is `transparent_only` that has to name it.
fn opacity() -> OpacityModel {
    OpacityModel::transparent_only([AIR, TORCH])
}

/// Nothing emits but `TORCH`, at fourteen.
fn emission() -> EmissionModel {
    EmissionModel::per_state([0, 0, 14])
}

fn level(chunk: &Chunk, x: u32, y: i32, z: u32) -> u8 {
    let row = (y - chunk.world().min_y()) as u32 % 16;
    chunk.section(y).block_light().get(x, row, z)
}

#[test]
fn a_column_with_nothing_glowing_in_it_is_completely_dark() {
    // The check the walk being shared with sky light makes worth writing: a
    // pass that seeded from the sky floor would fill this with fifteens and
    // every other test here would still pass.
    let mut chunk = air_column();
    let spent = ColumnBlockLight::seed(&mut chunk, &opacity(), &emission(), Budget::default())
        .expect("nothing to do");
    assert_eq!(spent, 0, "no emitters means no walk at all");
    for y in [-64, 0, 63, 100, 319] {
        assert_eq!(level(&chunk, 8, y, 8), 0, "y {y}");
    }
}

#[test]
fn a_model_that_emits_nothing_does_no_work_even_with_a_torch_in_the_column() {
    // The state a server with no constants table is in. It is not an
    // approximation of Minecraft — it is declining to invent how bright a
    // torch is — and the fast path says so by costing nothing.
    let mut chunk = air_column();
    chunk.set_block(8, 0, 8, TORCH);
    let spent = ColumnBlockLight::seed(
        &mut chunk,
        &opacity(),
        &EmissionModel::nothing(),
        Budget::default(),
    )
    .expect("nothing to do");
    assert_eq!(spent, 0);
    assert_eq!(level(&chunk, 8, 0, 8), 0);
}

#[test]
fn a_torch_holds_its_own_emission_and_falls_off_a_level_a_step() {
    let mut chunk = air_column();
    chunk.set_block(8, 0, 8, TORCH);
    ColumnBlockLight::seed(&mut chunk, &opacity(), &emission(), Budget::default())
        .expect("one torch fits any budget");

    assert_eq!(level(&chunk, 8, 0, 8), 14, "the torch's own cell");
    // Sideways as far as the column goes, then upward for the rest: a column
    // is only sixteen wide and the light reaches fourteen.
    for step in 1..=7u32 {
        assert_eq!(
            level(&chunk, 8 + step, 0, 8),
            14 - step as u8,
            "{step} block(s) east"
        );
    }
    for step in 1..=13i32 {
        assert_eq!(
            level(&chunk, 8, step, 8),
            14 - step as u8,
            "{step} block(s) up"
        );
    }
    assert_eq!(level(&chunk, 8, 14, 8), 0, "fourteen up is out of reach");
}

#[test]
fn a_wall_stops_it() {
    // The whole reason a walk is needed rather than a distance: light goes
    // round a wall and not through it, so a cell behind one is lit by the long
    // way or not at all.
    let mut chunk = air_column();
    chunk.set_block(8, 0, 8, TORCH);
    for x in 0..16u32 {
        for y in -8..8i32 {
            chunk.set_block(x, y, 10, STONE);
        }
    }
    assert_eq!(level(&chunk, 8, 0, 11), 0, "before lighting");
    ColumnBlockLight::seed(&mut chunk, &opacity(), &emission(), Budget::default())
        .expect("a small room fits any budget");
    assert_eq!(level(&chunk, 8, 0, 9), 13, "in front of the wall");
    assert_eq!(level(&chunk, 8, 0, 10), 0, "the wall itself");
    assert_eq!(
        level(&chunk, 8, 0, 11),
        0,
        "behind a wall that runs the width of the column and is taller than \
         the light can climb"
    );
}

#[test]
fn lighting_twice_gives_the_same_answer() {
    // The arrays are cleared and recomputed rather than corrected, so a second
    // pass must not add to the first. A walk that raised on top of what was
    // there would be idempotent by accident here and wrong the moment a torch
    // was removed.
    let mut chunk = air_column();
    chunk.set_block(8, 0, 8, TORCH);
    ColumnBlockLight::seed(&mut chunk, &opacity(), &emission(), Budget::default()).expect("once");
    let once: Vec<u8> = (0..16u32).map(|x| level(&chunk, x, 0, 8)).collect();
    ColumnBlockLight::seed(&mut chunk, &opacity(), &emission(), Budget::default()).expect("twice");
    let twice: Vec<u8> = (0..16u32).map(|x| level(&chunk, x, 0, 8)).collect();
    assert_eq!(once, twice);
}

#[test]
fn a_torch_that_is_taken_away_takes_its_light_with_it() {
    // The case the clearing pass exists for. Relighting a column whose torch
    // has gone must produce darkness, not the previous answer.
    let mut chunk = air_column();
    chunk.set_block(8, 0, 8, TORCH);
    ColumnBlockLight::seed(&mut chunk, &opacity(), &emission(), Budget::default()).expect("lit");
    assert_eq!(level(&chunk, 8, 0, 8), 14);

    chunk.set_block(8, 0, 8, AIR);
    ColumnBlockLight::seed(&mut chunk, &opacity(), &emission(), Budget::default()).expect("relit");
    for x in 0..16u32 {
        assert_eq!(level(&chunk, x, 0, 8), 0, "x {x}");
    }
}

#[test]
fn opacity_costs_the_block_and_not_the_block_plus_the_step() {
    // `step_cost` seen from block light's side. A column of something with an
    // opacity of one reads 14, 13, 12 away from a torch and not 14, 12, 10.
    let mut chunk = air_column();
    chunk.set_block(8, 0, 8, TORCH);
    for x in 9..16u32 {
        chunk.set_block(x, 0, 8, STONE);
    }
    let water_like = OpacityModel::per_state([0, 1, 0]);
    ColumnBlockLight::seed(&mut chunk, &water_like, &emission(), Budget::default())
        .expect("a short row");
    assert_eq!(level(&chunk, 9, 0, 8), 13);
    assert_eq!(level(&chunk, 10, 0, 8), 12);
    assert_eq!(level(&chunk, 11, 0, 8), 11);
}

#[test]
fn sky_light_is_untouched_by_the_block_pass() {
    // Two arrays, two walks, and the failure worth guarding is one writing into
    // the other's array — which would look right in isolation and wrong to any
    // client, because a renderer takes the brighter of the two.
    let mut chunk = air_column();
    chunk.set_block(8, 0, 8, TORCH);
    for x in 0..16u32 {
        for z in 0..16u32 {
            chunk.section_mut(0).sky_light_mut().set(x, 0, z, 7);
        }
    }
    ColumnBlockLight::seed(&mut chunk, &opacity(), &emission(), Budget::default()).expect("lit");
    assert_eq!(chunk.section(0).sky_light().get(8, 0, 8), 7);
    assert_eq!(level(&chunk, 8, 0, 8), 14);
}
