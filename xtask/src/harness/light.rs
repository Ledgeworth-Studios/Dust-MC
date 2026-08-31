//! `harness light` — how close is Dust's sky light to Minecraft's?
//!
//! **Sky light only.** Dust has no block light, so there is nothing on this
//! side to compare a real server's block-light arrays against — see decision
//! record 0008 for what that waits on. Every number this prints says
//! "sky-light" for that reason: a bare percentage would read as "the lighting
//! is 99.4% right" when half of lighting is not implemented.
//!
//! # Why this is measurable at all
//!
//! A chunk vanilla wrote carries the light vanilla computed. It is stored per
//! section as a nibble array, and the arrays it *omits* are as informative as
//! the ones it writes: a section below the lit range has none and is dark, a
//! section above it has none and is daylight. Read off a real world's chunk:
//!
//! ```text
//! Y=-4..2   no SkyLight     bedrock, deepslate, stone
//! Y=3       SkyLight        L0 3225 cells, then a gradient up to L15
//! Y=4       SkyLight        L15 4035 cells, L14 44, L13 14, L0 3
//! Y=5       SkyLight        L15 4096, every cell
//! Y=6..19   no SkyLight     air
//! ```
//!
//! So the convention is: **a section with no array is dark, except above the
//! highest section that has one, where it is daylight.** That is applied here
//! so the comparison covers a whole column rather than the three sections in
//! the middle.
//!
//! The exception is not the same rule as "below the lowest stored section",
//! which was the first guess and is wrong. A chunk two chunks from spawn has
//! sections 2, 4 and 5 with arrays and section **3 without one, in between** —
//! solid stone, all dark, nothing worth storing. Vanilla omits an array
//! whenever the section is uniform, and the only place a uniform section is
//! daylight rather than darkness is above the terrain. That case was found by
//! this verb refusing to guess about it rather than by reading it somewhere.
//!
//! # Two models, one run
//!
//! Opacity is not one of Dust's numbers. Minecraft keeps `getLightBlock` as
//! Java code; `cargo xtask extract --only light` asks the game for it against
//! the operator's own jar; and this verb measures **both** that answer and the
//! stand-in that stood in for it — over the same chunks of the same world, in
//! the same run. Two runs of one verb with a flag between them would be two
//! numbers taken under conditions nobody held fixed, which is the mistake the
//! ring histogram below exists to avoid.
//!
//! ```text
//!                            agree     cells short
//!   seed 0, radius 2
//!     air only, stand-in    99.419%         14,276
//!     Minecraft's own       99.975%            611
//!   seed 1, radius 3
//!     air only, stand-in    96.482%        169,480
//!     Minecraft's own      100.000%              0
//! ```
//!
//! **Seed 1 is exact**: 4,816,896 sky-light cells of an ocean world, and not
//! one of them disagrees with the light Minecraft wrote.
//!
//! # What the second model found, which was not opacity
//!
//! Minecraft's numbers on their own moved seed 0 from 99.419% to **99.423%**,
//! which is a hundred and seven cells. What they changed was not how many
//! cells were short but *how short*: 6,128 cells short by fourteen became
//! nineteen cells short by thirteen. Light was reaching under the water and
//! into the leaves and arriving at half the level it should.
//!
//! The cause was in the engine. `dust_world::propagation` charged `1 + opacity`
//! for a step where Minecraft charges `max(1, opacity)`, so every block with an
//! opacity of one cost two. **Nothing could see it while the only opacity model
//! answered 0 or 15**, because the two rules agree at both ends. See
//! `propagation::step_cost`; fixing it is what takes seed 0 to 99.975% and
//! seed 1 to exact.
//!
//! A wrong constant hidden by another wrong constant, found by putting the real
//! data through the same walk — which is this harness's whole argument, made
//! once more and from a direction nobody was watching.
//!
//! # The ladder: four inputs, one engine
//!
//! Sky light has four inputs and only one of them is Dust's lighting. The verb
//! measures them as a ladder — four models over the same chunks in the same
//! run, each row the one above it plus a single named change — because a table
//! like that is what says which input owns which part of the gap. A single
//! percentage cannot, and twice now it has been read as saying something it
//! was not.
//!
//! ```text
//! seed 0, radius 2                              short   sweep
//!   air only, one column, Dust's heightmap     14,276    102 ms
//!   + Minecraft's own opacity                     611    101 ms
//!   + a 3x3 volume of columns                     179    611 ms
//!   + the heightmaps Minecraft wrote                0    544 ms
//! ```
//!
//! **The last row is a hundred per cent, on both seeds.** 2,457,600 cells and
//! 4,816,896 cells, and not one disagrees with the light Minecraft computed. So
//! the walks are right, and everything above that row is something Dust is
//! *told* about the world rather than something it does with what it is told.
//!
//! The last rung is not a mode a server could run in: a chunk somebody has
//! edited has a heightmap its file does not. It is there to take the last input
//! out of the measurement.
//!
//! **`--volume 2` buys exactly nothing** — the same 179, not fewer — which is
//! the argument for a finite volume confirmed rather than assumed. Light loses
//! a level a block and a chunk is sixteen of them, so one ring of neighbours is
//! not an approximation of the infinite volume; it is the infinite volume.
//!
//! Decision record 0010 is why the 3x3 is measured here and not adopted in the
//! engine: it closes 432 of the 611 for six times the work, and the heightmap
//! predicate closes the other 179 for a column in a table Dust already reads.
//!
//! # The ring histogram, and what it can and cannot separate
//!
//! The shortfall is split by how far each cell sits from its column's edge.
//! Light arriving from a neighbouring column enters at a face and loses a level
//! per step inward; opacity has no reason to care where in a column a cell is.
//! A rate that falls towards the middle is the first; a flat one is not.
//!
//! ```text
//! distance from a face    0      1      2      3      4      5      6      7
//! seed 0, air only     0.660  0.595  0.561  0.548  0.530  0.510  0.530  0.581
//! seed 0, Minecraft's  0.072  0.021  0.008  0.007  0.005  0.005  0.006  0.018
//! seed 0, + 3x3        0.009  0.009  0.006  0.006  0.005  0.005  0.006  0.018
//! ```
//!
//! Flat under the stand-in; falling by an order of magnitude once opacity is
//! Minecraft's, which is the shape a neighbour effect makes and was the first
//! time this verb had seen one; flat again once the volume has taken that away,
//! leaving a third thing that is neither.
//!
//! **What it cannot do is separate a cause nobody proposed.** It read "flat,
//! therefore opacity" and was right, and was equally right about a step cost
//! that doubled every opacity that was not 0 or 15. It is a discriminator
//! between two named hypotheses and not a detector.
//!
//! **The rate and not the count is the whole measurement.** A 16x16 column has
//! `60 - 8d` columns at distance `d` — sixty at the face and four in the
//! middle, fifteen to one — so a raw count reads as "it is all at the edges"
//! for any cause whatsoever, including one that is perfectly uniform. A
//! histogram without its denominator would have confirmed the impression it was
//! built to test.
//!
//! # The stand-in's percentage is a property of the world, not of the engine
//!
//! Under the stand-in, seed 0 reads **99.4%** and seed 1 reads **96.5%** with
//! the same server: seed 1 spawns in deep ocean, and 168,428 of its 169,480
//! shortfalls are water — an even 12,544 cells at each level from 14 downwards,
//! which is one cell per column per level, the water column marching down. A
//! single headline number would be a number about whichever world somebody
//! captured.
//!
//! That is why both seeds are quoted, and it is also why the second model's
//! numbers are worth more than one seed of them: the world that was *worst*
//! under the stand-in is the one that comes out exact.
//!
//! **There are no over-lit cells, at any radius, either seed or either model,
//! and getting to that took three corrections — every one of them to this
//! harness rather than to the engine.**
//!
//! 1. It lit each column against *itself* on all four sides. A column lower
//!    than its neighbour was told the neighbour was as low as it, so light
//!    came in from a side vanilla says is a wall: 805 over-lit cells.
//! 2. It compared chunks vanilla had not finished. A world holds
//!    partly-generated chunks around whatever was forced, and vanilla lights a
//!    chunk when it reaches `full`: 167,000 more, and the agreement fell to
//!    98.1% with no change to the engine.
//! 3. It took sky floors from *neighbours* vanilla had not finished, so Dust
//!    was told there was open sky where the finished world has terrain: the
//!    last thirty-two, every one within a step of a chunk edge.
//!
//! Every one was caught by the same thing — the report separates over-lighting
//! from under-lighting, and both known gaps under-light — and the last was
//! found by printing the coordinates and seeing that all thirty-two sat on an
//! edge. A single "0.6% disagree" line would have hidden all three inside a
//! number that already looked good.
//!
//! # Exit codes
//!
//! `0` always, unless the run itself failed (`2`). This is a **measurement and
//! not a gate**: the number it reports is expected to be short of a hundred
//! per cent today, and a verb that returned failure for a known gap would be
//! red every time it ran, which teaches people to stop running it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use dust_server::net::source::RegistryNames;
use dust_server::net::world;
use dust_world::column_light::{Skirt, SkyFloor};
use dust_world::coords::ChunkPos;
use dust_world::heightmap::WorldHeight;

use super::{cache, digest, nbt, region};

const USAGE: &str = "\
harness light --version <v> [--seed <n>] [--radius <r>]

Reads a world Minecraft generated and lit, lights the same chunks with Dust's
own engine, and compares the sky light cell by cell. Prints how much agrees and
names what the disagreements are standing on.

Measured twice where this checkout has a light table — once with Minecraft's
own opacity for every block state and once with the stand-in that treats
everything but air as a wall — over the same chunks in the same run. Write one
with `cargo xtask extract --version <v> --only light`; nothing is committed and
none of it leaves .dust-extract/.

  --version <v>   Minecraft version, e.g. 1.21.1.
  --seed <n>      The provisioned world's seed. Default 0.
  --volume <k>    Also light each column inside a (2k+1)x(2k+1) block of
                  columns and report what that buys. Default 1, so a 3x3 is
                  measured; 0 turns it off. Needs a world generated k chunks
                  wider than --radius, or the edge chunks are skipped.
  --radius <r>    Chunks either side of the origin. Default 2 (a 5x5).
                  The origin and not the world's spawn point: `expected_chunks`
                  is centred on chunk 0,0. Those were the same place only
                  while Dust put every player at x 0, z 0.
";

#[derive(Debug)]
pub struct Options {
    pub version: String,
    pub seed: i64,
    pub radius: i32,
    /// The widest multi-column volume to measure, in chunks either side. `0`
    /// measures none of them; `1` adds a 3x3, `2` adds a 5x5 as well.
    pub volume: i32,
}

pub fn parse(args: &[String]) -> Result<Options, String> {
    let mut version = None;
    let mut seed = 0i64;
    let mut radius = 2i32;
    let mut volume = 1i32;
    let mut seen: Vec<(&'static str, String)> = Vec::new();
    let mut at = 0;
    while at < args.len() {
        match args[at].as_str() {
            "--version" => {
                at = super::take_value(&mut seen, "--version", args, at + 1)?;
                version = Some(seen.last().expect("just stored").1.clone());
            }
            "--seed" => {
                at = super::take_value(&mut seen, "--seed", args, at + 1)?;
                seed = seen
                    .last()
                    .expect("just stored")
                    .1
                    .parse()
                    .map_err(|_| "--seed needs a signed 64-bit integer")?;
            }
            "--volume" => {
                at = super::take_value(&mut seen, "--volume", args, at + 1)?;
                volume = seen
                    .last()
                    .expect("just stored")
                    .1
                    .parse()
                    .map_err(|_| "--volume needs a whole number")?;
            }
            "--radius" => {
                at = super::take_value(&mut seen, "--radius", args, at + 1)?;
                radius = seen
                    .last()
                    .expect("just stored")
                    .1
                    .parse()
                    .map_err(|_| "--radius needs a whole number")?;
            }
            other => return Err(format!("unknown light option `{other}`\n\n{USAGE}")),
        }
    }
    Ok(Options {
        version: version
            .ok_or_else(|| format!("light needs --version, e.g. `--version 1.21.1`\n\n{USAGE}"))?,
        seed,
        radius,
        volume,
    })
}

pub fn run(options: &Options) -> ExitCode {
    match measure(options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("harness light: {e}");
            ExitCode::from(2)
        }
    }
}

/// One column's sky light, as sixteen-wide nibbles per world y.
///
/// Indexed `[y - min_y][x + z * 16]`. A plain array rather than the packed
/// nibble form both sides store it in: this is a comparison and not a
/// serializer, and unpacking once is cheaper to read than two packing schemes
/// that have to agree.
struct Column {
    min_y: i32,
    levels: Vec<[u8; 256]>,
}

impl Column {
    fn empty(height: WorldHeight) -> Self {
        Self {
            min_y: height.min_y(),
            levels: vec![[0u8; 256]; height.height() as usize],
        }
    }

    fn at(&self, x: usize, y: i32, z: usize) -> u8 {
        self.levels[(y - self.min_y) as usize][x + z * 16]
    }

    fn set(&mut self, x: usize, y: i32, z: usize, level: u8) {
        self.levels[(y - self.min_y) as usize][x + z * 16] = level;
    }
}

/// What one comparison found.
#[derive(Default)]
struct Tally {
    cells: u64,
    agree: u64,
    /// Dust darker than vanilla, by how much, and what block the cell holds.
    darker: BTreeMap<u8, u64>,
    /// Dust brighter than vanilla. No known gap produces one of these.
    brighter: BTreeMap<u8, u64>,
    /// Which block a cell Dust under-lit sits *in*, most common first. Named
    /// because "0.6% disagree" is a number and "0.6% disagree and they are
    /// leaves and water" is a cause.
    darker_blocks: BTreeMap<String, u64>,
    /// The same for cells Dust over-lit, kept apart from the others precisely
    /// because no known gap produces one: a list they share would let a
    /// hundred unexplained cells hide inside ten thousand explained ones.
    brighter_blocks: BTreeMap<String, u64>,
    /// Under-lit cells by how far they sit from the nearest column edge,
    /// `min(x, 15 - x, z, 15 - z)`, so 0 is a face and 7 is the middle.
    ///
    /// This is the second known gap made countable. Light that would have to
    /// travel *through* a neighbouring column arrives from a face and
    /// attenuates a level per step, so shortfalls it causes lean towards zero;
    /// shortfalls opacity causes have no reason to care where in the column
    /// they are.
    darker_by_edge: BTreeMap<u8, u64>,
    /// Every cell compared, by the same distance.
    ///
    /// **Without this the ring histogram lies**, and by nearly fifteen to one.
    /// A 16x16 column has 60 - 8d columns at distance d: sixty at the face and
    /// four in the middle. So a shortfall spread evenly through a column still
    /// puts fifteen times more of itself on the edge than in the centre, and a
    /// raw count would read as "it is all at the edges" for a cause that is
    /// not at the edges at all. The rates below are per cell at that distance.
    cells_by_edge: BTreeMap<u8, u64>,
}

fn measure(options: &Options) -> Result<(), String> {
    let dirs = cache::Layout::resolve()?;
    let run_dir = dirs.server_dir(&options.version, options.seed);
    let region_dir = run_dir.join("world/region");
    if !region_dir.is_dir() {
        return Err(format!(
            "no world at {}; run `cargo xtask harness capture --version {} --seed {} \
             --radius {}` first",
            region_dir.display(),
            options.version,
            options.seed,
            options.radius
        ));
    }
    let names = RegistryNames::new()
        .ok_or_else(|| "the generated registry has no biome table".to_owned())?;
    let height = WorldHeight::OVERWORLD;
    let air = dust_registry::Block::from_name("minecraft:air")
        .ok_or_else(|| "the generated block table has no minecraft:air".to_owned())?
        .default_state()
        .id();

    let expected = digest::expected_chunks(options.radius);
    println!(
        "comparing the sky light of {} chunk(s) of Minecraft {} seed {}",
        expected.len(),
        options.version,
        options.seed
    );

    // Every column's sky floors, including the ring around the square, so
    // each chunk is lit against its *real* neighbours.
    //
    // **The first version used each column's own floors on all four sides and
    // that is what produced the only unexplained result**: a column lower than
    // its neighbour was told the neighbour was as low as it, so light came in
    // from a side vanilla says is a wall, and every over-lit cell in the report
    // was air. A comparison that measures the harness's shortcut instead of
    // the engine is worse than no comparison, because the number looks like an
    // answer.
    //
    // One map per heightmap mode, because the sky floor *is* the heightmap and
    // a run that lit a column against its own recomputed floors and its
    // neighbours' written ones would be measuring the difference between the
    // two rather than either.
    let mut floors: BTreeMap<Heightmaps, BTreeMap<(i32, i32), SkyFloor>> = BTreeMap::new();
    for mode in [Heightmaps::Recomputed, Heightmaps::AsWritten] {
        let floors = floors.entry(mode).or_default();
        for &(x, z) in &expected {
            for (nx, nz) in [(x, z), (x - 1, z), (x + 1, z), (x, z - 1), (x, z + 1)] {
                if floors.contains_key(&(nx, nz)) {
                    continue;
                }
                // Only from a neighbour vanilla finished. **A neighbour below
                // `full` has different blocks than the ones vanilla lit against**,
                // so its sky floor is a different world's — and the column beside
                // it comes out brighter than vanilla, because Dust is told there
                // is open sky where the finished world has terrain. That is what
                // the last thirty-two over-lit cells were: every one of them
                // within a step of a chunk edge, in a fading gradient, each
                // exactly one brighter than vanilla.
                //
                // A column with a neighbour missing from this map is skipped
                // below rather than lit against a guess.
                if !is_full(&region_dir, nx, nz)? {
                    continue;
                }
                if let Ok(chunk) = dust_chunk(&region_dir, nx, nz, height, &names, air, mode) {
                    floors.insert((nx, nz), SkyFloor::of(&chunk));
                }
            }
        }
    }

    // The table, if this checkout has one. **Both models are measured in one
    // run**, because the number that matters is not "Minecraft's opacity gets
    // 99.9%" — it is the difference between the two on the same chunks of the
    // same world, and two runs of one verb with a flag between them invites
    // exactly the mistake the ring histogram was built to avoid: comparing
    // numbers taken under conditions nobody held fixed.
    let table = light_table(&options.version)?;
    match &table {
        Some((path, table)) => println!(
            "light table: {} — {} states, {} of them emitting",
            path.display(),
            table.len(),
            table.emitting()
        ),
        None => println!(
            "no light table in this checkout, so only the stand-in is measured; \
             `cargo xtask extract --version {} --only light` writes one",
            options.version
        ),
    }

    // **A ladder, and each rung adds exactly one thing.** Sky light has four
    // inputs and only one of them is the engine; a table of four numbers where
    // each row differs from the one above it in a single named way is what
    // says which input owns which part of the gap. Two of the four turned out
    // not to be what this project thought they were, and both times it was a
    // row of this table that said so.
    let mut models = vec![Model {
        name: "air only, one column, Dust's heightmap".to_owned(),
        from_minecraft: false,
        opacity: world::opacity_of(air, None),
        volume: Volume::Column,
        heightmaps: Heightmaps::Recomputed,
    }];
    if let Some((_, table)) = &table {
        let real = world::opacity_of(air, Some(table));
        models.push(Model {
            name: "+ Minecraft's own opacity, from the operator's jar".to_owned(),
            from_minecraft: true,
            opacity: real.clone(),
            volume: Volume::Column,
            heightmaps: Heightmaps::Recomputed,
        });
        for k in 1..=options.volume {
            let side = 2 * k + 1;
            models.push(Model {
                name: format!("+ a {side}x{side} volume of columns"),
                from_minecraft: true,
                opacity: real.clone(),
                volume: Volume::Area(k),
                heightmaps: Heightmaps::Recomputed,
            });
        }
        // The last rung, and the only one no server could stand on: a chunk
        // that has been edited has a heightmap its file does not. It is here
        // to take the last input out of the measurement and leave the engine.
        models.push(Model {
            name: "+ the heightmaps Minecraft wrote (no server can do this)".to_owned(),
            from_minecraft: true,
            opacity: real,
            volume: if options.volume >= 1 {
                Volume::Area(options.volume)
            } else {
                Volume::Column
            },
            heightmaps: Heightmaps::AsWritten,
        });
    }

    // **One chunk set for every model.** A wider volume needs more of its
    // neighbourhood finished than a single column does, so letting each model
    // pick its own eligible chunks would compare two percentages of two
    // different worlds — which is exactly the confound the ring histogram
    // exists to avoid, one level up.
    let reach = models
        .iter()
        .map(|model| match model.volume {
            Volume::Column => 1,
            Volume::Area(k) => k,
        })
        .max()
        .unwrap_or(1);
    let mut comparable = Vec::new();
    for &(x, z) in &expected {
        let mut ok = true;
        for dx in -reach..=reach {
            for dz in -reach..=reach {
                // A neighbour vanilla has not finished has different blocks
                // than the ones it lit against, and a column lit against those
                // is measuring the harness rather than the engine.
                if !is_full(&region_dir, x + dx, z + dz)? {
                    ok = false;
                }
            }
        }
        // The column model also needs its four orthogonal floors, which is a
        // subset of the square above but is checked separately because it is a
        // different question: `floors` also drops a chunk that would not read.
        for mode in floors.values() {
            for key in [(x - 1, z), (x + 1, z), (x, z - 1), (x, z + 1)] {
                if !mode.contains_key(&key) {
                    ok = false;
                }
            }
        }
        if ok {
            comparable.push((x, z));
        }
    }
    let skipped = expected.len() - comparable.len();
    if skipped > 0 {
        // Said, not swallowed, and said once: which chunks are comparable is a
        // fact about the world vanilla wrote, not about the model.
        println!(
            "{skipped} of {} chunk(s) skipped: vanilla has not finished them or a \
             neighbour within {reach} of them, so its own light for them is unfinished",
            expected.len()
        );
    }
    if comparable.is_empty() {
        return Err(
            "every chunk was skipped; capture a wider world or ask for a smaller radius".to_owned(),
        );
    }

    let mut ladder: Vec<Rung> = Vec::new();
    for model in &models {
        println!();
        println!("--- {} ---", model.name);
        let started = std::time::Instant::now();
        let tally = sweep(&Sweep {
            region_dir: &region_dir,
            comparable: &comparable,
            floors: floors
                .get(&model.heightmaps)
                .expect("both heightmap modes were built"),
            height,
            names: &names,
            air,
            model,
        })?;
        ladder.push(Rung {
            name: &model.name,
            agree: tally.agree,
            cells: tally.cells,
            took: started.elapsed(),
        });
        report(&tally, model);
    }

    ladder_summary(&ladder);
    Ok(())
}

/// One rung of the ladder: what it agreed on, and what it cost.
struct Rung<'a> {
    name: &'a str,
    agree: u64,
    cells: u64,
    /// Wall time for the whole sweep — reading the chunks and lighting them.
    ///
    /// **Not a benchmark and it says so below.** It reads every chunk from
    /// disk for every rung, so a wider volume pays for `(2k+1)²` reads a
    /// server would not repeat. What it is good for is the ratio between two
    /// rows of one run, which is the only comparison anybody makes here.
    took: std::time::Duration,
}

/// The four numbers side by side, after the four reports.
///
/// The reports say what each model's disagreements are standing on; this says
/// what each rung *bought*, which is a different question and the one anybody
/// reading the verb's output actually came for. Printed last because it is the
/// conclusion and not the evidence.
fn ladder_summary(ladder: &[Rung]) {
    if ladder.len() < 2 {
        return;
    }
    println!();
    println!("what each one buys, over the same chunks:");
    println!();
    for rung in ladder {
        #[expect(clippy::cast_precision_loss, reason = "counts here are far below 2^53")]
        let percent = rung.agree as f64 * 100.0 / rung.cells as f64;
        println!(
            "  {percent:8.3}%  {:>8} short  {:>7} ms   {}",
            rung.cells - rung.agree,
            rung.took.as_millis(),
            rung.name
        );
    }
    println!();
    println!("  Each row is the one above it plus a single named change, and only one");
    println!("  of those changes is to Dust's lighting: the volume. The others are");
    println!("  inputs — what a block does to light, and where the sky starts.");
    println!();
    println!("  The milliseconds are this verb's, not a server's: every rung re-reads");
    println!("  every chunk, and a wider volume re-reads its neighbours once per");
    println!("  centre. Read the ratio between rows and not the numbers.");
}

/// Where a column's heightmaps come from, which is where its sky floor comes
/// from.
///
/// Ordered so `BTreeMap` can key on it, and that is the only reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Heightmaps {
    /// Recomputed from the blocks with "anything that is not air blocks
    /// motion", which is what a server does and is an approximation of
    /// vanilla's `MOTION_BLOCKING` — see `dust_chunk` for what it costs.
    Recomputed,
    /// Taken from the chunk as Minecraft wrote it. Not a mode a server can
    /// run in; a diagnostic that takes the predicate out of the measurement.
    AsWritten,
}

/// How much of the world around a column enters the walk that lights it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Volume {
    /// One column, with its four neighbours' sky floors as sources along the
    /// faces — what a running Dust server does.
    Column,
    /// A `(2k+1)²` block of columns lit together, with only the centre read
    /// back. See `super::area` for why this lives in the harness.
    Area(i32),
}

/// One thing to measure: a name, an opacity model and a volume.
struct Model {
    name: String,
    /// Whether the opacity is Minecraft's own, which changes which gaps the
    /// report is allowed to blame.
    from_minecraft: bool,
    opacity: dust_world::propagation::OpacityModel,
    volume: Volume,
    heightmaps: Heightmaps,
}

/// Everything one pass over the square needs, so the pass takes one argument
/// rather than eight.
struct Sweep<'a> {
    region_dir: &'a Path,
    /// The chunks every model compares — the same list for all of them.
    comparable: &'a [(i32, i32)],
    floors: &'a BTreeMap<(i32, i32), SkyFloor>,
    height: WorldHeight,
    names: &'a RegistryNames,
    air: u32,
    model: &'a Model,
}

/// Light every comparable chunk with one model and compare it with what
/// vanilla wrote.
fn sweep(run: &Sweep) -> Result<Tally, String> {
    let mut tally = Tally::default();
    for &(x, z) in run.comparable {
        let Some(root) = read(run.region_dir, x, z)? else {
            return Err(format!("chunk {x},{z} has never been generated"));
        };
        let vanilla = vanilla_light(&root, run.height)
            .map_err(|e| format!("chunk {x},{z}: reading Minecraft's light: {e}"))?;

        let (chunk, dust) = match run.model.volume {
            Volume::Column => {
                let mut chunk = dust_chunk(
                    run.region_dir,
                    x,
                    z,
                    run.height,
                    run.names,
                    run.air,
                    run.model.heightmaps,
                )?;
                let skirt = skirt_for(run.floors, x, z, run.height);
                let dust = dust_light(&mut chunk, skirt, run.height, &run.model.opacity);
                (chunk, dust)
            }
            Volume::Area(k) => area_light(run, x, z, k)?,
        };

        compare(&chunk, &vanilla, &dust, run.height, &mut tally);
    }
    Ok(tally)
}

/// Light a `(2k+1)²` block of columns together and read the centre back.
///
/// The whole block is read from disk for every centre, which is `(2k+1)²`
/// times the reading. That is affordable here and is precisely the cost a
/// server would have to think about, which is why the number is worth taking
/// before anything is moved into the engine.
fn area_light(
    run: &Sweep,
    x: i32,
    z: i32,
    k: i32,
) -> Result<(dust_world::chunk::Chunk, Column), String> {
    let side = 2 * k + 1;
    let mut chunks = Vec::with_capacity((side * side) as usize);
    for cz in -k..=k {
        for cx in -k..=k {
            chunks.push(dust_chunk(
                run.region_dir,
                x + cx,
                z + cz,
                run.height,
                run.names,
                run.air,
                run.model.heightmaps,
            )?);
        }
    }
    let _ = super::area::AreaSkyLight::seed(
        &mut chunks,
        side,
        &run.model.opacity,
        dust_world::propagation::Budget::new(AREA_LIGHT_BUDGET),
    );

    let centre = &chunks[((k) + (k) * side) as usize];
    let mut column = Column::empty(run.height);
    for y in run.height.min_y()..run.height.min_y() + run.height.height() as i32 {
        let row = (y - run.height.min_y()) as u32 % 16;
        let section = centre.section(y);
        for cx in 0..16usize {
            for cz in 0..16usize {
                column.set(
                    cx,
                    y,
                    cz,
                    section.sky_light().get(cx as u32, row, cz as u32),
                );
            }
        }
    }
    let centre = chunks.swap_remove(((k) + (k) * side) as usize);
    Ok((centre, column))
}

/// How much work lighting one block of columns may do.
///
/// The server's per-column budget times a generous factor for the widest
/// volume this verb offers, because a walk that ran out would report a shortfall
/// that is the budget rather than the volume — a measurement measuring its own
/// limit, which is the failure this whole file keeps finding.
const AREA_LIGHT_BUDGET: u64 = 1 << 30;

/// The light table `cargo xtask extract --only light` wrote for this version,
/// if this checkout has one.
///
/// **A developer route and deliberately only that.** The oracle runs from a
/// Rust checkout against a jar in the extractor's cache; how the same numbers
/// reach a *server operator* is the question decision record 0008 has left, and
/// this verb existing does not answer it. What it does is put a number on what
/// answering it would buy.
///
/// Absent is not an error — most of the reasons to run this verb do not need
/// one, and the run says which models it measured.
fn light_table(version: &str) -> Result<Option<(PathBuf, dust_registry::LightTable)>, String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the workspace root")
        .join(format!(".dust-extract/oracle-{version}/light.tsv"));
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let table =
        dust_registry::LightTable::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Some((path, table)))
}

/// Whether vanilla finished generating — and therefore lighting — this chunk.
///
/// `digest::scan` refuses a chunk below `full` for the same reason: what is
/// stored under it is a partial answer that looks like a complete one.
fn is_full(region_dir: &Path, x: i32, z: i32) -> Result<bool, String> {
    let Some(root) = read(region_dir, x, z)? else {
        return Ok(false);
    };
    let status = root.get("Status").and_then(nbt::Node::as_str);
    Ok(matches!(status, Some("minecraft:full" | "full")))
}

fn read(region_dir: &Path, x: i32, z: i32) -> Result<Option<nbt::Node>, String> {
    let path = region::region_file_path(region_dir, x, z);
    let bytes =
        std::fs::read(&path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let Some((compression, payload)) = region::read_chunk(&bytes, x, z)? else {
        return Ok(None);
    };
    let decompressed = region::decompress(compression, &payload)?;
    nbt::read_root(&decompressed)
        .map(Some)
        .map_err(|e| format!("chunk {x},{z}: {e}"))
}

/// Minecraft's own sky light for one column, with the missing arrays resolved.
///
/// The resolution is the whole subtlety and it is the module note's rule:
/// below the lowest stored section a cell is dark, above the highest it is
/// daylight. A section between two stored ones with no array of its own would
/// be ambiguous — vanilla does not write one, which is itself the check below.
fn vanilla_light(root: &nbt::Node, height: WorldHeight) -> Result<Column, String> {
    let sections = match root.get("sections") {
        Some(node @ nbt::Node::List(_)) => node.list(),
        _ => return Err("no sections".to_owned()),
    };

    let mut stored: BTreeMap<i32, &[u8]> = BTreeMap::new();
    for section in sections {
        // `Y` is a TAG_Byte in a section, not a TAG_Int. `Node::as_i32` is
        // deliberately strict — the digest code depends on it not widening —
        // so the narrowing is done here where the format is known.
        let y = match section.get("Y") {
            Some(nbt::Node::Byte(v)) => i32::from(*v),
            Some(nbt::Node::Int(v)) => *v,
            _ => return Err("a section has no Y".to_owned()),
        };
        if let Some(nbt::Node::ByteArray(bytes)) = section.get("SkyLight") {
            if bytes.len() != 2048 {
                return Err(format!("section {y} has a {}-byte SkyLight", bytes.len()));
            }
            stored.insert(y, bytes);
        }
    }
    let Some(highest) = stored.keys().next_back().copied() else {
        // A column vanilla stored no light for at all: every section is either
        // solid or sky, and there is no boundary to write. Resolving it would
        // need the terrain height, which is exactly what the comparison is
        // about — so this refuses rather than answers its own question.
        return Err("no section carries SkyLight".to_owned());
    };

    let mut column = Column::empty(height);
    for y in height.min_y()..height.min_y() + height.height() as i32 {
        let section_y = y.div_euclid(16);
        let bytes = stored.get(&section_y);
        // Above every stored array is open sky; a missing array anywhere else
        // is a uniform section, and a uniform section that is not sky is dark.
        let uniform = if section_y > highest { 15 } else { 0 };
        for x in 0..16usize {
            for z in 0..16usize {
                let value = match bytes {
                    Some(bytes) => nibble(bytes, x, y.rem_euclid(16) as usize, z),
                    None => uniform,
                };
                column.set(x, y, z, value);
            }
        }
    }
    Ok(column)
}

/// One nibble out of a 2048-byte light array. Low nibble first, y-z-x order.
fn nibble(bytes: &[u8], x: usize, y: usize, z: usize) -> u8 {
    let index = y * 256 + z * 16 + x;
    let byte = bytes[index / 2];
    if index % 2 == 0 {
        byte & 0x0F
    } else {
        byte >> 4
    }
}

/// The same chunk, read and lit by Dust.
fn dust_chunk(
    region_dir: &Path,
    x: i32,
    z: i32,
    height: WorldHeight,
    names: &RegistryNames,
    air: u32,
    heightmaps: Heightmaps,
) -> Result<dust_world::chunk::Chunk, String> {
    let path = region::region_file_path(region_dir, x, z);
    let bytes =
        std::fs::read(&path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let Some((compression, payload)) = region::read_chunk(&bytes, x, z)? else {
        return Err(format!("chunk {x},{z} is missing"));
    };
    let decompressed = region::decompress(compression, &payload)?;
    let named =
        dust_nbt::read::from_bytes(&decompressed).map_err(|e| format!("chunk {x},{z}: {e}"))?;
    let dust_nbt::Tag::Compound(root) = &named.tag else {
        return Err(format!("chunk {x},{z} is not a compound"));
    };
    let mut chunk =
        dust_world::anvil::chunk(root, height, names).map_err(|e| format!("chunk {x},{z}: {e}"))?;
    // **The fourth input, and the last one that is not Minecraft's.**
    //
    // Dust recomputes the heightmaps rather than trusting the file, because a
    // server that has edited a block has a heightmap the file does not — and
    // the predicate it recomputes with is "anything that is not air", where
    // vanilla's `MOTION_BLOCKING` is "blocks motion, or holds a fluid". Short
    // grass and flowers are the difference: vanilla lets daylight fall through
    // them at fifteen and Dust puts its sky floor above them, so the cell they
    // stand in comes out at fourteen.
    //
    // `Heightmaps::AsWritten` is not a mode a server could run in — it cannot
    // know the heightmap of a chunk it has changed. It is here to take the
    // predicate out of the measurement, so that what is left is the engine.
    if heightmaps == Heightmaps::Recomputed {
        chunk.recompute_heightmaps(|_, state| state != air);
    }
    let _ = ChunkPos::new(x, z);
    Ok(chunk)
}

/// The four columns around one, from what was read, falling back to open sky.
///
/// Open sky for an absent neighbour is what Dust itself uses at the edge of a
/// world — a column that has not been generated is not a wall — so the
/// fallback here is the server's behaviour and not a convenience.
fn skirt_for(
    floors: &BTreeMap<(i32, i32), SkyFloor>,
    x: i32,
    z: i32,
    height: WorldHeight,
) -> Skirt {
    let open = SkyFloor::open(height.min_y());
    let at = |nx: i32, nz: i32| floors.get(&(nx, nz)).copied().unwrap_or(open);
    Skirt {
        west: at(x - 1, z),
        east: at(x + 1, z),
        north: at(x, z - 1),
        south: at(x, z + 1),
    }
}

/// Light a column with Dust's own engine and read the result back out.
fn dust_light(
    chunk: &mut dust_world::chunk::Chunk,
    skirt: Skirt,
    height: WorldHeight,
    opacity: &dust_world::propagation::OpacityModel,
) -> Column {
    let _ = dust_server::net::world::light_column(chunk, opacity, skirt);

    let mut column = Column::empty(height);
    for y in height.min_y()..height.min_y() + height.height() as i32 {
        let row = (y - height.min_y()) as u32 % 16;
        let section = chunk.section(y);
        for x in 0..16usize {
            for z in 0..16usize {
                column.set(x, y, z, section.sky_light().get(x as u32, row, z as u32));
            }
        }
    }
    column
}

/// How far a cell sits from the nearest of its column's four faces.
///
/// Zero on a face, seven in the middle of a 16x16 column. The measure a
/// neighbour's light has to cross: it enters at a face with what a cell at
/// fifteen offers across one step and loses a level per step inward, so a
/// shortfall the multi-column volume would repair cannot be far from one.
fn edge_distance(x: usize, z: usize) -> u8 {
    let from = |v: usize| v.min(15 - v);
    u8::try_from(from(x).min(from(z))).expect("0..=7")
}

fn compare(
    chunk: &dust_world::chunk::Chunk,
    vanilla: &Column,
    dust: &Column,
    height: WorldHeight,
    tally: &mut Tally,
) {
    for y in height.min_y()..height.min_y() + height.height() as i32 {
        for x in 0..16usize {
            for z in 0..16usize {
                let want = vanilla.at(x, y, z);
                let got = dust.at(x, y, z);
                tally.cells += 1;
                let from_edge = edge_distance(x, z);
                *tally.cells_by_edge.entry(from_edge).or_default() += 1;
                if want == got {
                    tally.agree += 1;
                    continue;
                }
                let state = chunk.get_block(x as u32, y, z as u32);
                let name = dust_registry::BlockState::from_id(state)
                    .map(|s| s.block().name().to_owned())
                    .unwrap_or_else(|| format!("state {state}"));
                if got < want {
                    *tally.darker.entry(want - got).or_default() += 1;
                    *tally.darker_blocks.entry(name).or_default() += 1;
                    *tally.darker_by_edge.entry(from_edge).or_default() += 1;
                } else {
                    if std::env::var_os("DUST_LIGHT_TRACE").is_some() {
                        let pos = chunk.pos();
                        eprintln!(
                            "over-lit: chunk {},{} local ({x},{y},{z}) vanilla={want} dust={got}                              edge={}",
                            pos.x,
                            pos.z,
                            x == 0 || x == 15 || z == 0 || z == 15
                        );
                    }
                    *tally.brighter.entry(got - want).or_default() += 1;
                    *tally.brighter_blocks.entry(name).or_default() += 1;
                }
            }
        }
    }
}

/// The blocks a set of disagreeing cells sits in, most common first.
/// Print the shortfall as a rate per ring, innermost figure last.
///
/// **A rate and not a count.** The rings are not the same size — 60 columns
/// at the face against 4 in the middle — so counts alone say "the edges" for
/// any cause whatsoever. What separates the two known gaps is whether the
/// *rate* falls as you walk inward.
///
/// Reading it: a rate that collapses towards the middle is light failing to
/// arrive from a neighbour, and is what the multi-column volume would repair.
/// A flat rate is opacity, which does not care where in a column it is. This
/// is the verb's answer to "which of the two gaps is bigger", which was an
/// impression before it was a column of numbers.
fn rings(tally: &Tally) {
    if tally.darker_by_edge.is_empty() {
        return;
    }
    println!();
    println!("      by distance from a column's edge (0 = a face, 7 = the middle):");
    for distance in 0u8..=7 {
        let short = tally.darker_by_edge.get(&distance).copied().unwrap_or(0);
        let of = tally.cells_by_edge.get(&distance).copied().unwrap_or(0);
        if of == 0 {
            continue;
        }
        #[expect(clippy::cast_precision_loss, reason = "counts here are far below 2^53")]
        let rate = short as f64 * 100.0 / of as f64;
        println!("        {distance}: {rate:6.3}% of {of} cell(s) short");
    }
    println!("      a rate that falls towards the middle is light not arriving");
    println!("      from a neighbour; a flat one is opacity.");
}

fn histogram(blocks: &BTreeMap<String, u64>) {
    let mut rows: Vec<(&String, &u64)> = blocks.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    for (name, count) in rows.iter().take(8) {
        println!("      {name:<32} {count}");
    }
    if rows.len() > 8 {
        println!("      ... and {} more kinds", rows.len() - 8);
    }
}

fn report(tally: &Tally, model: &Model) {
    let disagree = tally.cells - tally.agree;
    let percent = |n: u64| {
        if tally.cells == 0 {
            0.0
        } else {
            n as f64 * 100.0 / tally.cells as f64
        }
    };
    println!();
    // **Sky light, and it says so.** Dust has no block light at all, so a
    // percentage that did not name which light it was about would read as "the
    // lighting is 99.4% right" when half of lighting is not implemented. The
    // block-light arrays a real server writes are not compared here because
    // there is nothing on this side to compare them to; see decision record
    // 0008 for what that is waiting on.
    println!("{} sky-light cells compared", tally.cells);
    println!(
        "{} agree ({:.3}%), {disagree} do not ({:.3}%)",
        tally.agree,
        percent(tally.agree),
        percent(disagree)
    );
    if disagree == 0 {
        return;
    }

    let brighter_total: u64 = tally.brighter.values().sum();
    if brighter_total == 0 {
        // The headline when it holds. Both known gaps under-light, so a run
        // in which *every* disagreement is a shortfall is a run whose
        // disagreements are all accounted for — and one over-lit cell would
        // mean something else is wrong, which is why this is a sentence and
        // not a silence.
        println!(
            "every one of them is Dust being darker, which is the direction every \
             known gap points in"
        );
        // Under the stand-in the percentage is a fact about the world rather
        // than about the engine — seed 0 reads 99.4% and seed 1, which spawns
        // in deep ocean, reads 96.5% with the same server — so it is said on
        // that rung and not on the others, where opacity has stopped making
        // the answer depend on how much water is in view.
        if !model.from_minecraft {
            println!(
                "the percentage is how much of *this* world is made of those blocks; \
                 run another seed before quoting it"
            );
        }
    }

    let darker: u64 = tally.darker.values().sum();
    let brighter: u64 = tally.brighter.values().sum();

    println!();
    println!("  {darker} cell(s) darker in Dust than in Minecraft");
    // Which gaps are actually in force under *this* model, rather than a
    // sentence that names all of them whatever is running. A report that still
    // blamed opacity under Minecraft's own numbers would be pointing at the
    // wrong rung of its own ladder.
    let mut gaps: Vec<&str> = Vec::new();
    if !model.from_minecraft {
        gaps.push("every block but air is fully opaque here");
    }

    if model.volume == Volume::Column {
        gaps.push("light does not travel through a neighbouring column");
    }
    if model.heightmaps == Heightmaps::Recomputed {
        gaps.push("the sky floor is `not air` where vanilla's is `blocks motion`");
    }
    if gaps.is_empty() {
        println!("      (every known gap is closed in this model, so these are a fourth thing)");
    } else {
        for (at, gap) in gaps.iter().enumerate() {
            let open = if at == 0 { "(" } else { " " };
            let close = if at + 1 == gaps.len() { ")" } else { "," };
            println!("      {open}{gap}{close}");
        }
    }
    for (by, count) in tally.darker.iter().rev().take(5) {
        println!("      short by {by:>2}: {count}");
    }
    histogram(&tally.darker_blocks);
    rings(tally);

    if brighter > 0 {
        // Printed loudly and with its own block list rather than folded into
        // the totals. Every known gap under-lights, so an over-lit cell is
        // something else — and a shared list would let a hundred unexplained
        // cells hide inside ten thousand explained ones.
        println!();
        println!("  {brighter} cell(s) BRIGHTER in Dust than in Minecraft");
        println!("      every known gap under-lights, so these are something else");
        for (by, count) in tally.brighter.iter().rev().take(5) {
            println!("      over by  {by:>2}: {count}");
        }
        histogram(&tally.brighter_blocks);
    }
}
