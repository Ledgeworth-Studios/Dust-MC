//! Entity types: the registry against the report, with total coverage.
//!
//! [`crate::EntityType`] is a newtype over the flat registry, and the shared
//! golden rows in `generated::registries` already sample it at six positions.
//! What this file adds is what `generated::entity_types` exists for: every one
//! of the 130 entries checked against rows walked out of the report's own map,
//! so no change to the shared sampling rule can quietly narrow what is
//! verified about the table the server will index mobs by.
//!
//! There is deliberately nothing here about bounding boxes or spawn
//! categories: 1.21.1's generators publish none of it. See that module's
//! header for what the gap means and when it closes.

use dust_registry::generated::entity_types::{DEFAULT_ENTITY, ENTITY_SAMPLES};
use dust_registry::{EntityType, Registry, DATA_VERSION};

#[test]
fn every_entity_decodes_and_encodes_as_the_report_states() {
    assert_eq!(ENTITY_SAMPLES.len(), EntityType::all().count());
    for &(name, id) in ENTITY_SAMPLES {
        let by_name = EntityType::from_name(name).unwrap_or_else(|| panic!("{name} absent"));
        let by_id = EntityType::from_protocol_id(id).unwrap_or_else(|| panic!("id {id} absent"));
        assert_eq!(
            by_name, by_id,
            "{name} and id {id} resolve to different entities"
        );
    }
}

#[test]
fn the_default_is_a_real_entity_and_the_one_minecraft_names() {
    // Written down rather than looked up in the same table twice: the constant
    // came from the report; resolving it goes through the generated table. A
    // default that drifted onto an entry that does not exist fails here.
    let default = EntityType::from_name(DEFAULT_ENTITY)
        .unwrap_or_else(|| panic!("{DEFAULT_ENTITY} is the default and not an entry"));
    assert_eq!(
        EntityType::registry().default_entry(),
        Some(default.name()),
        "the registry table and this module disagree about the default"
    );
}

#[test]
fn the_named_facts_about_this_registry_are_still_true() {
    assert_eq!(EntityType::all().count(), 130);
    assert_eq!(DEFAULT_ENTITY, "minecraft:pig");
    // First and last by protocol id, as the report has them.
    assert_eq!(
        EntityType::from_protocol_id(0).map(EntityType::name),
        Some("minecraft:allay")
    );
    assert_eq!(
        EntityType::from_protocol_id(129).map(EntityType::name),
        Some("minecraft:fishing_bobber")
    );
    assert_eq!(
        EntityType::from_protocol_id(124).map(EntityType::name),
        Some("minecraft:zombie")
    );
}

#[test]
fn the_registry_table_covers_the_same_set_the_samples_do() {
    // The samples are the report; the registry table is the extraction. If a
    // future run dropped an entry from either side, the two would stop being
    // the same set and this says which way they moved.
    let mut sampled: Vec<&str> = ENTITY_SAMPLES.iter().map(|(n, _)| *n).collect();
    sampled.sort_unstable();

    let mut tabled: Vec<&'static str> = EntityType::all().map(EntityType::name).collect();
    tabled.sort_unstable();

    assert_eq!(sampled, tabled);
}

#[test]
fn entities_are_reachable_through_the_generic_path_too() {
    // One fact, two doors. The dedicated module exists so this cannot be the
    // only check, but it should stay true anyway.
    let registry = Registry::from_name("minecraft:entity_type").expect("extracted");
    for &(name, id) in ENTITY_SAMPLES {
        assert_eq!(registry.entry_id(name), Some(id), "{name}");
    }
}

#[test]
fn the_table_says_which_version_it_came_from() {
    assert_eq!(DATA_VERSION, "1.21.1");
    assert_eq!(
        dust_registry::generated::entity_types::DATA_VERSION,
        DATA_VERSION
    );
}
