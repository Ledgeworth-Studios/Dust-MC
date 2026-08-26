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
//! # The five directories, and the one left out
//!
//! Block, item, fluid, entity-type and game-event tags are extracted because
//! those are the registries Dust already holds tables for — a tag directory
//! over an unknown registry could not be checked against anything. Everything
//! else under `tags/` (`worldgen`, `damage_type`, `enchantment` and friends)
//! is reported as seen-but-not-taken on every run: extending the set is adding
//! a row to [`TAKEN`] and nothing else, precisely so it happens as a decision
//! rather than an accident.
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

/// The tag directories taken, paired with the registry each is checked
/// against.
const TAKEN: &[(&str, &str)] = &[
    ("block", "minecraft:block"),
    ("item", "minecraft:item"),
    ("fluid", "minecraft:fluid"),
    ("entity_type", "minecraft:entity_type"),
    ("game_event", "minecraft:game_event"),
];

fn registry_for(directory: &str) -> Option<&'static str> {
    TAKEN
        .iter()
        .find(|(dir, _)| *dir == directory)
        .map(|(_, registry)| *registry)
}

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

/// Read the five tag directories out of every namespace in the tree.
pub fn parse(data_root: &Path, registries: &Registries, blocks: &Blocks) -> Result<Tags, String> {
    // Membership lookups, built once: block names come from the block report,
    // everything else from the registry report.
    let mut known: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    let block_names: BTreeSet<&str> = blocks.blocks.iter().map(|b| b.name.as_str()).collect();
    known.insert("minecraft:block", block_names);
    for (_, registry_name) in TAKEN {
        if *registry_name == "minecraft:block" {
            continue;
        }
        let Some(registry) = registries
            .registries
            .iter()
            .find(|r| r.name == *registry_name)
        else {
            return Err(format!("the registry report has no {registry_name}"));
        };
        let names: BTreeSet<&str> = registry.entries.iter().map(|e| e.name.as_str()).collect();
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
        for entry in std::fs::read_dir(&root)
            .map_err(|e| format!("could not read {}: {e}", root.display()))?
        {
            let entry = entry.map_err(|e| format!("could not read {}: {e}", root.display()))?;
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let directory = entry.file_name().to_string_lossy().into_owned();
            let Some(registry_name) = registry_for(&directory) else {
                skipped_directories.insert(directory);
                continue;
            };

            for path in json_files(&entry.path())? {
                let relative = path
                    .strip_prefix(entry.path())
                    .map_err(|_| format!("{} escaped {}", path.display(), entry.path().display()))?
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
                            (registry_name.to_owned(), target.clone()),
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

    if tags.is_empty() {
        return Err(format!(
            "{} holds no tags in the five directories this extraction takes. This is not \\
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

    fn sample_tag(registry: &str, id: &str, members: &[&str]) -> Tag {
        Tag {
            registry: registry.to_owned(),
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
        let mut tags = vec![
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
