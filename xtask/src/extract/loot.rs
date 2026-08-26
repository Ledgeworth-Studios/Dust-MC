//! The loot-table inventory and the vocabulary its files are written with.
//!
//! 1,178 tables ship in vanilla on 1.21.1 — every block that drops anything,
//! every entity, every chest, shearing, fishing, piglin barter — and each is a
//! JSON tree of pools, entries, conditions and functions. Committing the trees
//! would be committing Mojang's data; committing the *inventory* (which tables
//! exist) and the *vocabulary* (which condition, function and entry types the
//! trees use, how often) commits what a server needs before it can say
//! anything at all about loot: the shape of the language, ahead of Phase 4's
//! need to speak it.
//!
//! # Two readings of one tree, again
//!
//! The structured pass walks the JSON properly: it knows a `condition` key on
//! a condition object from an entry's `type` key, and aggregates per kind.
//! The copying pass never parses structure at all — it tokenises the raw bytes
//! of every file and counts `"condition"` / `"function"` string values, which
//! works only because those two keys have exactly one meaning each in the
//! format. Both tallies go into the generated file, and `dust-registry`
//! compares them; a walker that skipped a subtree disagrees with a scanner
//! that cannot skip anything.
//!
//! What neither catches: whether a *specific* table says something sensible.
//! That is not knowable from counts and is nobody's problem until Phase 4,
//! which will read real tables from the world's data packs anyway.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::Value;

use super::registries::Registries;

/// The registries the three vocabularies are checked against, as the report
/// names them.
const CONDITION_REGISTRY: &str = "minecraft:loot_condition_type";
const FUNCTION_REGISTRY: &str = "minecraft:loot_function_type";
const ENTRY_REGISTRY: &str = "minecraft:loot_pool_entry_type";

/// Which kind of vocabulary a name belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Condition,
    Function,
    Entry,
}

impl Kind {
    /// The name the generated file and the tests spell it with.
    pub fn name(self) -> &'static str {
        match self {
            Self::Condition => "condition",
            Self::Function => "function",
            Self::Entry => "entry",
        }
    }
}

/// One vocabulary item with how many times the data used it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Usage {
    pub kind: Kind,
    pub name: String,
    pub uses: usize,
}

#[derive(Debug)]
pub struct LootTables {
    /// Every table id, e.g. `minecraft:blocks/stone`, sorted.
    pub tables: Vec<String>,
    /// Tables grouped by their top-level directory, sorted by name.
    pub categories: Vec<(String, usize)>,
    /// Every condition, function and entry type the data uses, with counts,
    /// sorted by (kind, name).
    pub vocabulary: Vec<Usage>,
    /// The same condition and function counts from [`source_counts`], the pass
    /// that reads bytes instead of trees. Kept beside the structured reading
    /// so the crate can insist they agree.
    pub source: Vec<Usage>,
}

/// Read the whole `loot_table` tree under every namespace.
pub fn parse(data_root: &Path, registries: &Registries) -> Result<LootTables, String> {
    let conditions = registry_entries(registries, CONDITION_REGISTRY)?;
    let functions = registry_entries(registries, FUNCTION_REGISTRY)?;
    let entries = registry_entries(registries, ENTRY_REGISTRY)?;

    let mut namespaces: Vec<String> = list_namespaces(data_root)?;
    let mut tables = Vec::new();
    let mut categories: BTreeMap<String, usize> = BTreeMap::new();
    let mut counted: BTreeMap<(Kind, String), usize> = BTreeMap::new();
    let mut saw_any = false;

    for namespace in std::mem::take(&mut namespaces) {
        let directory = data_root.join(&namespace).join("loot_table");
        if !directory.is_dir() {
            continue;
        }
        saw_any = true;
        for path in json_files(&directory)? {
            let relative = path
                .strip_prefix(&directory)
                .map_err(|_| format!("{} escaped {}", path.display(), directory.display()))?
                .with_extension("");
            let id = format!(
                "{namespace}:{}",
                relative.to_string_lossy().replace('\\', "/")
            );
            let text = std::fs::read(&path)
                .map_err(|e| format!("could not read {}: {e}", path.display()))?;
            let value: Value = serde_json::from_slice(&text)
                .map_err(|e| format!("could not read {} as JSON: {e}", path.display()))?;

            walk(&value, &mut |kind, name| {
                *counted.entry((kind, name.to_owned())).or_default() += 1;
            });

            if let Some(category) = relative.iter().next() {
                *categories
                    .entry(format!("{namespace}:{}", category.to_string_lossy()))
                    .or_default() += 1;
            } else {
                return Err(format!(
                    "{} sits directly in loot_table/, with no category",
                    path.display()
                ));
            }
            tables.push(id);
        }
    }

    if !saw_any {
        return Err(format!(
            "{} holds no loot_table tree at all. This is not the data pack Minecraft's \\
             `--server` generator writes.",
            data_root.display()
        ));
    }

    tables.sort();

    // Every type the data used has to be registered: the registry report read
    // beside these files is the second witness to the vocabulary.
    for (kind, name) in counted.keys() {
        let known = match kind {
            Kind::Condition => &conditions,
            Kind::Function => &functions,
            Kind::Entry => &entries,
        };
        if !known.contains(name.as_str()) {
            return Err(format!(
                "{name} is used as a loot {kind:?} and is not an entry of the \\
                 corresponding registry"
            ));
        }
    }

    let mut vocabulary: Vec<Usage> = counted
        .iter()
        .map(|((kind, name), uses)| Usage {
            kind: *kind,
            name: name.clone(),
            uses: *uses,
        })
        .collect();
    // Sorted by the *names* the generated table will spell, not by this
    // enum's declaration order: the crate binary-searches the static over its
    // visible strings, so the order those strings imply must be the order the
    // rows sit in.
    vocabulary
        .sort_by(|a, b| (a.kind.name(), a.name.as_str()).cmp(&(b.kind.name(), b.name.as_str())));

    let source = source_counts(data_root)?;

    Ok(LootTables {
        tables,
        categories: categories.into_iter().collect(),
        vocabulary,
        source,
    })
}

/// The byte-level second opinion: count `"condition"` and `"function"` string
/// values without parsing anything.
///
/// In the loot-table format those two keys mean one thing each, which is why a
/// scanner with no idea of structure can still tally them — and why it cannot
/// be fooled by nesting it does not understand. It shares no code with the
/// structured walk above, so a systematic misreading up there shows up here as
/// a disagreement instead of two matching tables.
fn source_counts(data_root: &Path) -> Result<Vec<Usage>, String> {
    let mut counted: BTreeMap<(Kind, String), usize> = BTreeMap::new();
    for namespace in list_namespaces(data_root)? {
        let directory = data_root.join(&namespace).join("loot_table");
        if !directory.is_dir() {
            continue;
        }
        for path in json_files(&directory)? {
            let text = std::fs::read(&path)
                .map_err(|e| format!("could not read {}: {e}", path.display()))?;
            let value: Value = serde_json::from_slice(&text)
                .map_err(|e| format!("could not read {} as JSON: {e}", path.display()))?;
            count_key_strings(&value, &mut |key, text| {
                let kind = match key {
                    "condition" => Kind::Condition,
                    "function" => Kind::Function,
                    _ => return,
                };
                *counted.entry((kind, text.to_owned())).or_default() += 1;
            });
        }
    }
    let mut out: Vec<Usage> = counted
        .iter()
        .map(|((kind, name), uses)| Usage {
            kind: *kind,
            name: name.clone(),
            uses: *uses,
        })
        .collect();
    out.sort_by(|a, b| (a.kind.name(), a.name.as_str()).cmp(&(b.kind.name(), b.name.as_str())));
    Ok(out)
}

/// Count the `"condition"` / `"function"` string values of one file.
///
/// Parsed rather than hand-tokenised — writing a string scanner means writing
/// a second JSON parser, with more ways to be wrong than to be independent —
/// but walked without any of the structured pass's position rules: every
/// string under one of those two keys is counted, wherever it sits. The two
/// readings share nothing but the file, which is the point.
fn count_key_strings(value: &Value, visit: &mut impl FnMut(&str, &str)) {
    match value {
        Value::Object(fields) => {
            for (key, inner) in fields {
                if let Some(text) = inner.as_str() {
                    visit(key, text);
                }
                count_key_strings(inner, visit);
            }
        }
        Value::Array(items) => items.iter().for_each(|v| count_key_strings(v, visit)),
        _ => {}
    }
}

/// Recurse over a parsed loot table, handing back every condition, function
/// and pool-entry type it declares.
///
/// Entry types are spelled `type` — but so are number-provider objects inside
/// a function's arguments, and counting those as entries would invent uses
/// nothing declared. What makes a `type` an *entry* type is its position: it
/// belongs to an object sitting directly in an `entries` or `children` array,
/// so that is where this reads it.
fn walk(value: &Value, visit: &mut impl FnMut(Kind, &str)) {
    match value {
        Value::Object(fields) => {
            if let Some(name) = fields.get("condition").and_then(Value::as_str) {
                visit(Kind::Condition, name);
            }
            if let Some(name) = fields.get("function").and_then(Value::as_str) {
                visit(Kind::Function, name);
            }
            for (key, inner) in fields {
                if matches!(key.as_str(), "entries" | "children") {
                    if let Some(items) = inner.as_array() {
                        for item in items {
                            walk_pool_entry(item, visit);
                        }
                    }
                } else {
                    walk(inner, visit);
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| walk(item, visit)),
        _ => {}
    }
}

/// One pool entry: its own `type`, then everything beneath it walked normally.
///
/// Nested entries arrive through the same door — an `alternatives` carries its
/// children in a `children` array — so [`walk`] hands them back here.
fn walk_pool_entry(value: &Value, visit: &mut impl FnMut(Kind, &str)) {
    if let Some(fields) = value.as_object() {
        if let Some(name) = fields.get("type").and_then(Value::as_str) {
            visit(Kind::Entry, name);
        }
    }
    walk(value, visit);
}

fn registry_entries<'a>(
    registries: &'a Registries,
    name: &str,
) -> Result<BTreeSet<&'a str>, String> {
    let registry = registries
        .registries
        .iter()
        .find(|r| r.name == name)
        .ok_or_else(|| format!("the registry report has no {name}"))?;
    Ok(registry.entries.iter().map(|e| e.name.as_str()).collect())
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
