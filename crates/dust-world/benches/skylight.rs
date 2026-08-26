//! How long one column's sky-light seeding takes.
//!
//! The stopwatch pattern the other benches here use: no framework, one
//! workload, a number printed. It exists because that number decided a design
//! — see `dust-server`'s flat world, which caches a template column rather than
//! lighting each one.

use dust_world::chunk::Chunk;
use dust_world::column_light::ColumnSkyLight;
use dust_world::coords::ChunkPos;
use dust_world::heightmap::WorldHeight;
use dust_world::propagation::{Budget, DefaultOpacity};

fn main() {
    let world = WorldHeight::OVERWORLD;
    let opacity = DefaultOpacity::transparent_only([0]);
    let rounds = 20;

    let started = std::time::Instant::now();
    for _ in 0..rounds {
        let mut chunk = Chunk::uniform(ChunkPos::new(0, 0), world, 26_684, 64, 0, 0);
        for x in 0..16 {
            for z in 0..16 {
                for y in world.min_y()..-59 {
                    chunk.set_block(x, y, z, 1);
                }
            }
        }
        chunk.recompute_heightmaps(|_, state| state != 0);
        ColumnSkyLight::seed(&mut chunk, &opacity, Budget::new(40_000_000)).expect("budget");
    }
    let each = started.elapsed() / rounds;
    println!("overworld column, generate + seed skylight: {each:?}");
}
