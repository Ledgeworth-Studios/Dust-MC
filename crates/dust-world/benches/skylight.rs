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

use dust_world::chunk::Chunk;
use dust_world::column_light::ColumnSkyLight;
use dust_world::coords::ChunkPos;
use dust_world::heightmap::{HeightmapKind, WorldHeight};
use dust_world::propagation::{seed_skylight, Budget, DefaultOpacity, LightGraph};

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
fn seed_every_open_cell(chunk: &mut Chunk, opacity: &DefaultOpacity) {
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
    let opacity = DefaultOpacity::transparent_only([AIR]);

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
}
