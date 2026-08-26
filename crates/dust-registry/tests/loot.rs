//! The loot inventory and vocabulary, with two readings that must agree.
//!
//! The inventory has no round-trip to offer either: a sorted list proves its
//! own sortedness and nothing else. What these tests check is the accounting —
//! totals that add up, categories that partition the set, vocabulary names
//! that are registered where they claim to be — and, the load-bearing part,
//! that [`VOCABULARY`] and [`SOURCE_COUNTS`] agree exactly on conditions and
//! functions. Those two tables were built by passes sharing nothing but the
//! files: one reads positions, one reads every string. A walker that skipped a
//! subtree, or invented uses by miscounting a provider's `type`, survives
//! every other test in here and fails that one.
//!
//! That was tested the way the registries were: by breaking the structured
//! walker on purpose so it stopped descending into pool entries. The named
//! facts moved (741 `survives_explosion` became fewer), and
//! [`the_two_readings_of_the_tree_agree_exactly`] failed against the untouched
//! string scan — while totals, categories and lookups stayed green, which is
//! exactly why the second reading exists.

use dust_registry::generated::loot::{CATEGORIES, SOURCE_COUNTS, TABLES, TABLE_COUNT, VOCABULARY};
use dust_registry::loot::{self, Kind};
use dust_registry::{Registry, DATA_VERSION};

#[test]
fn the_inventory_totals_add_up_and_the_table_is_sorted() {
    assert_eq!(TABLE_COUNT, TABLES.len());
    let named_categories: u32 = CATEGORIES.iter().map(|(_, count)| *count).sum();
    assert_eq!(
        named_categories as usize, TABLE_COUNT,
        "the categories do not partition the table list"
    );
    assert!(TABLES.windows(2).all(|pair| pair[0] < pair[1]));
    // Sortedness is what makes table_exists a binary search; this is what
    // makes it sound.
    assert_eq!(
        TABLES
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        TABLES.len(),
        "a table id appears twice"
    );
}

#[test]
fn lookups_find_tables_by_whole_namespaced_id() {
    assert!(loot::table_exists("minecraft:blocks/stone"));
    assert!(loot::table_exists("minecraft:entities/zombie"));
    // Bare paths and unknown ids both miss.
    assert!(!loot::table_exists("blocks/stone"));
    assert!(!loot::table_exists("minecraft:blocks/not_a_block"));
}

#[test]
fn every_vocabulary_name_is_registered_where_it_belongs() {
    let registries = [
        (Kind::Condition, "minecraft:loot_condition_type"),
        (Kind::Function, "minecraft:loot_function_type"),
        (Kind::Entry, "minecraft:loot_pool_entry_type"),
    ];
    for (kind, registry_name) in registries {
        let registry = Registry::from_name(registry_name).expect("extracted beside the vocabulary");
        for (name, _) in loot::vocabulary(kind) {
            assert!(
                registry.entry_id(name).is_some(),
                "{name} is used as a {kind:?} and is missing from {registry_name}"
            );
        }
    }
}

#[test]
fn the_two_readings_of_the_tree_agree_exactly() {
    // The test this file exists for. Conditions and functions are counted by
    // both passes; entries only by the structured one, because a bare string
    // scan cannot tell an entry's `type` from a provider's.
    for kind in [Kind::Condition, Kind::Function] {
        let structured: Vec<_> = loot::vocabulary(kind).collect();
        let scanned: Vec<&str> = SOURCE_COUNTS
            .iter()
            .filter(|(k, _, _)| *k == kind.name())
            .map(|(_, n, _)| *n)
            .collect();
        assert_eq!(
            structured.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            scanned,
            "{kind:?}: the two readings see different vocabularies"
        );
        for (name, uses) in &structured {
            assert_eq!(
                Some(*uses),
                loot::source_uses(kind, name),
                "{name}: the tallies disagree"
            );
        }
    }
    assert_eq!(
        VOCABULARY.len() - SOURCE_COUNTS.len(),
        loot::vocabulary(Kind::Entry).count(),
        "every non-source row should be an entry type"
    );
}

#[test]
fn the_named_facts_about_vanilla_loot_are_still_true() {
    assert_eq!(TABLE_COUNT, 1178);
    assert_eq!(
        loot::categories().first().copied(),
        Some(("minecraft:archaeology", 6))
    );
    assert_eq!(CATEGORIES.len(), 10);

    // The three heaviest hitters of each kind, written down so a change has to
    // explain itself rather than pass by agreement.
    assert_eq!(
        loot::uses(Kind::Condition, "minecraft:survives_explosion"),
        Some(741)
    );
    assert_eq!(
        loot::uses(Kind::Condition, "minecraft:match_tool"),
        Some(190)
    );
    assert_eq!(loot::uses(Kind::Function, "minecraft:set_count"), Some(776));
    assert_eq!(loot::uses(Kind::Entry, "minecraft:item"), Some(2160));

    // And an unknown name is not zero uses — it is no answer.
    assert_eq!(
        loot::uses(Kind::Condition, "minecraft:not_a_condition"),
        None
    );
    assert_eq!(
        loot::uses(Kind::Function, "minecraft:survives_explosion"),
        None,
        "a condition asked about as a function has no answer"
    );
}

#[test]
fn blocks_dominate_because_every_dropping_block_is_a_table() {
    let (_, blocks) = CATEGORIES
        .iter()
        .find(|(category, _)| *category == "minecraft:blocks")
        .expect("the blocks category exists");
    assert_eq!(*blocks, 982);
    assert!(*blocks > TABLE_COUNT as u32 / 2);
}

#[test]
fn the_table_says_which_version_it_came_from() {
    assert_eq!(DATA_VERSION, "1.21.1");
    assert_eq!(dust_registry::generated::loot::DATA_VERSION, DATA_VERSION);
}
