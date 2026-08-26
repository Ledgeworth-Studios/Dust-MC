//! The worldgen vocabulary, checked against itself and against the report.
//!
//! The vocabulary is counts and names, so the checks are accounting plus the
//! named facts: argument lists sorted, uses summed per type, and the nether's
//! five points saying what Mojang's report says — warped_forest's humidity of
//! 0.5 and offset of 0.375 written out by hand, because a golden sample that
//! only agreed with itself would agree with anything.

use dust_gen::worldgen::{
    self, density_function_type, nether_biome_points, parameter_value, PARAMETER_NAMES,
};

#[test]
fn lookups_find_density_functions_by_whole_id() {
    let add = density_function_type("minecraft:add").expect("exists");
    assert_eq!(add.arguments, ["argument1", "argument2"]);
    assert!(density_function_type("add").is_none());
    assert!(density_function_type("minecraft:not_a_function").is_none());
}

#[test]
fn the_named_facts_about_terrain_are_still_true() {
    // 25 of the 32 registered types appear in vanilla's trees; `add` is the
    // workhorse at 50 appearances. Written down so a change has to explain
    // itself rather than pass by agreement.
    assert_eq!(worldgen::density_function_types().count(), 25);
    assert_eq!(
        density_function_type("minecraft:add").map(|f| f.uses),
        Some(50)
    );
    assert_eq!(
        density_function_type("minecraft:noise").map(|f| f.arguments),
        Some(&["noise", "xz_scale", "y_scale"][..])
    );

    // Every noise setting wires the same fifteen slots.
    assert_eq!(dust_gen::generated::worldgen::NOISE_ROUTER_SLOTS.len(), 15);
    assert!(dust_gen::generated::worldgen::NOISE_ROUTER_SLOTS.contains(&"final_density"));
    assert!(dust_gen::generated::worldgen::NOISE_ROUTER_SLOTS.contains(&"continents"));
}

#[test]
fn every_argument_list_is_sorted_so_lookups_are_sound() {
    for function in worldgen::density_function_types() {
        assert!(
            function.arguments.windows(2).all(|pair| pair[0] < pair[1]),
            "{}: arguments are not sorted",
            function.name
        );
    }
}

#[test]
fn the_biome_parameters_are_exactly_the_seven() {
    // Sorted, because parameter_value binary-searches them.
    assert_eq!(
        PARAMETER_NAMES,
        [
            "continentalness",
            "depth",
            "erosion",
            "humidity",
            "offset",
            "temperature",
            "weirdness"
        ]
    );
}

#[test]
fn the_nethers_five_points_say_what_the_report_says() {
    let mut points: Vec<_> = nether_biome_points().collect();
    points.sort_by_key(|(biome, _)| (*biome).to_owned());
    assert_eq!(points.len(), 5);

    let wastes = points
        .iter()
        .find(|(biome, _)| *biome == "minecraft:nether_wastes")
        .expect("the nether has wastes")
        .1;
    // All zeroes: the reference point of the whole space.
    assert!(wastes.iter().all(|value| *value == 0.0));

    let warped = points
        .iter()
        .find(|(biome, _)| *biome == "minecraft:warped_forest")
        .expect("the nether has a warped forest")
        .1;
    assert_eq!(parameter_value(warped, "humidity"), Some(0.5));
    assert_eq!(parameter_value(warped, "offset"), Some(0.375));
    assert_eq!(parameter_value(warped, "temperature"), Some(0.0));

    let basalt = points
        .iter()
        .find(|(biome, _)| *biome == "minecraft:basalt_deltas")
        .expect("the nether has basalt deltas")
        .1;
    assert_eq!(parameter_value(basalt, "temperature"), Some(-0.5));
}

#[test]
fn the_dimension_summaries_match_the_reports_shape() {
    use dust_gen::worldgen::BIOME_PARAMETER_DIMENSIONS;
    assert_eq!(BIOME_PARAMETER_DIMENSIONS.len(), 2);

    let overworld = BIOME_PARAMETER_DIMENSIONS
        .iter()
        .find(|d| d.dimension == "minecraft:overworld")
        .expect("an overworld");
    assert_eq!(overworld.entries, 7593);
    assert_eq!(
        overworld.ranged_entries, 7593,
        "every overworld entry is range-shaped"
    );
    assert_eq!(overworld.distinct_biomes, 53);

    let nether = BIOME_PARAMETER_DIMENSIONS
        .iter()
        .find(|d| d.dimension == "minecraft:nether")
        .expect("a nether");
    assert_eq!(nether.entries, 5);
    assert_eq!(nether.ranged_entries, 0);
    assert_eq!(nether.distinct_biomes, 5);
}

#[test]
fn the_table_says_which_version_it_came_from() {
    assert_eq!(dust_gen::generated::worldgen::DATA_VERSION, "1.21.1");
}
