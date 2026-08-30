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
            // The four with a dedicated type in this crate.
            TagRegistry::Block => Block::from_name(member).is_some(),
            TagRegistry::Item => Item::from_name(member).is_some(),
            TagRegistry::Fluid => Fluid::from_name(member).is_some(),
            TagRegistry::EntityType => EntityType::from_name(member).is_some(),
            // Code registries: a protocol id compiled into the game, so the
            // registry report is the table.
            TagRegistry::GameEvent
            | TagRegistry::PointOfInterestType
            | TagRegistry::CatVariant
            | TagRegistry::Instrument => Registry::from_name(tag.registry.name())
                .and_then(|r| r.entry_id(member))
                .is_some(),
            // Datapack registries: no protocol id at all, so the table is the
            // synced names — which is also where the id in the packet comes
            // from, so a membership and an id cannot disagree about what
            // exists.
            TagRegistry::Biome
            | TagRegistry::Enchantment
            | TagRegistry::DamageType
            | TagRegistry::BannerPattern
            | TagRegistry::PaintingVariant => dust_registry::synced::by_name(tag.registry.name())
                .and_then(|r| r.id_of(member))
                .is_some(),
        }
    }
}

/// The thirteen registries and their tag counts, as a real 1.21.1 server sent
/// them in one 25,200-byte `update_tags`.
///
/// A fixture off the wire rather than a computation over `TAGS`, for the usual
/// reason: a table built from the wrong directory agrees with itself perfectly.
const CAPTURED: &[(&str, usize)] = &[
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

#[test]
fn the_thirteen_registries_and_their_counts_are_the_ones_a_real_server_sent() {
    assert_eq!(
        TagRegistry::ALL.len(),
        CAPTURED.len(),
        "thirteen registries"
    );
    for (registry, (name, count)) in TagRegistry::ALL.into_iter().zip(CAPTURED) {
        assert_eq!(&registry.name(), name, "in the order the server sent them");
        assert_eq!(
            tags::by_registry(registry).count(),
            *count,
            "{name} tag count against the capture"
        );
    }
    assert_eq!(TAGS.len(), CAPTURED.iter().map(|(_, n)| n).sum::<usize>());
}

#[test]
fn every_registry_name_round_trips() {
    for registry in TagRegistry::ALL {
        assert_eq!(TagRegistry::from_name(registry.name()), Some(registry));
    }
    assert_eq!(TagRegistry::from_name("minecraft:not_a_registry"), None);
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
    // 3,809 entries were read across the thirteen directories; both `sand`
    // tags listed `suspicious_sand` twice and a tag is a set, so the table
    // holds two fewer. This is the *stored* count — plain members plus `#`
    // references — and not what goes on the wire; see
    // `the_flattened_membership_counts_are_the_ones_a_real_server_sent`.
    assert_eq!(
        memberships, 3807,
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
fn the_thirteen_registries_are_exactly_the_thirteen_taken() {
    // The per-registry counts live in
    // `the_thirteen_registries_and_their_counts_are_the_ones_a_real_server_sent`,
    // against the capture. What is left here is the total and the shape: every
    // registry the enum names holds at least one tag, so a variant added
    // without an extraction behind it fails rather than reads as empty.
    assert_eq!(TagRegistry::ALL.len(), 13);
    for registry in TagRegistry::ALL {
        assert!(
            tags::by_registry(registry).count() > 0,
            "{} names no tags",
            registry.name()
        );
    }
    assert_eq!(TAGS.len(), 514);
}

#[test]
fn the_table_says_which_version_it_came_from() {
    assert_eq!(DATA_VERSION, "1.21.1");
    assert_eq!(dust_registry::generated::tags::DATA_VERSION, DATA_VERSION);
}

/// The memberships a real 1.21.1 server put on the wire, per registry.
///
/// The stored form is not the wire form: a tag file may name another tag, and
/// the client is sent a flat list of ids. Vanilla's own files carry 3,655
/// plain members and 154 references between them; flattened, that is the 6,362
/// below. **This is the check that says the flattening is right**, and it is
/// the only one available — a resolver tested against its own idea of what a
/// reference means would agree with itself under any definition.
const CAPTURED_ENTRIES: &[(&str, usize)] = &[
    ("minecraft:block", 3289),
    ("minecraft:entity_type", 252),
    ("minecraft:worldgen/biome", 554),
    ("minecraft:game_event", 119),
    ("minecraft:item", 1512),
    ("minecraft:point_of_interest_type", 30),
    ("minecraft:enchantment", 301),
    ("minecraft:fluid", 4),
    ("minecraft:damage_type", 176),
    ("minecraft:banner_pattern", 42),
    ("minecraft:cat_variant", 21),
    ("minecraft:instrument", 16),
    ("minecraft:painting_variant", 46),
];

#[test]
fn the_flattened_membership_counts_are_the_ones_a_real_server_sent() {
    let mut total = 0;
    for (registry, (name, expected)) in TagRegistry::ALL.into_iter().zip(CAPTURED_ENTRIES) {
        assert_eq!(&registry.name(), name);
        let wire = tags::wire(registry).expect("every tag resolves");
        let found: usize = wire.iter().map(|tag| tag.entries.len()).sum();
        assert_eq!(found, *expected, "{name} flattened memberships");
        total += found;
    }
    assert_eq!(
        total, 6362,
        "the whole packet, as it was counted off the wire"
    );
}

#[test]
fn a_reference_becomes_the_ids_it_points_at() {
    // `minecraft:logs` is four other tags and no plain member at all. On the
    // wire it is every log, which is what makes an axe work on all of them.
    let stored = tags::from_id(TagRegistry::Block, "minecraft:logs").expect("a vanilla tag");
    assert!(
        stored.members.iter().all(|m| m.starts_with('#')),
        "minecraft:logs is references and nothing else"
    );
    let wire = tags::wire(TagRegistry::Block).expect("resolves");
    let logs = wire
        .iter()
        .find(|tag| tag.id == "minecraft:logs")
        .expect("still there");
    assert!(logs.entries.len() > stored.members.len());
    let oak = Block::from_name("minecraft:oak_log").expect("a block");
    assert!(logs.entries.contains(&oak.protocol_id()));
}

#[test]
fn the_ids_of_every_tag_are_ascending_and_unique() {
    // The client builds a set, and two tags reaching one member through
    // different references is ordinary — `logs` and `logs_that_burn` overlap.
    for registry in TagRegistry::ALL {
        for tag in tags::wire(registry).expect("resolves") {
            assert!(
                tag.entries.windows(2).all(|pair| pair[0] < pair[1]),
                "{} repeats or misorders an id",
                tag.id
            );
        }
    }
}

#[test]
fn a_biome_tag_numbers_its_members_the_way_the_sync_does() {
    // The load-bearing one. A chunk's biome container and a biome tag both
    // carry ids, and both have to mean the same thing — the position in the
    // registry the server synced. Taken from anywhere else, the client would
    // have two meanings for one number.
    let wire = tags::wire(TagRegistry::Biome).expect("resolves");
    let overworld = wire
        .iter()
        .find(|tag| tag.id == "minecraft:is_overworld")
        .expect("a vanilla biome tag");
    let biomes = dust_registry::synced::by_name("minecraft:worldgen/biome").expect("synced");
    let plains = biomes.id_of("minecraft:plains").expect("plains is a biome") as u32;
    assert!(overworld.entries.contains(&plains));
    for id in &overworld.entries {
        assert!(
            (*id as usize) < biomes.entries.len(),
            "{id} is past the end of the synced registry"
        );
    }
}

#[test]
fn the_wire_order_is_the_same_every_time() {
    // The client builds a set and does not care, but two builds of this server
    // that disagreed about the order would produce a diff nobody could read
    // and a capture nobody could compare. Vanilla's own order is its map's,
    // which is why the byte streams are the same length and not the same
    // bytes — the contents were compared as sets against a real server and
    // matched exactly, all 514 tags and all 6,362 ids.
    for registry in TagRegistry::ALL {
        let once = tags::wire(registry).expect("resolves");
        let twice = tags::wire(registry).expect("resolves");
        assert_eq!(once, twice);
        assert!(
            once.windows(2).all(|pair| pair[0].id < pair[1].id),
            "{} is not in id order",
            registry.name()
        );
    }
}
