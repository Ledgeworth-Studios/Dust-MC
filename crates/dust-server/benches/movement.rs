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
//! 4. **region files** — the same walk over a world read from `.mca`, once
//!    with the feet in the terrain and once on top of it. This is the row the
//!    four-column cache exists for; run it with
//!    `DUST_BENCH_REGION=<a world's region directory>`.
//! 5. **region files, resident** — the same walk again, with the server
//!    keeping the columns around the player the way `net/residency.rs` has it
//!    keep them: a claim taken on the walking thread as the player crosses
//!    into a column, and the building done on another thread that the walk
//!    never waits for. The difference between this row and the one above it is
//!    the whole of decision record 0021.
//!
//! **Every row prints what its first round did as well as the median**, and
//! the two column counts beside it. That is not decoration. A residency row
//! whose second round is fast because its first round filled the map is a
//! measurement of the second round; the first-round number and `built=` — how
//! many columns the check had to build *on its own thread* — are what say
//! whether a player walking into terrain nobody has been in waits for a disk.
//!
//! **Read the two region rows as a pair and neither of them alone**, and know
//! what the into-the-ground one is measuring. A box question stops at the
//! first solid cell it finds, so a walk with the feet in the ground answers on
//! its first cell and never reads the rest of the box; and because the bench
//! sends the next packet from where the walk says rather than from where the
//! server put the player, a refused packet is followed by a *longer* move,
//! and a longer move is split into more samples. That row is therefore mostly
//! a measurement of the sample loop, and it is blind to everything about the
//! cells above the feet — the three poses all read the same on it, which is
//! how you can tell. The in-the-open row walks the terrain's own surface, is
//! accepted end to end, reads every cell in the box, and is the one a real
//! player generates.
//!
//! Each of the world rows is then run again at the two shorter poses, because
//! how much of a player is measured is the other input and a single number for
//! "the movement check" cannot say how much of it belongs to the head. The
//! feet-only row is what this check cost before it knew what shape a player
//! was, measured in the same run rather than by checking out another commit.
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

// The allocator trait is `unsafe` to implement by nature; the wrapper below
// forwards every call to [`System`] untouched and adds nothing but a counter,
// which is the whole of its safety argument. The workspace's own deny stays
// meaningful for the server; this opt-out is scoped to the bench binary, and
// `dust-nbt/benches/allocation.rs` took it first for the same reason.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dust_guard::{Movement, Posture, SpeedLimit};
use dust_server::net::collide::Ground;
use dust_server::net::edits::EditedWorld;
use dust_server::net::source::{AnvilWorld, RegistryNames, Source};
use dust_server::net::view::column_of;
use dust_server::net::world::{FlatWorld, Palette};
use dust_world::coords::ChunkPos;

/// How many bytes are on the heap that have not been given back.
static LIVE_BYTES: AtomicIsize = AtomicIsize::new(0);

/// The system allocator, counting.
///
/// Here because "a column is about a megabyte" has been in three modules'
/// documentation for the life of the project and nothing ever measured it, and
/// the whole case for residency is a memory one. An exact count and not a
/// resident-set reading: `ps` reports pages the process has taken from the
/// operating system, and an allocator that has just freed a thousand columns
/// hands the next thousand the same pages back — so RSS answers **zero bytes a
/// column** for a measurement taken after any other row has run. It was tried.
///
/// The cost is two relaxed atomics per allocation. Every row in this file is
/// measured through it, so the comparison the file exists to make is unaffected
/// by it; a movement check that hits a cached column allocates nothing at all.
struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LIVE_BYTES.fetch_add(layout.size() as isize, Ordering::Relaxed);
        System.alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE_BYTES.fetch_sub(layout.size() as isize, Ordering::Relaxed);
        System.dealloc(ptr, layout)
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        LIVE_BYTES.fetch_add(layout.size() as isize, Ordering::Relaxed);
        System.alloc_zeroed(layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        LIVE_BYTES.fetch_add(
            new_size as isize - layout.size() as isize,
            Ordering::Relaxed,
        );
        System.realloc(ptr, layout, new_size)
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// What one round of a row did, beside how long it took.
///
/// Three numbers rather than one, because "how fast was it" cannot say whether
/// the check read the world or gave up on it. `built` is columns this walk had
/// to build on its own thread — the 0.9 ms each that D20 measured — and
/// `resident` is columns it took out of the server's shared set instead.
#[derive(Default, Clone, Copy)]
struct Tally {
    accepted: u32,
    built: u32,
    resident: u32,
}

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

/// How many packets the paced row sends. Twenty a second, so this is fifteen
/// seconds of one player and about four chunk boundaries — enough to answer
/// whether the warming thread is ahead of a walk, and short enough that a
/// bench somebody runs is not a bench somebody waits for.
const PACED_PACKETS: u32 = 300;

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

    row("no world", || Tally {
        accepted: walk(
            &mut Movement::new(limit(), start(surface)),
            surface,
            Posture::default(),
            |m, to| m.claimed(to, 1, &mut dust_guard::Open),
        ),
        ..Tally::default()
    });
    for (pose, posture) in POSES {
        row(&format!("flat, in the open, {pose}"), || {
            let mut ground =
                Ground::of(&world, Some(&constants)).expect("the table said it was solid");
            let mut player = player(surface, posture);
            let accepted = walk(&mut player, surface, posture, |m, to| {
                m.claimed(to, 1, &mut ground)
            });
            tally(accepted, &ground)
        });
    }
    // Feet one block under the surface, so every box question finds the grass
    // and every one of them asks the second question as well. A player cannot
    // get here honestly, which is the point: it is the ceiling on the cost and
    // not a case anybody pays for.
    let sunk = surface - 1.0;
    row("flat, into the ground", || {
        let mut ground = Ground::of(&world, Some(&constants)).expect("the table said it was solid");
        let accepted = walk(
            &mut player(sunk, Posture::default()),
            sunk,
            Posture::default(),
            |m, to| m.claimed(to, 1, &mut ground),
        );
        tally(accepted, &ground)
    });

    let Some(directory) = std::env::var_os("DUST_BENCH_REGION").map(std::path::PathBuf::from)
    else {
        println!(
            "region files: not run. Set DUST_BENCH_REGION to a world's region directory \
             to measure the row the column cache exists for."
        );
        return;
    };
    // Built more than once on purpose: the paced row at the end needs a world
    // nobody has been near, and a residency that has already been filled by
    // one row would answer that row's question for it.
    let make_world = || {
        let names = RegistryNames::new()?;
        Some(Arc::new(EditedWorld::new(Source::Anvil(Box::new(
            AnvilWorld::new(
                directory.clone(),
                names,
                FlatWorld::new(palette, 0, 64),
                dust_server::net::world::opacity_of(palette.air, Some(&constants)),
                Some(std::sync::Arc::new(constants.clone())),
            ),
        )))))
    };
    // Behind an `Arc` because the residency rows below hand the same world to a
    // warming thread, which is the shape the server has: one world, many
    // threads asking it for columns.
    let Some(world) = make_world() else {
        eprintln!("no synced biome registry; the region row cannot be built");
        return;
    };
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
    for (pose, posture) in POSES {
        row(&format!("region files, into the ground, {pose}"), || {
            let mut ground =
                Ground::of(&world, Some(&constants)).expect("the table said it was solid");
            let accepted = walk(&mut player(y, posture), y, posture, |m, to| {
                m.claimed(to, 1, &mut ground)
            });
            tally(accepted, &ground)
        });
    }
    // And the case that actually happens: a player over the terrain rather
    // than through it, where nothing in the box answers and every cell in it
    // has to be read. High enough to clear the whole walk, because a walk at
    // a constant height over real terrain that is *sometimes* inside the
    // ground is the row above wearing this row's name.
    let Some(ceiling) = clearance(&world, &constants) else {
        eprintln!("no ground along the walk; the in-the-open region rows cannot be built");
        return;
    };
    let air = f64::from(ceiling + 1);
    for (pose, posture) in POSES {
        row(&format!("region files, in the open, {pose}"), || {
            let mut ground =
                Ground::of(&world, Some(&constants)).expect("the table said it was solid");
            let accepted = walk(&mut player(air, posture), air, posture, |m, to| {
                m.claimed(to, 1, &mut ground)
            });
            tally(accepted, &ground)
        });
    }
    // The number that explains the two blocks above, counted rather than
    // inferred from the shape of them. Untimed and outside every row.
    let mut ground = Ground::of(&world, Some(&constants)).expect("the table said it was solid");
    let posture = POSES[0].1;
    walk(&mut player(air, posture), air, posture, |m, to| {
        m.claimed(to, 1, &mut ground)
    });
    println!(
        "  one {PACKETS}-packet walk built {} columns out of the region files, \
         which is {} blocks of walking and a chunk boundary every 16 of them",
        ground.columns_built(),
        (f64::from(PACKETS) * 0.216) as u32,
    );

    // The same walk with the server keeping the columns around the player.
    //
    // The warming thread stands in for the blocking pool the session task
    // hands its builds to: the walk claims the ring as it crosses into a
    // column, sends the centre, and carries straight on. It never waits, and
    // whether that was enough is not an argument — it is `built=` in the row,
    // which counts the columns the check had to build on its own thread
    // because the warm had not got there yet.
    for (pose, posture) in POSES {
        row(
            &format!("region files, resident, in the open, {pose}"),
            || {
                let mut ground =
                    Ground::of(&world, Some(&constants)).expect("the table said it was solid");
                let mut here: Option<ChunkPos> = None;
                let accepted = walk(&mut player(air, posture), air, posture, |m, to| {
                    let centre = column_of(to.0, to.2);
                    if here != Some(centre) {
                        // Exactly what `net/session.rs` does on an accepted move:
                        // claim, hand the build to another thread, let go of the
                        // ring behind. Nine hash lookups and a channel send.
                        world.hold(centre);
                        world.want_ring(centre);
                        if let Some(previous) = here.replace(centre) {
                            world.release(previous);
                        }
                    }
                    m.claimed(to, 1, &mut ground)
                });
                // A session ending gives its ring up; a row that did not would
                // hold nine more columns for every round it ran.
                if let Some(last) = here {
                    world.release(last);
                }
                tally(accepted, &ground)
            },
        );
    }
    println!(
        "  the server is keeping {} columns now that the walk has finished; \
         every one of them is retired and none is held",
        world.resident_columns(),
    );
    // The two rows above are a warm residency and a player who moved faster
    // than any client can. This is the case the change is actually about.
    // A world each, and both cold. `warm_cost` builds the ring it times, so
    // running the paced walk on the same world would hand it columns that were
    // already there and answer its question for it.
    //
    // The centre is on the walk's own line, which is the one place the bench
    // has probed and found terrain. That matters more than it looks: a column
    // a region file does not contain falls back to the flat template, which is
    // a clone and not a read — the first version of this timed a ring at
    // (20, 20), got 0.01 ms a column against D20's 0.9, and the 90x was the
    // fallback rather than anything to do with residency.
    if let Some(fresh) = make_world() {
        warm_cost(&fresh, ChunkPos::new(2, 0));
    }
    // The same paced walk twice, on two cold worlds: once with the server
    // keeping columns and once the way it worked before. A mean cannot answer
    // this and neither can a median — a stall is not felt as an average, so
    // what both rows report is the **worst single packet**.
    for resident in [false, true] {
        if let Some(fresh) = make_world() {
            paced(&fresh, &constants, air, resident);
        }
    }

    column_bytes(&world);
}

/// One walk at the rate a client actually sends, into a world nobody has been
/// in, timed one packet at a time.
///
/// The rows above cannot answer the question this change exists for. They send
/// two thousand packets as fast as the machine will judge them, which crosses a
/// chunk boundary every three microseconds — a player moving about a million
/// times faster than a client can claim to — so the warming thread is behind
/// from the first crossing and the check builds its own columns. That is a
/// measurement of a bench, not of a server.
///
/// So this one sleeps. The warming is the world's own thread — the same one a
/// server uses, not a stand-in for it. [`PACED_PACKETS`] packets at twenty a
/// second is what a walking client sends, and the number that matters is not the mean: it is
/// **the slowest single packet**, because a stall is not felt as an average.
/// `built` beside it says whether the check ever had to read a region file
/// itself.
fn paced(
    world: &Arc<EditedWorld>,
    constants: &dust_registry::BlockConstants,
    y: f64,
    resident: bool,
) {
    let Some(mut ground) = Ground::of(world, Some(constants)) else {
        return;
    };
    let posture = POSES[0].1;
    let mut player = player(y, posture);
    // The join, which claims and warms the ring before the player is let in
    // and before any movement packet exists to wait for it — see
    // `net/session.rs`. Timed and printed, because it is real work; it is not
    // on the movement path, which is the whole reason it is done there.
    //
    // Without it the first movement packet of a session finds its own column
    // missing and reads a region file on the spot. Measured, by leaving it
    // out: **6.4 ms for that one packet**, which is the hitch this change is
    // about, arriving at the worst possible moment.
    let start = column_of(0.5, 0.5);
    let mut here: Option<ChunkPos> = None;
    if resident {
        world.hold(start);
        let joined = Instant::now();
        let warmed = world.warm(start);
        println!(
            "  the join warmed {warmed} columns in {:.1} ms, before the first movement packet \
             exists and off the movement path",
            joined.elapsed().as_secs_f64() * 1000.0,
        );
        here = Some(start);
    }
    let mut worst = Duration::ZERO;
    let mut total = Duration::ZERO;
    let mut crossings = 0;
    for i in 1..=PACED_PACKETS {
        let along = (f64::from(i) * 0.216) % (2.0 * SPAN);
        let x = if along <= SPAN {
            along
        } else {
            2.0 * SPAN - along
        };
        let to = (0.5 + x, y, 0.5);
        // Timed around everything the session task does for one movement
        // packet: the claim, the hand-off, and the check itself.
        let at = Instant::now();
        let centre = column_of(to.0, to.2);
        if resident && here != Some(centre) {
            world.hold(centre);
            world.want_ring(centre);
            if let Some(previous) = here.replace(centre) {
                world.release(previous);
            }
            crossings += 1;
        }
        if player.claimed(to, 1, &mut ground) != dust_guard::Claim::Accepted {
            player = self::player(y, posture);
        }
        let took = at.elapsed();
        worst = worst.max(took);
        total += took;
        std::thread::sleep(Duration::from_millis(50).saturating_sub(took));
    }
    if let Some(last) = here {
        world.release(last);
    }
    println!(
        "  paced at 20 packets a second, {PACED_PACKETS} packets into a world nobody had been \
         in, {}: mean {} ns, WORST SINGLE PACKET {} ns, {} columns built on the check's own \
         thread and {} taken from the residency ({crossings} claims)",
        if resident {
            "with the server keeping columns"
        } else {
            "the way it worked before"
        },
        total.as_nanos() / u128::from(PACED_PACKETS),
        worst.as_nanos(),
        ground.columns_built(),
        ground.columns_resident(),
    );
}

/// What one column of this world costs to keep, measured rather than repeated.
///
/// The columns are built once and dropped before the reading starts, so the
/// sky-floor cache and the open region files are paid for by the first pass and
/// what the second one adds is the chunks and nothing else. That trick works
/// only because the counter is exact: an allocator that has just handed a
/// thousand columns back would show a resident-set reading no growth at all.
fn column_bytes(world: &EditedWorld) {
    const COLUMNS: i32 = 16;
    let build = || {
        (0..COLUMNS)
            .flat_map(|x| (0..COLUMNS).map(move |z| ChunkPos::new(x, z)))
            .map(|pos| world.chunk(pos))
            .collect::<Vec<_>>()
    };
    drop(build());
    let before = LIVE_BYTES.load(Ordering::Relaxed);
    let held = build();
    let after = LIVE_BYTES.load(Ordering::Relaxed);
    let count = held.len() as f64;
    let each = (after - before) as f64 / count;
    println!(
        "  column size: {count} columns of this world are {} bytes on the heap, {:.0} KB each. \
         Nine of them a player, shared, is {:.1} MB; the four a session built for itself was \
         {:.1} MB per player and was not shared with anybody",
        after - before,
        each / 1024.0,
        9.0 * each / (1024.0 * 1024.0),
        4.0 * each / (1024.0 * 1024.0),
    );
    drop(held);
}

/// How long it takes to build the ring around a column nobody has been near,
/// against how long a player takes to walk out of the one they are in.
///
/// The two numbers that decide whether residency works, and neither of them is
/// a row above: the rows send packets as fast as the machine can judge them,
/// which is a player moving about a million times faster than any client can
/// claim to. This is the pair that says what a *real* player experiences —
/// [`paced`] then runs one at the real rate and checks the answer.
fn warm_cost(world: &EditedWorld, at: ChunkPos) {
    world.hold(at);
    let start = Instant::now();
    let built = world.warm(at);
    let took = start.elapsed();
    world.release(at);
    // A ring that built nothing, or built the flat fallback, would report a
    // warm that costs nothing and prove only that the world is not there.
    assert!(built > 0, "the ring at {at:?} was already resident");
    // `dust_guard::SpeedLimit` is 10 blocks a second, which is the fastest
    // this server will believe a walking player; a column is sixteen wide.
    let crossing = Duration::from_secs_f64(16.0 / 10.0);
    println!(
        "  warming a cold ring: {built} columns in {:.1} ms, {:.2} ms each. A player crossing \
         the column they are standing in takes {} ms at the speed limit, so the ring ahead of \
         them is ready {:.0} times over",
        took.as_secs_f64() * 1000.0,
        took.as_secs_f64() * 1000.0 / f64::from(built.max(1)),
        crossing.as_millis(),
        crossing.as_secs_f64() / took.as_secs_f64(),
    );
}

/// What a row read, from the `Ground` that read it.
fn tally(accepted: u32, ground: &Ground<'_>) -> Tally {
    Tally {
        accepted,
        built: ground.columns_built(),
        resident: ground.columns_resident(),
    }
}

/// The highest solid block in the first column of the walk that has one, or
/// `None` if none of them does. What the into-the-ground rows put the feet in.
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

/// The three heights a player is measured at, and the signals that produce
/// each. Named by what the server thinks the player is doing, because that is
/// what an operator reading the row wants to match up with a player.
const POSES: [(&str, Posture); 3] = [
    (
        "standing (1.8)",
        Posture {
            sneaking: false,
            sprinting: false,
            flying: false,
            gliding: false,
            on_ground: true,
        },
    ),
    (
        "crouching (1.5)",
        Posture {
            sneaking: true,
            sprinting: false,
            flying: false,
            gliding: false,
            on_ground: true,
        },
    ),
    // What this check measured before it knew what shape a player was: the
    // bottom 0.6 and nothing above it.
    (
        "feet only (0.6)",
        Posture {
            sneaking: false,
            sprinting: true,
            flying: false,
            gliding: false,
            on_ground: false,
        },
    ),
];

/// A player at `y` who has told the server this much about their own shape.
fn player(y: f64, posture: Posture) -> Movement {
    let mut player = Movement::new(limit(), start(y));
    player.posture(posture);
    player
}

/// The highest solid block anywhere along the walk, or `None` if there is none.
///
/// Every whole block of it, unlike [`highest_solid`], which stops at the first
/// column that has anything and answers about that column alone.
fn clearance(world: &EditedWorld, constants: &dust_registry::BlockConstants) -> Option<i32> {
    let mut ground = Ground::of(world, Some(constants))?;
    let height = world.height();
    let mut best = None;
    for x in 0..=(SPAN as i32) {
        let mut y = height.max_y_exclusive() - 1;
        while y >= height.min_y() {
            if dust_guard::Solidity::first_solid(&mut ground, (x, y, 0), (x, y, 0)).is_some() {
                best = Some(best.map_or(y, |b: i32| b.max(y)));
                break;
            }
            y -= 1;
        }
    }
    best
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
fn walk<F>(movement: &mut Movement, y: f64, posture: Posture, mut judge: F) -> u32
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
        let to = (0.5 + x, y, 0.5);
        if judge(movement, to) == dust_guard::Claim::Accepted {
            accepted += 1;
        } else {
            // A refused player is put back where they were, and the walk goes
            // on from where it says rather than from there — so without this,
            // the *next* packet is a longer move, and a longer move is split
            // into more samples, and by the far end of the walk every packet
            // is sixty-four box questions. That is a measurement of
            // `SAMPLE_SPAN` and not of anything this bench is about; a row
            // where the three poses read the same is how it shows.
            *movement = Movement::new(limit(), to);
            movement.posture(posture);
        }
    }
    accepted
}

/// Run a workload `ROUNDS` times and print the median nanoseconds per packet,
/// the first round's, and what the first round read.
///
/// The first round is printed on its own because for any row that fills a
/// cache it is the only cold one, and a median of five over a set that four of
/// them found already warm is a number about the last four. It is also the
/// round a real player generates: the first walk into terrain nobody has been
/// in.
fn row<F: FnMut() -> Tally>(name: &str, mut work: F) {
    let name = format!("{name:<40}");
    let mut times = Vec::with_capacity(ROUNDS as usize);
    let mut tally = Tally::default();
    let mut first = 0;
    for round in 0..ROUNDS {
        let at = Instant::now();
        let round_tally = work();
        let ns = at.elapsed().as_nanos() / u128::from(PACKETS);
        if round == 0 {
            first = ns;
            tally = round_tally;
        }
        times.push(ns);
    }
    let mut sorted = times.clone();
    sorted.sort_unstable();
    println!(
        "  {name} {:>7} ns/packet   (first {first}, fastest {}, slowest {}, \
         {}/{PACKETS} accepted, first round built {} columns and shared {})",
        sorted[sorted.len() / 2],
        sorted[0],
        sorted[sorted.len() - 1],
        tally.accepted,
        tally.built,
        tally.resident,
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
