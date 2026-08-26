//! Which directories under `data/<namespace>/` mean something, and what.
//!
//! # Why the loader is driven by a list instead of a directory walk
//!
//! Given the file `data/minecraft/loot_table/blocks/stone.json`, nothing in the
//! path says where the registry ends and the resource name begins. It could be
//! registry `loot_table` holding `minecraft:blocks/stone`, or a registry
//! `loot_table/blocks` holding `minecraft:stone`. Both readings are consistent
//! with the layout, and the corpus contains cases that go each way:
//! `loot_table/blocks/…` is the first, `worldgen/biome/…` is the second.
//! Minecraft resolves this by never walking generically — it asks each registry
//! it knows about for the files under that registry's directory.
//!
//! Dust does the same thing, by matching the **longest** directory prefix in
//! the table below. That removes the ambiguity, and it has two consequences
//! worth having:
//!
//! * A directory that is not a registry is *noticed*. `data/minecraft/mystery/`
//!   produces a warning rather than a thousand resources named
//!   `minecraft:mystery/…` that nothing will ever ask for. A pack directory
//!   that silently does nothing is the failure mode this project rules out.
//! * The vanilla data tree contains `data/minecraft/datapacks/bundle/` and
//!   `data/minecraft/datapacks/trade_rebalance/` — two complete nested packs,
//!   each with its own `pack.mcmeta`. A generic walk swallows 64 files from
//!   them into whatever registry the walker guessed. Here `datapacks` matches
//!   no registry, so the walk stops at it and says so.
//!
//! # What this table does not catch
//!
//! It is vanilla 1.21.1's list. A mod that adds a registry adds a directory
//! this does not know, and the warning about it will be wrong — helpful for a
//! typo, noise for a mod. [`Registries::with_extra`] is how that is fixed, and
//! it is the caller's job to call it, because this crate has no way to find out
//! what registries the server ended up with.

use std::collections::BTreeMap;

/// What a registry's directory holds and how collisions between packs resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryKind {
    /// JSON documents. A later pack's copy replaces an earlier pack's.
    Content,
    /// JSON tag files. A later pack's copy *merges* into an earlier pack's
    /// unless it says `"replace": true`. See [`crate::tag`].
    ///
    /// The string is the registry the tag's entries name — `tags/block` holds
    /// tags whose entries are blocks. It is what a [`crate::Vocabulary`] is
    /// asked about, and it is a plain name rather than a [`RegistryId`]
    /// because most of them (`block`, `item`, `entity_type`, `fluid`) are
    /// built into the server and never appear as a datapack directory at all.
    Tag(&'static str),
    /// A directory Dust knows the name of and does not read, with the reason.
    ///
    /// These exist so the directory does not show up as a mystery. Saying "this
    /// is `.mcfunction`, which Dust does not run yet" is a different message
    /// from "this is not a registry", and the difference is what tells an
    /// operator whether they made a mistake.
    Unread {
        extension: &'static str,
        why: &'static str,
    },
}

/// One registry's directory, and the spellings of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryDef {
    /// The directory as 1.21 and later spell it, relative to `data/<ns>/`.
    pub key: RegistryId,
    /// Spellings used by older packs for the same registry. 1.21 renamed a
    /// number of these from plural to singular, and packs in the wild are
    /// still full of the old names.
    pub legacy: Vec<RegistryId>,
    pub kind: RegistryKind,
}

/// A registry directory, e.g. `recipe`, `worldgen/biome`, `tags/block`.
///
/// Cheap enough to clone as a `String`: there are about fifty of them and they
/// are the outer key of the loaded data, not the inner one.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RegistryId(String);

impl RegistryId {
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this is a `tags/…` directory. Cheaper than looking the registry
    /// up when all that is wanted is "does this merge or override".
    pub fn is_tags(&self) -> bool {
        self.0.starts_with("tags/")
    }
}

impl std::fmt::Display for RegistryId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for RegistryId {
    fn from(key: &str) -> Self {
        Self(key.to_owned())
    }
}

/// The longest directory prefix any registry uses, in segments.
///
/// Three, because of `tags/worldgen/biome`. The matcher tries prefixes from
/// this length down, so a new registry deeper than this would be matched at the
/// wrong depth rather than not at all — hence the assertion in the tests.
const MAX_KEY_SEGMENTS: usize = 3;

/// Vanilla 1.21.1's datapack registries.
///
/// Where a registry was renamed in 1.21, the 1.21 spelling is the key and the
/// pre-1.21 spelling is in `legacy`. Confirmed against
/// `.dust-extract/data-1.21.1`: the corpus uses `advancement`, `loot_table`,
/// `recipe` and `tags/block`, all singular.
fn vanilla_defs() -> Vec<RegistryDef> {
    use RegistryKind::{Content, Tag, Unread};

    let mut defs = Vec::new();
    let mut content = |key: &str, legacy: &[&str]| {
        defs.push(RegistryDef {
            key: RegistryId::new(key),
            legacy: legacy.iter().map(|s| RegistryId::new(*s)).collect(),
            kind: Content,
        });
    };

    content("advancement", &["advancements"]);
    content("banner_pattern", &[]);
    content("chat_type", &[]);
    content("damage_type", &[]);
    content("dimension", &[]);
    content("dimension_type", &[]);
    content("enchantment", &[]);
    content("enchantment_provider", &[]);
    content("instrument", &[]);
    content("item_modifier", &["item_modifiers"]);
    content("jukebox_song", &[]);
    content("loot_table", &["loot_tables"]);
    content("painting_variant", &[]);
    content("predicate", &["predicates"]);
    content("recipe", &["recipes"]);
    content("trim_material", &[]);
    content("trim_pattern", &[]);
    content("wolf_variant", &[]);
    content("worldgen/biome", &[]);
    content("worldgen/configured_carver", &[]);
    content("worldgen/configured_feature", &[]);
    content("worldgen/density_function", &[]);
    content("worldgen/flat_level_generator_preset", &[]);
    content("worldgen/multi_noise_biome_source_parameter_list", &[]);
    content("worldgen/noise", &[]);
    content("worldgen/noise_settings", &[]);
    content("worldgen/placed_feature", &[]);
    content("worldgen/processor_list", &[]);
    content("worldgen/structure", &[]);
    content("worldgen/structure_set", &[]);
    content("worldgen/template_pool", &[]);
    content("worldgen/world_preset", &[]);

    let mut tag = |key: &str, of: &'static str, legacy: &[&str]| {
        defs.push(RegistryDef {
            key: RegistryId::new(key),
            legacy: legacy.iter().map(|s| RegistryId::new(*s)).collect(),
            kind: Tag(of),
        });
    };

    tag("tags/banner_pattern", "banner_pattern", &[]);
    tag("tags/block", "block", &["tags/blocks"]);
    tag("tags/cat_variant", "cat_variant", &[]);
    tag("tags/damage_type", "damage_type", &[]);
    tag("tags/enchantment", "enchantment", &[]);
    tag("tags/entity_type", "entity_type", &["tags/entity_types"]);
    tag("tags/fluid", "fluid", &["tags/fluids"]);
    tag("tags/function", "function", &["tags/functions"]);
    tag("tags/game_event", "game_event", &["tags/game_events"]);
    tag("tags/instrument", "instrument", &[]);
    tag("tags/item", "item", &["tags/items"]);
    tag("tags/painting_variant", "painting_variant", &[]);
    tag("tags/point_of_interest_type", "point_of_interest_type", &[]);
    tag("tags/worldgen/biome", "worldgen/biome", &[]);
    tag(
        "tags/worldgen/flat_level_generator_preset",
        "worldgen/flat_level_generator_preset",
        &[],
    );
    tag("tags/worldgen/structure", "worldgen/structure", &[]);
    tag("tags/worldgen/world_preset", "worldgen/world_preset", &[]);

    defs.push(RegistryDef {
        key: RegistryId::new("function"),
        legacy: vec![RegistryId::new("functions")],
        kind: Unread {
            extension: ".mcfunction",
            why: "Dust does not run commands yet. The files are left alone \
                  rather than half-read",
        },
    });
    defs.push(RegistryDef {
        key: RegistryId::new("structure"),
        legacy: vec![RegistryId::new("structures")],
        kind: Unread {
            extension: ".nbt",
            why: "structure templates are NBT, not JSON, and belong to \
                  dust-nbt rather than to this crate",
        },
    });

    defs
}

/// The set of registries a load knows about.
#[derive(Debug, Clone)]
pub struct Registries {
    defs: Vec<RegistryDef>,
    /// Every spelling — canonical and legacy — to the index of its definition.
    by_directory: BTreeMap<RegistryId, usize>,
}

/// What a path inside a pack turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryMatch<'a> {
    pub def: &'a RegistryDef,
    /// The directory as it was actually written, which is the legacy spelling
    /// when one was used. Kept so the warning can quote the pack.
    pub written_as: RegistryId,
    /// The rest of the path, with `.json` still on it.
    pub remainder: &'a str,
}

impl DirectoryMatch<'_> {
    /// Whether the pack spelled this directory the old way.
    pub fn is_legacy(&self) -> bool {
        self.written_as != self.def.key
    }
}

impl Default for Registries {
    fn default() -> Self {
        Self::vanilla()
    }
}

impl Registries {
    /// Vanilla 1.21.1's registries and nothing else.
    pub fn vanilla() -> Self {
        Self::from_defs(vanilla_defs())
    }

    /// The vanilla set plus registries something else contributed.
    ///
    /// A later definition for a directory that is already claimed replaces the
    /// earlier one, so a caller can correct this table as well as extend it.
    pub fn with_extra(mut self, extra: impl IntoIterator<Item = RegistryDef>) -> Self {
        let mut defs = std::mem::take(&mut self.defs);
        for def in extra {
            match defs.iter_mut().find(|existing| existing.key == def.key) {
                Some(existing) => *existing = def,
                None => defs.push(def),
            }
        }
        Self::from_defs(defs)
    }

    fn from_defs(defs: Vec<RegistryDef>) -> Self {
        let mut by_directory = BTreeMap::new();
        for (index, def) in defs.iter().enumerate() {
            by_directory.insert(def.key.clone(), index);
            for legacy in &def.legacy {
                by_directory.insert(legacy.clone(), index);
            }
        }
        Self { defs, by_directory }
    }

    pub fn all(&self) -> &[RegistryDef] {
        &self.defs
    }

    pub fn get(&self, key: &RegistryId) -> Option<&RegistryDef> {
        self.by_directory.get(key).map(|index| &self.defs[*index])
    }

    /// Split a path relative to `data/<namespace>/` into a registry and the
    /// rest, by the longest directory prefix that names a registry.
    ///
    /// `None` means no registry claims this path — either a typo or a
    /// registry Dust has not been told about.
    pub fn classify<'a>(&'a self, relative: &'a str) -> Option<DirectoryMatch<'a>> {
        let mut boundaries = Vec::with_capacity(MAX_KEY_SEGMENTS);
        for (offset, _) in relative.match_indices('/').take(MAX_KEY_SEGMENTS) {
            boundaries.push(offset);
        }
        // Longest first, so `tags/worldgen/biome` wins over a hypothetical
        // `tags/worldgen` and `tags` never matches at all.
        for boundary in boundaries.into_iter().rev() {
            let candidate = RegistryId::new(&relative[..boundary]);
            if let Some(index) = self.by_directory.get(&candidate) {
                return Some(DirectoryMatch {
                    def: &self.defs[*index],
                    written_as: candidate,
                    remainder: &relative[boundary + 1..],
                });
            }
        }
        None
    }

    /// The first path segment, for naming a directory that matched nothing.
    pub fn unmatched_directory(relative: &str) -> &str {
        relative.split('/').next().unwrap_or(relative)
    }

    /// Every canonical directory name, for "did you mean".
    pub fn directory_names(&self) -> impl Iterator<Item = &str> {
        self.defs.iter().map(|def| def.key.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_longest_prefix_wins() {
        let registries = Registries::vanilla();
        let matched = registries
            .classify("worldgen/biome/plains.json")
            .expect("worldgen/biome is a registry");
        assert_eq!(matched.def.key, RegistryId::new("worldgen/biome"));
        assert_eq!(matched.remainder, "plains.json");
    }

    #[test]
    fn a_nested_resource_name_is_not_mistaken_for_a_registry() {
        let registries = Registries::vanilla();
        let matched = registries
            .classify("loot_table/blocks/stone.json")
            .expect("loot_table is a registry");
        assert_eq!(matched.def.key, RegistryId::new("loot_table"));
        assert_eq!(matched.remainder, "blocks/stone.json");
    }

    #[test]
    fn tags_of_worldgen_registries_are_three_segments_deep() {
        let registries = Registries::vanilla();
        let matched = registries
            .classify("tags/worldgen/biome/is_overworld.json")
            .expect("tags/worldgen/biome is a registry");
        assert_eq!(matched.def.key, RegistryId::new("tags/worldgen/biome"));
        assert_eq!(matched.remainder, "is_overworld.json");
        assert!(matched.def.key.is_tags());
    }

    #[test]
    fn no_registry_key_is_deeper_than_the_matcher_looks() {
        // The matcher only tries prefixes up to MAX_KEY_SEGMENTS long. A
        // registry added below that depth would be silently unmatched, which
        // is the one way this table can be wrong without anything failing.
        for def in Registries::vanilla().all() {
            let segments = def.key.as_str().split('/').count();
            assert!(
                segments <= MAX_KEY_SEGMENTS,
                "`{}` is {segments} segments deep; MAX_KEY_SEGMENTS is {MAX_KEY_SEGMENTS}",
                def.key
            );
        }
    }

    #[test]
    fn the_pre_1_21_spellings_still_resolve_and_are_marked_as_such() {
        let registries = Registries::vanilla();
        for (old, new) in [
            ("loot_tables/blocks/stone.json", "loot_table"),
            ("recipes/stick.json", "recipe"),
            ("advancements/story/root.json", "advancement"),
            ("tags/blocks/logs.json", "tags/block"),
            ("tags/items/logs.json", "tags/item"),
        ] {
            let matched = registries.classify(old).expect(old);
            assert_eq!(matched.def.key, RegistryId::new(new), "{old}");
            assert!(matched.is_legacy(), "{old} should be flagged as legacy");
        }
    }

    #[test]
    fn the_current_spelling_is_not_flagged_as_legacy() {
        let registries = Registries::vanilla();
        let matched = registries.classify("recipe/stick.json").expect("recipe");
        assert!(!matched.is_legacy());
    }

    #[test]
    fn the_nested_vanilla_datapacks_directory_matches_nothing() {
        // The trap this table exists to avoid: `data/minecraft/datapacks/`
        // holds two complete packs in the real vanilla tree.
        let registries = Registries::vanilla();
        assert!(registries
            .classify("datapacks/bundle/data/minecraft/recipe/bundle.json")
            .is_none());
        assert_eq!(
            Registries::unmatched_directory("datapacks/bundle/data/x.json"),
            "datapacks"
        );
    }

    #[test]
    fn a_bare_file_at_the_namespace_root_matches_nothing() {
        assert!(Registries::vanilla().classify("stray.json").is_none());
    }

    #[test]
    fn an_extra_registry_can_be_added_and_an_existing_one_corrected() {
        let registries = Registries::vanilla().with_extra([RegistryDef {
            key: RegistryId::new("mystery"),
            legacy: Vec::new(),
            kind: RegistryKind::Content,
        }]);
        assert!(registries.classify("mystery/thing.json").is_some());
        assert_eq!(
            registries.all().len(),
            Registries::vanilla().all().len() + 1
        );

        let corrected = Registries::vanilla().with_extra([RegistryDef {
            key: RegistryId::new("recipe"),
            legacy: Vec::new(),
            kind: RegistryKind::Content,
        }]);
        assert_eq!(corrected.all().len(), Registries::vanilla().all().len());
        assert!(
            corrected.classify("recipes/stick.json").is_none(),
            "replacing a definition should drop its old aliases"
        );
    }

    #[test]
    fn tag_registries_name_the_registry_their_entries_belong_to() {
        let registries = Registries::vanilla();
        let block_tags = registries.get(&RegistryId::new("tags/block")).unwrap();
        assert_eq!(block_tags.kind, RegistryKind::Tag("block"));
    }
}
