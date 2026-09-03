//! Score how long Dust says a block takes to break against what a real server
//! took.
//!
//! `tools/bot/drops.js --survival --times` breaks blocks on a running vanilla
//! 1.21.1 server and writes down the milliseconds each one took. This reads
//! those numbers, asks [`dust_sim::mining`] the same question of the
//! operator's own extracted constants, and counts the rows that agree.
//!
//! # Neither side restates the other
//!
//! The measured side is a real Minecraft server destroying a real block for a
//! real client's packets. The computed side is Dust's rule over
//! `dust-constants.tsv`'s hardness column and each item's own
//! `minecraft:tool` component — the operator's jar, not a table anybody typed.
//! Decision record 0022's trap was a survey drafted from `minecraft-data` and
//! then checked against `minecraft-data`; this has no such loop, and the check
//! that says so is [`Options::without_hardness`]: with the hardness column
//! withheld the computed side falls back to an instant break and the agreement
//! collapses, which is what a check that is really reading the column looks
//! like when the column is taken away.
//!
//! # A tick is the unit and a tick of slack is allowed
//!
//! The measurement polls the block at 25 ms and the packets travel over a
//! loopback socket, so a break that takes 23 ticks is observed somewhere in
//! `[23, 24]`. One tick of slack, not more: the whole point of scoring against
//! real numbers is to catch an error of a few ticks, and a tolerance wide
//! enough to swallow one cannot.

use std::path::{Path, PathBuf};

use dust_registry::{Block, BlockConstants, Item};
use dust_sim::mining::{Digger, Progress};

/// How far a measured break may sit from the computed one and still agree, in
/// ticks.
///
/// One. The observation is a poll and the poll is coarser than the tick it is
/// trying to name, so the tick a break is seen on is the tick it happened on
/// or the one after.
const SLACK: i64 = 1;

#[derive(Debug)]
pub struct Options {
    pub answers: PathBuf,
    pub tables: Option<PathBuf>,
    /// Withhold the hardness column from the computed side, which is the
    /// negative control: every block then breaks instantly, and a scorer that
    /// still agrees with a real server is not reading the column it claims to.
    pub without_hardness: bool,
    pub verbose: bool,
}

/// One row of a `--times` survey.
#[derive(Debug)]
struct Timing {
    block: String,
    held: String,
    tool: Option<Item>,
    /// Ticks the real server took, or `None` for a row it could not measure.
    ticks: Option<u32>,
}

pub fn run(options: &Options) -> std::process::ExitCode {
    let timings = match read_timings(&options.answers) {
        Ok(timings) => timings,
        Err(why) => {
            eprintln!("{why}");
            return std::process::ExitCode::from(2);
        }
    };
    let root = match options.tables.clone().or_else(default_tables) {
        Some(root) => root,
        None => {
            eprintln!(
                "no constants. Pass --tables <[data] path>, or run \
                 `cargo xtask extract --version 1.21.1 --only constants` so the cache has some."
            );
            return std::process::ExitCode::from(2);
        }
    };
    let constants = match constants(&root) {
        Ok(Some(constants)) => constants,
        Ok(None) => {
            eprintln!(
                "{} holds no dust-constants.tsv, so there is no hardness to compare against. \
                 Run `cargo xtask extract --version 1.21.1 --only constants`.",
                root.display()
            );
            return std::process::ExitCode::from(2);
        }
        Err(why) => {
            eprintln!("{why}");
            return std::process::ExitCode::from(2);
        }
    };

    println!(
        "{} timing(s) from {}, constants from {}",
        timings.len(),
        options.answers.display(),
        root.display()
    );
    // Said before the score and not after it, because it is the sentence that
    // decides whether the score means anything: a table with no hardness
    // column scores every block as instant, and a reader who saw only the
    // percentage would read that as a defect in the rule.
    println!(
        "  the hardness is {}",
        if options.without_hardness {
            "WITHHELD by --without-hardness, so every block computes as instant. \
             This is the negative control and it is meant to score badly."
                .to_owned()
        } else if constants.has_destroy_speed() {
            let unbreakable = (0..constants.len() as u32)
                .filter(|state| constants.destroy_speed(*state).is_some_and(|h| h < 0.0))
                .count();
            format!(
                "known: {} state(s), {unbreakable} of them unbreakable",
                constants.len()
            )
        } else {
            "unknown, because this dust-constants.tsv has no destroy_speed column; \
             re-run `cargo xtask extract --only constants` for it"
                .to_owned()
        }
    );

    // Resolved once, outside the loop. `None` is a table extracted before the
    // column existed, and it reads as "no block asks for a tool" — which makes
    // every hand the right one and every break the fast divisor. That is a
    // different wrong answer from the one `--without-hardness` produces, and
    // it is said above rather than left to the score.
    let requires_tool = constants
        .flag("requires_tool")
        .map(|flag| (&constants, flag));

    let (mut agreed, mut disagreed, mut unmeasured, mut unknown) = (0u32, 0u32, 0u32, 0u32);
    let mut worst: i64 = 0;
    let mut worklist: Vec<String> = Vec::new();
    for timing in &timings {
        let Some(measured) = timing.ticks else {
            unmeasured += 1;
            continue;
        };
        let Some(block) = Block::from_name(&timing.block) else {
            unknown += 1;
            worklist.push(format!("{}: not a block on this version", timing.block));
            continue;
        };
        // The default state, which is what a `setblock` with no properties
        // puts down and so is what the survey broke. Hardness does not vary
        // with a block's properties for any vanilla block, but reading the
        // default rather than state zero is the reading that stays true if one
        // ever does.
        let state = block.default_state().id();
        let hardness = if options.without_hardness {
            0.0
        } else {
            match constants.destroy_speed(state) {
                Some(hardness) => hardness,
                None => {
                    unknown += 1;
                    worklist.push(format!(
                        "{}: no hardness for state {state}",
                        timing.block
                    ));
                    continue;
                }
            }
        };
        let digger = Digger {
            speed: dust_registry::mining::speed(timing.tool, block),
            // The survey holds a plain `/give` tool, so no row here is about
            // an enchantment. A run that put an efficiency pickaxe in the hand
            // and scored it against this would be scoring a zero.
            efficiency: 0,
            // Composed, never the drops verdict alone — see
            // `dust_sim::mining::tool_is_correct`, and the eight rows of this
            // survey that say 15 and 60 where the drops verdict alone predicts
            // 50 and 200.
            correct: dust_sim::mining::tool_is_correct(
                requires_tool
                    .is_some_and(|(table, flag)| table.is_set(flag, state)),
                dust_registry::mining::correct_for_drops(timing.tool, block),
            ),
            on_ground: true,
        };
        let progress = Progress::of(hardness, &digger);
        let Some(computed) = progress.ticks() else {
            disagreed += 1;
            worklist.push(format!(
                "{} with {}: Dust says unbreakable, the server took {measured} tick(s)",
                timing.block, timing.held
            ));
            continue;
        };
        let drift = i64::from(measured) - i64::from(computed);
        if drift.abs() <= SLACK {
            agreed += 1;
            if options.verbose {
                println!(
                    "  agreed      {:<26} {:<24} {computed} tick(s), measured {measured}",
                    timing.block, timing.held
                );
            }
        } else {
            disagreed += 1;
            worst = worst.max(drift.abs());
            worklist.push(format!(
                "{} with {}: Dust says {computed} tick(s), the server took {measured} \
                 ({drift:+})",
                timing.block, timing.held
            ));
        }
    }

    let scored = agreed + disagreed;
    println!(
        "{agreed}/{scored} scored row(s) agree within {SLACK} tick, {disagreed} do not, \
         {unmeasured} unmeasured, {unknown} unknown"
    );
    if disagreed > 0 {
        println!("  the largest disagreement is {worst} tick(s)");
    }
    for line in &worklist {
        println!("  {line}");
    }
    if disagreed == 0 && unknown == 0 {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}

fn constants(root: &Path) -> Result<Option<BlockConstants>, String> {
    let path = root.join("dust-constants.tsv");
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    BlockConstants::parse(&text)
        .map(Some)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// The extract cache, which is where a developer's constants already are.
fn default_tables() -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.dust-extract/data-1.21.1/data");
    path.is_dir().then_some(path)
}

fn read_timings(path: &Path) -> Result<Vec<Timing>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        format!(
            "could not read {}: {e}. Produce one with `DUST_SERVER_CONSOLE=<fifo> \
             node tools/bot/drops.js <port> <blocks> --survival --times`.",
            path.display()
        )
    })?;
    let mut out = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 4 {
            return Err(format!(
                "line {}: {} field(s) where a timing has four",
                index + 1,
                fields.len()
            ));
        }
        let held = fields[1].to_owned();
        let tool = if held == "-" {
            None
        } else {
            let namespaced = if held.contains(':') {
                held.clone()
            } else {
                format!("minecraft:{held}")
            };
            let item = Item::from_name(&namespaced);
            if item.is_none() {
                return Err(format!(
                    "line {}: `{held}` is not an item on this version, so the survey \
                     was run against a different one",
                    index + 1
                ));
            }
            item
        };
        out.push(Timing {
            block: fields[0].to_owned(),
            held,
            tool,
            // The fourth field is the survey's own tick count. A row that
            // timed out spells it `-`, and it is counted apart rather than
            // read as a very slow break — the same three-outcome rule the
            // drops survey learned.
            ticks: fields[3].parse::<u32>().ok(),
        });
    }
    if out.is_empty() {
        return Err(format!("{} holds no timings", path.display()));
    }
    Ok(out)
}

pub fn parse(args: &[String]) -> Result<Options, String> {
    let mut answers = None;
    let mut tables = None;
    let mut without_hardness = false;
    let mut verbose = false;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        index += 1;
        match flag {
            "--verbose" => verbose = true,
            "--without-hardness" => without_hardness = true,
            "--answers" | "--tables" => {
                let value = args
                    .get(index)
                    .ok_or_else(|| format!("{flag} needs a value"))?;
                index += 1;
                let slot = if flag == "--answers" {
                    &mut answers
                } else {
                    &mut tables
                };
                if slot.is_some() {
                    return Err(format!("{flag} given twice"));
                }
                *slot = Some(PathBuf::from(value));
            }
            other => return Err(format!("unknown option `{other}` for harness break")),
        }
    }
    Ok(Options {
        answers: answers.ok_or("harness break needs --answers <file>")?,
        tables,
        without_hardness,
        verbose,
    })
}
