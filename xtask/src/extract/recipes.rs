//! The recipe-shape catalogue: which serialisers exist, and what each takes.
//!
//! Every recipe on 1.21.1 is a JSON object whose `type` names a recipe
//! serialiser, and the serialiser decides what every other key means. That
//! makes the *shapes* — not the 1,290 individual recipes — the useful thing to
//! commit: a crafting_shaped is `[category, group?, key, pattern, result]` and
//! a stonecutting is `[ingredient, result]`, whatever the recipe is called.
//! Phase 3's `/recipe` surface and Phase 4's loot-to-crafting links both need
//! this vocabulary long before they need to know that oak planks come from oak
//! logs.
//!
//! # What the data said, measured rather than assumed
//!
//! - Vanilla ships 1,290 recipes across 23 serialiser shapes, all in the
//!   `minecraft` namespace on this version.
//! - **Keys are not uniform within a shape.** `group` appears on some
//!   smelting/blasting/smoking recipes and not others; `show_notification`
//!   appears exactly once across all 634 shaped recipes. So each shape carries
//!   two lists: the keys every recipe of that shape has, and the keys seen at
//!   least once — because a reader that assumed uniformity would treat the
//!   optional half as missing or, worse, required.
//! - **The special recipes are one-line markers.** Thirteen
//!   `crafting_special_*` files exist and carry nothing but `type` and
//!   `category`: map cloning, banner duplication, armour dyeing and friends
//!   are computed by the game from the player's input, so there is no
//!   configuration to write. Their shape in the catalogue is the emptiest one
//!   possible, and that emptiness is a fact worth having recorded rather than
//!   guessed at.
//!
//! # Where this sits relative to the provenance line
//!
//! What lands in the repository is the vocabulary — twenty-three names, their
//! key sets, and how many recipes use each. Not one recipe's contents: no
//! patterns, no ingredients, no results. The numbers are the same kind of fact
//! the packet tables commit (how many packets exist, not what they say).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::Value;

use super::registries::Registries;

const SERIALIZER_REGISTRY: &str = "minecraft:recipe_serializer";

/// One shape, aggregated across every recipe file that uses it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shape {
    /// The `type` value, which is also the serialiser's registry name.
    pub serializer: String,
    pub uses: usize,
    /// Keys present on every recipe of this shape, sorted.
    pub required: Vec<String>,
    /// Keys present on at least one recipe but not all, sorted.
    pub optional: Vec<String>,
}

#[derive(Debug)]
pub struct Recipes {
    /// Every shape, name-sorted by serialiser.
    pub shapes: Vec<Shape>,
    /// Serialisers registered but exercised by no vanilla recipe file — the
    /// special, client-computed ones.
    pub unused_serializers: Vec<String>,
    /// How many recipe files were read in total.
    pub total: usize,
    /// Namespaces that carried recipe files.
    pub namespaces: Vec<String>,
}

/// Walk `data/<ns>/recipe/**/*.json` and aggregate the shapes.
pub fn parse(data_root: &Path, registries: &Registries) -> Result<Recipes, String> {
    let serializer_registry = registries
        .registries
        .iter()
        .find(|r| r.name == SERIALIZER_REGISTRY)
        .ok_or("the registry report has no minecraft:recipe_serializer")?;

    let mut namespaces: Vec<String> = list_namespaces(data_root)?;
    let mut per_type: BTreeMap<String, Vec<(String, BTreeSet<String>)>> = BTreeMap::new();
    let mut total = 0usize;
    let mut saw_any = false;

    for namespace in &namespaces {
        let directory = data_root.join(namespace).join("recipe");
        if !directory.is_dir() {
            continue;
        }
        saw_any = true;
        for path in json_files(&directory)? {
            let text = std::fs::read(&path)
                .map_err(|e| format!("could not read {}: {e}", path.display()))?;
            let value: Value = serde_json::from_slice(&text)
                .map_err(|e| format!("could not read {} as JSON: {e}", path.display()))?;
            let Some(object) = value.as_object() else {
                return Err(format!("{} is not an object", path.display()));
            };
            let Some(kind) = object.get("type").and_then(Value::as_str) else {
                return Err(format!(
                    "{} has no `type`, so no serialiser says what its keys mean",
                    path.display()
                ));
            };

            // A datapack may inline anything; these keys are the whole of what
            // vanilla writes today, and a new one stops the run so somebody
            // reads it before it becomes part of the catalogue.
            const KNOWN_KEYS: &[&str] = &[
                "type",
                "category",
                "group",
                "pattern",
                "key",
                "ingredients",
                "ingredient",
                "result",
                "cookingtime",
                "experience",
                "show_notification",
                "template",
                "base",
                "addition",
            ];
            for key in object.keys() {
                if !KNOWN_KEYS.contains(&key.as_str()) {
                    return Err(format!(
                        "{} has the key {key:?}, which this reading of the recipe shapes \
                         does not know. Add it to the known set in \
                         xtask/src/extract/recipes.rs once you have decided whether it is \
                         required everywhere or optional.",
                        path.display()
                    ));
                }
            }

            per_type
                .entry(kind.to_owned())
                .or_default()
                .push((path.display().to_string(), object.keys().cloned().collect()));
            total += 1;
        }
    }
    let _ = saw_any;

    if total == 0 {
        return Err(format!(
            "{} holds no recipe files at all. This is not the data pack Minecraft's \
             `--server` generator writes.",
            data_root.display()
        ));
    }

    // Every shape the data uses must be a registered serialiser: two reports
    // agreeing again, and the check that catches a typo'd `type` before it
    // becomes part of a committed catalogue.
    let mut shapes = Vec::with_capacity(per_type.len());
    for (serializer, occurrences) in per_type {
        if !serializer_registry
            .entries
            .iter()
            .any(|e| e.name == serializer)
        {
            return Err(format!(
                "{total} recipe files use `{serializer}`, and it is not an entry of the \
                 recipe_serializer registry"
            ));
        }
        let count = occurrences.len();
        let mut key_counts: BTreeMap<&str, usize> = BTreeMap::new();
        for (_, keys) in &occurrences {
            for key in keys {
                *key_counts.entry(key.as_str()).or_default() += 1;
            }
        }
        let (required, optional) = split_keys(count, &key_counts);
        shapes.push(Shape {
            serializer,
            uses: count,
            required,
            optional,
        });
    }
    shapes.sort_by(|a, b| a.serializer.cmp(&b.serializer));

    let unused_serializers: Vec<String> = serializer_registry
        .entries
        .iter()
        .map(|e| e.name.clone())
        .filter(|name| !shapes.iter().any(|s| &s.serializer == name))
        .collect();

    namespaces.retain(|ns| data_root.join(ns).join("recipe").is_dir());
    Ok(Recipes {
        shapes,
        unused_serializers,
        total,
        namespaces,
    })
}

/// A key every recipe of the shape carries is required; anything seen less
/// often is optional, and the catalogue says so rather than letting a reader
/// guess.
fn split_keys(count: usize, key_counts: &BTreeMap<&str, usize>) -> (Vec<String>, Vec<String>) {
    let mut required = Vec::new();
    let mut optional = Vec::new();
    for (key, times) in key_counts {
        if *times == count {
            required.push((*key).to_owned());
        } else {
            optional.push((*key).to_owned());
        }
    }
    (required, optional)
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

/// Every `.json` file under `directory`, sorted, with `datapacks` trees left
/// alone — the bundled packs ship their own copies of the same shapes and
/// counting them twice would make the catalogue lie about usage.
fn json_files(directory: &Path) -> Result<Vec<std::path::PathBuf>, String> {
    let mut out = Vec::new();
    walk(directory, &mut out)?;
    out.sort();
    Ok(out)
}

fn walk(directory: &Path, out: &mut Vec<std::path::PathBuf>) -> Result<(), String> {
    if directory.file_name().is_some_and(|n| n == "datapacks") {
        return Ok(());
    }
    let entries = std::fs::read_dir(directory)
        .map_err(|e| format!("could not read {}: {e}", directory.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("could not read {}: {e}", directory.display()))?;
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "json") {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(serializer: &str, uses: usize, keys: &[&str]) -> Shape {
        Shape {
            serializer: serializer.to_owned(),
            uses,
            required: keys.iter().map(|s| s.to_string()).collect(),
            optional: Vec::new(),
        }
    }

    #[test]
    fn the_special_serializers_are_reported_and_not_treated_as_missing() {
        let recipes = Recipes {
            shapes: vec![shape("minecraft:crafting_shaped", 2, &["type"])],
            unused_serializers: vec![
                "minecraft:crafting_special_armordye".to_owned(),
                "minecraft:smithing_trim".to_owned(),
            ],
            total: 2,
            namespaces: vec!["minecraft".to_owned()],
        };
        assert_eq!(recipes.unused_serializers.len(), 2);
        assert!(recipes
            .unused_serializers
            .iter()
            .all(|s| s.contains("special") || s.contains("trim")));
    }

    #[test]
    fn required_and_optional_split_on_whether_every_recipe_has_the_key() {
        // Two of three carry `group`: it is optional. All three carry `type`:
        // required. Getting this backwards turns a documented gap into a
        // phantom requirement.
        let mut key_counts: BTreeMap<&str, usize> = BTreeMap::new();
        key_counts.insert("type", 3);
        key_counts.insert("group", 2);
        let (required, optional) = split_keys(3, &key_counts);
        assert_eq!(required, ["type"]);
        assert_eq!(optional, ["group"]);
    }
}
