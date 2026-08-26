//! The vanilla tag baseline, checked against the tables it groups.
//!
//! Every plain member of every tag was verified at extraction against its
//! registry's table; these tests re-run the same membership question through
//! the public API, so the generated rows and the crate's own lookups cannot
//! drift apart. References are checked to resolve inside the table — nothing
//! dangles — and the named facts are written out directly, because a tag
//! baseline that quietly stopped saying what water is would still pass every
//! structural check in this file.

use dust_registry::generated::tags::TAGS;
use dust_registry::tags::{self, TagRegistry};
use dust_registry::{Block, EntityType, Fluid, Item, Registry, DATA_VERSION};

fn contains_member(tag: &tags::TagDef, member: &str) -> bool {
    // A `#` reference resolves inside the table itself; a plain id must be
    // something the extracted registries actually hold.
    // A reference always names a tag of the *same* registry as the tag
    // holding it, so resolution needs no namespace parsing at all.
    if let Some(referenced) = member.strip_prefix('#') {
        tags::from_id(tag.registry, referenced).is_some()
    } else {
        match tag.registry {
            TagRegistry::Block => Block::from_name(member).is_some(),
            TagRegistry::Item => Item::from_name(member).is_some(),
            TagRegistry::Fluid => Fluid::from_name(member).is_some(),
            TagRegistry::EntityType => EntityType::from_name(member).is_some(),
            TagRegistry::GameEvent => Registry::from_name("minecraft:game_event")
                .and_then(|r| r.entry_id(member))
                .is_some(),
        }
    }
}

#[test]
fn every_tag_is_sorted_and_findable_by_its_own_id() {
    // Sorted by the registry *names*, not by the enum's declaration order:
    // `from_id` binary-searches over exactly those two strings, so the order
    // the strings imply has to be the order the rows sit in.
    assert!(TAGS.windows(2).all(|pair| {
        (pair[0].registry.name(), pair[0].id) < (pair[1].registry.name(), pair[1].id)
    }));
    for registry in TagRegistry::ALL {
        for tag in tags::by_registry(registry) {
            assert_eq!(tags::from_id(registry, tag.id), Some(tag));
        }
    }
    assert_eq!(
        tags::from_id(TagRegistry::Block, "minecraft:not_a_tag"),
        None
    );
}

#[test]
fn members_are_sorted_so_contains_can_binary_search() {
    for tag in TAGS {
        assert!(
            tag.members.windows(2).all(|pair| pair[0] < pair[1]),
            "{} is unsorted",
            tag.id
        );
        // And the search answers only about what the row holds.
        assert!(!tag.contains("minecraft:definitely_not_a_member"));
    }
}

#[test]
fn every_membership_resolves_through_the_public_tables() {
    let mut memberships = 0usize;
    for tag in TAGS {
        for member in tag.members {
            assert!(
                contains_member(tag, member),
                "{} holds {member}, which no table accounts for",
                tag.id
            );
            memberships += 1;
        }
    }
    // 3,038 entries were read; both `sand` tags listed `suspicious_sand`
    // twice and a tag is a set, so the table holds two fewer.
    assert_eq!(
        memberships, 3036,
        "the number of memberships moved without anybody noticing"
    );

    // The collapse itself, kept as a named fact.
    let sand = tags::from_id(TagRegistry::Block, "minecraft:sand").expect("exists");
    assert_eq!(sand.members.len(), 3);
}

#[test]
fn every_reference_resolves_to_a_tag_of_the_same_registry() {
    let references = TAGS.iter().flat_map(TagDef::references).count();
    assert!(references > 0, "no references were found any more");
    for tag in TAGS {
        for reference in tag.references() {
            let body = reference.strip_prefix('#').expect("starts with #");
            assert!(
                body.starts_with("minecraft:"),
                "{} references {} outside the minecraft namespace",
                tag.id,
                reference
            );
            assert!(
                tags::from_id(tag.registry, body).is_some(),
                "{} references {}, which does not exist",
                tag.id,
                reference
            );
        }
    }
}

use dust_registry::tags::TagDef;

#[test]
fn the_named_facts_about_vanilla_tags_are_still_true() {
    // The fluid water tag is exactly both halves of the water.
    let water = tags::from_id(TagRegistry::Fluid, "minecraft:water").expect("exists");
    assert_eq!(
        water.members,
        ["minecraft:flowing_water", "minecraft:water"]
    );

    // Pickaxes mine stone. If the extraction ever started reading the wrong
    // array, this is where somebody notices first.
    let pickaxe = tags::from_id(TagRegistry::Block, "minecraft:mineable/pickaxe").expect("exists");
    assert!(pickaxe.contains("minecraft:stone"));
    assert!(pickaxe.contains("minecraft:deepslate"));
    assert!(pickaxe.members.len() > 300);

    // Allays do not take fall damage.
    let immune =
        tags::from_id(TagRegistry::EntityType, "minecraft:fall_damage_immune").expect("exists");
    assert!(immune.contains("minecraft:allay"));
    assert!(!immune.contains("minecraft:pig"));

    // Arrows are arrows.
    let arrows = tags::from_id(TagRegistry::Item, "minecraft:arrows").expect("exists");
    assert_eq!(
        arrows.members,
        [
            "minecraft:arrow",
            "minecraft:spectral_arrow",
            "minecraft:tipped_arrow"
        ]
    );
}

#[test]
fn the_five_registries_are_exactly_the_five_taken() {
    assert_eq!(TagRegistry::ALL.len(), 5);
    let block_count = tags::by_registry(TagRegistry::Block).count();
    assert_eq!(block_count, 184);
    assert_eq!(tags::by_registry(TagRegistry::Item).count(), 147);
    assert_eq!(tags::by_registry(TagRegistry::Fluid).count(), 2);
    assert_eq!(tags::by_registry(TagRegistry::EntityType).count(), 34);
    assert_eq!(tags::by_registry(TagRegistry::GameEvent).count(), 5);
    assert_eq!(TAGS.len(), 372);
}

#[test]
fn the_table_says_which_version_it_came_from() {
    assert_eq!(DATA_VERSION, "1.21.1");
    assert_eq!(dust_registry::generated::tags::DATA_VERSION, DATA_VERSION);
}
