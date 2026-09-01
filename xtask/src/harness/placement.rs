//! How much of Minecraft's placement Dust reproduces, as a number.
//!
//! # What this is for
//!
//! Decision record 0011 chose rules in Dust over a table on the operator's
//! disk, and said the rules are worth exactly what their check says they are
//! worth. This is the check. It reads the answers `tools/bot/placement.js`
//! asked of a real server, asks Dust the same questions, and counts.
//!
//! It is a **measurement and not a gate**, exactly as `harness light` is: a
//! verb that failed for a known gap would be red on every run and read by
//! nobody. What it produces is a number that goes down as rules are written,
//! and a list of what is still wrong.
//!
//! # Where the answers come from
//!
//! Not from here. They are Minecraft's, so they live in the harness cache on
//! the operator's own disk beside everything else the extractor produces —
//! `tools/bot/README.md` is how to make them. Without a file this verb says so
//! and stops; it does not guess.
//!
//! # What it is comparing against
//!
//! Today, the block's **default state**, because that is what
//! `dust_server::net::session` puts down. This file deliberately does not
//! reimplement that choice: it calls the same `dust_registry` the server calls,
//! so the day the server computes something better this verb reports the
//! improvement without being edited. A checker with its own copy of the rule
//! agrees with itself.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use dust_registry::{Block, Item, ItemBlocks};

/// What `harness placement` was asked to do.
#[derive(Debug)]
pub struct Options {
    /// The answers, as `tools/bot/placement.js` wrote them.
    pub answers: PathBuf,
    /// The item-to-block table, which says which block an item puts down.
    pub items: Option<PathBuf>,
    /// Print every disagreement rather than a sample of each kind.
    pub verbose: bool,
}

/// One row of the answers file.
#[derive(Debug)]
struct Answer {
    item: String,
    face: u8,
    yaw: i32,
    pitch: i32,
    cursor_y: String,
    /// The state Minecraft put down, `REFUSED`, or a row the tool could not
    /// take.
    result: Outcome,
}

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// A state, in `minecraft:name[a=b,c=d]` spelling.
    Placed(String),
    /// Minecraft placed nothing.
    Refused,
    /// The tool's own arena did not settle. Not a finding about either server,
    /// and counted apart so that it cannot quietly become one.
    Unmeasured,
}

/// Read the answers, ask Dust the same questions, and print the score.
pub fn run(options: &Options) -> ExitCode {
    match measure(options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("error: {why}");
            ExitCode::from(2)
        }
    }
}

fn measure(options: &Options) -> Result<(), String> {
    let text = std::fs::read_to_string(&options.answers)
        .map_err(|e| format!("could not read {}: {e}", options.answers.display()))?;
    let answers = parse_answers(&text)?;
    if answers.is_empty() {
        return Err(format!(
            "{} holds no rows; see tools/bot/README.md for how to make one",
            options.answers.display()
        ));
    }

    let items = load_items(options.items.as_deref())?;

    let mut score = Score::default();
    for answer in &answers {
        score.add(answer, &items);
    }
    // A file where nothing resolved is a file in the wrong shape, and printing
    // `0 of 0 agree` for it is a measurement of nothing dressed as a clean run.
    // The commonest cause is the one that produced this check: an answers file
    // whose items are spelled without their namespace.
    if score.agreed + score.disagreed == 0 && score.unresolved > 0 {
        return Err(format!(
            "none of the {} placements name an item this build has — the first is {:?}. \
             An item is `minecraft:stone` here and not `stone`; a file written by an older \
             tools/bot/placement.js spells them the short way and has to be made again.",
            score.unresolved,
            score.first_unresolved.as_deref().unwrap_or("")
        ));
    }
    score.report(options.verbose);
    Ok(())
}

/// Everything counted, and enough of what disagreed to act on.
#[derive(Debug, Default)]
struct Score {
    /// Situations where Minecraft placed a state and Dust would place the same.
    agreed: u64,
    /// Situations where it would place a different one.
    disagreed: u64,
    /// Situations Minecraft refused. Dust's own refusals are a different
    /// question — placement *rules*, not placement *state* — and are not what
    /// this verb measures.
    refused: u64,
    /// Rows the measuring tool could not take.
    unmeasured: u64,
    /// Items whose block this build cannot resolve at all.
    unresolved: u64,
    /// The first of them, for the message that names what is wrong.
    first_unresolved: Option<String>,
    /// One example per (item, what Dust would place, what Minecraft placed),
    /// so a run prints a worklist rather than ten thousand rows.
    disagreements: BTreeMap<String, (String, String, u64)>,
    /// Which items ever placed anything, and which of those ever disagreed.
    placing: BTreeMap<String, bool>,
}

impl Score {
    fn add(&mut self, answer: &Answer, items: &Option<ItemBlocks>) {
        let expected = match &answer.result {
            Outcome::Unmeasured => {
                self.unmeasured += 1;
                return;
            }
            Outcome::Refused => {
                self.refused += 1;
                return;
            }
            Outcome::Placed(state) => state,
        };

        let Some(theirs) = dust_state(&answer.item, items) else {
            self.unresolved += 1;
            self.first_unresolved
                .get_or_insert_with(|| answer.item.clone());
            return;
        };
        let seen = self.placing.entry(answer.item.clone()).or_insert(false);
        if theirs == *expected {
            self.agreed += 1;
            return;
        }
        *seen = true;
        self.disagreed += 1;
        let entry = self
            .disagreements
            .entry(answer.item.clone())
            .or_insert_with(|| (theirs.clone(), expected.clone(), 0));
        entry.2 += 1;
        // The example kept is the first one. Which situation it was is in the
        // answers file; what is wanted here is the *shape* of the difference,
        // and one row of each shape is a worklist.
        let _ = (&answer.face, &answer.yaw, &answer.pitch, &answer.cursor_y);
    }

    fn report(&self, verbose: bool) {
        let measured = self.agreed + self.disagreed;
        println!("placement, against Minecraft's own answers");
        println!();
        println!(
            "  {:>7}  situations where Minecraft placed a block",
            measured
        );
        println!(
            "  {:>7}  of them Dust would place the same state ({})",
            self.agreed,
            percent(self.agreed, measured)
        );
        println!(
            "  {:>7}  of them it would not ({})",
            self.disagreed,
            percent(self.disagreed, measured)
        );
        println!();
        println!(
            "  {:>7}  Minecraft refused, so there is nothing to compare",
            self.refused
        );
        if self.unmeasured > 0 {
            println!(
                "  {:>7}  the measuring tool could not take, and they are not a finding",
                self.unmeasured
            );
        }
        if self.unresolved > 0 {
            println!(
                "  {:>7}  name an item this build has no block for",
                self.unresolved
            );
        }

        let items = self.placing.len();
        let wrong = self.placing.values().filter(|w| **w).count();
        println!();
        println!(
            "  {wrong} of the {items} items that placed anything come out wrong in at least \
             one situation"
        );

        if self.disagreements.is_empty() {
            return;
        }
        println!();
        println!("what is still wrong, one line per item:");
        let mut rows: Vec<(&String, &(String, String, u64))> = self.disagreements.iter().collect();
        rows.sort_by_key(|(item, (_, _, count))| (std::cmp::Reverse(*count), (*item).clone()));
        let shown = if verbose {
            rows.len()
        } else {
            rows.len().min(25)
        };
        for (item, (dust, vanilla, count)) in rows.iter().take(shown) {
            println!("  {count:>4}x {item}");
            println!("         dust {dust}");
            println!("         them {vanilla}");
        }
        if shown < rows.len() {
            println!(
                "  … and {} more items; pass --verbose for all of them",
                rows.len() - shown
            );
        }
    }
}

fn percent(part: u64, whole: u64) -> String {
    if whole == 0 {
        return "—".to_owned();
    }
    format!("{:.1}%", (part as f64 / whole as f64) * 100.0)
}

/// The state Dust would put down for an item, in the answers file's spelling.
///
/// `None` when the item is one this build has no block for, which is a version
/// skew between the answers and the build rather than a disagreement.
fn dust_state(item: &str, items: &Option<ItemBlocks>) -> Option<String> {
    let item = Item::from_name(item)?;
    let block = match items {
        // With the item table, the block is Minecraft's own answer to "what
        // does this item place".
        Some(table) => table.places(item)?,
        // Without it, the name — which is right about 909 of the 925 placing
        // items and wrong about sixteen, exactly as decision record 0008 says.
        // Reported rather than silently assumed: `measure` says which it used.
        None => Block::from_name(item.name())?,
    };
    Some(render(block))
}

/// A block's default state, in the answers file's spelling.
fn render(block: Block) -> String {
    let state = block.default_state();
    let mut properties: Vec<String> = state
        .properties()
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect();
    // Sorted by name, which is what the measuring tool does. The state's own
    // order is the property order of the block table and means nothing to a
    // reader; agreeing on *an* order is what lets two strings be compared.
    properties.sort();
    if properties.is_empty() {
        block.name().to_owned()
    } else {
        format!("{}[{}]", block.name(), properties.join(","))
    }
}

/// Read the item-to-block table, from the given path or the extract cache.
fn load_items(given: Option<&Path>) -> Result<Option<ItemBlocks>, String> {
    let path = match given {
        Some(path) => path.to_path_buf(),
        None => {
            let Some(found) = default_items_table() else {
                println!(
                    "note: no item-to-block table found, so an item's block is taken from its \
                     name — right about 909 of the 925 placing items and wrong about sixteen. \
                     Pass --items to use the real one."
                );
                return Ok(None);
            };
            found
        }
    };
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let table = ItemBlocks::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    println!(
        "items: {} placements from {}",
        table.placing(),
        path.display()
    );
    Ok(Some(table))
}

/// The newest `items.tsv` the extractor has written, if there is one.
fn default_items_table() -> Option<PathBuf> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .join(".dust-extract");
    let mut found: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("items.tsv"))
        .filter(|path| path.is_file())
        .collect();
    found.sort();
    found.pop()
}

/// Read the answers file.
///
/// Rows the reader does not understand are an error rather than a skip: a file
/// half of which was silently dropped would report a score for the other half
/// and call it the score.
fn parse_answers(text: &str) -> Result<Vec<Answer>, String> {
    let mut out = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let at = index + 1;
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 6 {
            return Err(format!(
                "line {at}: {} field(s) where an answer has at least six",
                fields.len()
            ));
        }
        let number = |what: &str, text: &str| -> Result<i32, String> {
            text.parse()
                .map_err(|_| format!("line {at}: {what} is {text:?}, which is not a number"))
        };
        let result = match fields[5] {
            "REFUSED" => Outcome::Refused,
            other if other.starts_with("ARENA") => Outcome::Unmeasured,
            state => Outcome::Placed(state.to_owned()),
        };
        out.push(Answer {
            item: fields[0].to_owned(),
            face: u8::try_from(number("face", fields[1])?)
                .map_err(|_| format!("line {at}: face {} is not one of the six", fields[1]))?,
            yaw: number("yaw", fields[2])?,
            pitch: number("pitch", fields[3])?,
            cursor_y: fields[4].to_owned(),
            result,
        });
    }
    Ok(out)
}

/// Parse this verb's arguments.
pub fn parse(args: &[String]) -> Result<Options, String> {
    let mut answers = None;
    let mut items = None;
    let mut verbose = false;
    let mut seen: Vec<(&'static str, String)> = Vec::new();
    let mut at = 0;
    while at < args.len() {
        match args[at].as_str() {
            "--answers" => {
                at = super::take_value(&mut seen, "--answers", args, at + 1)?;
                answers = Some(PathBuf::from(&seen.last().expect("just stored").1));
            }
            "--items" => {
                at = super::take_value(&mut seen, "--items", args, at + 1)?;
                items = Some(PathBuf::from(&seen.last().expect("just stored").1));
            }
            "--verbose" => {
                verbose = true;
                at += 1;
            }
            other => return Err(format!("placement does not take {other:?}")),
        }
    }
    Ok(Options {
        answers: answers.ok_or(
            "placement needs --answers <file>, written by tools/bot/placement.js; \
             see tools/bot/README.md",
        )?,
        items,
        verbose,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_state_renders_the_way_the_measuring_tool_writes_one() {
        // The two sides meet on this string and nothing checks it but this
        // test. Namespaced, properties in name order, comma separated, square
        // brackets — and no brackets at all for a block that has no
        // properties, because `stone[]` is a different string from `stone`.
        let stone = Block::from_name("minecraft:stone").expect("this build has stone");
        assert_eq!(render(stone), "minecraft:stone");

        let stairs = Block::from_name("minecraft:oak_stairs").expect("this build has stairs");
        let rendered = render(stairs);
        assert!(rendered.starts_with("minecraft:oak_stairs["), "{rendered}");
        assert!(rendered.ends_with(']'), "{rendered}");
        let inside = rendered
            .trim_start_matches("minecraft:oak_stairs[")
            .trim_end_matches(']');
        let names: Vec<&str> = inside
            .split(',')
            .map(|kv| kv.split('=').next().unwrap())
            .collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "properties are in name order: {rendered}");
    }

    #[test]
    fn the_three_outcomes_are_told_apart() {
        let text = "\
# item\tface\tyaw\tpitch\tcursor_y\tresult\tsurvived
minecraft:stone\t1\t0\t0\t0.25\tminecraft:stone\tstood
minecraft:torch\t0\t0\t0\t0.25\tREFUSED\t-
minecraft:torch\t4\t0\t90\t0.25\tARENA DID NOT SETTLE\t-
";
        let answers = parse_answers(text).expect("three well-formed rows");
        assert_eq!(answers.len(), 3);
        assert_eq!(
            answers[0].result,
            Outcome::Placed("minecraft:stone".to_owned())
        );
        assert_eq!(answers[1].result, Outcome::Refused);
        // Not a finding about either server, and told apart from a refusal so
        // that it cannot quietly become one.
        assert_eq!(answers[2].result, Outcome::Unmeasured);
    }

    #[test]
    fn a_row_the_reader_does_not_understand_stops_the_run() {
        // Skipping it would report a score for whatever was left and call it
        // the score.
        let text = "# item\tface\nminecraft:stone\t1\n";
        assert!(parse_answers(text).is_err());
    }

    #[test]
    fn a_run_with_no_item_table_falls_back_to_the_name() {
        // And is right about stone, which is one of the 909 the fallback gets
        // right. The sixteen it does not are why `--items` exists.
        assert_eq!(
            dust_state("minecraft:stone", &None),
            Some("minecraft:stone".to_owned())
        );
        assert_eq!(dust_state("minecraft:diamond_sword", &None), None);
    }
}
