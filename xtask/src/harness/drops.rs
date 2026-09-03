//! Score what Dust says a broken block yields against what a real server did.
//!
//! `tools/bot/drops.js --survival` breaks blocks on a running vanilla 1.21.1
//! server, in survival — because **a creative player's break drops nothing**,
//! and a survey run in creative would record an empty answer for every block
//! and call it a measurement. This reads those answers, asks
//! [`dust_sim::drops`] the same questions of the operator's own loot tables,
//! and counts.
//!
//! # A drop is a distribution, so the comparison is about support
//!
//! One break of one oak leaf block is not an answer about oak leaves: it
//! yields nothing 19 times in 20 and a sapling once. So a row is scored by
//! asking whether Dust **can** produce what Minecraft produced, over enough
//! rolls to see the tail, and by printing how often it does. A row where the
//! observed drop is impossible under Dust's reading of the table is a real
//! disagreement; a row where it is rare is not.
//!
//! That cuts the other way too, and the other way is the one worth stating:
//! Dust producing something Minecraft **never** could is invisible to a
//! one-observation survey, and this prints the most likely outcome beside the
//! observed one so a reader can see it.
//!
//! # It asks `dust-sim`, not a copy of it
//!
//! The same argument `harness placement` makes: a checker with its own copy of
//! the rule agrees with itself under any rule, including a wrong one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use dust_registry::loot::BlockLoot;
use dust_registry::{Block, BlockConstants, Item};
use dust_sim::drops::{self, Break, Rng, Tables, Tool};

/// How many times each block's table is rolled before its answer is believed.
///
/// The rarest thing any vanilla block table can do is a fortune-less
/// `table_bonus` at 0.005 — a leaf block's stick. Two thousand rolls sees that
/// ten times on average, which is enough for "never" to mean never.
const ROLLS: u32 = 2_000;

#[derive(Debug)]
pub struct Options {
    pub answers: PathBuf,
    pub tables: Option<PathBuf>,
    /// Score every row as though the tool were unenchanted, which is the
    /// negative control for the enchantment seam: every silk-touch and
    /// fortune branch then takes its other side, and a scorer that still
    /// agrees is not reading the column it says it reads.
    pub without_enchantments: bool,
    pub verbose: bool,
}

/// What one row of the survey said.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    /// The block was broken and these came out, spelled `item*count`, sorted.
    Yielded(String),
    /// The block was broken and nothing came out.
    Nothing,
    /// The run could not ask the question. Counted apart from both, because a
    /// tool failure read as "yields nothing" is a defect invented out of a
    /// broken harness — and because a break that yields nothing and a break
    /// that never happened leave exactly the same air behind.
    Unmeasured(String),
}

#[derive(Debug)]
struct Answer {
    block: String,
    /// What the survey held while it broke the block, or `None` for a bare
    /// hand. Read from the answer file rather than assumed, because the two
    /// rows that decision record 0022 left disagreeing were both about the
    /// tool: a survey that reported which tool it used and a scorer that
    /// hardcoded a netherite pickaxe could not have found them.
    tool: Option<Item>,
    /// The tool as the survey spelled it, for the worklist.
    held: String,
    /// What was on it, as `(name, level)`. Empty for a plain tool, which is
    /// almost every row.
    enchantments: Vec<(String, u32)>,
    outcome: Outcome,
}

pub fn run(options: &Options) -> std::process::ExitCode {
    let answers = match read_answers(&options.answers) {
        Ok(answers) => answers,
        Err(why) => {
            eprintln!("{why}");
            return std::process::ExitCode::from(2);
        }
    };
    let root = match options.tables.clone().or_else(default_tables) {
        Some(root) => root,
        None => {
            eprintln!(
                "no loot tables. Pass --tables <[data] path>, or run \
                 `cargo xtask extract --version 1.21.1 --only loot` so the cache has some."
            );
            return std::process::ExitCode::from(2);
        }
    };
    let loot = match block_loot(&root) {
        Ok(loot) => loot,
        Err(why) => {
            eprintln!("{why}");
            return std::process::ExitCode::from(2);
        }
    };
    let tables = match load(&root, loot.as_ref()) {
        Ok(tables) => tables,
        Err(why) => {
            eprintln!("{why}");
            return std::process::ExitCode::from(2);
        }
    };
    let constants = match constants(&root) {
        Ok(constants) => constants,
        Err(why) => {
            eprintln!("{why}");
            return std::process::ExitCode::from(2);
        }
    };
    let needs_tool = constants
        .as_ref()
        .and_then(|table| table.flag("requires_tool").map(|flag| (table, flag)));
    println!(
        "{} answer(s) from {}, {} block table(s) from {}",
        answers.len(),
        options.answers.display(),
        tables.len(),
        root.display()
    );
    println!(
        "  the block-to-table relation is {}",
        match &loot {
            Some(loot) => format!(
                "known: {} block(s), {} drawing from another block's file",
                loot.len(),
                loot.elsewhere()
            ),
            None => format!(
                "guessed from each file's own name, because {} holds no dust-blocks.tsv",
                root.display()
            ),
        }
    );
    println!(
        "  the tool requirement is {}",
        match needs_tool {
            Some((table, flag)) => format!(
                "known: {} of {} state(s) want the right tool",
                (0..table.len() as u32)
                    .filter(|state| table.is_set(flag, *state))
                    .count(),
                table.len()
            ),
            None => "unknown, because there is no dust-constants.tsv with a \
                     requires_tool column here"
                .to_owned(),
        }
    );

    let (mut agreed, mut disagreed, mut unmeasured, mut untabled) = (0u32, 0u32, 0u32, 0u32);
    let mut worklist: Vec<String> = Vec::new();
    for answer in &answers {
        let wanted = match &answer.outcome {
            Outcome::Unmeasured(why) => {
                unmeasured += 1;
                if options.verbose {
                    println!("  unmeasured  {:<34} {why}", answer.block);
                }
                continue;
            }
            Outcome::Nothing => String::from("-"),
            Outcome::Yielded(spelling) => spelling.clone(),
        };
        let Some(block) = Block::from_name(&answer.block) else {
            untabled += 1;
            worklist.push(format!("{}: not a block on this version", answer.block));
            continue;
        };
        let Some(table) = tables.table(block) else {
            // The 78 blocks decision record 0022 names: no table of their own
            // name, which is not the same as no drops. Counted apart so the
            // agreement figure is about the tables that exist.
            untabled += 1;
            worklist.push(format!(
                "{}: no loot table of its own name, so Dust drops nothing",
                answer.block
            ));
            continue;
        };
        let requires_tool =
            needs_tool.is_some_and(|(table, flag)| table.is_set(flag, block.default_state().id()));
        let enchantments: Vec<(&str, u32)> = answer
            .enchantments
            .iter()
            .filter(|_| !options.without_enchantments)
            .map(|(name, level)| (name.as_str(), *level))
            .collect();
        let seen = distribution(
            table,
            block,
            answer.tool,
            &enchantments,
            requires_tool,
            ROLLS,
        );
        let times = seen.get(&wanted).copied().unwrap_or(0);
        if times > 0 {
            agreed += 1;
            if options.verbose {
                println!(
                    "  agree       {:<34} {:<22} {wanted}  ({} in {ROLLS})",
                    answer.block, answer.held, times
                );
            }
        } else {
            disagreed += 1;
            let likeliest = seen
                .iter()
                .max_by_key(|(_, count)| **count)
                .map(|(spelling, count)| format!("{spelling} ({count} in {ROLLS})"))
                .unwrap_or_else(|| "nothing at all".to_owned());
            worklist.push(format!(
                "{} with {}: Minecraft gave {wanted}, Dust never does; its commonest \
                 is {likeliest}",
                answer.block, answer.held
            ));
        }
    }

    let scored = agreed + disagreed;
    println!("\n  {scored:>5}  rows Dust could be asked about");
    if scored > 0 {
        println!(
            "  {agreed:>5}  where what Minecraft dropped is something Dust drops ({:.1}%)",
            100.0 * f64::from(agreed) / f64::from(scored)
        );
        println!(
            "  {disagreed:>5}  where it is not ({:.1}%)",
            100.0 * f64::from(disagreed) / f64::from(scored)
        );
    }
    println!("  {untabled:>5}  rows with no table of that block's own name");
    println!("  {unmeasured:>5}  rows the survey could not ask");

    if !worklist.is_empty() {
        println!("\nworklist:");
        for line in &worklist {
            println!("  {line}");
        }
    }
    // A measurement and not a gate, for the same reason `light` and
    // `placement` are: what it prints is a fact about a version's data, and a
    // number that stops CI would stop it on somebody else's world.
    std::process::ExitCode::SUCCESS
}

/// Every distinct thing a table yields over `rolls` breaks of its default
/// state, with how often, spelled the way the survey spells it.
///
/// The default state and not every state, because that is what the survey
/// broke: `/setblock minecraft:wheat` places `age=0`. A survey that named the
/// state would be scored against the state; this one does not, so this does
/// not invent one.
fn distribution(
    table: &drops::Table,
    block: Block,
    held: Option<Item>,
    enchantments: &[(&str, u32)],
    requires_tool: bool,
    rolls: u32,
) -> BTreeMap<String, u32> {
    // Whatever the survey held, including nothing and including what was on
    // it. The tool column may name enchantments — `netherite_pickaxe@fortune:3`
    // — and a scorer that dropped them would compare a fortune run against the
    // unenchanted branch and call every extra ore a disagreement.
    let tool = Tool {
        item: held,
        enchantments,
    };
    let context = Break {
        state: block.default_state(),
        tool,
        broken_by_entity: true,
        requires_tool,
        neighbours: &[],
    };
    let mut rng = Rng::from_seed(0xd005_5eed);
    let mut out = BTreeMap::new();
    let mut rolled = Vec::new();
    for _ in 0..rolls {
        rolled.clear();
        table.roll(&context, &mut rng, &mut rolled);
        let mut spelled: Vec<String> = rolled
            .iter()
            .map(|drop| format!("{}*{}", drop.item.name(), drop.count))
            .collect();
        spelled.sort();
        let key = if spelled.is_empty() {
            "-".to_owned()
        } else {
            spelled.join(",")
        };
        *out.entry(key).or_insert(0) += 1;
    }
    out
}

/// The block-to-loot-table relation, if the operator's data carries it.
///
/// Optional here and not in the server, because this is a measurement tool and
/// the number it prints without one is a real number about a real server: a
/// reader who has not re-run the extractor should see the score they would get.
fn block_loot(root: &Path) -> Result<Option<BlockLoot>, String> {
    let path = root.join("dust-blocks.tsv");
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    BlockLoot::parse(&text)
        .map(Some)
        .map_err(|e| format!("{}: {e}", path.display()))
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

fn load(root: &Path, loot: Option<&BlockLoot>) -> Result<Tables, String> {
    let blocks = root.join("minecraft/loot_table/blocks");
    let entries = std::fs::read_dir(&blocks)
        .map_err(|e| format!("could not read {}: {e}", blocks.display()))?;
    let mut tables = Tables::default();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let drawn: Vec<Block> = match loot {
            Some(loot) => loot
                .drawing_from(&format!("minecraft:blocks/{stem}"))
                .to_vec(),
            None => drops::block_of_file("minecraft", stem)
                .into_iter()
                .collect(),
        };
        if drawn.is_empty() {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("could not read {}: {e}", path.display()))?;
        if let Err(why) = tables.insert_for(&drawn, &text) {
            return Err(format!("{}: {why}", path.display()));
        }
    }
    if tables.is_empty() {
        return Err(format!("{} holds no block tables", blocks.display()));
    }
    Ok(tables)
}

/// The extract cache, which is where a developer's tables already are.
///
/// Relative to the workspace root and not to a run directory, because
/// `cargo xtask` runs from the root and the cache is a sibling of `crates/`.
fn default_tables() -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.dust-extract/data-1.21.1/data");
    path.is_dir().then_some(path)
}

/// Split `netherite_pickaxe@fortune:3+efficiency:5` into the item and what is
/// on it.
///
/// The same spelling `tools/bot/drops.js --tool` takes, so one column in a
/// survey file means one thing on both sides of the comparison. A plain tool
/// has no `@` and produces an empty list, which is almost every row.
fn split_tool(spelling: &str) -> (&str, Vec<(String, u32)>) {
    let Some((item, rest)) = spelling.split_once('@') else {
        return (spelling, Vec::new());
    };
    let enchantments = rest
        .split('+')
        .filter(|pair| !pair.is_empty())
        .filter_map(|pair| {
            let (name, level) = pair.rsplit_once(':')?;
            let name = if name.contains(':') {
                name.to_owned()
            } else {
                format!("minecraft:{name}")
            };
            Some((name, level.parse::<u32>().ok()?))
        })
        .collect();
    (item, enchantments)
}

fn read_answers(path: &Path) -> Result<Vec<Answer>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        format!(
            "could not read {}: {e}. Produce one with \
             `DUST_SERVER_CONSOLE=<fifo> node tools/bot/drops.js <port> <blocks> --survival`.",
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
                "line {}: {} field(s) where an answer has four",
                index + 1,
                fields.len()
            ));
        }
        let outcome = match fields[2] {
            "BROKE" => Outcome::Yielded(fields[3].to_owned()),
            "NOTHING" => Outcome::Nothing,
            other => Outcome::Unmeasured(format!("{other} ({})", fields[3])),
        };
        // A bare hand is spelled `-`, which is what the survey writes when it
        // put nothing in the player's hand at all. It is a tool the drop rules
        // have a great deal to say about, and reading it as "no answer" would
        // silently drop every row that is about it.
        let held = fields[1].to_owned();
        // `item@enchantment:level+enchantment:level`, which is what
        // `tools/bot/drops.js --tool` writes when it puts an enchanted tool in
        // the hand. Split before the item is resolved, because
        // `netherite_pickaxe@fortune:3` is not an item name.
        let (spelling, enchantments) = split_tool(&held);
        let tool = if spelling == "-" {
            None
        } else {
            let namespaced = if spelling.contains(':') {
                spelling.to_owned()
            } else {
                format!("minecraft:{spelling}")
            };
            let item = Item::from_name(&namespaced);
            if item.is_none() {
                return Err(format!(
                    "line {}: `{spelling}` is not an item on this version, so the survey \
                     was run against a different one",
                    index + 1
                ));
            }
            item
        };
        out.push(Answer {
            block: fields[0].to_owned(),
            tool,
            held,
            enchantments,
            outcome,
        });
    }
    if out.is_empty() {
        return Err(format!("{} holds no answers", path.display()));
    }
    Ok(out)
}

pub fn parse(args: &[String]) -> Result<Options, String> {
    let mut answers = None;
    let mut tables = None;
    let mut without_enchantments = false;
    let mut verbose = false;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        index += 1;
        match flag {
            "--verbose" => verbose = true,
            "--without-enchantments" => without_enchantments = true,
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
            other => return Err(format!("unknown option `{other}` for harness drops")),
        }
    }
    Ok(Options {
        answers: answers.ok_or("harness drops needs --answers <file>")?,
        tables,
        without_enchantments,
        verbose,
    })
}
