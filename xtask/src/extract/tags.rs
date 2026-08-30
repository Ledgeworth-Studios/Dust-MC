//! The vanilla tag directories: the baseline layer a datapack overlays.
//!
//! Tags are how Minecraft groups things — `minecraft:mineable/pickaxe`,
//! `minecraft:logs`, `minecraft:fall_damage_immune` — and they reach the server
//! twice on this project: once as this extracted baseline from vanilla's own
//! data pack, and later as whatever the world's datapacks write over it. What
//! the registry sync and the block/loot logic need from the baseline side is
//! membership: which ids a vanilla tag names, and which references it makes to
//! other tags.
//!
//! # The thirteen directories, and why it is exactly thirteen
//!
//! A running 1.21.1 server sends `update_tags` for thirteen registries, and it
//! was counted off the wire rather than off a list: 13 registries, 514 tags,
//! 6,362 memberships, 25,200 bytes consumed exactly. Those thirteen are what
//! is taken here.
//!
//! It was five for a while, and the five were the registries Dust held tables
//! for — a tag directory over an unknown registry cannot be checked against
//! anything, and rows nobody checked are rows that agree with themselves. The
//! other eight became checkable in two steps: `point_of_interest_type`,
//! `cat_variant` and `instrument` are ordinary code registries and were in the
//! registry report the whole time, and `worldgen/biome`, `damage_type`,
//! `banner_pattern`, `painting_variant` and `enchantment` are *datapack*
//! registries whose names arrived with the `synced` extraction. So every one
//! of the thirteen is still checked against a table extracted separately, and
//! [`TAKEN`] still refuses a directory it has no table for rather than
//! emitting unchecked rows.
//!
//! **Why all thirteen and not the easy ones.** A partial tag set is worse than
//! none. A client told that `minecraft:mineable/pickaxe` holds eleven blocks
//! believes the other nine hundred are not mineable; a client told nothing
//! falls back to its own copy. That rule is why none were sent while five were
//! extracted, and it is why the answer had to be all of them.
//!
//! # Every membership is checked, not sampled
//!
//! A plain member (`minecraft:stone`) must exist in the extracted table of its
//! registry — two sources of truth agreeing, verified over all ~3,600 of them,
//! because a wrong name in a tag is a group that quietly does not hold where a
//! player expects it to. A `#`-prefixed member must name another tag of the
//! *same* registry in this dataset, which is what makes the baseline
//! self-contained: vanilla's tags resolve inside vanilla's data.
//!
//! Two facts about the format were measured rather than assumed, and both are
//! now checks: every plain member is fully namespaced (there are zero bare ids
//! in vanilla's files), and no vanilla file sets `replace` at all — that word
//! belongs to datapacks overlaying this baseline, so a row carrying it stops
//! the extraction instead of smuggling overlay semantics into the layer being
//! overlaid.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::Value;

use super::blocks::Blocks;
use super::registries::Registries;

/// Where a tag directory's members are checked against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The block report — `minecraft:block` alone, which has its own report
    /// and is deliberately absent from the registry one.
    BlockReport,
    /// `reports/registries.json`: a registry with a protocol id compiled into
    /// the game.
    RegistryReport,
    /// The data pack's own directories, read by [`super::synced`]: a datapack
    /// registry has no protocol id and its entries are addressed by name.
    DataPack,
}

/// The tag directories taken, the registry each groups, and where that
/// registry's names come from.
///
/// The order is the order a real 1.21.1 server sent them in, captured off the
/// wire. It is not load-bearing for the client — each registry names itself in
/// the packet — but a table in an order somebody chose is a table two people
/// can disagree about, and this one has an answer.
const TAKEN: &[(&str, &str, Source)] = &[
    ("block", "minecraft:block", Source::BlockReport),
    (
        "entity_type",
        "minecraft:entity_type",
        Source::RegistryReport,
    ),
    (
        "worldgen/biome",
        "minecraft:worldgen/biome",
        Source::DataPack,
    ),
    ("game_event", "minecraft:game_event", Source::RegistryReport),
    ("item", "minecraft:item", Source::RegistryReport),
    (
        "point_of_interest_type",
        "minecraft:point_of_interest_type",
        Source::RegistryReport,
    ),
    ("enchantment", "minecraft:enchantment", Source::DataPack),
    ("fluid", "minecraft:fluid", Source::RegistryReport),
    ("damage_type", "minecraft:damage_type", Source::DataPack),
    (
        "banner_pattern",
        "minecraft:banner_pattern",
        Source::DataPack,
    ),
    (
        "cat_variant",
        "minecraft:cat_variant",
        Source::RegistryReport,
    ),
    ("instrument", "minecraft:instrument", Source::RegistryReport),
    (
        "painting_variant",
        "minecraft:painting_variant",
        Source::DataPack,
    ),
];

fn registry_for(directory: &str) -> Option<&'static str> {
    TAKEN
        .iter()
        .find(|(dir, _, _)| *dir == directory)
        .map(|(_, registry, _)| *registry)
}

/// Every registry taken, with the tag count a real 1.21.1 server sent for it.
///
/// A fixture read off the wire, not a computation over this extraction: a
/// table built from the wrong directory would agree with itself perfectly.
/// The whole packet was 25,200 bytes, 13 registries, 514 tags and 6,362
/// memberships, and every byte of it was consumed by a reader that shares no
/// code with this one.
pub const CAPTURED: &[(&str, usize)] = &[
    ("minecraft:block", 184),
    ("minecraft:entity_type", 34),
    ("minecraft:worldgen/biome", 70),
    ("minecraft:game_event", 5),
    ("minecraft:item", 147),
    ("minecraft:point_of_interest_type", 3),
    ("minecraft:enchantment", 22),
    ("minecraft:fluid", 2),
    ("minecraft:damage_type", 32),
    ("minecraft:banner_pattern", 9),
    ("minecraft:cat_variant", 2),
    ("minecraft:instrument", 3),
    ("minecraft:painting_variant", 1),
];

/// One tag, as the generated table holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    /// Which of the five registries this tag groups.
    pub registry: &'static str,
    /// Namespaced tag id, e.g. `minecraft:mineable/pickaxe`.
    pub id: String,
    /// Members sorted. Plain entries are namespaced ids of `registry`; entries
    /// starting with `#` reference other tags of the same registry.
    pub members: Vec<String>,
}

#[derive(Debug)]
pub struct Tags {
    /// Every tag, sorted by (registry, id).
    pub tags: Vec<Tag>,
    /// Directories seen under `tags/` that were not taken, e.g. `worldgen`.
    /// Reported rather than swallowed.
    pub skipped_directories: BTreeSet<String>,
    /// How many memberships were checked against the extracted tables.
    pub memberships: usize,
    /// How many `#tag` references were resolved inside the dataset.
    pub references: usize,
    /// Vanilla ships two members twice — both spellings of `minecraft:sand`
    /// list `minecraft:suspicious_sand` two times — and a tag is a set, so
    /// duplicates are collapsed rather than committed. Counted here because
    /// "the baseline repeats itself" is a fact worth seeing once.
    pub duplicates_collapsed: usize,
}

/// Read the thirteen tag directories out of every namespace in the tree.
///
/// `synced` is the datapack registries as [`super::synced::parse`] read them.
/// Five of the thirteen group registries that have no protocol id at all, so
/// their member names cannot come from the registry report and have to come
/// from the same place the sync packet's names do — which also means a
/// membership and an id in the sync packet cannot disagree about what exists.
pub fn parse(
    data_root: &Path,
    registries: &Registries,
    blocks: &Blocks,
    synced: &[super::synced::SyncedRegistry],
) -> Result<Tags, String> {
    // Membership lookups, built once, each from the source `TAKEN` names for
    // it. Three sources rather than one because the registries genuinely live
    // in three places, and a lookup that fell back between them would make a
    // typo in one report resolve against another.
    let mut known: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (_, registry_name, source) in TAKEN {
        let names: BTreeSet<&str> = match source {
            Source::BlockReport => blocks.blocks.iter().map(|b| b.name.as_str()).collect(),
            Source::RegistryReport => {
                let Some(registry) = registries
                    .registries
                    .iter()
                    .find(|r| r.name == *registry_name)
                else {
                    return Err(format!("the registry report has no {registry_name}"));
                };
                registry.entries.iter().map(|e| e.name.as_str()).collect()
            }
            Source::DataPack => {
                let Some(registry) = synced.iter().find(|r| r.name == *registry_name) else {
                    return Err(format!(
                        "the datapack registries have no {registry_name}; the `synced` \
                         extraction must run before this one"
                    ));
                };
                registry.entries.iter().map(String::as_str).collect()
            }
        };
        if names.is_empty() {
            return Err(format!(
                "{registry_name} extracted no names to check against"
            ));
        }
        known.insert(*registry_name, names);
    }

    // References are collected while reading and resolved once every tag has
    // been seen: a file earlier in the walk may reference a tag defined later,
    // and forward references are exactly as legal as backward ones.
    let mut pending_references: BTreeMap<(String, String), String> = BTreeMap::new();

    let mut tags: Vec<Tag> = Vec::new();
    let mut skipped_directories: BTreeSet<String> = BTreeSet::new();
    let mut memberships = 0usize;
    let mut duplicates_collapsed = 0usize;

    for namespace in list_namespaces(data_root)? {
        let root = data_root.join(&namespace).join("tags");
        if !root.is_dir() {
            continue;
        }
        // Driven by the table rather than by a directory walk, because
        // `worldgen/biome` is a registry two directories deep and a walk
        // cannot tell that from a registry `worldgen` holding names beginning
        // `biome/`. The same ambiguity `dust_data::registry` resolves by
        // longest prefix; here the list is short enough to be the list.
        // Anything under `tags/` the table does not name is reported below.
        for (directory, registry_name, _) in TAKEN {
            let dir = root.join(directory);
            if !dir.is_dir() {
                continue;
            }
            for path in json_files(&dir)? {
                let relative = path
                    .strip_prefix(&dir)
                    .map_err(|_| format!("{} escaped {}", path.display(), dir.display()))?
                    .with_extension("");
                let text = std::fs::read(&path)
                    .map_err(|e| format!("could not read {}: {e}", path.display()))?;
                let value: Value = serde_json::from_slice(&text)
                    .map_err(|e| format!("could not read {} as JSON: {e}", path.display()))?;
                let Some(object) = value.as_object() else {
                    return Err(format!("{} is not an object", path.display()));
                };
                if let Some(replace) = object.get("replace") {
                    return Err(format!(
                        "{} sets `replace` to {replace}, which vanilla's own tags never do. \\
                         That field belongs to datapacks overlaying this baseline.",
                        path.display()
                    ));
                }

                let Some(Value::Array(values)) = object.get("values") else {
                    return Err(format!("{} has no `values` array", path.display()));
                };
                let mut members = Vec::with_capacity(values.len());
                for value in values {
                    let Some(member) = value.as_str() else {
                        return Err(format!(
                            "{} has a member {value:?}, which is not a string",
                            path.display()
                        ));
                    };
                    if let Some(referenced) = member.strip_prefix('#') {
                        // Vanilla spells references both ways — bare
                        // (`#logs_that_burn`) and namespaced
                        // (`#minecraft:crimson_stems`) — and a bare one is
                        // relative to the file's own namespace.
                        let target = if referenced.contains(':') {
                            referenced.to_owned()
                        } else {
                            format!("{namespace}:{referenced}")
                        };
                        pending_references.insert(
                            ((*registry_name).to_owned(), target.clone()),
                            path.display().to_string(),
                        );
                        memberships += 1;
                        members.push(format!("#{target}"));
                        continue;
                    }
                    if !member.contains(':') {
                        return Err(format!(
                            "{} has the bare member {member:?}. Vanilla always spells its \\
                             tag values namespaced, so this is either a datapack file in \\
                             the wrong tree or a reading that stopped being careful.",
                            path.display()
                        ));
                    }
                    if !known[registry_name].contains(member) {
                        return Err(format!(
                            "{} names {member}, which is not an entry of {registry_name} \\
                             in the extracted tables",
                            path.display()
                        ));
                    }
                    memberships += 1;
                    members.push(member.to_owned());
                }
                // A tag is a set, and vanilla's own files occasionally repeat
                // themselves (both `sand` tags carry `suspicious_sand`
                // twice). Sorted and deduplicated, with the collapse counted.
                let before_dedup = members.len();
                members.sort();
                members.dedup();
                duplicates_collapsed += before_dedup - members.len();

                tags.push(Tag {
                    registry: registry_name,
                    id: format!(
                        "{namespace}:{}",
                        relative.to_string_lossy().replace('\\', "/")
                    ),
                    members,
                });
            }
        }
    }

    // What is under `tags/` that the table does not name. Reported rather
    // than swallowed: a fourteenth registry in a future version should arrive
    // as a line somebody reads, not as tags that quietly go unsent.
    for namespace in list_namespaces(data_root)? {
        let root = data_root.join(&namespace).join("tags");
        if !root.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(&root)
            .map_err(|e| format!("could not read {}: {e}", root.display()))?
        {
            let entry = entry.map_err(|e| format!("could not read {}: {e}", root.display()))?;
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let directory = entry.file_name().to_string_lossy().into_owned();
            if registry_for(&directory).is_some() {
                continue;
            }
            // A directory that is only a *prefix* of a taken one — `worldgen`,
            // which holds `worldgen/biome` — is looked into rather than
            // reported whole, so `worldgen/structure` is still noticed.
            let prefix = format!("{directory}/");
            let nested: Vec<&str> = TAKEN
                .iter()
                .filter_map(|(dir, _, _)| dir.strip_prefix(&prefix))
                .collect();
            if nested.is_empty() {
                skipped_directories.insert(directory);
                continue;
            }
            for inner in std::fs::read_dir(entry.path())
                .map_err(|e| format!("could not read {}: {e}", entry.path().display()))?
            {
                let inner = inner.map_err(|e| format!("could not read a nested tag dir: {e}"))?;
                if !inner.file_type().is_ok_and(|t| t.is_dir()) {
                    continue;
                }
                let name = inner.file_name().to_string_lossy().into_owned();
                if !nested.contains(&name.as_str()) {
                    skipped_directories.insert(format!("{directory}/{name}"));
                }
            }
        }
    }

    if tags.is_empty() {
        return Err(format!(
            "{} holds no tags in the thirteen directories this extraction takes. This is not \\
             the data pack Minecraft's `--server` generator writes.",
            data_root.display()
        ));
    }

    // Every reference must land on a tag this dataset defines. Vanilla's tags
    // resolve inside vanilla's data; a dangling one means the walk missed a
    // tree or Mojang shipped a broken baseline, and neither should be quiet.
    let known_tags: BTreeSet<(&str, &str)> =
        tags.iter().map(|t| (t.registry, t.id.as_str())).collect();
    for ((registry_name, target), source) in &pending_references {
        if !known_tags.contains(&(registry_name.as_str(), target.as_str())) {
            return Err(format!(
                "{source} references {target}, and no such {registry_name} tag exists in \\
                 this dataset"
            ));
        }
    }
    // Against the wire, before anything is written. A registry short by one
    // tag is a client told a group is smaller than it is, which is invisible
    // until somebody's pickaxe stops working on one block in nine hundred.
    for (registry, expected) in CAPTURED {
        let found = tags.iter().filter(|t| t.registry == *registry).count();
        if found != *expected {
            return Err(format!(
                "{registry} extracted {found} tags; a real 1.21.1 server sent {expected}. \
                 Either this is not 1.21.1's data pack or the walk missed a tree."
            ));
        }
    }
    if tags.len() != CAPTURED.iter().map(|(_, n)| n).sum::<usize>() {
        return Err(format!(
            "{} tags in total, against the {} a real server sent",
            tags.len(),
            CAPTURED.iter().map(|(_, n)| n).sum::<usize>()
        ));
    }

    tags.sort_by(|a, b| (&a.registry, &a.id).cmp(&(&b.registry, &b.id)));

    Ok(Tags {
        tags,
        skipped_directories,
        memberships,
        references: pending_references.len(),
        duplicates_collapsed,
    })
}

/// Every `.json` file under `directory`, sorted, with bundled datapack trees
/// left alone: the trade-rebalance pack ships its own tags and counting those
/// would put someone else's baseline inside vanilla's.
fn json_files(directory: &Path) -> Result<Vec<std::path::PathBuf>, String> {
    let mut out = Vec::new();
    collect_json(directory, &mut out)?;
    out.sort();
    Ok(out)
}

fn collect_json(directory: &Path, out: &mut Vec<std::path::PathBuf>) -> Result<(), String> {
    if directory.file_name().is_some_and(|n| n == "datapacks") {
        return Ok(());
    }
    let entries = std::fs::read_dir(directory)
        .map_err(|e| format!("could not read {}: {e}", directory.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("could not read {}: {e}", directory.display()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_json(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "json") {
            out.push(path);
        }
    }
    Ok(())
}

fn list_namespaces(data_root: &Path) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let entries = std::fs::read_dir(data_root)
        .map_err(|e| format!("could not read {}: {e}", data_root.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("could not read {}: {e}", data_root.display()))?;
        if entry.file_type().is_ok_and(|t| t.is_dir()) && entry.file_name() != "datapacks" {
            out.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_tag(registry: &'static str, id: &str, members: &[&str]) -> Tag {
        Tag {
            registry,
            id: id.to_owned(),
            members: members.iter().map(|m| m.to_string()).collect(),
        }
    }

    #[test]
    fn the_five_taken_directories_map_to_the_five_registries() {
        assert_eq!(TAKEN.len(), 5);
        assert_eq!(registry_for("block"), Some("minecraft:block"));
        assert_eq!(registry_for("entity_type"), Some("minecraft:entity_type"));
        assert_eq!(registry_for("worldgen"), None);
    }

    #[test]
    fn tags_sort_by_registry_then_id() {
        let mut tags = [
            sample_tag("minecraft:block", "minecraft:logs", &[]),
            sample_tag("minecraft:block", "minecraft:aquarium_blocks", &[]),
            sample_tag("minecraft:item", "minecraft:arrows", &[]),
        ];
        tags.sort_by(|a, b| (&a.registry, &a.id).cmp(&(&b.registry, &b.id)));
        let order: Vec<&str> = tags.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(
            order,
            [
                "minecraft:aquarium_blocks",
                "minecraft:logs",
                "minecraft:arrows"
            ]
        );
    }
}
