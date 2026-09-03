//! What an item entity costs per tick, and what a thousand cost.
//!
//! Item entities are the classic way a Minecraft server dies: a thousand
//! dropped stacks in a tunnel, each one ticking, each one checked against
//! every player. `net/items.rs` claims three mechanisms keep that bounded —
//! nothing far from a player is ticked, two of the same item become one, and
//! everything despawns — and the first of those is the one only a measurement
//! can settle. These rows are that measurement.
//!
//! No framework, for the reason `dust-nbt`'s benches give: a fixed workload
//! timed by hand answers "how fast" without adding a dozen dependencies to the
//! lockfile.
//!
//! Each row is the one above it plus a single named change, and there are two
//! groups of them because **an item entity has two costs and they differ by an
//! order of magnitude**:
//!
//! ```text
//!   falling    the fifteen ticks between a block breaking and the item
//!              landing: gravity, a ground query, the merge pass
//!   at rest    every tick after that, which is what a tunnel full of drops
//!              actually is
//! ```
//!
//! Measuring only the second was a real hazard here rather than a theoretical
//! one. An item takes an early return out of `step` once it has settled, so a
//! bench that popped a thousand items and then ran a thousand ticks would
//! spend fifteen of them measuring physics and nine hundred and eighty-five
//! measuring a branch — and would print the average as though it were the
//! cost of an item. Each falling row therefore reports **how many of its
//! items were still moving when it finished**, and a row where that is zero
//! says so, because a bench whose subject has stopped is measuring it having
//! stopped. The same defect, from the other end, is what W2 found in the
//! movement bench on 2026-09-03.
//!
//! Run it on an idle machine. Three other agents building at once is the
//! difference between these numbers and numbers twice their size.
//!
//! ```text
//!   cargo bench -p dust-server --bench items
//! ```
//!
//! The block table is optional and its absence is printed rather than guessed
//! at: without `full_collision` an item cannot land, so the falling rows
//! measure the fallback. Point `DUST_BENCH_CONSTANTS` at a `constants.tsv` to
//! measure the real thing.
//!
//! **The world underneath matters, and it is where the surprise is.** `Ground`
//! is built inside every `ItemWorld::tick`, so its four-column cache lives for
//! one tick and is thrown away. On a flat world that costs nothing. On a world
//! read out of region files, measured with `DUST_BENCH_REGION`:
//!
//! ```text
//!   1 falling item, flat              38 ns/tick
//!   1 falling item, region files 558,308 ns/tick     14,700x
//!   100 falling items, region     5,299,738 ns/tick  10.6% of a tick
//! ```
//!
//! One column rebuilt per tick, for one item. The fix is a column cache that
//! outlives a tick, which is `collide.rs`'s to give; until then a break on a
//! saved world costs about half a millisecond a tick for the second and a half
//! the item is in the air. This is the same shape W2 found in the movement
//! bench on 2026-09-03 and for the same reason: 97% of a movement check on a
//! saved world was rebuilding a column.

use std::time::Instant;

use dust_protocol::types::Position;
use dust_registry::{BlockConstants, Item};
use dust_server::net::edits::EditedWorld;
use dust_server::net::items::ItemWorld;
use dust_server::net::players::Roster;
use dust_server::net::source::{AnvilWorld, RegistryNames, Source};
use dust_server::net::world::{FlatWorld, Palette};

/// Ticks in an at-rest row. A thousand is fifty seconds of game time.
const TICKS: u32 = 1_000;

/// Ticks in a falling row.
///
/// Long enough for an item popped at 0.2 upwards to arc and land, and short
/// enough that it has not settled: `an_item_falls_and_settles_once` says the
/// whole fall is inside sixty ticks, and the row prints how many were still
/// moving so this number cannot quietly stop being true.
const FALLING: u32 = 15;

/// Rounds per row, of which the median is printed. Five is enough to see a
/// machine that was busy for one of them.
const ROUNDS: usize = 5;

fn main() {
    let Ok(palette) = Palette::resolve() else {
        eprintln!("the generated block table has no minecraft:air");
        return;
    };
    let constants = table();
    println!(
        "block table: {}",
        match &constants {
            Some(table) => format!("{} states", table.len()),
            None => "absent, so nothing is solid and no item lands".to_owned(),
        }
    );
    let world = EditedWorld::new(Source::Flat(Box::new(FlatWorld::new(palette, 0, 64))));
    let roster = Roster::default();
    let here = [(8.0, 65.0, 8.0)];
    let far = [(4_000.0, 65.0, 4_000.0)];

    println!("\nmedian of {ROUNDS} rounds\n");
    println!("  falling — the fifteen ticks between the break and the landing");
    row(
        "nothing on the floor",
        0,
        FALLING,
        &world,
        constants.as_ref(),
        &here,
        false,
        ItemWorld::default,
    );
    for count in [1usize, 100, 1_000] {
        row(
            &format!("{count} item(s), somebody near"),
            count,
            FALLING,
            &world,
            constants.as_ref(),
            &here,
            false,
            || filled(&roster, count),
        );
    }

    println!("\n  at rest — every tick after that");
    for count in [1usize, 100, 1_000] {
        row(
            &format!("{count} item(s), somebody near"),
            count,
            TICKS,
            &world,
            constants.as_ref(),
            &here,
            false,
            || {
                let items = filled(&roster, count);
                settle(&items, &world, constants.as_ref(), &here);
                items
            },
        );
    }
    row(
        "1000 item(s), nobody near",
        1_000,
        TICKS,
        &world,
        constants.as_ref(),
        &far,
        false,
        || {
            let items = filled(&roster, 1_000);
            settle(&items, &world, constants.as_ref(), &here);
            items
        },
    );

    region_row(palette, constants.as_ref(), &roster, &here);
}

/// The same falling row over a world read out of region files.
///
/// `Ground` is built fresh inside every `ItemWorld::tick`, so its four-column
/// cache lives for one tick. On a flat world that costs nothing, because the
/// flat source lends a template; on an Anvil world a miss is a column
/// decompressed and rebuilt, which the movement bench measured at about 0.9
/// ms. This row is what says whether that reaches a falling item.
fn region_row(
    palette: Palette,
    constants: Option<&BlockConstants>,
    roster: &Roster,
    players: &[(f64, f64, f64)],
) {
    let Some(directory) = std::env::var_os("DUST_BENCH_REGION").map(std::path::PathBuf::from)
    else {
        println!(
            "\n  region files: not run. Set DUST_BENCH_REGION to a world's region directory \
             to measure the row `Ground`'s per-tick cache exists for."
        );
        return;
    };
    let Some(constants) = constants else {
        eprintln!("\n  region files: not run, because nothing is solid without a block table");
        return;
    };
    let Some(names) = RegistryNames::new() else {
        eprintln!("\n  region files: not run, no synced biome registry");
        return;
    };
    let anvil = AnvilWorld::new(
        directory,
        names,
        FlatWorld::new(palette, 0, 64),
        dust_server::net::world::opacity_of(palette.air, Some(constants)),
        Some(std::sync::Arc::new(constants.clone())),
    );
    let world = EditedWorld::new(Source::Anvil(Box::new(anvil)));
    println!("\n  falling, over region files");
    for count in [1usize, 100] {
        for resident in [false, true] {
            row(
                &name(count, resident),
                count,
                FALLING,
                &world,
                Some(constants),
                players,
                resident,
                || filled(roster, count),
            );
        }
    }
    // The rows that matter more than the ones above them, and the reason is
    // arithmetic: an item falls for fifteen ticks and then lies there for five
    // minutes, which is six thousand. What a heap of cobblestone costs a
    // server is almost entirely what it costs at rest.
    //
    // They are also the rows the claim is honest on. A falling item drifts —
    // `pop` gives it a random horizontal push — so a claim taken before a
    // fifteen-tick burst can be a column behind by the end of it, and this
    // bench runs its ticks back to back where a server puts fifty milliseconds
    // between them and re-claims in the gap. A settled heap does not move, so
    // the claim is exactly right and stays right.
    println!("\n  at rest, over region files");
    for count in [1usize, 100] {
        for resident in [false, true] {
            row(
                &name(count, resident),
                count,
                TICKS,
                &world,
                Some(constants),
                players,
                resident,
                || {
                    let items = filled(roster, count);
                    settle(&items, &world, Some(constants), players);
                    items
                },
            );
        }
    }
}

/// What a region row is called: the count, and which side of the change it is.
fn name(count: usize, resident: bool) -> String {
    format!(
        "{count} item(s), {}",
        if resident {
            "server keeping columns"
        } else {
            "the way it worked before"
        }
    )
}

/// Run the items to rest without timing it, so an at-rest row starts at rest.
fn settle(
    items: &ItemWorld,
    world: &EditedWorld,
    constants: Option<&BlockConstants>,
    players: &[(f64, f64, f64)],
) {
    let mut near = Vec::new();
    for _ in 0..60 {
        items.tick(world, constants, players, &mut near);
    }
}

/// A world with `count` items spread over a hundred blocks, which is what a
/// mining session leaves: not all in one cell, and not one per chunk either.
///
/// Spread on purpose. All of them in one cell would merge on the first tick
/// and measure a world with one item in it; spreading them past the merge
/// reach is what keeps the count the count.
fn filled(roster: &Roster, count: usize) -> ItemWorld {
    let items = ItemWorld::default();
    let item = Item::from_name("minecraft:cobblestone").expect("a 1.21.1 item");
    for index in 0..count {
        let step = index as i32;
        items.pop(
            roster,
            Position {
                x: (step % 100) * 2,
                y: 66,
                z: (step / 100) * 2,
            },
            item,
            1,
            index as u64 + 1,
        );
    }
    items
}

#[allow(clippy::too_many_arguments)]
fn row<F>(
    name: &str,
    count: usize,
    ticks: u32,
    world: &EditedWorld,
    constants: Option<&BlockConstants>,
    players: &[(f64, f64, f64)],
    resident: bool,
    mut build: F,
) where
    F: FnMut() -> ItemWorld,
{
    let mut runs = Vec::with_capacity(ROUNDS);
    let mut moving = 0;
    let mut footprint = Vec::new();
    for _ in 0..ROUNDS {
        let items = build();
        let mut near = Vec::new();
        let started = Instant::now();
        for _ in 0..ticks {
            if resident {
                // What `net::items::ItemTicker` does with a
                // `net::residency::ColumnClaim`, every tick and before the
                // tick that reads them — because a falling item is given a
                // random horizontal push and the column it reads this tick is
                // not always the one it read last.
                //
                // Two differences from the server, both of which make this row
                // **pessimistic**: the warm is waited for rather than handed to
                // the world's warming thread, and these ticks run back to back
                // where a server puts fifty milliseconds between them for that
                // thread to work in. Every column built here is charged to the
                // timed loop; on a server almost none of them would be.
                dust_server::net::items::footprint_into(&items, players, &mut footprint);
                world.hold_columns(&footprint);
                world.warm_columns(&footprint);
            }
            items.tick(world, constants, players, &mut near);
            if resident {
                world.release_columns(&footprint);
            }
        }
        runs.push(started.elapsed().as_nanos() / u128::from(ticks));
        moving = items.len() - items.at_rest();
    }
    runs.sort_unstable();
    let median = runs[ROUNDS / 2];
    let per_item = if count == 0 {
        String::new()
    } else {
        format!("  ({} ns/item)", median / count as u128)
    };
    println!(
        "    {name:<30} {median:>8} ns/tick{per_item}   fastest {}, slowest {}",
        runs[0],
        runs[ROUNDS - 1]
    );
    // The guard the module documentation is about. Only a row that meant to be
    // measuring motion can fail it, and it names itself rather than being an
    // average nobody questions.
    if count > 0 && ticks == FALLING && moving == 0 {
        println!(
            "      ...and not one of them was still moving at the end, so that row \
             measured items at rest"
        );
    }
}

/// The block table, from `DUST_BENCH_CONSTANTS` or the extract cache.
///
/// A missing input prints what would produce one and returns `None` rather
/// than failing: this bench measures something real without it, and says which
/// something.
fn table() -> Option<BlockConstants> {
    let path = std::env::var("DUST_BENCH_CONSTANTS").unwrap_or_else(|_| DEFAULT_TABLE.to_owned());
    let Ok(text) = std::fs::read_to_string(&path) else {
        eprintln!(
            "no block table at {path}. Run `cargo xtask extract --version 1.21.1 \
             --only constants`, or set DUST_BENCH_CONSTANTS."
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
