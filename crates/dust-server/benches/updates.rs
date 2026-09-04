//! What the world costs when it reacts to being changed.
//!
//! Neighbour updates cascade. One break queues seven positions; each of those
//! that reacts writes a block, and each of those writes queues seven more. The
//! failure this measures for is the one that kills a Minecraft server: a player
//! digs one block and a thousand positions come due inside one tick. `PER_TICK`
//! is the ceiling that stops it, and these rows are what says the ceiling is in
//! the right place.
//!
//! No framework, for the reason `dust-nbt`'s benches give: a fixed workload
//! timed by hand answers "how fast" without adding a dozen dependencies to the
//! lockfile.
//!
//! Three workloads, in the order a player meets them:
//!
//! ```text
//!   one break       a torch on a wall, mined. Seven positions, and the tick
//!                   that drains them.
//!   sand column     sixty-four blocks of sand whose bottom is dug out. Every
//!                   one of them becomes an entity, falls and lands again.
//!   a felled tree   a trunk removed under a canopy. The relabel cascade, and
//!                   then a hundred leaves waiting on their own draws.
//! ```
//!
//! And the row that says what the rest are measured against: **an idle tick**,
//! which is what the other 99.9% of ticks are.
//!
//! Run it on an idle machine. Three other agents building at once is the
//! difference between these numbers and numbers twice their size.
//!
//! ```text
//!   cargo bench -p dust-server --bench updates
//! ```
//!
//! The block table is not optional here and its absence is printed rather than
//! guessed at: with no support columns there are no rules, nothing reacts, and
//! every row would measure an empty queue.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use dust_protocol::types::Position;
use dust_registry::BlockConstants;
use dust_server::clock::ManualClock;
use dust_server::logging::{Level, Logger};
use dust_server::net::edits::EditedWorld;
use dust_server::net::falling::FallingWorld;
use dust_server::net::items::ItemWorld;
use dust_server::net::players::Roster;
use dust_server::net::source::Source;
use dust_server::net::updates::WorldTicker;
use dust_server::net::world::{FlatWorld, Palette};
use dust_server::participant::{TickContext, TickParticipant};

/// Rounds per row, of which the median is printed. Five is enough to see a
/// machine that was busy for one of them.
const ROUNDS: usize = 5;

/// How many ticks a workload is given to settle.
///
/// Past [`DECAY_HORIZON`](dust_server::net::updates::DECAY_HORIZON), because
/// the slowest thing here is the last leaf of a felled tree and it may wait
/// five minutes. Every row prints how many ticks it actually used: a row that
/// used all of these has not finished and is measuring a cut-off rather than
/// a collapse.
const SETTLE: u64 = 6_100;

fn main() {
    let Ok(palette) = Palette::resolve() else {
        eprintln!("the generated block table has no minecraft:air");
        return;
    };
    let Some(constants) = table() else {
        return;
    };
    println!("block table: {} states", constants.len());
    let constants = Arc::new(constants);
    println!("\nmedian of {ROUNDS} rounds, 50,000,000 ns to a tick\n");

    row("an idle tick", palette, &constants, |_| ());

    row(
        "one break: a torch on a wall, mined",
        palette,
        &constants,
        |w| {
            let wall = Position { x: 8, y: 66, z: 8 };
            w.set_block(Position { y: 67, ..wall }, id("minecraft:torch"));
            w.set_block(wall, id("minecraft:air"));
        },
    );

    row(
        "a 64-block sand column, dug out from under",
        palette,
        &constants,
        |w| {
            for step in 0..64 {
                w.set_block(
                    Position {
                        x: 8,
                        y: 70 + step,
                        z: 8,
                    },
                    id("minecraft:sand"),
                );
            }
            w.set_block(Position { x: 8, y: 69, z: 8 }, id("minecraft:air"));
        },
    );

    // The row that says the ceilings bound a rate and not an outcome: 1,024
    // blocks of sand want to be entities at once and `MAX_ENTITIES` is 512,
    // so half of them are put back on the schedule and go a few ticks later.
    // `fell` has to reach 1,024 here. The first version of this row measured
    // 512, and the other 512 hung in the air for ever.
    row(
        "a 32x32 raft of sand, 1,024 blocks, floor pulled out",
        palette,
        &constants,
        |w| {
            for x in 0..32 {
                for z in 0..32 {
                    w.set_block(Position { x, y: 80, z }, id("minecraft:sand"));
                }
            }
            for x in 0..32 {
                for z in 0..32 {
                    w.set_block(Position { x, y: 79, z }, id("minecraft:air"));
                }
            }
        },
    );

    row("an oak felled under its canopy", palette, &constants, |w| {
        let (x, z) = (8, 8);
        for step in 0..5 {
            w.set_block(Position { x, y: 65 + step, z }, id("minecraft:oak_log"));
        }
        // The canopy Minecraft grows: a ball of leaves around the top of the
        // trunk, at the distances the trunk gives them.
        for dy in -2i32..=2 {
            for dx in -2i32..=2 {
                for dz in -2i32..=2 {
                    if dx == 0 && dz == 0 && dy <= 0 {
                        continue;
                    }
                    if dx.abs() + dz.abs() + dy.abs() > 4 {
                        continue;
                    }
                    let at = Position {
                        x: x + dx,
                        y: 69 + dy,
                        z: z + dz,
                    };
                    let far =
                        u32::try_from(dx.abs().max(dz.abs()).max(dy.abs()).max(1)).unwrap_or(1);
                    w.set_block(at, leaf(far));
                }
            }
        }
        // The axe.
        for step in 0..5 {
            w.set_block(Position { x, y: 65 + step, z }, id("minecraft:air"));
        }
    });
}

/// Build the world, apply `setup`, then tick until the world stops changing.
fn row(
    name: &str,
    palette: Palette,
    constants: &Arc<BlockConstants>,
    setup: impl Fn(&EditedWorld),
) {
    let mut ticks = Vec::new();
    let mut totals = Vec::new();
    let mut last = None;
    for _ in 0..ROUNDS {
        let world = Arc::new(EditedWorld::new(Source::Flat(Box::new(FlatWorld::new(
            palette, 0, 64,
        )))));
        let roster = Arc::new(Roster::default());
        let items = Arc::new(ItemWorld::default());
        let falling = Arc::new(FallingWorld::default());
        let drops = Arc::new(dust_sim::drops::Tables::default());
        let mut ticker = WorldTicker::new(
            Arc::clone(&world),
            Arc::clone(&items),
            Arc::clone(&falling),
            Arc::clone(&roster),
            drops,
            Some(Arc::clone(constants)),
            palette.air,
        );
        setup(&world);
        let logger = quiet();
        let started = Instant::now();
        let mut used = 0u64;
        for tick_index in 0..SETTLE {
            ticker.tick(&TickContext {
                tick_index,
                tick_duration_ns: 50_000_000,
                logger: &logger,
            });
            used = tick_index + 1;
            // Everything the tick loop could still do is one of these three.
            // A scheduled decay an hour out is not "still settling", so a
            // round stops when nothing is queued and nothing is in the air.
            if world.updates_pending() == 0 && falling.is_empty() && ticker.scheduled() == 0 {
                break;
            }
        }
        totals.push(started.elapsed().as_nanos() as u64);
        ticks.push(used);
        last = Some(ticker.counts());
    }
    totals.sort_unstable();
    ticks.sort_unstable();
    let total = totals[ROUNDS / 2];
    let used = ticks[ROUNDS / 2];
    let counts = last.expect("a round ran");
    println!("  {name}");
    println!(
        "      {total:>12} ns over {used} tick(s), {:>10} ns/tick",
        total / used.max(1)
    );
    println!(
        "      examined {}, broken {}, fell {}, landed {}, relabelled {}, decayed {}",
        counts.examined,
        counts.broken,
        counts.fell,
        counts.landed,
        counts.relabelled,
        counts.decayed
    );
    if counts.deferred > 0 {
        println!(
            "      {} cell(s) waited for room under the entity ceiling",
            counts.deferred
        );
    }
}

fn quiet() -> Logger {
    Logger::new(
        Arc::new(Mutex::new(std::io::sink())),
        Level::Error,
        Arc::new(ManualClock::default()),
    )
}

fn id(name: &str) -> u32 {
    dust_registry::Block::from_name(name)
        .expect("the bench names a real block")
        .default_state()
        .id()
}

/// An oak leaf at one distance from the trunk, as a grown one is.
fn leaf(distance: u32) -> u32 {
    dust_registry::Block::from_name("minecraft:oak_leaves")
        .expect("oak leaves")
        .default_state()
        .with(
            "distance",
            ["0", "1", "2", "3", "4", "5", "6", "7"][distance as usize],
        )
        .expect("oak leaves carry a distance")
        .with("persistent", "false")
        .expect("oak leaves carry a persistent")
        .id()
}

/// The block table, from `DUST_BENCH_CONSTANTS` or the extract cache.
fn table() -> Option<BlockConstants> {
    let path = std::env::var("DUST_BENCH_CONSTANTS").unwrap_or_else(|_| DEFAULT_TABLE.to_owned());
    let Ok(text) = std::fs::read_to_string(&path) else {
        eprintln!(
            "no block table at {path}. Run `cargo xtask extract --version 1.21.1 \
             --only constants`, or set DUST_BENCH_CONSTANTS. Without one there \
             are no rules and every row below would measure an empty queue."
        );
        return None;
    };
    match BlockConstants::parse(&text) {
        Ok(table) => Some(table),
        Err(why) => {
            eprintln!("{path}: {why}");
            None
        }
    }
}

const DEFAULT_TABLE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../.dust-extract/oracle-1.21.1/constants.tsv"
);
