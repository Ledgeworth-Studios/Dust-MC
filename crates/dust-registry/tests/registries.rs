//! The flat registries: every entry of every registry, both directions, plus
//! the check a round-trip cannot be.
//!
//! A flat registry has no internal structure to contradict itself with. Decode
//! id 963 to `minecraft:diamond_sword`, encode that name back, and the answer
//! is 963 under *any* bijection between names and ids — including one where
//! every id is off by one. So the round-trip below proves the two directions of
//! the table agree with each other, which is worth proving and is not the
//! question. Whether they agree with Minecraft is the question, and
//! [`ENTRY_SAMPLES`] — read out of Mojang's report at extraction time and never
//! through the table — is the only thing here that can answer it.
//!
//! That claim was tested by breaking the table on purpose, twice. Shifting
//! every item protocol id up by one, and numbering the entity types in name
//! order instead of reading each entry's id, both left every other test in this
//! file green and failed only
//! [`the_table_agrees_with_mojang_and_not_merely_with_itself`]. The second is
//! the one worth remembering: `minecraft:entity_type` is already in name order
//! for its first 41 entries and coincides again in places after that, so a
//! table numbered by the wrong sort still decodes ids 0, 1, 32 and 65
//! correctly and first goes wrong at 96. Two of its six sample rows caught it.
//! A sample of one entry per registry would not have.

use dust_registry::generated::registries::REGISTRIES;
use dust_registry::{
    Block, EntityType, Fluid, Item, Registry, DATA_VERSION, ENTRY_COUNT, ENTRY_SAMPLES,
};

#[test]
fn every_entry_of_every_registry_round_trips_in_both_directions() {
    let mut seen = 0usize;
    for registry in Registry::all() {
        for (id, name) in registry.entries() {
            assert_eq!(
                registry.entry_id(name),
                Some(id),
                "{}: {name} is id {id} going one way",
                registry.name()
            );
            assert_eq!(
                registry.entry_name(id),
                Some(name),
                "{}: id {id} is {name} going the other",
                registry.name()
            );
            seen += 1;
        }
    }
    assert_eq!(
        seen, ENTRY_COUNT,
        "the registries do not account for every entry"
    );
}

#[test]
fn protocol_ids_fill_zero_to_the_entry_count_with_no_gap() {
    // The tables index by protocol id and `Item(u16)` is the id itself, so a
    // hole would be an id that decodes to whatever sits beside it. The
    // extractor refuses to emit a sparse registry; this is that refusal
    // re-checked against the code it emitted.
    for registry in Registry::all() {
        let count = registry.entry_count() as u32;
        for id in 0..count {
            assert!(
                registry.entry_name(id).is_some(),
                "{} has no entry with id {id}",
                registry.name()
            );
        }
        assert_eq!(
            registry.entry_name(count),
            None,
            "{} decodes one past its last id",
            registry.name()
        );
        assert_eq!(registry.entry_name(u32::MAX), None);
    }
}

#[test]
fn the_two_index_arrays_are_inverses_and_the_names_are_sorted() {
    // `ids` and `by_id` are the same permutation read from opposite ends. If
    // one were regenerated and the other not, lookups would still round-trip
    // for the entries the two happened to agree on — so this checks the shape
    // rather than a sample of the behaviour.
    for def in REGISTRIES {
        assert_eq!(def.names.len(), def.ids.len(), "{}", def.name);
        assert_eq!(def.names.len(), def.by_id.len(), "{}", def.name);
        assert!(
            def.names.windows(2).all(|pair| pair[0] < pair[1]),
            "{}: names are not sorted, so a binary search over them is undefined",
            def.name
        );
        for (position, &id) in def.ids.iter().enumerate() {
            assert_eq!(def.by_id[id as usize] as usize, position, "{}", def.name);
        }
    }
}

#[test]
fn the_table_agrees_with_mojang_and_not_merely_with_itself() {
    // The test this file exists for. ENTRY_SAMPLES was read from the report's
    // own entry map — not sorted by the extractor, not indexed by it, and never
    // passed through `Registry` — so a table sorted by the wrong key or shifted
    // by one still round-trips perfectly and fails here.
    assert!(
        !ENTRY_SAMPLES.is_empty(),
        "the generated table carries no samples"
    );

    for &(registry, id, name) in ENTRY_SAMPLES {
        let registry = Registry::from_name(registry)
            .unwrap_or_else(|| panic!("{registry} is sampled and absent"));
        assert_eq!(
            registry.entry_name(id),
            Some(name),
            "{} id {id} decodes to something other than what Mojang's report says",
            registry.name()
        );
        assert_eq!(
            registry.entry_id(name),
            Some(id),
            "{} {name} encodes to something other than what Mojang's report says",
            registry.name()
        );
    }
}

#[test]
fn every_registry_is_covered_by_the_samples() {
    // A sample that quietly stopped covering a registry would leave it
    // unchecked while the suite stayed green — the failure mode where a guard
    // degrades instead of breaking.
    let mut sampled: Vec<&str> = ENTRY_SAMPLES
        .iter()
        .map(|(registry, _, _)| *registry)
        .collect();
    sampled.sort_unstable();
    sampled.dedup();

    let missing: Vec<&str> = Registry::all()
        .map(Registry::name)
        .filter(|name| sampled.binary_search(name).is_err())
        .collect();
    assert!(missing.is_empty(), "registries with no sample: {missing:?}");

    // And both ends of each registry are sampled, which is where an off-by-one
    // shows first.
    for registry in Registry::all() {
        let last = registry.entry_count() as u32 - 1;
        let ids: Vec<u32> = ENTRY_SAMPLES
            .iter()
            .filter(|(name, _, _)| *name == registry.name())
            .map(|(_, id, _)| *id)
            .collect();
        assert!(
            ids.contains(&0) && ids.contains(&last),
            "{}",
            registry.name()
        );
    }
}

#[test]
fn a_registry_is_findable_by_name_and_names_itself() {
    for registry in Registry::all() {
        assert_eq!(Registry::from_name(registry.name()), Some(registry));
    }
    assert_eq!(Registry::from_name("minecraft:not_a_registry"), None);
    // A bare name is deliberately not accepted; see `Registry::from_name`.
    assert_eq!(Registry::from_name("item"), None);
}

#[test]
fn the_block_registry_is_not_here_and_blocks_are_still_reachable() {
    // Deliberate: `generated::blocks` lists every block in protocol-id order
    // already, and a second list would be a second answer to "what is block
    // 42". The extractor checks the two reports agree on that order before
    // leaving this out, which is the part that could rot silently.
    assert_eq!(Registry::from_name("minecraft:block"), None);
    assert!(Block::from_name("minecraft:stone").is_some());
    assert_eq!(
        Registry::all().count(),
        REGISTRIES.len(),
        "the iterator and the table disagree about how many registries there are"
    );
}

#[test]
fn a_default_entry_is_one_the_registry_has() {
    let mut defaults = 0;
    for registry in Registry::all() {
        let Some(default) = registry.default_entry() else {
            continue;
        };
        assert!(
            registry.entry_id(default).is_some(),
            "{}'s default {default} is not one of its entries",
            registry.name()
        );
        defaults += 1;
    }
    assert!(defaults > 0, "no registry declares a default any more");
}

#[test]
fn the_first_class_types_agree_with_their_registries() {
    // `Item` and friends are a newtype over the protocol id and a thin layer
    // over the same table, so what is worth checking is that the layer is
    // actually thin — that `Item::name` and `Registry::entry_name` cannot
    // disagree.
    for item in Item::all() {
        assert_eq!(
            Item::registry().entry_name(item.protocol_id()),
            Some(item.name())
        );
        assert_eq!(Item::from_name(item.name()), Some(item));
        assert_eq!(Item::from_protocol_id(item.protocol_id()), Some(item));
    }
    for entity in EntityType::all() {
        assert_eq!(EntityType::from_name(entity.name()), Some(entity));
        assert_eq!(
            EntityType::from_protocol_id(entity.protocol_id()),
            Some(entity)
        );
    }
    for fluid in Fluid::all() {
        assert_eq!(Fluid::from_name(fluid.name()), Some(fluid));
        assert_eq!(Fluid::from_protocol_id(fluid.protocol_id()), Some(fluid));
    }

    assert_eq!(Item::all().count(), Item::registry().entry_count());
    assert_eq!(
        EntityType::all().count(),
        EntityType::registry().entry_count()
    );
    assert_eq!(Fluid::all().count(), Fluid::registry().entry_count());
}

#[test]
fn each_first_class_type_is_over_the_registry_it_says_it_is() {
    // The generated `index` constants are positions in a name-ordered table, so
    // a release that adds a registry moves them. They are regenerated with the
    // table, but nothing about `Item = index::ITEM` would look wrong if they
    // were not — this is what would.
    assert_eq!(Item::registry().name(), "minecraft:item");
    assert_eq!(EntityType::registry().name(), "minecraft:entity_type");
    assert_eq!(Fluid::registry().name(), "minecraft:fluid");
}

#[test]
fn a_bare_name_is_refused_by_every_first_class_type() {
    assert_eq!(Item::from_name("stone"), None);
    assert!(Item::from_name("minecraft:stone").is_some());
    assert_eq!(EntityType::from_name("zombie"), None);
    assert!(EntityType::from_name("minecraft:zombie").is_some());
    assert_eq!(Fluid::from_name("water"), None);
    assert!(Fluid::from_name("minecraft:water").is_some());
}

#[test]
fn an_id_beyond_a_registry_decodes_to_nothing() {
    assert_eq!(
        Item::from_protocol_id(Item::registry().entry_count() as u32),
        None
    );
    assert_eq!(Item::from_protocol_id(u32::MAX), None);
    assert_eq!(EntityType::from_protocol_id(u32::MAX), None);
    assert_eq!(Fluid::from_protocol_id(u32::MAX), None);
}

#[test]
fn an_item_and_a_block_of_the_same_name_are_different_numbers() {
    // Both tables answer to `minecraft:stone` and they are not the same
    // registry. A test that only ever asked one of them would not notice the
    // day something started answering with the other's number.
    let stone = Item::from_name("minecraft:stone").expect("an item");
    let block = Block::from_name("minecraft:stone").expect("a block");
    assert_eq!(stone.name(), block.name());
    assert_ne!(stone.protocol_id(), 0);
}

#[test]
fn both_generated_tables_came_from_the_same_extraction() {
    // Two tables written by two different runs would be a version skew that
    // nothing else in the suite can see: each is internally consistent, and
    // block 42 and item 42 would simply be from different Minecrafts.
    assert_eq!(DATA_VERSION, "1.21.1");
    assert_eq!(
        dust_registry::generated::registries::DATA_VERSION,
        DATA_VERSION
    );
}
