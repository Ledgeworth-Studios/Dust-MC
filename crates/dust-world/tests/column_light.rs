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

/// The boundary seeding must agree with whole-region seeding, cell for cell.
///
/// `ColumnSkyLight::seed` fills the open region directly and hands the walk
/// only the cells on its edge, because a lit cell whose neighbours are all lit
/// cannot brighten any of them. That is an argument, and an argument is not a
/// result: this runs the version it replaces — every open cell seeded through
/// `seed_skylight`, which is `dust-world`'s own reference — and requires the
/// two to produce identical arrays.
///
/// A faster answer that differs anywhere is not an optimisation, so the
/// comparison is every cell rather than a sample.
mod agreement {
    use super::*;
    use dust_world::propagation::{seed_skylight, LightGraph};

    /// The implementation this replaced: seed every cell the sky reaches.
    fn seed_the_slow_way(chunk: &mut Chunk, opacity: &DefaultOpacity) {
        let min_y = chunk.world().min_y();
        let max_y = min_y + chunk.world().height() as i32;
        let mut columns = Vec::with_capacity(16 * 16);
        for x in 0..16u32 {
            for z in 0..16u32 {
                let from = chunk
                    .heightmaps()
                    .get(dust_world::heightmap::HeightmapKind::MotionBlocking)
                    .first_available(x, z)
                    .max(min_y);
                columns.push((x as i32, z as i32, from..max_y));
            }
        }
        let mut graph = ColumnSkyLight::new(chunk, opacity);
        seed_skylight(&mut graph, columns, Budget::new(400_000_000)).expect("within budget");
    }

    fn arrays(chunk: &Chunk) -> Vec<u8> {
        chunk
            .sections()
            .iter()
            .flat_map(|s| s.sky_light().as_bytes().to_vec())
            .collect()
    }

    /// Build a column with a deliberately awkward roof: steps, a hole, and an
    /// overhang, so the two seedings have somewhere to disagree.
    fn broken_terrain() -> Chunk {
        let world = WorldHeight::new(-64, 384);
        let mut chunk = Chunk::uniform(ChunkPos::new(0, 0), world, REGISTRY, BIOMES, AIR, 0);
        for x in 0..16 {
            for z in 0..16 {
                chunk.set_block(x, world.min_y(), z, STONE);
                // A staircase across x, so every column has a different sky
                // floor and the open region's boundary is not a plane.
                let top = -60 + (x as i32 % 5) * 3;
                for y in (world.min_y() + 1)..=top {
                    chunk.set_block(x, y, z, STONE);
                }
                // A lid well above it, with a hole punched through — light has
                // to fall through the hole and spread sideways underneath.
                if !(6..10).contains(&x) || !(6..10).contains(&z) {
                    chunk.set_block(x, 20, z, STONE);
                }
            }
        }
        chunk.recompute_heightmaps(|_, state| state != AIR);
        chunk
    }

    #[test]
    fn the_two_seedings_agree_cell_for_cell_on_broken_terrain() {
        let opacity = DefaultOpacity::transparent_only([AIR]);

        let mut fast = broken_terrain();
        ColumnSkyLight::seed(&mut fast, &opacity, Budget::new(400_000_000)).expect("budget");

        let mut slow = broken_terrain();
        seed_the_slow_way(&mut slow, &opacity);

        let (fast, slow) = (arrays(&fast), arrays(&slow));
        assert_eq!(fast.len(), slow.len());
        let differing = fast
            .iter()
            .zip(&slow)
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(i, _)| i)
            .take(5)
            .collect::<Vec<_>>();
        assert!(
            differing.is_empty(),
            "boundary seeding differs from whole-region seeding at byte(s) {differing:?}"
        );
    }

    #[test]
    fn they_agree_on_a_flat_world_too() {
        // The degenerate case, where the open region is one slab and its
        // boundary is one layer. Worth its own test because it is the shape
        // the server actually sends, and because an off-by-one in "which cells
        // are on the edge" would show here and nowhere else.
        let opacity = DefaultOpacity::transparent_only([AIR]);
        let mut fast = column(0);
        ColumnSkyLight::seed(&mut fast, &opacity, Budget::new(400_000_000)).expect("budget");
        let mut slow = column(0);
        seed_the_slow_way(&mut slow, &opacity);
        assert_eq!(arrays(&fast), arrays(&slow));
    }

    #[test]
    fn the_fast_path_examines_far_fewer_edges() {
        // Not a timing assertion — those belong in a bench and are flaky in a
        // test. The work the walk does is returned as an edge count, and that
        // number is deterministic. Ten times fewer is a loose bound around a
        // difference that is nearer a hundred, chosen so this fails on a
        // regression rather than on noise.
        let opacity = DefaultOpacity::transparent_only([AIR]);

        let mut fast = column(0);
        let fast_edges =
            ColumnSkyLight::seed(&mut fast, &opacity, Budget::new(400_000_000)).expect("budget");

        let mut slow = column(0);
        let slow_edges = {
            let min_y = slow.world().min_y();
            let max_y = min_y + slow.world().height() as i32;
            let mut columns = Vec::new();
            for x in 0..16u32 {
                for z in 0..16u32 {
                    let from = slow
                        .heightmaps()
                        .get(dust_world::heightmap::HeightmapKind::MotionBlocking)
                        .first_available(x, z)
                        .max(min_y);
                    columns.push((x as i32, z as i32, from..max_y));
                }
            }
            let mut graph = ColumnSkyLight::new(&mut slow, &opacity);
            seed_skylight(&mut graph, columns, Budget::new(400_000_000)).expect("budget")
        };

        assert!(
            fast_edges * 10 < slow_edges,
            "the boundary walk spent {fast_edges} edges against {slow_edges}"
        );
    }

    // Keeps the unused-import warning honest: the trait is needed for the
    // reference implementation's `set_level` calls inside `seed_skylight`.
    #[allow(dead_code)]
    fn _uses_trait(g: &mut ColumnSkyLight<'_>) {
        let _ = g.level(0, 0, 0);
    }
}

/// The heightmap walk skips whole sections; these are the cases that lets it
/// get wrong.
///
/// `recompute_from_sections` decides per *section* whether it can contain a
/// match, so that a column of air above the terrain costs nothing. Three
/// answers, and each has a way to be wrong: a uniformly non-matching section
/// skipped when it should not be, a uniformly *matching* one answered from its
/// top row when the row is off by one, and a mixed one walked as before.
mod heightmaps {
    use super::*;
    use dust_world::heightmap::HeightmapKind;

    fn world() -> WorldHeight {
        WorldHeight::new(-64, 384)
    }

    fn empty() -> Chunk {
        Chunk::uniform(ChunkPos::new(0, 0), world(), REGISTRY, BIOMES, AIR, 0)
    }

    fn height(chunk: &Chunk, x: u32, z: u32) -> i32 {
        chunk
            .heightmaps()
            .get(HeightmapKind::MotionBlocking)
            .first_available(x, z)
    }

    #[test]
    fn a_world_of_nothing_but_air_has_its_floor_as_the_first_available_row() {
        let mut chunk = empty();
        chunk.recompute_heightmaps(|_, state| state != AIR);
        assert_eq!(height(&chunk, 0, 0), world().min_y());
        assert_eq!(height(&chunk, 15, 15), world().min_y());
    }

    #[test]
    fn a_column_solid_to_the_ceiling_reports_the_ceiling() {
        // The `Some(true)` case: a section where *every* cell matches is
        // answered from its top row without reading a cell, and the top row of
        // the top section is the top of the world. An off-by-one here would
        // report the row below the ceiling and nothing else would notice.
        let mut chunk = empty();
        let top = world().min_y() + world().height() as i32 - 1;
        for x in 0..16 {
            for z in 0..16 {
                for y in world().min_y()..=top {
                    chunk.set_block(x, y, z, STONE);
                }
            }
        }
        chunk.recompute_heightmaps(|_, state| state != AIR);
        assert_eq!(height(&chunk, 7, 7), top + 1, "one above the highest block");
    }

    #[test]
    fn one_block_in_an_otherwise_empty_section_is_still_found() {
        // The case a section-level skip could swallow: the section is *not*
        // uniform, so it must be walked, and the single block in it is the
        // answer. If the uniformity test read the wrong cell — cell zero of a
        // mixed container is air here — this would report the floor.
        let mut chunk = empty();
        chunk.set_block(3, 100, 5, STONE);
        chunk.recompute_heightmaps(|_, state| state != AIR);
        assert_eq!(height(&chunk, 3, 5), 101);
        assert_eq!(
            height(&chunk, 4, 5),
            world().min_y(),
            "and the column beside it is untouched"
        );
    }

    #[test]
    fn a_higher_block_wins_over_a_lower_one_across_a_section_boundary() {
        // Two blocks in two different sections, with uniform air between and
        // above. The walk has to skip the air, find the higher one, and stop —
        // reporting the lower would mean it walked past the higher without
        // seeing it.
        let mut chunk = empty();
        chunk.set_block(1, 0, 1, STONE);
        chunk.set_block(1, 200, 1, STONE);
        chunk.recompute_heightmaps(|_, state| state != AIR);
        assert_eq!(height(&chunk, 1, 1), 201);
    }

    #[test]
    fn a_block_at_the_very_top_and_very_bottom_are_both_found() {
        // The boundaries of the walk. The top row of the top section and the
        // bottom row of the bottom one are the two the section arithmetic can
        // put out of range.
        let top = world().min_y() + world().height() as i32 - 1;

        let mut high = empty();
        high.set_block(0, top, 0, STONE);
        high.recompute_heightmaps(|_, state| state != AIR);
        assert_eq!(height(&high, 0, 0), top + 1);

        let mut low = empty();
        low.set_block(0, world().min_y(), 0, STONE);
        low.recompute_heightmaps(|_, state| state != AIR);
        assert_eq!(height(&low, 0, 0), world().min_y() + 1);
    }

    #[test]
    fn a_predicate_that_accepts_air_makes_every_section_uniformly_matching() {
        // The `Some(true)` path again, reached from the other direction: the
        // world is all air and the predicate accepts air, so the top section
        // answers from its top row. A skip that assumed "uniform means skip"
        // rather than asking the predicate would report the floor here.
        let mut chunk = empty();
        let top = world().min_y() + world().height() as i32 - 1;
        chunk.recompute_heightmaps(|_, _state| true);
        assert_eq!(height(&chunk, 9, 9), top + 1);
    }
}
