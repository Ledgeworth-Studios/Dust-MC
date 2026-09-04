//! Score what Dust does about a changed neighbourhood against what a real
//! server did.
//!
//! `tools/bot/updates.js` stands a block in a shell of six, takes one of the
//! six away, and writes down whether the block was still there afterwards.
//! This reads those answers, asks [`dust_sim::updates`] the same question of
//! the operator's own constants table, and counts.
//!
//! # Why this is a real differential and not a restatement
//!
//! The standing warning is that a differential cannot catch a rule that is
//! wrong on both sides, and it bites hardest where the reference and the
//! subject share a source. These two do not. Dust's answer comes from
//! `dust-constants.tsv`, whose support columns the block oracle produced by
//! calling Minecraft's `canSurvive` **through reflection, with no world**. The
//! survey's answer comes from a running server pushing block-change packets
//! over a socket. A rule that is wrong in the oracle's proxy — a `canSurvive`
//! that read something the proxy answered with a blank — is wrong in exactly
//! one of those two places, which is what makes disagreement mean something.
//!
//! # Two shells, and why one is not a measurement
//!
//! A dandelion in a shell of **stone** breaks when *any* of its six
//! neighbours is taken away, because a dandelion wants dirt and stone is not
//! dirt: the shell itself is what killed it, and every one of its six rows is
//! about the arena rather than about a support rule. The survey therefore runs
//! twice, with `stone` and with `dirt`, and this scores the union. A block that
//! stood in neither shell contributes nothing and is counted apart.
//!
//! # What is scored, and what is deliberately not
//!
//! Scored: **did the block go away**. Three outcomes come off the wire —
//! `stayed`, `broke`, `changed` — and `changed` is a cell that came back
//! holding a different *state* of the same block, which is `dust_sim`'s
//! placement rules and not this module's. Counting a pressure plate's
//! `powered` as a break was the first version of this and it was four rows
//! wrong before the survey grew the third outcome.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use dust_registry::{Block, BlockConstants, BlockState};
use dust_sim::placement::{Around, Face};
use dust_sim::updates::Rules;

#[derive(Debug)]
pub struct Options {
    /// The survey files. Repeatable, because the survey is run once per shell
    /// and both halves are one measurement.
    pub answers: Vec<PathBuf>,
    pub tables: Option<PathBuf>,
    /// Score every row with the support columns withheld, which is the
    /// negative control: with no columns there are no rules, nothing ever
    /// breaks, and every row Minecraft broke has to go red. A scorer that
    /// still agrees is not reading what it says it reads.
    pub without_support: bool,
    pub verbose: bool,
}

/// One row of the survey.
#[derive(Debug, Clone)]
struct Row {
    block: String,
    face: Face,
    shell: String,
    /// The state that really stood in the cell, read back off the wire.
    stood: String,
    /// The six cells as they really were, before the removal.
    before: String,
    /// Whether Minecraft took the block away.
    broke: bool,
}

pub fn run(options: &Options) -> std::process::ExitCode {
    let mut rows = Vec::new();
    let mut not_placed = 0usize;
    let mut skipped = 0usize;
    for path in &options.answers {
        match read(path) {
            Ok((read, missing, unusable)) => {
                rows.extend(read);
                not_placed += missing;
                skipped += unusable;
            }
            Err(why) => {
                eprintln!("{why}");
                return std::process::ExitCode::from(2);
            }
        }
    }
    if rows.is_empty() {
        eprintln!("no scoreable rows; every one of them said the block was never placed");
        return std::process::ExitCode::from(2);
    }

    let root = match options.tables.clone().or_else(default_tables) {
        Some(root) => root,
        None => {
            eprintln!(
                "harness updates needs a [data] path holding dust-constants.tsv; \
                 pass --tables <path>"
            );
            return std::process::ExitCode::from(2);
        }
    };
    let text = match std::fs::read_to_string(root.join("dust-constants.tsv")) {
        Ok(text) => text,
        Err(e) => {
            eprintln!("could not read {}/dust-constants.tsv: {e}", root.display());
            return std::process::ExitCode::from(2);
        }
    };
    let text = if options.without_support {
        withhold_support(&text)
    } else {
        text
    };
    let constants = match BlockConstants::parse(&text) {
        Ok(constants) => constants,
        Err(e) => {
            eprintln!("{}/dust-constants.tsv: {e}", root.display());
            return std::process::ExitCode::from(2);
        }
    };
    let rules = Rules::from_constants(&constants);

    let mut agreed = 0usize;
    let mut kept_wrongly = 0usize;
    let mut broke_wrongly = 0usize;
    let mut unreadable = 0usize;
    let mut arena = 0usize;
    // Which blocks disagree, and about which faces.
    let mut worklist: BTreeMap<String, Vec<String>> = BTreeMap::new();

    // A shell that refused the block on all six sides did not measure a
    // support rule; it measured the arena. `/setblock` places a state without
    // asking `canSurvive`, so a dandelion can be stood on stone and then dies
    // at the first update whichever side moved. Six of six is the signature,
    // because no state in 1.21.1 needs all six of its neighbours, and the
    // block that made this visible is a flower that scored five disagreements
    // in stone and six agreements in dirt.
    let refusing: std::collections::BTreeSet<(String, String)> = {
        let mut broke: BTreeMap<(String, String), (usize, usize)> = BTreeMap::new();
        for row in &rows {
            let entry = broke
                .entry((row.block.clone(), row.shell.clone()))
                .or_default();
            entry.0 += 1;
            entry.1 += usize::from(row.broke);
        }
        broke
            .into_iter()
            .filter(|(_, (seen, broke))| *seen == Face::ALL.len() && seen == broke)
            .map(|(key, _)| key)
            .collect()
    };

    for row in &rows {
        if refusing.contains(&(row.block.clone(), row.shell.clone())) {
            arena += 1;
            continue;
        }
        let Some(state) = parse_state(&row.stood) else {
            unreadable += 1;
            continue;
        };
        let Some(around) = parse_around(&row.before) else {
            unreadable += 1;
            continue;
        };
        // **The neighbourhood the rule is asked about is the one after the
        // removal, not before it.** `state_before` is what the six cells
        // really held when the block stood there, read back off the wire; the
        // survey then emptied one of them, and scoring against the shell as it
        // was is asking whether the block could stay somewhere it was not.
        // Every one of the first run's sixty disagreements was that, including
        // a torch whose floor Minecraft had just taken away.
        let around = around.with(row.face, air());
        // No table, no rules, and every block stays — which is what the
        // negative control is measuring.
        //
        // `Stay` and not `survives`, because the question the survey asked is
        // "is the cell empty now" and a block that fell out of it is as empty
        // as one that broke. Gravel is the row that says so.
        let survives = rules
            .as_ref()
            .is_none_or(|r| r.reaction(state, around) == dust_sim::updates::Reaction::Stay);
        if survives != row.broke {
            agreed += 1;
            continue;
        }
        if row.broke {
            kept_wrongly += 1;
        } else {
            broke_wrongly += 1;
        }
        worklist.entry(row.block.clone()).or_default().push(format!(
            "{:>5} in {:<5} Minecraft {}, Dust {}",
            face_name(row.face),
            row.shell,
            if row.broke { "broke it" } else { "kept it " },
            if survives { "keeps it" } else { "breaks it" },
        ));
    }

    let scored = agreed + kept_wrongly + broke_wrongly;
    println!("harness updates");
    println!("  {scored} row(s) scored, out of {} read", rows.len());
    println!("  {agreed} agree with the server");
    println!("  {kept_wrongly} Minecraft broke and Dust keeps");
    println!("  {broke_wrongly} Minecraft kept and Dust breaks");
    if unreadable > 0 {
        println!("  {unreadable} row(s) named a state or a neighbourhood this build has not");
    }
    if not_placed > 0 {
        println!("  {not_placed} row(s) never stood the block up and are not scored");
    }
    if skipped > 0 {
        println!("  {skipped} row(s) the server reshaped rather than broke, which is D14's rule");
    }
    if arena > 0 {
        println!(
            "  {arena} row(s) in a shell that refused the block on all six sides, \
             which is the arena and not a rule"
        );
    }
    if rules.is_none() {
        println!(
            "  the constants table has no support columns, so no rule ran; \
             every disagreement above is one Minecraft broke"
        );
    }
    if !worklist.is_empty() {
        println!();
        println!("what disagrees, by block:");
        for (block, entries) in &worklist {
            println!("  {block}");
            if options.verbose {
                for entry in entries {
                    println!("      {entry}");
                }
            } else {
                println!("      {} of 12 face-and-shell row(s)", entries.len());
            }
        }
    }
    std::process::ExitCode::SUCCESS
}

/// Cut the support columns out of a constants table.
///
/// The negative control, and it is a column cut rather than a flag in the
/// reader for the reason decision record 0014's was: withholding the *input*
/// exercises the same code path an operator with an older table takes, and a
/// flag in the reader would exercise a path nobody runs.
fn withhold_support(text: &str) -> String {
    let mut lines = text.lines();
    let Some(header) = lines.next() else {
        return text.to_owned();
    };
    let names: Vec<&str> = header.trim_start_matches("# ").split('\t').collect();
    let keep: Vec<usize> = names
        .iter()
        .enumerate()
        .filter(|(_, name)| {
            **name != dust_sim::updates::SURVIVES_ALONE
                && !dust_sim::updates::SUPPORT.contains(name)
        })
        .map(|(at, _)| at)
        .collect();
    let pick = |line: &str, prefix: &str| {
        let fields: Vec<&str> = line.trim_start_matches(prefix).split('\t').collect();
        let picked: Vec<&str> = keep
            .iter()
            .filter_map(|at| fields.get(*at).copied())
            .collect();
        format!("{prefix}{}", picked.join("\t"))
    };
    let mut out = pick(header, "# ");
    for line in lines {
        out.push('\n');
        out.push_str(&pick(line, ""));
    }
    out.push('\n');
    out
}

/// A state from `minecraft:name[a=b,c=d]`.
fn parse_state(text: &str) -> Option<BlockState> {
    let (name, rest) = match text.split_once('[') {
        Some((name, rest)) => (name, rest.strip_suffix(']')?),
        None => (text, ""),
    };
    let mut state = Block::from_name(name)?.default_state();
    for property in rest.split(',').filter(|p| !p.is_empty()) {
        let (name, value) = property.split_once('=')?;
        state = state.with(name, value)?;
    }
    Some(state)
}

/// A neighbourhood from `down=minecraft:stone;up=minecraft:air;…`.
fn parse_around(text: &str) -> Option<Around> {
    let mut around = Around::empty();
    for entry in text.split(';').filter(|e| !e.is_empty()) {
        let (side, state) = entry.split_once('=')?;
        let face = face_of(side)?;
        around = around.with(face, parse_state(state)?);
    }
    Some(around)
}

/// Air, which is what the survey put in the cell it emptied.
fn air() -> BlockState {
    Block::from_name("minecraft:air")
        .expect("every version of the game has air")
        .default_state()
}

fn face_of(name: &str) -> Option<Face> {
    Some(match name {
        "down" => Face::Down,
        "up" => Face::Up,
        "north" => Face::North,
        "south" => Face::South,
        "west" => Face::West,
        "east" => Face::East,
        _ => return None,
    })
}

fn face_name(face: Face) -> &'static str {
    match face {
        Face::Down => "down",
        Face::Up => "up",
        Face::North => "north",
        Face::South => "south",
        Face::West => "west",
        Face::East => "east",
    }
}

/// Read one survey file: the scoreable rows, how many never stood the block
/// up, and how many the server reshaped rather than broke.
fn read(path: &Path) -> Result<(Vec<Row>, usize, usize), String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let mut rows = Vec::new();
    let mut not_placed = 0;
    let mut reshaped = 0;
    for (number, line) in text.lines().enumerate() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 8 {
            return Err(format!(
                "{}:{}: expected 8 columns and found {}; is this a `node updates.js` \
                 support survey rather than a `--fall` one?",
                path.display(),
                number + 1,
                fields.len()
            ));
        }
        match fields[6] {
            "not_placed" => {
                not_placed += 1;
                continue;
            }
            "changed" => {
                reshaped += 1;
                continue;
            }
            _ => {}
        }
        let Some(face) = face_of(fields[1]) else {
            return Err(format!(
                "{}:{}: `{}` is not one of the six sides",
                path.display(),
                number + 1,
                fields[1]
            ));
        };
        rows.push(Row {
            block: fields[0].to_owned(),
            face,
            shell: fields[2].to_owned(),
            stood: fields[3].to_owned(),
            before: fields[4].to_owned(),
            broke: fields[6] == "broke",
        });
    }
    Ok((rows, not_placed, reshaped))
}

fn default_tables() -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.dust-extract/data-1.21.1/data");
    path.is_dir().then_some(path)
}

pub fn parse(args: &[String]) -> Result<Options, String> {
    let mut answers = Vec::new();
    let mut tables = None;
    let mut without_support = false;
    let mut verbose = false;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        index += 1;
        match flag {
            "--verbose" => verbose = true,
            "--without-support" => without_support = true,
            "--answers" => {
                let value = args
                    .get(index)
                    .ok_or_else(|| format!("{flag} needs a value"))?;
                index += 1;
                answers.push(PathBuf::from(value));
            }
            "--tables" => {
                let value = args
                    .get(index)
                    .ok_or_else(|| format!("{flag} needs a value"))?;
                index += 1;
                if tables.is_some() {
                    return Err("--tables given twice".to_owned());
                }
                tables = Some(PathBuf::from(value));
            }
            other => return Err(format!("unknown option `{other}` for harness updates")),
        }
    }
    if answers.is_empty() {
        return Err(
            "harness updates needs --answers <file>, and takes it more than once".to_owned(),
        );
    }
    Ok(Options {
        answers,
        tables,
        without_support,
        verbose,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_state_and_a_neighbourhood_read_back_out_of_the_surveys_spelling() {
        let state = parse_state("minecraft:wall_torch[facing=north]").expect("a real state");
        assert_eq!(state.block().name(), "minecraft:wall_torch");
        let around = parse_around(
            "down=minecraft:stone;up=minecraft:air;north=minecraft:stone;\
             south=minecraft:air;west=minecraft:stone;east=minecraft:stone",
        )
        .expect("a real neighbourhood");
        assert_eq!(around.at(Face::Down).block().name(), "minecraft:stone");
        assert_eq!(around.at(Face::South).block().name(), "minecraft:air");
    }

    #[test]
    fn withholding_the_support_columns_leaves_a_table_that_still_parses() {
        let mut header = String::from("# state_id\topacity\temission\tocclude\treplaceable");
        header.push_str("\tSURVIVES_ALONE");
        for column in dust_sim::updates::SUPPORT {
            header.push('\t');
            header.push_str(column);
        }
        header.push_str("\tfalls\n");
        let mut text = header;
        for id in 0..dust_registry::STATE_COUNT {
            text.push_str(&format!("{id}\t0\t0\t0\t0\t1\t0\t0\t0\t0\t0\t0\t0\n"));
        }
        let cut = withhold_support(&text);
        assert!(!cut.contains("SURVIVES_ALONE"));
        assert!(!cut.contains("SUPPORT_DOWN"));
        assert!(cut.contains("falls"));
        let constants = BlockConstants::parse(&cut).expect("the cut table still parses");
        assert!(
            Rules::from_constants(&constants).is_none(),
            "a table with no support columns builds no rules, which is the control"
        );
    }

    #[test]
    fn a_fall_survey_handed_to_the_support_scorer_says_so() {
        let file = std::env::temp_dir().join("dust-updates-scorer-shape.tsv");
        std::fs::write(&file, "# block\theight\n sand\t8\n").expect("a temp file");
        let why = read(&file).expect_err("a fall survey is not a support survey");
        assert!(why.contains("support survey"), "{why}");
        let _ = std::fs::remove_file(&file);
    }
}
