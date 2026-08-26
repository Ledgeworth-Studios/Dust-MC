//! Provenance: the merged view remembering who won, and the diagnostic dump
//! saying so readably, identically, every time.
//!
//! These are also the first tests of the Phase 10 tooling surface: a dump that
//! is stable across runs is diffable, and a diffable dump is how "what does
//! this modpack actually change" becomes answerable without writing a parser.

mod support;

use dust_data::registry::RegistryId;
use dust_data::{load, LoadOptions, PackSource};
use support::PackBuilder;

fn recipe_body(result: &str) -> String {
    format!(r#"{{"type":"minecraft:crafting_shapeless","result":{{"item":"{result}"}}}}"#)
}

fn two_packs() -> (support::MemPack, support::MemPack) {
    let base = PackBuilder::new("base")
        .resource(
            "minecraft",
            "recipe",
            "shared",
            &recipe_body("minecraft:from_base"),
        )
        .resource(
            "minecraft",
            "recipe",
            "only_base",
            &recipe_body("minecraft:b"),
        )
        .resource("somemod", "recipe", "modded", &recipe_body("somemod:thing"))
        .resource(
            "minecraft",
            "tags/item",
            "tools",
            r#"{"values":["minecraft:iron_pickaxe"]}"#,
        );
    let over = PackBuilder::new("overlaying")
        .resource(
            "minecraft",
            "recipe",
            "shared",
            &recipe_body("minecraft:from_over"),
        )
        .resource(
            "minecraft",
            "tags/item",
            "tools",
            r#"{"values":["minecraft:diamond_pickaxe"]}"#,
        );
    (base.build(), over.build())
}

#[test]
fn the_dump_names_the_winner_the_displaced_and_the_file() {
    let (a, b) = two_packs();
    let data = load(
        &[&a as &dyn PackSource, &b as &dyn PackSource],
        &LoadOptions::default(),
    );
    let dump = data.diagnostic_dump();

    assert!(dump.contains("minecraft:shared <- overlaying"), "{dump}");
    assert!(dump.contains("(displaced: base)"), "{dump}");
    assert!(dump.contains("data/minecraft/recipe/shared.json"), "{dump}");
    // Losers stay listed as resources only where they still hold.
    assert!(dump.contains("minecraft:only_base <- base"), "{dump}");
    assert!(dump.contains("somemod:modded <- base"), "{dump}");

    // Tags show their contributing packs and files, with written-entry counts.
    assert!(
        dump.contains("#minecraft:tools: 2 written entries from"),
        "{dump}"
    );
    assert!(
        dump.contains("base (data/minecraft/tags/item/tools.json)"),
        "{dump}"
    );

    // Findings are rendered in full.
    if !data.findings().is_empty() {
        assert!(dump.contains(&data.findings()[0].to_string()), "{dump}");
    }
}

#[test]
fn the_dump_is_identical_across_two_loads_of_the_same_packs() {
    let (a, b) = two_packs();
    let one = load(
        &[&a as &dyn PackSource, &b as &dyn PackSource],
        &LoadOptions::default(),
    );
    let two = load(
        &[&a as &dyn PackSource, &b as &dyn PackSource],
        &LoadOptions::default(),
    );
    assert_eq!(one.diagnostic_dump(), two.diagnostic_dump());
}

#[test]
fn the_namespace_view_slices_one_namespace_with_provenance() {
    let (a, b) = two_packs();
    let data = load(
        &[&a as &dyn PackSource, &b as &dyn PackSource],
        &LoadOptions::default(),
    );

    let view = data.namespace("somemod");
    assert_eq!(view.resources.len(), 1);
    let entries = view
        .resources
        .get(&RegistryId::new("recipe"))
        .expect("registry");
    assert!(entries.contains_key(&dust_data::ResourceLocation::parse("somemod:modded").unwrap()));
    assert!(view.tags.is_empty());

    let minecraft = data.namespace("minecraft");
    let shared = minecraft
        .resources
        .get(&RegistryId::new("recipe"))
        .unwrap()
        .get(&dust_data::ResourceLocation::parse("minecraft:shared").unwrap())
        .expect("present");
    assert_eq!(shared.pack, "overlaying");
}

#[test]
fn a_pack_report_counts_what_each_namespace_held_and_gave() {
    // `typo/` is not a registry, so those files were seen but never became
    // anything. The per-namespace roll-up is where that shows up without
    // needing to re-read the findings.
    let pack = PackBuilder::new("mixed")
        .resource("minecraft", "recipe", "good", &recipe_body("minecraft:x"))
        .file("data/minecraft/typo/bad.json", "{}")
        .build();

    let data = load(&[&pack as &dyn PackSource], &LoadOptions::default());
    let tally = data.packs()[0]
        .namespaces
        .get("minecraft")
        .expect("namespace counted");
    assert_eq!(tally.files_seen, 2);
    assert_eq!(tally.files_read, 1);

    let dump = data.diagnostic_dump();
    assert!(dump.contains("minecraft (1/2)"), "{dump}");
}
