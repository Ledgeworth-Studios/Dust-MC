//! Overlays: the per-format layers a pack stacks on top of its own `data/`.
//!
//! The exact semantics — last matching entry wins per file, non-matching
//! entries inert and unremarked — are pinned in `src/overlay.rs`'s
//! documentation. These tests hold those sentences to the loader end to end:
//! real packs on disk and in zips, through `load`, with the findings an
//! operator would see.

mod support;

use dust_data::registry::RegistryId;
use dust_data::vocabulary::Unchecked;
use dust_data::{
    load, DirectoryPack, Finding, LoadOptions, PackSource, ResourceLocation, Severity,
};
use support::PackBuilder;

fn recipe_body(from: &str) -> String {
    format!(r#"{{"type":"minecraft:crafting_shapeless","result":{{"item":"{from}"}}}}"#)
}

fn location(text: &str) -> ResourceLocation {
    ResourceLocation::parse(text).expect("valid")
}

/// The base holds the file, one overlay shadows it, and nothing about the load
/// changes for files the overlay does not carry.
#[test]
fn an_applicable_overlay_shadows_the_base_per_file() {
    let pack = PackBuilder::new("overlaid")
        .mcmeta(
            r#"{"pack":{"pack_format":48,"description":"d"},
               "overlays":{"entries":[{"directory":"overlay_new","formats":48}]}}"#,
        )
        .resource(
            "minecraft",
            "recipe",
            "kept",
            &recipe_body("minecraft:base"),
        )
        .resource(
            "minecraft",
            "recipe",
            "swapped",
            &recipe_body("minecraft:base"),
        )
        .file(
            "overlay_new/data/minecraft/recipe/swapped.json",
            &recipe_body("somemod:overlay"),
        )
        .build();

    let data = load(&[&pack as &dyn PackSource], &LoadOptions::default());
    let recipe = RegistryId::new("recipe");
    assert_eq!(data.error_count(), 0, "{:?}", data.findings());

    // Shadowed: the overlay's copy won.
    let swapped = data
        .get(&recipe, &location("minecraft:swapped"))
        .expect("loaded");
    assert!(
        swapped.value.to_string().contains("somemod:overlay"),
        "{}",
        swapped.value
    );
    assert_eq!(swapped.pack, "overlaid");

    // Unshadowed: the base's copy survived.
    assert!(data.get(&recipe, &location("minecraft:kept")).is_some());

    // And the shadowed file is not double-counted anywhere: four physical
    // files (mcmeta, two base recipes, one overlay recipe), two of which
    // became resources.
    assert_eq!(data.stats().files_seen, 4);
    assert_eq!(data.stats().files_read, 2);
    assert_eq!(data.packs()[0].files_skipped, 2);
}

#[test]
fn the_last_declared_overlay_wins_when_several_apply() {
    let pack = PackBuilder::new("stacked")
        .mcmeta(
            // A multi-version pack declares the whole span it covers;
            // overlays then choose which directory each format sees.
            r#"{"pack":{"pack_format":48,"description":"d","supported_formats":[46,49]},
               "overlays":{"entries":[
                  {"directory":"first","formats":[46,48]},
                  {"directory":"second","formats":[47,49]}]}}"#,
        )
        .resource(
            "minecraft",
            "recipe",
            "contested",
            &recipe_body("minecraft:base"),
        )
        .file(
            "first/data/minecraft/recipe/contested.json",
            &recipe_body("first:wins_sometimes"),
        )
        .file(
            "second/data/minecraft/recipe/contested.json",
            &recipe_body("second:wins_more"),
        )
        .build();

    let data = load(&[&pack as &dyn PackSource], &LoadOptions::default());
    let contested = data
        .get(&RegistryId::new("recipe"), &location("minecraft:contested"))
        .expect("loaded");
    assert!(
        contested.value.to_string().contains("second:wins_more"),
        "the later entry must win at format 48: {}",
        contested.value
    );

    // At a format only the earlier entry covers, the earlier entry is the top
    // layer. Same packs, different negotiation.
    let options = LoadOptions {
        pack_format: 46,
        ..LoadOptions::default()
    };
    let data = load(&[&pack as &dyn PackSource], &options);
    let contested = data
        .get(&RegistryId::new("recipe"), &location("minecraft:contested"))
        .expect("loaded");
    assert!(
        contested.value.to_string().contains("first:wins_sometimes"),
        "{}",
        contested.value
    );
}

#[test]
fn an_overlay_for_another_format_is_inert_and_costs_no_warning() {
    // Multi-version packs are full of entries that do not match; warning on
    // them would be a line on every correctly-built pack.
    let pack = PackBuilder::new("multiversion")
        .mcmeta(
            r#"{"pack":{"pack_format":48,"description":"d"},
               "overlays":{"entries":[{"directory":"for_1_20","formats":[15,26]}]}}"#,
        )
        .resource(
            "minecraft",
            "recipe",
            "only",
            &recipe_body("minecraft:current"),
        )
        .file(
            "for_1_20/data/minecraft/recipe/old_spelling.json",
            &recipe_body("minecraft:ancient"),
        )
        .build();

    let data = load(&[&pack as &dyn PackSource], &LoadOptions::default());
    assert!(data.findings().is_empty(), "{:?}", data.findings());
    let recipe = RegistryId::new("recipe");
    assert!(data.get(&recipe, &location("minecraft:only")).is_some());
    // The old directory's files were never in play.
    assert!(data
        .get(&recipe, &location("minecraft:old_spelling"))
        .is_none());
}

#[test]
fn a_zip_and_a_directory_layer_identically() {
    // The rule that makes one reader enough: the same pack, both containers,
    // one answer.
    let builder = PackBuilder::new("twin")
        .mcmeta(
            r#"{"pack":{"pack_format":48,"description":"d"},
               "overlays":{"entries":[{"directory":"ov","formats":48}]}}"#,
        )
        .resource(
            "minecraft",
            "recipe",
            "a",
            &recipe_body("minecraft:from_base"),
        )
        .file(
            "ov/data/minecraft/recipe/a.json",
            &recipe_body("minecraft:from_overlay"),
        )
        .resource(
            "minecraft",
            "recipe",
            "b",
            &recipe_body("minecraft:untouched"),
        );

    let root = support::TempDir::new("overlay_twins");
    let dir_pack = DirectoryPack::open(builder.build_directory(&root));
    let zip_bytes = builder.build_zip_bytes();
    let zip_pack = dust_data::ZipPack::from_bytes(zip_bytes, "twin", "<zip:twin>").expect("zip");

    let from_dir = load(&[&dir_pack as &dyn PackSource], &LoadOptions::default());
    let from_zip = load(&[&zip_pack as &dyn PackSource], &LoadOptions::default());

    assert_eq!(
        from_dir.stats(),
        from_zip.stats(),
        "{:?}\n{:?}",
        from_dir.diagnostic_dump(),
        from_zip.diagnostic_dump()
    );
    let recipe = RegistryId::new("recipe");
    for name in ["minecraft:a", "minecraft:b"] {
        assert_eq!(
            from_dir
                .get(&recipe, &location(name))
                .map(|r| r.value.clone()),
            from_zip
                .get(&recipe, &location(name))
                .map(|r| r.value.clone()),
            "{name}"
        );
    }
    let overlaid = from_zip
        .get(&recipe, &location("minecraft:a"))
        .expect("loaded");
    assert!(overlaid.value.to_string().contains("from_overlay"));
}

#[test]
fn an_overlaid_tag_still_merges_across_packs_below_it() {
    // Overlay layers change which file wins *inside one pack*; they do not
    // turn tags into overriding resources. The merge across packs happens
    // above all of that.
    let base = PackBuilder::new("base").resource(
        "minecraft",
        "tags/block",
        "ores",
        r#"{"values":["minecraft:iron_ore"]}"#,
    );
    let over = PackBuilder::new("over")
        .mcmeta(
            r#"{"pack":{"pack_format":48,"description":"d"},
               "overlays":{"entries":[{"directory":"layer","formats":48}]}}"#,
        )
        .file(
            "data/minecraft/tags/block/ores.json",
            r#"{"values":["minecraft:copper_ore"]}"#,
        )
        .file(
            "layer/minecraft_placeholder.txt",
            "the layer carries no tag file",
        );

    let base_pack = base.build();
    let over_pack = over.build();
    let data = load(
        &[&base_pack as &dyn PackSource, &over_pack as &dyn PackSource],
        &LoadOptions::default(),
    );
    assert_eq!(data.error_count(), 0, "{:?}", data.findings());
    let (tags, findings) = data.resolve_tags(&Unchecked);
    assert!(findings.is_empty(), "{findings:?}");
    let ores = tags
        .get(&RegistryId::new("tags/block"), &location("minecraft:ores"))
        .expect("merged");
    assert_eq!(ores.len(), 2);
}

#[test]
fn an_overlay_directory_that_climbs_out_is_an_error_not_a_layer() {
    let pack = PackBuilder::new("escaping")
        .mcmeta(
            r#"{"pack":{"pack_format":48,"description":"d"},
               "overlays":{"entries":[{"directory":"../outside","formats":48}]}}"#,
        )
        .resource(
            "minecraft",
            "recipe",
            "safe",
            &recipe_body("minecraft:fine"),
        )
        .build();

    let data = load(&[&pack as &dyn PackSource], &LoadOptions::default());
    let errors: Vec<&Finding> = data
        .findings()
        .iter()
        .filter(|f| f.severity == Severity::Error)
        .collect();
    assert_eq!(errors.len(), 1, "{:?}", data.findings());
    assert!(errors[0].message.contains("../outside"), "{}", errors[0]);
    // Everything else loaded normally.
    assert!(data
        .get(&RegistryId::new("recipe"), &location("minecraft:safe"))
        .is_some());
}

#[test]
fn the_committed_mcmeta_fixture_parses_with_every_section_in_play() {
    // The hand-written fixture exercises all four optional sections at once:
    // overlays apply (one of the two), features and filter are reported as
    // not applied, and the description is a text component.
    let body = include_str!("fixtures/synthetic_full_mcmeta.json");
    let pack = PackBuilder::new("fixture").mcmeta(body).build();
    let data = load(&[&pack as &dyn PackSource], &LoadOptions::default());

    let meta = data.packs()[0].meta.as_ref().expect("parsed");
    assert_eq!(meta.overlays.len(), 2);
    assert!(meta.overlays[0].directory == "overlay_older" && !meta.overlays[0].applies_to(48));
    assert!(meta.overlays[1].applies_to(48));

    let warnings: Vec<&str> = data
        .findings()
        .iter()
        .filter(|f| f.severity == Severity::Warning)
        .map(|f| f.message.as_str())
        .collect();
    assert!(
        warnings.iter().any(|m| m.contains("feature flag")),
        "{warnings:?}"
    );
    assert!(
        warnings.iter().any(|m| m.contains("filter")),
        "{warnings:?}"
    );
    assert_eq!(
        meta.description.as_ref().expect("present").plain_text(),
        "A synthetic pack, written for these tests"
    );
}
