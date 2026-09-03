//! What one movement packet's collision check costs.
//!
//! The stopwatch pattern this repository's other benches use: no framework, one
//! workload, numbers printed. It exists because a number decided a design —
//! whether a server can afford to ask the world about every movement packet
//! from every player, twenty times a second each — and a decision made on a
//! measurement should be re-checkable by rerunning it.
//!
//! Not `dust_guard::Movement` against a `HashSet`, which would measure the
//! arithmetic and nothing that matters. This is the production path with the
//! socket taken off it: a real `EditedWorld` over a real `Source`, the real
//! block table read from disk, and `Movement::claimed` called with the real
//! `Ground` a session builds.
//!
//! Four rows, each the one above it plus a single named change, because a
//! single number cannot say which half of a cost belongs to which input:
//!
//! 1. **no world** — `dust_guard::Open`, which is what the server ran before
//!    this check existed and what it runs with `movement_collision = false`.
//!    The floor: everything a movement packet cost yesterday.
//! 2. **flat, in the open** — a player walking across a superflat with nothing
//!    solid in the box. One box question, no hit.
//! 3. **flat, into the ground** — the same walk with the feet a block lower, so
//!    the box does find something and the second question, about where the
//!    player came from, is asked too. The worst case for a flat world.
//! 4. **region files** — the same walk over a world read from `.mca`. This is
//!    the row the four-column cache exists for; run it with
//!    `DUST_BENCH_REGION=<a world's region directory>`.
//!
//! **Run it on an idle machine.** A reading taken beside a build is not a
//! control; the same line read three times as long on this machine while three
//! other agents were compiling.
//!
//! Needs Minecraft's own block table, which is never committed:
//!
//! ```text
//! cargo xtask extract --version 1.21.1 --only constants
//! DUST_BENCH_CONSTANTS=.dust-extract/oracle-1.21.1/constants.tsv \
//!   cargo bench -p dust-server --bench movement
//! ```

use std::time::Instant;

use dust_guard::{Movement, SpeedLimit};
use dust_server::net::collide::Ground;
use dust_server::net::edits::EditedWorld;
use dust_server::net::source::{AnvilWorld, RegistryNames, Source};
use dust_server::net::world::{FlatWorld, Palette};

/// Movement packets per row. A walking client sends twenty a second, so this
/// is about a minute and a half of one player.
const PACKETS: u32 = 2_000;

/// How far the walk goes before turning round, in blocks.
///
/// A there-and-back rather than a straight line, and four chunks rather than
/// twenty-seven. Two reasons, and the second is the one that matters: a
/// straight line off into a world read from region files leaves the generated
/// chunks after a hundred blocks or so and spends the rest of the row on the
/// flat fallback, which is a measurement of the flat row wearing the region
/// row's name. Turning round keeps every step on real terrain and still crosses
/// a chunk boundary every seventy-four steps, which is what the column cache is
/// there for.
const SPAN: f64 = 64.0;

/// How many times each row is run. The median of five, because a single
/// reading on a shared machine is a reading of the machine.
const ROUNDS: u32 = 5;

fn main() {
    let Some(constants) = table() else { return };
    let Some(palette) = Palette::resolve().ok() else {
        eprintln!("the generated block table has no bedrock; nothing to bench");
        return;
    };
    let solid = dust_server::net::collide::solid_states(&constants);
    println!(
        "block table: {} states, {} of them solid",
        constants.len(),
        solid.map_or("no full_collision column, so 0".to_owned(), |n| n
            .to_string())
    );
    if solid.is_none_or(|n| n == 0) {
        eprintln!("nothing is solid, so the collision rows would measure an early return");
        return;
    }

    let flat = FlatWorld::new(palette, 0, 64);
    let surface = f64::from(dust_server::net::world::SURFACE_Y + 1);
    let world = EditedWorld::new(Source::Flat(Box::new(flat)));

    row("no world", || {
        walk(
            &mut Movement::new(limit(), start(surface)),
            surface,
            |m, to| m.claimed(to, 1, &mut dust_guard::Open),
        )
    });
    row("flat, in the open", || {
        let mut ground = Ground::of(&world, Some(&constants)).expect("the table said it was solid");
        walk(
            &mut Movement::new(limit(), start(surface)),
            surface,
            |m, to| m.claimed(to, 1, &mut ground),
        )
    });
    // Feet one block under the surface, so every box question finds the grass
    // and every one of them asks the second question as well. A player cannot
    // get here honestly, which is the point: it is the ceiling on the cost and
    // not a case anybody pays for.
    let sunk = surface - 1.0;
    row("flat, into the ground", || {
        let mut ground = Ground::of(&world, Some(&constants)).expect("the table said it was solid");
        walk(&mut Movement::new(limit(), start(sunk)), sunk, |m, to| {
            m.claimed(to, 1, &mut ground)
        })
    });

    let Some(directory) = std::env::var_os("DUST_BENCH_REGION").map(std::path::PathBuf::from)
    else {
        println!(
            "region files: not run. Set DUST_BENCH_REGION to a world's region directory \
             to measure the row the column cache exists for."
        );
        return;
    };
    let Some(names) = RegistryNames::new() else {
        eprintln!("no synced biome registry; the region row cannot be built");
        return;
    };
    let constants_for_world = std::sync::Arc::new(constants.clone());
    let anvil = AnvilWorld::new(
        directory,
        names,
        FlatWorld::new(palette, 0, 64),
        dust_server::net::world::opacity_of(palette.air, Some(&constants)),
        Some(constants_for_world),
    );
    let world = EditedWorld::new(Source::Anvil(Box::new(anvil)));
    // Where the terrain actually is. Probed rather than assumed, and printed,
    // because the flat fallback is what a region directory answers for a chunk
    // it does not contain — so a row that found nothing solid would be the flat
    // row again under another name, and would say so by reporting the
    // superflat's own surface.
    let Some(top) = highest_solid(&world, &constants) else {
        eprintln!("nothing solid anywhere along the walk; the region files are not being read");
        return;
    };
    println!("  region terrain: highest solid block along the walk is y = {top}");
    if top == dust_server::net::world::SURFACE_Y {
        eprintln!(
            "  ...which is the superflat's own surface, so this row is measuring the fallback"
        );
    }
    // Feet in the terrain, which is the worst case and the one that asks both
    // questions, over columns the region files really contain.
    let y = f64::from(top);
    row("region files", || {
        let mut ground = Ground::of(&world, Some(&constants)).expect("the table said it was solid");
        walk(&mut Movement::new(limit(), (0.5, y, 0.5)), y, |m, to| {
            m.claimed(to, 1, &mut ground)
        })
    });
}

/// The highest solid block anywhere along the walk, or `None` if there is none.
fn highest_solid(world: &EditedWorld, constants: &dust_registry::BlockConstants) -> Option<i32> {
    let mut ground = Ground::of(world, Some(constants))?;
    let height = world.height();
    let mut x = 0;
    while f64::from(x) <= SPAN {
        let mut y = height.max_y_exclusive() - 1;
        while y >= height.min_y() {
            if dust_guard::Solidity::first_solid(&mut ground, (x, y, 0), (x, y, 0)).is_some() {
                return Some(y);
            }
            y -= 1;
        }
        x += 8;
    }
    None
}

fn limit() -> SpeedLimit {
    SpeedLimit::new(10.0)
}

fn start(y: f64) -> (f64, f64, f64) {
    (0.5, y, 0.5)
}

/// One row's workload: `PACKETS` steps of 0.216 blocks, which is what a walking
/// client's own packets carry — `tools/bot/movement.js` measured it.
///
/// The distance is deliberately enough to cross chunk boundaries: 2,000 steps
/// is 432 blocks of walking, which is a chunk boundary crossed every 74 steps
/// and twenty-seven of them in a row.
fn walk<F>(movement: &mut Movement, y: f64, mut judge: F) -> u32
where
    F: FnMut(&mut Movement, (f64, f64, f64)) -> dust_guard::Claim,
{
    let mut accepted = 0;
    for i in 1..=PACKETS {
        let along = (f64::from(i) * 0.216) % (2.0 * SPAN);
        let x = if along <= SPAN {
            along
        } else {
            2.0 * SPAN - along
        };
        if judge(movement, (0.5 + x, y, 0.5)) == dust_guard::Claim::Accepted {
            accepted += 1;
        }
    }
    accepted
}

/// Run a workload `ROUNDS` times and print the median nanoseconds per packet.
fn row<F: FnMut() -> u32>(name: &str, mut work: F) {
    let mut times = Vec::with_capacity(ROUNDS as usize);
    let mut accepted = 0;
    for _ in 0..ROUNDS {
        let at = Instant::now();
        accepted = work();
        times.push(at.elapsed().as_nanos() / u128::from(PACKETS));
    }
    times.sort_unstable();
    println!(
        "  {name:<24} {:>7} ns/packet   (fastest {}, slowest {}, {accepted}/{PACKETS} accepted)",
        times[times.len() / 2],
        times[0],
        times[times.len() - 1],
    );
}

/// Minecraft's own block table, from wherever the operator put it.
fn table() -> Option<dust_registry::BlockConstants> {
    // Cargo runs a bench from the crate directory, not the workspace root, so
    // the default is anchored to the manifest rather than to the shell's idea
    // of where it is.
    let path = std::env::var_os("DUST_BENCH_CONSTANTS").map_or_else(
        || {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(DEFAULT_TABLE)
        },
        std::path::PathBuf::from,
    );
    match std::fs::read_to_string(&path) {
        Ok(text) => match dust_registry::BlockConstants::parse(&text) {
            Ok(table) => Some(table),
            Err(e) => {
                eprintln!("{}: {e}", path.display());
                None
            }
        },
        Err(_) => {
            println!(
                "no block table at {}. This bench measures Minecraft's own answers and \
                 nothing here ships them: run `cargo xtask extract --only constants` and \
                 point DUST_BENCH_CONSTANTS at the file it writes.",
                path.display()
            );
            None
        }
    }
}

/// Where `cargo xtask extract` writes it, from the workspace root.
const DEFAULT_TABLE: &str = ".dust-extract/oracle-1.21.1/constants.tsv";
