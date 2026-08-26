//! What the common loot conditions and functions are called and what keys
//! they take — a vocabulary, not a second reader.
//!
//! # Why this exists, and why it is opt-in
//!
//! A loot condition is an object that opens with `"condition":
//! "minecraft:random_chance"` and then carries whatever *that* serializer
//! reads. The crate documentation refuses to model those serializers as Rust
//! structs: generating them would be a second reader of the datapack schema,
//! and two readers disagree the moment Mojang adds a kind this table has not
//! heard of. But there is a layer between "hold the JSON untouched" and
//! "model every serializer": the **key names**. A misspelled key —
//! `predicat` where `predicate` belongs — is a setting that silently does
//! nothing, which is the one failure mode this project rules out everywhere
//! else, and it can be caught without deciding what any key *means*.
//!
//! So this module holds typed **definitions**: each built-in's id and the set
//! of top-level keys it reads. Nothing more. Values are never looked at,
//! nested predicates are never walked into, and nothing here runs during
//! [`crate::load`] — [`audit`] is a pass a caller chooses, the same way
//! [`crate::advancement::validate`] is. Loading stays shape-blind; the audit
//! is a report on top of a finished load.
//!
//! # The baseline tables are marked as baseline
//!
//! [`CONDITION_DEFS`] and [`FUNCTION_DEFS`] cover what vanilla 1.21.1 ships
//! and were confirmed against vanilla's own data files wherever those files
//! use the kind (the spellings `offsetX`/`enchanted_chance`/`set_potion`'s
//! `id` among them would be easy to get wrong from memory). They are still a
//! **local static baseline**, not an authority: when dust-registry lands it
//! owns the real serializer tables, and these should be replaced by it
//! rather than extended by hand. Until then the open-registry rule from
//! [`crate::shape`] applies twice over — a kind outside these definitions is
//! not wrong, it is simply unchecked, because no one here can say what keys
//! it was meant to take.
//!
//! # Unknown keys are preserved verbatim
//!
//! The audit reports unknown keys; it does not remove them. The raw document
//! travels with every resource unchanged ([`crate::Resource::value`]), so a
//! finding about `predicat` and the file still containing `predicat` are the
//! same fact seen twice, never two versions of one file.

use crate::finding::Finding;
use crate::json;
use crate::registry::RegistryId;
use crate::shape::{LootNode, LootTableSkeleton};
use crate::{LoadedData, ResourceLocation};

/// One built-in serializer: its id and the top-level keys it reads.
///
/// Key order in the slice is alphabetical for stable suggestions; the order
/// carries no meaning about the format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SerializerDef {
    /// The `type` / `condition` / `function` id, `minecraft:` included.
    pub id: &'static str,
    /// Every top-level key the built-in reads, including the id key itself.
    pub keys: &'static [&'static str],
}

/// The loot conditions vanilla 1.21.1 ships, with the keys each one reads.
pub const CONDITION_DEFS: &[SerializerDef] = &[
    SerializerDef {
        id: "minecraft:all_of",
        keys: &["condition", "terms"],
    },
    SerializerDef {
        id: "minecraft:any_of",
        keys: &["condition", "terms"],
    },
    SerializerDef {
        id: "minecraft:block_state_property",
        keys: &["block", "condition", "properties"],
    },
    SerializerDef {
        id: "minecraft:damage_source_properties",
        keys: &["condition", "predicate"],
    },
    SerializerDef {
        id: "minecraft:enchantment_active_check",
        keys: &["active", "condition"],
    },
    SerializerDef {
        id: "minecraft:entity_properties",
        keys: &["condition", "entity", "predicate"],
    },
    SerializerDef {
        id: "minecraft:entity_scores",
        keys: &["condition", "entity", "scores"],
    },
    SerializerDef {
        id: "minecraft:inverted",
        keys: &["condition", "term"],
    },
    // `inverse` was removed in 1.19.4; a pack carrying it is owed the news.
    SerializerDef {
        id: "minecraft:killed_by_player",
        keys: &["condition"],
    },
    SerializerDef {
        id: "minecraft:location_check",
        keys: &["condition", "offsetX", "offsetY", "offsetZ", "predicate"],
    },
    SerializerDef {
        id: "minecraft:match_tool",
        keys: &["condition", "predicate"],
    },
    SerializerDef {
        id: "minecraft:random_chance",
        keys: &["chance", "condition"],
    },
    SerializerDef {
        id: "minecraft:random_chance_with_enchanted_bonus",
        keys: &[
            "enchanted_chance",
            "enchantment",
            "condition",
            "unenchanted_chance",
        ],
    },
    SerializerDef {
        id: "minecraft:reference",
        keys: &["condition", "name"],
    },
    SerializerDef {
        id: "minecraft:survives_explosion",
        keys: &["condition"],
    },
    SerializerDef {
        id: "minecraft:table_bonus",
        keys: &["chances", "condition", "enchantment"],
    },
    SerializerDef {
        id: "minecraft:time_check",
        keys: &["condition", "period", "value"],
    },
    SerializerDef {
        id: "minecraft:value_check",
        keys: &["condition", "expected", "value"],
    },
    SerializerDef {
        id: "minecraft:weather_check",
        keys: &["condition", "raining", "thundering"],
    },
];

/// The loot functions vanilla 1.21.1 ships, with the keys each one reads.
pub const FUNCTION_DEFS: &[SerializerDef] = &[
    SerializerDef {
        id: "minecraft:apply_bonus",
        keys: &[
            "enchantment",
            "formula",
            "function",
            "parameters",
            "conditions",
        ],
    },
    SerializerDef {
        id: "minecraft:copy_components",
        keys: &["function", "include", "source", "conditions"],
    },
    SerializerDef {
        id: "minecraft:copy_nbt",
        keys: &["function", "ops", "source", "target", "conditions"],
    },
    SerializerDef {
        id: "minecraft:copy_state",
        keys: &["block", "function", "properties", "conditions"],
    },
    SerializerDef {
        id: "minecraft:enchanted_count_increase",
        keys: &["count", "enchantment", "function", "limit", "conditions"],
    },
    SerializerDef {
        id: "minecraft:enchant_randomly",
        keys: &["function", "options", "conditions"],
    },
    SerializerDef {
        id: "minecraft:enchant_with_levels",
        keys: &["function", "levels", "options", "conditions"],
    },
    SerializerDef {
        id: "minecraft:exploration_map",
        keys: &[
            "decoration",
            "destination",
            "function",
            "search_radius",
            "skip_existing_chunks",
            "zoom",
            "conditions",
        ],
    },
    SerializerDef {
        id: "minecraft:explosion_decay",
        keys: &["function", "conditions"],
    },
    SerializerDef {
        id: "minecraft:fill_player_head",
        keys: &["entity", "function", "conditions"],
    },
    SerializerDef {
        id: "minecraft:furnace_smelt",
        keys: &["function", "conditions"],
    },
    SerializerDef {
        id: "minecraft:limit_count",
        keys: &["function", "limit", "conditions"],
    },
    SerializerDef {
        id: "minecraft:modify_contents",
        keys: &["component", "function", "modifier", "conditions"],
    },
    // The one function that takes sub-functions instead of acting itself.
    SerializerDef {
        id: "minecraft:sequence",
        keys: &["function", "functions", "conditions"],
    },
    SerializerDef {
        id: "minecraft:set_attributes",
        keys: &["function", "modifiers", "conditions"],
    },
    SerializerDef {
        id: "minecraft:set_banner_pattern",
        keys: &["append", "function", "patterns", "conditions"],
    },
    SerializerDef {
        id: "minecraft:set_components",
        keys: &["components", "function", "conditions"],
    },
    SerializerDef {
        id: "minecraft:set_contents",
        keys: &["entries", "function", "type", "conditions"],
    },
    SerializerDef {
        id: "minecraft:set_count",
        keys: &["add", "count", "function", "conditions"],
    },
    SerializerDef {
        id: "minecraft:set_custom_data",
        keys: &["function", "tag", "conditions"],
    },
    SerializerDef {
        id: "minecraft:set_custom_model_data",
        keys: &["function", "value", "conditions"],
    },
    SerializerDef {
        id: "minecraft:set_damage",
        keys: &["add", "damage", "function", "conditions"],
    },
    SerializerDef {
        id: "minecraft:set_enchantments",
        keys: &["add", "enchantments", "function", "conditions"],
    },
    SerializerDef {
        id: "minecraft:set_instrument",
        keys: &["function", "options", "conditions"],
    },
    SerializerDef {
        id: "minecraft:set_loot_table",
        keys: &["function", "name", "seed", "conditions"],
    },
    SerializerDef {
        id: "minecraft:set_lore",
        keys: &["entity", "function", "lore", "conditions"],
    },
    SerializerDef {
        id: "minecraft:set_name",
        keys: &["function", "name", "target", "conditions"],
    },
    SerializerDef {
        id: "minecraft:set_ominous_bottle_amplifier",
        keys: &["amplifier", "function", "conditions"],
    },
    // Renamed from `potion` to `id` in 1.20.5; packs in the wild carry both.
    SerializerDef {
        id: "minecraft:set_potion",
        keys: &["function", "id", "conditions"],
    },
    SerializerDef {
        id: "minecraft:set_stew_effect",
        keys: &["effects", "function", "conditions"],
    },
    SerializerDef {
        id: "minecraft:toggle_tooltips",
        keys: &["function", "toggles", "conditions"],
    },
];

/// The definition for one condition id, if the baseline covers it.
pub fn condition_def(id: &str) -> Option<&'static SerializerDef> {
    CONDITION_DEFS.iter().find(|def| def.id == id)
}

/// The definition for one function id, if the baseline covers it.
pub fn function_def(id: &str) -> Option<&'static SerializerDef> {
    FUNCTION_DEFS.iter().find(|def| def.id == id)
}

/// The baseline serializer ids as a [`Vocabulary`](crate::vocabulary::Vocabulary),
/// over two pseudo-registries named `loot_condition` and `loot_function`.
///
/// This is the marked-baseline half of the module: a provider built from the
/// static tables above so a caller can ask "is `somemod:fancy_roll` one of
/// the kinds vanilla 1.21.1 ships?" through the same [`crate::Vocabulary`]
/// seam everything else uses. It answers from these tables and nothing else —
/// when dust-registry lands, its serializer tables replace this provider
/// rather than extending it, and anything chained behind it stops being asked
/// about ids these tables already cover.
pub fn kind_vocabulary() -> crate::vocabulary::KnownNames {
    let mut names = crate::vocabulary::KnownNames::new();
    names = names.with(
        "loot_condition",
        CONDITION_DEFS
            .iter()
            .filter_map(|def| ResourceLocation::parse(def.id).ok()),
    );
    names = names.with(
        "loot_function",
        FUNCTION_DEFS
            .iter()
            .filter_map(|def| ResourceLocation::parse(def.id).ok()),
    );
    names
}

/// Audit every loaded loot table against the baseline definitions.
///
/// For each condition and function whose id is covered, keys outside the
/// definition become warnings naming the key and suggesting the nearest
/// known one; ids outside the baseline draw no findings at all, because an
/// unchecked kind is not a broken one — see the module documentation.
/// Everything reported stays in the data verbatim; the audit only talks.
pub fn audit(data: &LoadedData) -> Vec<Finding> {
    let mut findings = Vec::new();
    let Some(registry) = data.registry(&RegistryId::new("loot_table")) else {
        return findings;
    };

    for (name, resource) in registry {
        let table = LootTableSkeleton::from_raw(&resource.value);
        let (pack, file) = (&resource.pack, &resource.file);
        walk_nodes(
            Role::Condition,
            &table.conditions,
            pack,
            file,
            name,
            &mut findings,
        );
        walk_nodes(
            Role::Function,
            &table.functions,
            pack,
            file,
            name,
            &mut findings,
        );
        for pool in &table.pools {
            walk_nodes(
                Role::Condition,
                &pool.conditions,
                pack,
                file,
                name,
                &mut findings,
            );
            walk_nodes(
                Role::Function,
                &pool.functions,
                pack,
                file,
                name,
                &mut findings,
            );
            for entry in &pool.entries {
                walk_entry(entry, pack, file, name.clone(), &mut findings);
            }
        }
    }

    findings
}

/// Which list a node was found on, which decides which table answers for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Condition,
    Function,
}

fn walk_nodes(
    role: Role,
    nodes: &[LootNode],
    pack: &str,
    file: &str,
    name: &ResourceLocation,
    findings: &mut Vec<Finding>,
) {
    for node in nodes {
        check_node(role, node, pack, file, name.clone(), findings);
    }
}

fn walk_entry(
    entry: &LootNode,
    pack: &str,
    file: &str,
    name: ResourceLocation,
    findings: &mut Vec<Finding>,
) {
    let mut walk = |role: Role, nodes: &[LootNode]| {
        for node in nodes {
            check_node(role, node, pack, file, name.clone(), findings);
        }
    };
    walk(Role::Condition, &entry.conditions);
    walk(Role::Function, &entry.functions);
    for child in &entry.children {
        walk_entry(child, pack, file, name.clone(), findings);
    }
}

fn check_node(
    role: Role,
    node: &LootNode,
    pack: &str,
    file: &str,
    name: ResourceLocation,
    findings: &mut Vec<Finding>,
) {
    // Conditions and functions may themselves carry conditions/functions
    // (an `apply_bonus` under a `killed_by_player` gate), so recurse before
    // or after checking this node — order does not matter for correctness,
    // only that every node is visited exactly once.
    let walk = |r: Role, nodes: &[LootNode], f: &mut Vec<Finding>| {
        for inner in nodes {
            check_node(r, inner, pack, file, name.clone(), f);
        }
    };
    walk(Role::Condition, &node.conditions, findings);
    walk(Role::Function, &node.functions, findings);

    let (kind_key, def) = match role {
        Role::Condition => ("condition", condition_def(&node.kind)),
        Role::Function => ("function", function_def(&node.kind)),
    };
    let Some(def) = def else {
        // Outside the baseline: no contract to check against, and saying so
        // per node would put a line on every modded roll.
        return;
    };
    let Some(object) = node.raw.as_object() else {
        // The skeleton already tolerates non-object nodes; nothing with keys
        // to check lives in them.
        return;
    };

    for finding in json::unknown_keys(
        object,
        def.keys,
        pack,
        file,
        &format!("the {kind_key} `{}`", node.kind),
    ) {
        findings.push(finding.about(name.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::MemPack;
    use crate::{load, LoadOptions, PackSource, Severity};

    fn audited(files: &[(&str, &str)]) -> (LoadedData, Vec<Finding>) {
        let pack = MemPack::with_meta("looty", files);
        let refs: Vec<&dyn PackSource> = vec![&pack];
        let data = load(&refs, &LoadOptions::default());
        let findings = audit(&data);
        (data, findings)
    }

    fn loot_table(body: &'static str) -> (&'static str, &'static str) {
        ("data/minecraft/loot_table/blocks/test.json", body)
    }

    #[test]
    fn every_baseline_condition_and_function_definition_is_in_the_kind_tables() {
        // A definition for a kind the shape tables do not know would make the
        // two halves of the vocabulary disagree about what vanilla ships.
        for def in CONDITION_DEFS {
            assert!(
                crate::shape::LOOT_CONDITION_KINDS.contains(&def.id),
                "{} is defined but not a known kind",
                def.id
            );
        }
        for def in FUNCTION_DEFS {
            assert!(
                crate::shape::LOOT_FUNCTION_KINDS.contains(&def.id),
                "{} is defined but not a known kind",
                def.id
            );
        }
    }

    #[test]
    fn every_definition_reads_the_key_that_names_it() {
        // A condition is identified by `condition`, a function by `function`;
        // a definition missing its own id key would flag every well-formed
        // use of the kind.
        for def in CONDITION_DEFS {
            assert!(
                def.keys.contains(&"condition"),
                "{} must list `condition`",
                def.id
            );
        }
        for def in FUNCTION_DEFS {
            assert!(
                def.keys.contains(&"function"),
                "{} must list `function`",
                def.id
            );
        }
    }

    #[test]
    fn correct_keys_draw_no_findings() {
        let (_, findings) = audited(&[loot_table(
            r#"{"type":"minecraft:block","pools":[{
                "rolls":1,
                "conditions":[{"condition":"minecraft:survives_explosion"}],
                "entries":[{
                    "type":"minecraft:item",
                    "name":"minecraft:stone",
                    "conditions":[
                        {"condition":"minecraft:random_chance","chance":0.5},
                        {"condition":"minecraft:killed_by_player"}
                    ],
                    "functions":[
                        {"function":"minecraft:apply_bonus","enchantment":"minecraft:fortune","formula":"minecraft:ore_drops"},
                        {"function":"minecraft:furnace_smelt"},
                        {"function":"minecraft:set_count","count":{"min":1,"max":3},"add":false}
                    ]
                }]
            }]}"#,
        )]);
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn the_enchantment_conditioned_chances_are_read_as_written() {
        let (_, findings) = audited(&[loot_table(
            r#"{"type":"minecraft:block","pools":[{
                "rolls":1,
                "entries":[{
                    "type":"minecraft:item","name":"minecraft:diamond",
                    "conditions":[
                        {"condition":"minecraft:match_tool","predicate":{"enchantments":[]}},
                        {"condition":"minecraft:table_bonus","enchantment":"minecraft:fortune","chances":[0.1,0.2]},
                        {"condition":"minecraft:random_chance_with_enchanted_bonus",
                         "unenchanted_chance":0.01,"enchanted_chance":0.05,"enchantment":"minecraft:fortune"}
                    ]
                }]
            }]}"#,
        )]);
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn a_misspelled_key_is_a_warning_that_suggests_the_real_one() {
        let (_, findings) = audited(&[loot_table(
            r#"{"type":"minecraft:block","pools":[{"rolls":1,"entries":[{
                "type":"minecraft:item","name":"minecraft:stone",
                "functions":[{"function":"minecraft:set_count","conut":3}]
            }]}]}"#,
        )]);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].severity, Severity::Warning);
        assert!(findings[0].message.contains("`conut`"), "{}", findings[0]);
        assert!(
            findings[0].message.contains("Did you mean `count`?"),
            "{}",
            findings[0]
        );
        assert!(findings[0].subject.is_some());
    }

    #[test]
    fn a_removed_legacy_key_is_reported_like_any_unknown() {
        // `inverse` died with 1.19.4; the modern condition takes nothing but
        // `condition`. A pack carrying the old spelling gets told.
        let (_, findings) = audited(&[loot_table(
            r#"{"type":"minecraft:block","pools":[{"rolls":1,"entries":[{
                "type":"minecraft:item","name":"minecraft:stone",
                "conditions":[{"condition":"minecraft:killed_by_player","inverse":true}]
            }]}]}"#,
        )]);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert!(findings[0].message.contains("`inverse`"), "{}", findings[0]);
    }

    #[test]
    fn kinds_outside_the_baseline_are_not_policed() {
        // A modded condition with modded keys is nobody's typo. This is the
        // line that keeps the audit from becoming noise on modpacks.
        let (_, findings) = audited(&[loot_table(
            r#"{"type":"minecraft:block","pools":[{"rolls":1,"entries":[{
                "type":"minecraft:item","name":"minecraft:stone",
                "conditions":[{"condition":"somemod:magic_roll","sparkles":true,"moon_phase":3}]
            }]}]}"#,
        )]);
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn conditions_nested_inside_functions_are_reached() {
        // An `apply_bonus` gated by a misspelled condition: the walk goes
        // through the function's own `conditions` list.
        let (_, findings) = audited(&[loot_table(
            r#"{"type":"minecraft:block","pools":[{"rolls":1,"entries":[{
                "type":"minecraft:item","name":"minecraft:coal",
                "functions":[{
                    "function":"minecraft:apply_bonus",
                    "enchantment":"minecraft:fortune","formula":"minecraft:ore_drops",
                    "conditions":[{"condition":"minecraft:random_chance","odds":0.5}]
                }]
            }]}]}"#,
        )]);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert!(findings[0].message.contains("`odds`"), "{}", findings[0]);
    }

    #[test]
    fn alternatives_children_are_walked_too() {
        let (_, findings) = audited(&[loot_table(
            r#"{"type":"minecraft:block","pools":[{"rolls":1,"entries":[{
                "type":"minecraft:alternatives",
                "children":[
                    {"type":"minecraft:item","name":"minecraft:iron_ore",
                     "conditions":[{"condition":"minecraft:match_tool","predicat":{}}]},
                    {"type":"minecraft:item","name":"minecraft:raw_iron"}
                ]
            }]}]}"#,
        )]);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert!(
            findings[0].message.contains("Did you mean `predicate`?"),
            "{}",
            findings[0]
        );
    }

    #[test]
    fn the_audit_never_touches_the_data_it_reports_on() {
        let (data, _) = audited(&[loot_table(
            r#"{"type":"minecraft:block","pools":[{"rolls":1,"entries":[{
                "type":"minecraft:item","name":"minecraft:stone",
                "functions":[{"function":"minecraft:set_count","conut":9}]
            }]}]}"#,
        )]);
        let resource = data
            .get(
                &RegistryId::new("loot_table"),
                &ResourceLocation::parse("minecraft:blocks/test").unwrap(),
            )
            .expect("loaded");
        let raw_text = serde_json::to_string(&resource.value).unwrap();
        assert!(
            raw_text.contains("\"conut\":9") || raw_text.contains("\"conut\": 9"),
            "{raw_text}"
        );
    }
}
