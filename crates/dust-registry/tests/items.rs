//! Item default components: the table against the report, not against itself.
//!
//! There is no round-trip to be had here. A component map is data with no
//! second direction to decode it back through, so the only question worth
//! asking is whether the table says what Mojang's report says — and the only
//! thing that can answer it is [`COMPONENT_SAMPLES`], rendered from the report
//! at extraction time.
//!
//! Two decisions make those rows evidence rather than decoration, and both come
//! from what the registry tables taught: a sample is only evidence where a
//! wrong answer and a right one differ.
//!
//! - **Every sampled item is a boundary item** — one whose protocol-id
//!   neighbours have a *different* component map. 1,020 of the 1,333 items
//!   share one map, and an index that slips by one inside that run produces the
//!   same answer it should have; a row there would pass a mutation it was
//!   written to catch. [`every_sample_sits_where_a_slip_would_show`] re-derives
//!   that property from the table, so the placement rule cannot quietly stop
//!   holding.
//! - **Every distinct component map is sampled once**, so every value shape in
//!   the report is covered: the four shapes of `minecraft:tool`, the six of
//!   `minecraft:food`, and every float literal whose f32 and f64 spellings
//!   differ.

use dust_registry::generated::items::{COMPONENT_MAPS, ITEM_COMPONENTS};
use dust_registry::{ComponentValue, Item, Rarity, COMPONENT_SAMPLES, DATA_VERSION};

/// One line of text for a value tree, keys in the order the table holds them.
///
/// A second implementation of the rule `codegen::canonical` uses, on purpose.
/// The samples were rendered from the report by that one; these rows are
/// rendered from the *table* by this one. Sharing a renderer would make the
/// comparison agree with itself the way a round-trip does, which is the whole
/// mistake this file exists to avoid.
fn canonical(value: ComponentValue) -> String {
    match value {
        ComponentValue::Bool(b) => b.to_string(),
        ComponentValue::Int(i) => i.to_string(),
        ComponentValue::Float(f) => format!("{f:?}"),
        ComponentValue::Str(s) => format!("{s:?}"),
        ComponentValue::List(items) => {
            let rendered: Vec<String> = items.iter().copied().map(canonical).collect();
            format!("[{}]", rendered.join(","))
        }
        ComponentValue::Map(fields) => {
            let rendered: Vec<String> = fields
                .iter()
                .map(|(name, field)| format!("{name:?}:{}", canonical(*field)))
                .collect();
            format!("{{{}}}", rendered.join(","))
        }
    }
}

fn canonical_components(item: Item) -> String {
    let rendered: Vec<String> = item
        .components()
        .iter()
        .map(|(name, value)| format!("{name:?}:{}", canonical(value)))
        .collect();
    format!("{{{}}}", rendered.join(","))
}

#[test]
fn the_table_says_what_mojangs_report_says() {
    assert!(
        !COMPONENT_SAMPLES.is_empty(),
        "the generated table carries no samples"
    );
    for &(name, expected) in COMPONENT_SAMPLES {
        let item = Item::from_name(name).unwrap_or_else(|| panic!("{name} is sampled and absent"));
        assert_eq!(
            canonical_components(item),
            expected,
            "{name}'s components are not what the report says"
        );
    }
}

#[test]
fn every_sample_sits_where_a_slip_would_show() {
    // The placement rule, re-derived from the table rather than taken on
    // trust. A sample on an item whose neighbours share its map cannot fail on
    // an off-by-one — the wrong answer and the right one are the same text —
    // so a row there is a row that will pass whatever happens.
    for &(name, _) in COMPONENT_SAMPLES {
        let item = Item::from_name(name).expect("sampled");
        let id = item.protocol_id() as usize;
        let mine = ITEM_COMPONENTS[id];
        let before = id.checked_sub(1).map(|i| ITEM_COMPONENTS[i]);
        let after = ITEM_COMPONENTS.get(id + 1).copied();
        assert!(
            before.is_none_or(|other| other != mine) || after.is_none_or(|other| other != mine),
            "{name} sits inside a run of identical component maps, where a shifted index \
             would decode to the same answer and this row would pass anyway"
        );
    }
}

#[test]
fn every_distinct_component_map_is_sampled() {
    // Coverage stated as a property of the table: 136 maps, 136 rows, all
    // different. A sample set that quietly stopped covering a map would leave
    // that map's values unchecked while the suite stayed green.
    let mut sampled: Vec<u16> = COMPONENT_SAMPLES
        .iter()
        .map(|(name, _)| {
            let item = Item::from_name(name).expect("sampled");
            ITEM_COMPONENTS[item.protocol_id() as usize]
        })
        .collect();
    sampled.sort_unstable();
    sampled.dedup();
    assert_eq!(
        sampled.len(),
        COMPONENT_MAPS.len(),
        "{} of the {} distinct component maps have no sample row",
        COMPONENT_MAPS.len() - sampled.len(),
        COMPONENT_MAPS.len()
    );
}

#[test]
fn every_item_has_components_and_the_index_lands_in_the_table() {
    assert_eq!(
        ITEM_COMPONENTS.len(),
        Item::all().count(),
        "the component index and the item registry disagree about how many items exist"
    );
    for item in Item::all() {
        let index = ITEM_COMPONENTS[item.protocol_id() as usize];
        assert!(
            (index as usize) < COMPONENT_MAPS.len(),
            "{} points at map {index}, and there are {}",
            item.name(),
            COMPONENT_MAPS.len()
        );
        assert!(
            !item.components().is_empty(),
            "{} has no components at all",
            item.name()
        );
    }
}

#[test]
fn every_map_is_sorted_by_name_all_the_way_down() {
    // `Components::get` and `ComponentValue::get` are binary searches, which
    // are undefined over an unsorted slice — they do not fail, they answer
    // wrongly. Checked recursively, because a nested map is searched the same
    // way and would go wrong the same way.
    fn check(value: ComponentValue, path: &str) {
        match value {
            ComponentValue::Map(fields) => {
                assert!(
                    fields.windows(2).all(|pair| pair[0].0 < pair[1].0),
                    "{path} is not sorted by name"
                );
                for (name, field) in fields {
                    check(*field, &format!("{path}.{name}"));
                }
            }
            ComponentValue::List(items) => {
                for (index, item) in items.iter().enumerate() {
                    check(*item, &format!("{path}[{index}]"));
                }
            }
            _ => {}
        }
    }
    for item in Item::all() {
        let components = item.components();
        let names: Vec<&str> = components.iter().map(|(name, _)| name).collect();
        assert!(
            names.windows(2).all(|pair| pair[0] < pair[1]),
            "{}'s components are not sorted by name",
            item.name()
        );
        for (name, value) in components.iter() {
            assert!(name.contains(':'), "{name} is not a namespaced id");
            check(value, name);
        }
    }
}

#[test]
fn the_typed_accessors_agree_with_the_tree_underneath_them() {
    // The typed accessors are the hybrid half of the representation: they read
    // the same tree everything else does, and they are only sound because the
    // extractor checks the shape they assume across all 1,333 items. This is
    // that check restated against the code that came out.
    for item in Item::all() {
        let components = item.components();
        assert_eq!(
            i64::from(item.max_stack_size()),
            components
                .get("minecraft:max_stack_size")
                .and_then(ComponentValue::as_i64)
                .unwrap_or_else(|| panic!("{} has no max_stack_size", item.name())),
        );
        assert!(
            (1..=99).contains(&item.max_stack_size()),
            "{} stacks to {}",
            item.name(),
            item.max_stack_size()
        );
        assert_eq!(
            item.rarity().name(),
            components
                .get("minecraft:rarity")
                .and_then(ComponentValue::as_str)
                .unwrap_or_else(|| panic!("{} has no rarity", item.name())),
        );
        assert_eq!(
            item.max_damage().map(i64::from),
            components
                .get("minecraft:max_damage")
                .and_then(ComponentValue::as_i64),
        );
        assert_eq!(
            item.damage().map(i64::from),
            components
                .get("minecraft:damage")
                .and_then(ComponentValue::as_i64),
        );
        assert_eq!(
            item.is_fire_resistant(),
            components.contains("minecraft:fire_resistant")
        );
    }
}

#[test]
fn the_items_the_whole_thing_is_for() {
    // D3 chose 1.21.1 for data components, and this is what one looks like.
    // Named values rather than derived ones: if the extraction silently started
    // reading a different field, every other test in this file would still
    // agree with itself.
    let sword = Item::from_name("minecraft:diamond_sword").expect("an item");
    assert_eq!(sword.max_damage(), Some(1561));
    assert_eq!(sword.max_stack_size(), 1);
    assert_eq!(sword.rarity(), Rarity::Common);

    let modifiers = sword
        .components()
        .get("minecraft:attribute_modifiers")
        .and_then(|c| c.get("modifiers"))
        .and_then(ComponentValue::as_list)
        .expect("a sword has modifiers");
    assert_eq!(modifiers.len(), 2);
    assert_eq!(
        modifiers[0].get("type").and_then(ComponentValue::as_str),
        Some("minecraft:generic.attack_damage")
    );
    assert_eq!(
        modifiers[0].get("amount").and_then(ComponentValue::as_f64),
        Some(6.0)
    );

    let rules = sword
        .components()
        .get("minecraft:tool")
        .and_then(|c| c.get("rules"))
        .and_then(ComponentValue::as_list)
        .expect("a sword is a tool");
    assert_eq!(rules.len(), 2);
    assert_eq!(
        rules[0].get("blocks").and_then(ComponentValue::as_str),
        Some("minecraft:cobweb")
    );
}

#[test]
fn floats_are_the_width_the_report_wrote_them_at() {
    // The trap this table was most likely to fall into, asserted at both of its
    // two shapes.
    //
    // The sword's attack speed is a Java float that reached the report widened
    // to a double, and the report spells it at double width. Narrowing it is
    // exact, and that is what makes it safe to send as a float.
    let speed = Item::from_name("minecraft:diamond_sword")
        .and_then(|sword| sword.components().get("minecraft:attribute_modifiers"))
        .and_then(|c| c.get("modifiers"))
        .and_then(ComponentValue::as_list)
        .map(|modifiers| modifiers[1])
        .and_then(|modifier| modifier.get("amount"))
        .and_then(ComponentValue::as_f64)
        .expect("an attack speed");
    assert_eq!(speed, -2.4000000953674316_f64);
    assert_eq!(
        f64::from(speed as f32),
        speed,
        "narrowing it is meant to be exact"
    );

    // Chicken's saturation is the other kind: the report spells it `1.2`, the
    // shortest text that round-trips through an *f32*. Read at f32 width and
    // widened again it would be 1.2000000476837158, which is a different
    // number from the one the report states — so the table must hold the f64
    // that `1.2` parses to and let the caller narrow if it wants to.
    let saturation = Item::from_name("minecraft:chicken")
        .and_then(|chicken| chicken.components().get("minecraft:food"))
        .and_then(|food| food.get("saturation"))
        .and_then(ComponentValue::as_f64)
        .expect("a saturation");
    assert_eq!(saturation, 1.2_f64);
    assert_ne!(
        saturation,
        f64::from(1.2_f32),
        "the table is holding the f32 and not the number the report wrote"
    );
    assert_eq!(
        saturation as f32, 1.2_f32,
        "narrowing still gives the float"
    );
}

#[test]
fn all_three_generated_tables_came_from_the_same_extraction() {
    assert_eq!(DATA_VERSION, "1.21.1");
    assert_eq!(dust_registry::generated::items::DATA_VERSION, DATA_VERSION);
    assert_eq!(
        dust_registry::generated::registries::DATA_VERSION,
        DATA_VERSION
    );
}
