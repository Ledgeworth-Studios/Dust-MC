//! The recipe-shape catalogue: the grammar, checked against its sources.
//!
//! The catalogue aggregates 1,290 files into 23 shapes, so the checks here are
//! about accounting rather than round-trips: every file lands in exactly one
//! shape, every shape is a registered serialiser, and the required/optional
//! split holds. The named facts — how many shaped recipes exist, what their
//! keys are — are written down directly so a change has to explain itself
//! rather than pass by agreement.

use dust_registry::generated::recipes::{
    NAMESPACES, RECIPE_COUNT, RECIPE_SHAPES, UNUSED_SERIALIZERS,
};
use dust_registry::{recipes as catalogue, Registry, DATA_VERSION};

#[test]
fn every_recipe_file_is_accounted_for_by_exactly_one_shape() {
    assert_eq!(
        RECIPE_SHAPES.iter().map(|s| s.uses).sum::<usize>(),
        RECIPE_COUNT,
        "the shapes and the total disagree about how many recipes were read"
    );
}

#[test]
fn every_shape_is_a_registered_serialiser_and_none_are_left_over() {
    let registry = Registry::from_name("minecraft:recipe_serializer")
        .expect("the serialiser registry is extracted beside this catalogue");
    assert_eq!(
        RECIPE_SHAPES.len() + UNUSED_SERIALIZERS.len(),
        registry.entry_count(),
        "used and unused shapes should account for every registered serialiser"
    );
    for shape in RECIPE_SHAPES {
        assert!(
            registry.entry_id(shape.serializer).is_some(),
            "{} is used by data and missing from the registry",
            shape.serializer
        );
    }
    for serializer in UNUSED_SERIALIZERS {
        assert!(
            registry.entry_id(serializer).is_some(),
            "{serializer} is listed as unused and is not even registered"
        );
    }
}

#[test]
fn keys_are_sorted_and_the_two_lists_never_overlap() {
    for shape in RECIPE_SHAPES {
        assert!(
            shape.required.windows(2).all(|pair| pair[0] < pair[1]),
            "{}: required keys are not sorted",
            shape.serializer
        );
        assert!(
            shape.optional.windows(2).all(|pair| pair[0] < pair[1]),
            "{}: optional keys are not sorted",
            shape.serializer
        );
        // `carries` is a membership test over both lists; overlap would make
        // it answer a question with two different truths.
        for key in shape.optional {
            assert!(
                !shape.required.contains(key),
                "{}: {key} is in both lists",
                shape.serializer
            );
        }
    }
}

#[test]
fn type_is_required_everywhere_because_it_is_what_names_the_shape() {
    for shape in RECIPE_SHAPES {
        assert!(
            shape.required.contains(&"type"),
            "{} does not list `type` as required",
            shape.serializer
        );
    }
}

#[test]
fn the_named_facts_about_this_catalogue_are_still_true() {
    assert_eq!(RECIPE_COUNT, 1290);
    assert_eq!(RECIPE_SHAPES.len(), 23);
    assert_eq!(UNUSED_SERIALIZERS.len(), 0);

    let shaped = catalogue::from_serializer("minecraft:crafting_shaped").expect("a shape");
    assert_eq!(shaped.uses, 634);
    assert_eq!(
        shaped.required,
        ["category", "key", "pattern", "result", "type"]
    );
    assert_eq!(shaped.optional, ["group", "show_notification"]);

    let stonecutting = catalogue::from_serializer("minecraft:stonecutting").expect("a shape");
    assert_eq!(stonecutting.uses, 250);
    assert_eq!(stonecutting.required, ["ingredient", "result", "type"]);
    assert_eq!(stonecutting.optional, &[] as &[&str]);

    let trim = catalogue::from_serializer("minecraft:smithing_trim").expect("a shape");
    assert_eq!(trim.uses, 18);
    assert_eq!(trim.required, ["addition", "base", "template", "type"]);
}

#[test]
fn the_special_recipes_are_one_line_markers_and_not_absent() {
    // The fact worth having from data instead of memory: armour dyeing and
    // friends ship as single files carrying nothing but `type` and
    // `category`. Someone writing this vocabulary by hand would probably have
    // left them out entirely.
    let special = catalogue::all()
        .filter(|s| s.serializer.contains("special"))
        .count();
    assert_eq!(special, 13);
    for shape in catalogue::all() {
        if !shape.serializer.contains("special") {
            continue;
        }
        assert_eq!(shape.uses, 1, "{}", shape.serializer);
        assert_eq!(shape.required, ["category", "type"], "{}", shape.serializer);
        assert_eq!(shape.optional, &[] as &[&str], "{}", shape.serializer);
    }
}

#[test]
fn lookups_find_shapes_by_serialiser_id() {
    assert!(catalogue::from_serializer("minecraft:smelting").is_some());
    assert_eq!(catalogue::from_serializer("smelting"), None);
    assert_eq!(
        catalogue::from_serializer("minecraft:not_a_serializer"),
        None
    );
}

#[test]
fn vanilla_recipes_come_from_one_namespace_on_this_version() {
    assert_eq!(NAMESPACES, ["minecraft"]);
}

#[test]
fn the_table_says_which_version_it_came_from() {
    assert_eq!(DATA_VERSION, "1.21.1");
    assert_eq!(
        dust_registry::generated::recipes::DATA_VERSION,
        DATA_VERSION
    );
}
