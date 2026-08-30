//! `harness light` — how close is Dust's sky light to Minecraft's?
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
//! # What a difference means, and what it does not
//!
//! Dust's light is known to be approximate, and this exists to put a number on
//! it rather than to pass. Two causes are known going in:
//!
//! * **Opacity.** Dust treats every block but air as fully opaque. Vanilla
//!   gives water, leaves, glass and ice an opacity of one or two, so light
//!   reaches under a tree and into the top of an ocean here and stops dead
//!   there. Light emission and opacity are code constants in Minecraft, in no
//!   report and no data pack, so there is nothing to extract yet.
//! * **Light through a neighbour.** A column is lit with its neighbours' sky
//!   floors as sources, which is exact where a neighbour is open to the sky
//!   and under-lights where the light would have to travel *through* one.
//!
//! Both under-light rather than over-light, so a cell where Dust is *brighter*
//! than vanilla is a third thing and is counted separately. It would mean
//! light arriving where vanilla says none does, which no known gap explains.
//!
//! # Exit codes
//!
//! `0` always, unless the run itself failed (`2`). This is a **measurement and
//! not a gate**: the number it reports is expected to be short of a hundred
//! per cent today, and a verb that returned failure for a known gap would be
//! red every time it ran, which teaches people to stop running it.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

use dust_server::net::source::RegistryNames;
use dust_world::column_light::{Skirt, SkyFloor};
use dust_world::coords::ChunkPos;
use dust_world::heightmap::WorldHeight;

use super::{cache, digest, nbt, region};

const USAGE: &str = "\
harness light --version <v> [--seed <n>] [--radius <r>]

Reads a world Minecraft generated and lit, lights the same chunks with Dust's
own engine, and compares the sky light cell by cell. Prints how much agrees and
names what the disagreements are standing on.

  --version <v>   Minecraft version, e.g. 1.21.1.
  --seed <n>      The provisioned world's seed. Default 0.
  --radius <r>    Chunks either side of spawn. Default 2 (a 5x5).
";

#[derive(Debug)]
pub struct Options {
    pub version: String,
    pub seed: i64,
    pub radius: i32,
}

pub fn parse(args: &[String]) -> Result<Options, String> {
    let mut version = None;
    let mut seed = 0i64;
    let mut radius = 2i32;
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
        "comparing {} chunk(s) of Minecraft {} seed {}",
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
    let mut floors: BTreeMap<(i32, i32), SkyFloor> = BTreeMap::new();
    for &(x, z) in &expected {
        for (nx, nz) in [(x, z), (x - 1, z), (x + 1, z), (x, z - 1), (x, z + 1)] {
            if floors.contains_key(&(nx, nz)) {
                continue;
            }
            // A neighbour outside the generated world has no floors to give.
            // Left out rather than invented: `skirt_for` falls back to open
            // sky, which is what Dust itself does at the edge of a world.
            if let Ok(chunk) = dust_chunk(&region_dir, nx, nz, height, &names, air) {
                floors.insert((nx, nz), SkyFloor::of(&chunk));
            }
        }
    }

    let mut tally = Tally::default();
    for &(x, z) in &expected {
        let Some(root) = read(&region_dir, x, z)? else {
            return Err(format!("chunk {x},{z} has never been generated"));
        };
        let vanilla = vanilla_light(&root, height)
            .map_err(|e| format!("chunk {x},{z}: reading Minecraft's light: {e}"))?;

        let mut chunk = dust_chunk(&region_dir, x, z, height, &names, air)?;
        let skirt = skirt_for(&floors, x, z, height);
        let dust = dust_light(&mut chunk, skirt, height);

        compare(&chunk, &vanilla, &dust, height, &mut tally);
    }

    report(&tally);
    Ok(())
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
    chunk.recompute_heightmaps(|_, state| state != air);
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
fn dust_light(chunk: &mut dust_world::chunk::Chunk, skirt: Skirt, height: WorldHeight) -> Column {
    let air = dust_registry::Block::from_name("minecraft:air")
        .expect("checked at startup")
        .default_state()
        .id();
    let opacity = dust_server::net::world::opacity_of(air);
    let _ = dust_server::net::world::light_column(chunk, &opacity, skirt);

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
                } else {
                    *tally.brighter.entry(got - want).or_default() += 1;
                    *tally.brighter_blocks.entry(name).or_default() += 1;
                }
            }
        }
    }
}

/// The blocks a set of disagreeing cells sits in, most common first.
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

fn report(tally: &Tally) {
    let disagree = tally.cells - tally.agree;
    let percent = |n: u64| {
        if tally.cells == 0 {
            0.0
        } else {
            n as f64 * 100.0 / tally.cells as f64
        }
    };
    println!();
    println!("{} cells compared", tally.cells);
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
            "every one of them is Dust being darker, which is the direction both \
             known gaps point in"
        );
    }

    let darker: u64 = tally.darker.values().sum();
    let brighter: u64 = tally.brighter.values().sum();

    println!();
    println!("  {darker} cell(s) darker in Dust than in Minecraft");
    println!("      (every block but air is fully opaque here, and light does");
    println!("       not travel through a neighbouring column)");
    for (by, count) in tally.darker.iter().rev().take(5) {
        println!("      short by {by:>2}: {count}");
    }
    histogram(&tally.darker_blocks);

    if brighter > 0 {
        // Printed loudly and with its own block list rather than folded into
        // the totals. Both known gaps under-light, so an over-lit cell is a
        // third thing — and a shared list would let a hundred unexplained
        // cells hide inside ten thousand explained ones.
        println!();
        println!("  {brighter} cell(s) BRIGHTER in Dust than in Minecraft");
        println!("      both known gaps under-light, so these are a third thing");
        for (by, count) in tally.brighter.iter().rev().take(5) {
            println!("      over by  {by:>2}: {count}");
        }
        histogram(&tally.brighter_blocks);
    }
}
