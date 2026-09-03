//! The loot tables beside the operator's data, and what reading them found.
//!
//! `<[data] path>/<namespace>/loot_table/blocks/<block>.json` — the files
//! Minecraft's own `--server` data generator writes, in the directory decision
//! record 0007 already asks the operator to produce. **No new file, no new
//! extraction step and nothing committed**: a server that can already sync its
//! registries can already say what a broken block yields.
//!
//! That is what separates this from `dust-constants.tsv`. Opacity, emission
//! and the sound a block makes are Java constants and needed an oracle to get
//! at; loot is a data pack, sitting in a directory the operator already has,
//! and asking them to run an extractor for a file they are already holding
//! would be asking twice.
//!
//! # Every namespace, in the order a data pack overrides
//!
//! A pack that ships `mypack/loot_table/blocks/stone.json` is describing its
//! own `mypack:stone`, not overriding `minecraft:stone`. The namespace is part
//! of the identity, so this walks each namespace directory and asks the block
//! registry for `<namespace>:<stem>` — which is why a namespace this build has
//! never heard of contributes nothing rather than shadowing something.
//!
//! # What it does not do, and says so
//!
//! Minecraft's block-to-table relation is a **code** constant
//! (`Block.getLootTable`), and 982 of the 1,060 blocks on 1.21.1 point at a
//! table of their own name while 78 do not. Some of those 78 genuinely drop
//! nothing — bedrock, air, the command blocks — and about sixty point at
//! another block's table: `minecraft:oak_wall_sign` drops an `oak_sign` out of
//! `blocks/oak_sign.json`. **This reader has no way to know which is which**,
//! so it reports the count and drops nothing for all 78, and the fix is one
//! more column from the oracle rather than a rule about names.

use std::path::{Path, PathBuf};

use dust_sim::drops::Tables;

/// Where the block tables live inside one namespace.
const UNDER: &str = "loot_table/blocks";

/// What reading the tables found, for the line the server prints at boot.
#[derive(Debug, Default)]
pub struct Report {
    /// Namespaces that had a `loot_table/blocks` directory at all.
    pub namespaces: Vec<String>,
    /// Files offered.
    pub files: u32,
    /// Blocks that now have a table.
    pub compiled: usize,
    /// Files named after nothing in the block registry.
    pub unnamed: u32,
    /// Files that would not compile, with why, capped so a broken pack cannot
    /// fill the log.
    pub errors: Vec<String>,
    /// Entries refused across every table read.
    pub refused_entries: u32,
    /// Functions wanting a block entity across every table read.
    pub needs_block_entity: u32,
}

/// How many failing files are named before the rest are counted.
const NAMED_ERRORS: usize = 5;

/// Read every block loot table beside a data directory.
///
/// `root` is `[data] path` — the directory holding `minecraft/`.
///
/// Unlike [`super::constants::beside`], a file that will not compile does
/// **not** stop the server. The two are different in the way that matters:
/// a wrong constants table makes every block in the world slightly wrong and
/// there is no way to tell from inside the game, whereas a loot table that
/// will not parse makes exactly one block drop nothing, is named in the boot
/// log, and a server that refused to start over one file in a data pack would
/// be a server an operator cannot run.
pub fn beside(root: impl AsRef<Path>) -> (Tables, Report) {
    let root = root.as_ref();
    let mut tables = Tables::default();
    let mut report = Report::default();

    let Ok(namespaces) = std::fs::read_dir(root) else {
        return (tables, report);
    };
    let mut roots: Vec<(String, PathBuf)> = Vec::new();
    for entry in namespaces.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let blocks = entry.path().join(UNDER);
        if blocks.is_dir() {
            roots.push((name, blocks));
        }
    }
    // Sorted so two machines with the same data print the same line.
    roots.sort();

    for (namespace, blocks) in roots {
        let Ok(files) = std::fs::read_dir(&blocks) else {
            continue;
        };
        let mut names: Vec<PathBuf> = files
            .flatten()
            .map(|file| file.path())
            .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("json"))
            .collect();
        names.sort();
        for path in names {
            report.files += 1;
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                report.unnamed += 1;
                tables.refuse();
                continue;
            };
            let Some(block) = dust_sim::drops::block_of_file(&namespace, stem) else {
                report.unnamed += 1;
                tables.refuse();
                continue;
            };
            let Ok(text) = std::fs::read_to_string(&path) else {
                report
                    .errors
                    .push(format!("{}: unreadable", path.display()));
                tables.refuse();
                continue;
            };
            if let Err(why) = tables.insert(block, &text) {
                report.errors.push(format!("{namespace}:{stem}: {why}"));
            }
        }
        report.namespaces.push(namespace);
    }

    report.compiled = tables.len();
    report.refused_entries = tables.refused_entries();
    report.needs_block_entity = tables.needs_block_entity();
    (tables, report)
}

impl Report {
    /// The one line a boot log gets when tables were found.
    pub fn summary(&self) -> String {
        let mut line = format!(
            "{} block loot table(s) from {} namespace(s), {} block(s) covered",
            self.files,
            self.namespaces.len(),
            self.compiled
        );
        if self.unnamed > 0 {
            line.push_str(&format!(", {} named after no block here", self.unnamed));
        }
        if self.refused_entries > 0 {
            line.push_str(&format!(
                ", {} entr(y/ies) this build cannot read",
                self.refused_entries
            ));
        }
        if self.needs_block_entity > 0 {
            line.push_str(&format!(
                ", {} function(s) wanting a block entity",
                self.needs_block_entity
            ));
        }
        if !self.errors.is_empty() {
            let named: Vec<&str> = self
                .errors
                .iter()
                .take(NAMED_ERRORS)
                .map(String::as_str)
                .collect();
            line.push_str(&format!(
                ". {} file(s) would not compile: {}",
                self.errors.len(),
                named.join("; ")
            ));
            if self.errors.len() > NAMED_ERRORS {
                line.push_str(" and more");
            }
        }
        line
    }
}
