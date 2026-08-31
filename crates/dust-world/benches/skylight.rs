//! What one column costs to build and to light.
//!
//! The stopwatch pattern the other benches here use: no framework, one
//! workload, numbers printed. It exists because these numbers decided two
//! designs — the boundary seeding in `column_light`, and `dust-server`'s flat
//! world caching a template column rather than lighting each one — and a
//! decision made on a measurement should be re-checkable by rerunning it.
//!
//! Generation and lighting are timed apart, because together they answer no
//! question: a reader wanting to know whether lighting is affordable cannot
//! subtract a number that was never printed.
//!
//! **Run it on an idle machine.** Twenty rounds is enough to separate numbers
//! that differ by a factor and not enough to separate ones that differ by a
//! third, and the same line read 1.4 ms on a quiet laptop and 6.0 ms on one
//! that was also compiling. A reading taken beside a build is not a control.
//!
//! What it says as of 2026-08-30, on an idle machine: generating an overworld
//! column is about 0.5 ms and lighting it about 0.9 ms. Lighting it against
//! neighbours shaped like itself — which is nearly every column of a real
//! world — is not distinguishable from lighting it alone; lighting it against
//! a cliff of open sky on all four sides, which is the most a skirt can ask
//! for, roughly doubles the lighting and leaves it under the cost of
//! generation.

use dust_world::chunk::Chunk;
use dust_world::column_light::{ColumnSkyLight, Skirt, SkyFloor};
use dust_world::coords::ChunkPos;
use dust_world::heightmap::{HeightmapKind, WorldHeight};
use dust_world::propagation::{seed_skylight, Budget, LightGraph, OpacityModel};

const AIR: u32 = 0;
const STONE: u32 = 1;
const SURFACE: i32 = -60;
const ROUNDS: u32 = 20;

fn generate(world: WorldHeight) -> Chunk {
    let mut chunk = Chunk::uniform(ChunkPos::new(0, 0), world, 26_684, 64, AIR, 0);
    for x in 0..16 {
        for z in 0..16 {
            for y in world.min_y()..=SURFACE {
                chunk.set_block(x, y, z, STONE);
            }
        }
    }
    chunk.recompute_heightmaps(|_, state| state != AIR);
    chunk
}

/// The implementation `ColumnSkyLight::seed` replaced: every cell the sky
/// reaches, handed to the walk as a seed.
fn seed_every_open_cell(chunk: &mut Chunk, opacity: &OpacityModel) {
    let min_y = chunk.world().min_y();
    let max_y = min_y + chunk.world().height() as i32;
    let mut columns = Vec::with_capacity(16 * 16);
    for x in 0..16u32 {
        for z in 0..16u32 {
            let from = chunk
                .heightmaps()
                .get(HeightmapKind::MotionBlocking)
                .first_available(x, z)
                .max(min_y);
            columns.push((x as i32, z as i32, from..max_y));
        }
    }
    let mut graph = ColumnSkyLight::new(chunk, opacity);
    seed_skylight(&mut graph, columns, Budget::new(400_000_000)).expect("budget");
    let _ = graph.level(0, 0, 0);
}

fn time(label: &str, rounds: u32, mut body: impl FnMut()) {
    let started = std::time::Instant::now();
    for _ in 0..rounds {
        body();
    }
    println!("{label}: {:?}", started.elapsed() / rounds);
}

fn main() {
    let world = WorldHeight::OVERWORLD;
    let opacity = OpacityModel::transparent_only([AIR]);

    // Generation split into its three parts, because "generation is 2.7 ms"
    // named no suspect. Two of these turned out to be nearly free.
    time("  allocate an empty column             ", ROUNDS, || {
        std::hint::black_box(Chunk::uniform(
            ChunkPos::new(0, 0),
            world,
            26_684,
            64,
            AIR,
            0,
        ));
    });
    time("  ...plus writing the five solid rows  ", ROUNDS, || {
        let mut chunk = Chunk::uniform(ChunkPos::new(0, 0), world, 26_684, 64, AIR, 0);
        for x in 0..16 {
            for z in 0..16 {
                for y in world.min_y()..=SURFACE {
                    chunk.set_block(x, y, z, STONE);
                }
            }
        }
        std::hint::black_box(chunk);
    });

    time("overworld column, generate only        ", ROUNDS, || {
        std::hint::black_box(generate(world));
    });

    time("overworld column, boundary seeding     ", ROUNDS, || {
        let mut chunk = generate(world);
        ColumnSkyLight::seed(&mut chunk, &opacity, Budget::new(400_000_000)).expect("budget");
        std::hint::black_box(chunk);
    });

    time("overworld column, whole-region seeding ", ROUNDS, || {
        let mut chunk = generate(world);
        seed_every_open_cell(&mut chunk, &opacity);
        std::hint::black_box(chunk);
    });

    // What crossing a chunk boundary costs. The neighbours here are a cliff —
    // open sky right down to the world's floor beside a column that is solid
    // to the surface — which is the *most* a skirt can ask for: every cell of
    // all four faces below the surface is a seed. Real terrain steps a few
    // blocks and seeds a few dozen.
    let cliff = Skirt::open(world.min_y());
    time("overworld column, lit against a cliff  ", ROUNDS, || {
        let mut chunk = generate(world);
        ColumnSkyLight::seed_with_neighbours(&mut chunk, &opacity, cliff, Budget::new(400_000_000))
            .expect("budget");
        std::hint::black_box(chunk);
    });

    // And against neighbours shaped like itself, which is what almost every
    // column in a real world has: the skirt finds nothing to add, and the
    // difference from the line above is the cost of asking.
    let flat = {
        let column = generate(world);
        let floors = SkyFloor::of(&column);
        Skirt {
            west: floors,
            east: floors,
            north: floors,
            south: floors,
        }
    };
    time("overworld column, lit against its like ", ROUNDS, || {
        let mut chunk = generate(world);
        ColumnSkyLight::seed_with_neighbours(&mut chunk, &opacity, flat, Budget::new(400_000_000))
            .expect("budget");
        std::hint::black_box(chunk);
    });
}
