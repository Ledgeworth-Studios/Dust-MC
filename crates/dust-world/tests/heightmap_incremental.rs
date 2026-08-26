//! Incremental heightmap maintenance against full recomputes, under random
//! edit schedules.
//!
//! Folding one block change into a heightmap is exact or it is nothing: any
//! disagreement with a recompute — a surface left high after its block was
//! removed, a column dropped to the floor because an interior edit was read
//! as the top leaving — is a mob standing in air, and none of it shows on a
//! single hand-picked edit. So this file plays hundreds of deterministic
//! schedules of random edits through [`Chunk::set_block_maintaining`] and
//! checks both ways after every step:
//!
//! * against [`HeightmapSet::recompute_from_sections`] over the same chunk,
//!   which is the incremental path's own definition of correct; and
//! * against a model built from raw column contents, independent of both,
//!   so a bug shared by the two implementations cannot hide between them.
//!
//! **On randomness:** fixed-seed xorshift throughout; a failure names its
//! seed and step and replays exactly.
//!
//! **What this does not catch:** predicates that disagree with the registry.
//! Both doors here take the predicate as a parameter, and a wrong one makes
//! every answer consistently wrong together.

use dust_world::{Chunk, ChunkPos, HeightmapKind, HeightmapSet, WorldHeight};

const COLUMNS: u32 = 16;

fn xorshift(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

/// The predicate under test: zero counts as nothing on any map, everything
/// else counts for the two maps a placed chunk maintains. Deliberately not
/// the real registry's answers -- the property must hold for whatever the
/// caller decides counting means.
fn counts(kind: HeightmapKind, state: u32) -> bool {
    match kind {
        HeightmapKind::WorldSurface | HeightmapKind::MotionBlocking => state != 0,
        _ => false,
    }
}

/// The truth according to the blocks themselves: for each column, one past
/// the highest counted row, or the floor if there is none. Written out here
/// rather than derived from either heightmap path.
fn expected_first_available(states: &[u32], world: WorldHeight, x: u32, z: u32) -> i32 {
    let rows = world.height();
    for row in (0..rows).rev() {
        let state = states[(x + z * COLUMNS) as usize * rows as usize + row as usize];
        if counts(HeightmapKind::WorldSurface, state) {
            return world.min_y() + row as i32 + 1;
        }
    }
    world.min_y()
}

#[test]
fn incremental_maintenance_matches_full_recomputes_across_random_edit_schedules() {
    // A four-section world: tall enough that schedules cross section borders
    // often, short enough that a full recompute after every step stays
    // affordable. Many short schedules rather than few long ones: each step
    // costs two whole-chunk checks, and independent starting states cover
    // more of the space than marinating one state for longer.
    let world = WorldHeight::new(0, 64);
    let rows = world.height();

    for seed in 0..100u64 {
        let mut state = seed.wrapping_mul(0xda3e_39cb_94b9_5b04) | 1;
        // A fresh chunk per seed, so every schedule starts from flat ground.
        let mut chunk = Chunk::uniform(ChunkPos::new(-7, 3), world, 4, 64, 0, 1);
        // Ground truth: the whole chunk's states, flat, x+z*16 major.
        let mut cells = vec![0u32; (COLUMNS * COLUMNS * rows) as usize];

        for step in 0..40usize {
            let x = (xorshift(&mut state) % 16) as u32;
            let z = (xorshift(&mut state) % 16) as u32;
            let y = world.min_y() + (xorshift(&mut state) % u64::from(rows)) as i32;
            let new_state = (xorshift(&mut state) % 4) as u32;
            let index = (x + z * COLUMNS) as usize * rows as usize + (y - world.min_y()) as usize;
            let previous = cells[index];
            cells[index] = new_state;

            chunk.set_block_maintaining(x, y, z, new_state, counts);

            // Door one: the incremental maps agree with a full recompute.
            let mut recomputed = HeightmapSet::new(world);
            let sections: Vec<_> = chunk.sections().iter().map(|s| s.states()).collect();
            recomputed.recompute_from_sections(&sections, counts);
            assert_eq!(
                chunk.heightmaps(),
                &recomputed,
                "seed {seed} step {step}: ({x}, {y}, {z}) {previous} -> {new_state}"
            );

            // Door two: both agree with what the blocks themselves say.
            for z in 0..COLUMNS {
                for x in 0..COLUMNS {
                    let want = expected_first_available(&cells, world, x, z);
                    let got = chunk
                        .heightmaps()
                        .get(HeightmapKind::WorldSurface)
                        .first_available(x, z);
                    assert_eq!(got, want, "seed {seed} step {step}: column ({x}, {z})");
                }
            }
        }
    }
}

#[test]
fn the_same_schedules_hold_at_the_overworld_width_where_nine_bit_columns_pack() {
    // 384 rows store at nine bits -- seven columns per long, padding bits
    // behind them. Sinking surfaces walk stored values down through every
    // bit pattern a real world produces, which is where a packing bug would
    // smear a column into its neighbour. Fewer seeds than the small world:
    // each recompute walks twenty-four sections.
    let world = WorldHeight::OVERWORLD;
    let rows = world.height();

    for seed in 0..20u64 {
        let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
        let mut chunk = Chunk::uniform(ChunkPos::new(11, -2), world, 4, 64, 0, 1);
        for step in 0..30usize {
            let x = (xorshift(&mut state) % 16) as u32;
            let z = (xorshift(&mut state) % 16) as u32;
            let y = world.min_y() + (xorshift(&mut state) % u64::from(rows)) as i32;
            let new_state = (xorshift(&mut state) % 4) as u32;

            chunk.set_block_maintaining(x, y, z, new_state, counts);

            let mut recomputed = HeightmapSet::new(world);
            let sections: Vec<_> = chunk.sections().iter().map(|s| s.states()).collect();
            recomputed.recompute_from_sections(&sections, counts);
            assert_eq!(
                chunk.heightmaps(),
                &recomputed,
                "seed {seed} step {step}: ({x}, {y}, {z}) -> {new_state}"
            );
        }
    }
}
