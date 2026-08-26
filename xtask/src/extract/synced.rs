//! The registries a server sends to a client during configuration.
//!
//! # Which eleven, and why these
//!
//! Since 1.20.5 a joining client is told the contents of every *datapack*
//! registry before it enters the world, because a datapack may have changed
//! them and the client has no other way to find out. On 1.21.1 there are
//! eleven, and the list is not a matter of taste: it was read off the wire of a
//! running 1.21.1 server, in the order that server sent them, and the entry
//! counts below were checked against what it sent.
//!
//! ```text
//! minecraft:worldgen/biome     64      minecraft:dimension_type      4
//! minecraft:chat_type           7      minecraft:damage_type        47
//! minecraft:trim_pattern       18      minecraft:banner_pattern     43
//! minecraft:trim_material      10      minecraft:enchantment        42
//! minecraft:wolf_variant        9      minecraft:jukebox_song       19
//! minecraft:painting_variant   50
//! ```
//!
//! These are not in `reports/registries.json`, which is the *code* registries —
//! the ones with protocol ids compiled into the game. A datapack registry has
//! no protocol id at all: its entries are addressed by name, and their order in
//! the sync packet is the order the server chooses. So they are read where they
//! actually live, which is as files in the data pack the jar ships.
//!
//! # Why only the names are extracted, and not the contents
//!
//! The sync packet carries, per entry, a name and an optional NBT blob. The
//! blob is **absent** when the client already has the data — which it signals
//! by acknowledging the server's known packs, and which every vanilla client
//! does for `minecraft:core`. Captured from a real server: all eleven
//! registries, every entry, `has_data = false`.
//!
//! That is worth stating plainly because it is the difference between this
//! extraction and a much larger one. A server whose datapacks are vanilla's
//! never has to send a biome's contents; it has to send the *names*, in an
//! order, so the client can build the same id-to-name mapping the server uses.
//! The day Dust supports a datapack that adds or changes an entry, that entry
//! needs its NBT and this file grows — and the day it does, `has_data` per
//! entry is the field that already exists to carry it.
//!
//! # What is checked rather than assumed
//!
//! Every directory must exist and be non-empty. A registry that silently
//! extracted zero entries would produce a server that sent an empty registry,
//! and a client told a registry is empty does not fall back to its own copy —
//! it disconnects, or renders a world with no biomes. Refusing loudly at
//! extraction time is the only place anybody is watching.

use std::collections::BTreeSet;
use std::path::Path;

/// One datapack registry, as sent during configuration.
#[derive(Debug)]
pub struct SyncedRegistry {
    /// The registry's namespaced id, e.g. `minecraft:worldgen/biome`.
    pub name: String,
    /// Entry names, namespaced, in the order they are sent.
    pub entries: Vec<String>,
}

/// The eleven, with the directory each is read from and the count a real
/// 1.21.1 server sent.
///
/// The count is a fixture, not a computation: it came off the wire, so a
/// mismatch means the data tree and the server disagree and one of them is not
/// 1.21.1. Extraction refuses rather than emitting a table that would make a
/// client and a server disagree about which biome id 37 is.
const SYNCED: &[(&str, &str, usize)] = &[
    ("minecraft:worldgen/biome", "worldgen/biome", 64),
    ("minecraft:chat_type", "chat_type", 7),
    ("minecraft:trim_pattern", "trim_pattern", 18),
    ("minecraft:trim_material", "trim_material", 10),
    ("minecraft:wolf_variant", "wolf_variant", 9),
    ("minecraft:painting_variant", "painting_variant", 50),
    ("minecraft:dimension_type", "dimension_type", 4),
    ("minecraft:damage_type", "damage_type", 47),
    ("minecraft:banner_pattern", "banner_pattern", 43),
    ("minecraft:enchantment", "enchantment", 42),
    ("minecraft:jukebox_song", "jukebox_song", 19),
];

/// Read all eleven out of the `--server` data tree.
pub fn parse(data_root: &Path) -> Result<Vec<SyncedRegistry>, String> {
    let namespace_root = data_root.join("data/minecraft");
    if !namespace_root.is_dir() {
        return Err(format!(
            "{} is not a data pack tree: no data/minecraft directory",
            data_root.display()
        ));
    }

    let mut out = Vec::with_capacity(SYNCED.len());
    for (name, directory, expected) in SYNCED {
        let path = namespace_root.join(directory);
        let entries = read_entry_names(&path, "minecraft")?;
        if entries.len() != *expected {
            return Err(format!(
                "{name} has {} entries in {} and a real 1.21.1 server sends {expected}; \
                 the tree and the protocol disagree, so one of them is not 1.21.1",
                entries.len(),
                path.display()
            ));
        }
        out.push(SyncedRegistry {
            name: (*name).to_owned(),
            entries,
        });
    }
    Ok(out)
}

/// Every `*.json` in `directory`, as namespaced ids, sorted by name.
///
/// Sorted rather than left in directory order, and this is load-bearing: the
/// order entries appear in the sync packet **is** their id order for the rest
/// of the session, and a directory listing's order is the file system's
/// business. A table whose order changed between two runs of the extractor on
/// two machines would produce two servers that disagree about which biome is
/// which, with nothing to see in the diff but a reordering.
///
/// Nested directories are walked, because the entry name includes the path
/// below the registry root — that is how `minecraft:worldgen/biome` entries
/// stay flat while the registry itself is nested.
fn read_entry_names(directory: &Path, namespace: &str) -> Result<Vec<String>, String> {
    let mut names = BTreeSet::new();
    walk(directory, directory, namespace, &mut names)?;
    if names.is_empty() {
        return Err(format!(
            "{} holds no .json entries; a registry that syncs as empty makes a \
             client disagree with the server about every id in it",
            directory.display()
        ));
    }
    Ok(names.into_iter().collect())
}

fn walk(
    root: &Path,
    directory: &Path,
    namespace: &str,
    out: &mut BTreeSet<String>,
) -> Result<(), String> {
    let listing = std::fs::read_dir(directory)
        .map_err(|e| format!("could not read {}: {e}", directory.display()))?;
    for entry in listing {
        let entry = entry.map_err(|e| format!("could not read {}: {e}", directory.display()))?;
        let path = entry.path();
        if path.is_dir() {
            walk(root, &path, namespace, out)?;
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| format!("{} is not under {}", path.display(), root.display()))?;
        let stem = relative
            .to_str()
            .ok_or_else(|| format!("{} is not a UTF-8 path", relative.display()))?
            .trim_end_matches(".json")
            // Windows writes separators the other way and a generated table
            // must not depend on which machine ran the extractor.
            .replace('\\', "/");
        out.insert(format!("{namespace}:{stem}"));
    }
    Ok(())
}
