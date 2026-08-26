//! Discovery: what `load_directory` finds in a pack folder, in what order,
//! and what it refuses.

mod support;

use dust_data::registry::RegistryId;
use dust_data::{discover, load, LoadOptions, PackSource, ResourceLocation};
use support::PackBuilder;

fn location(text: &str) -> ResourceLocation {
    ResourceLocation::parse(text).expect("valid")
}

fn recipe_body(result: &str) -> String {
    format!(r#"{{"type":"minecraft:crafting_shapeless","result":{{"item":"{result}"}}}}"#)
}

#[test]
fn the_order_is_the_name_order_and_the_last_pack_wins() {
    // Two packs defining the same recipe; vanilla stacks a folder
    // alphabetically, so `zz_late` must beat `aa_early`.
    let root = support::TempDir::new("discovery_order");
    let early = PackBuilder::new("aa_early").resource(
        "minecraft",
        "recipe",
        "shared",
        &recipe_body("minecraft:from_early"),
    );
    let late = PackBuilder::new("zz_late").resource(
        "minecraft",
        "recipe",
        "shared",
        &recipe_body("minecraft:from_late"),
    );
    early.build_directory(&root);
    late.build_directory(&root);

    let (packs, findings) = discover(&root.path);
    assert!(findings.is_empty(), "{findings:?}");
    assert_eq!(
        packs.iter().map(|p| p.id().to_owned()).collect::<Vec<_>>(),
        vec!["aa_early".to_owned(), "zz_late".to_owned()]
    );

    let refs: Vec<&dyn PackSource> = packs.iter().map(|p| p.as_ref()).collect();
    let data = load(&refs, &LoadOptions::default());
    let shared = data
        .get(&RegistryId::new("recipe"), &location("minecraft:shared"))
        .expect("loaded");
    assert!(shared.value.to_string().contains("from_late"));
    assert_eq!(shared.overridden, vec!["aa_early".to_owned()]);
}

#[test]
fn zips_and_directories_are_discovered_alike() {
    let root = support::TempDir::new("discovery_kinds");
    let dir = PackBuilder::new("a_directory").resource(
        "minecraft",
        "recipe",
        "one",
        &recipe_body("minecraft:x"),
    );
    let zip = PackBuilder::new("b_zipped").resource(
        "minecraft",
        "recipe",
        "two",
        &recipe_body("minecraft:y"),
    );
    dir.build_directory(&root);
    zip.build_zip(&root);
    std::fs::write(root.path.join("notes.txt"), "not a pack").unwrap();

    let (packs, findings) = discover(&root.path);
    // One warning: the text file that is neither a directory nor a zip.
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert!(
        findings[0].message.contains("not a pack"),
        "{}",
        findings[0]
    );
    assert_eq!(
        packs.iter().map(|p| p.id().to_owned()).collect::<Vec<_>>(),
        vec!["a_directory".to_owned(), "b_zipped".to_owned()],
        "the zip answers to its stem and sorts by name"
    );
}

#[test]
fn a_dot_prefixed_entry_is_skipped_silently() {
    // The hiding convention: `.disabled.zip` is how an operator puts a pack
    // aside without deleting it. Vanilla ignores it; so do we, without a
    // word — a warning here would be permanent noise for everyone who has
    // ever used the convention.
    let root = support::TempDir::new("discovery_hidden");
    let visible = PackBuilder::new("visible").resource(
        "minecraft",
        "recipe",
        "one",
        &recipe_body("minecraft:x"),
    );
    let hidden = PackBuilder::new("hidden").resource(
        "minecraft",
        "recipe",
        "two",
        &recipe_body("minecraft:y"),
    );
    visible.build_directory(&root);
    hidden.build_zip_bytes();
    std::fs::write(root.path.join(".hidden.zip"), hidden.build_zip_bytes()).unwrap();

    let (packs, findings) = discover(&root.path);
    assert!(findings.is_empty(), "{findings:?}");
    assert_eq!(packs.len(), 1);
    assert_eq!(packs[0].id(), "visible");
}

#[test]
fn two_packs_answering_to_one_name_are_both_refused() {
    let root = support::TempDir::new("discovery_duplicates");
    let one = PackBuilder::new("clash").resource(
        "minecraft",
        "recipe",
        "one",
        &recipe_body("minecraft:a"),
    );
    let two = PackBuilder::new("clash").resource(
        "minecraft",
        "recipe",
        "two",
        &recipe_body("minecraft:b"),
    );
    one.build_directory(&root);
    two.build_zip(&root); // `clash.zip` against `clash/`

    let (packs, findings) = discover(&root.path);
    assert!(packs.is_empty(), "neither may load");
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert!(
        findings[0].message.contains("Rename one"),
        "{}",
        findings[0]
    );
}

#[test]
fn a_missing_pack_directory_is_one_error_and_no_packs() {
    let (packs, findings) = discover("/nowhere/at/all");
    assert!(packs.is_empty());
    assert_eq!(findings.len(), 1);
    assert!(
        findings[0].message.contains("could not be read"),
        "{}",
        findings[0]
    );
}

#[test]
fn load_directory_reports_what_it_skipped_before_what_it_loaded() {
    // The convenience entry point stitches discovery findings ahead of the
    // load's own, so reading the report top-down goes from cause to effect.
    let root = support::TempDir::new("load_directory");
    let good = PackBuilder::new("good").resource(
        "minecraft",
        "recipe",
        "fine",
        // Deliberately untyped: the load will warn about it.
        r#"{"pattern":["X"]}"#,
    );
    good.build_directory(&root);
    std::fs::write(root.path.join("stray.jar"), "not a pack").unwrap();

    let data = dust_data::load_directory(&root.path, &LoadOptions::default());
    let first = &data.findings()[0];
    assert!(
        first.message.contains("not a pack"),
        "discovery findings come first: {:?}",
        data.findings()
    );
    assert!(
        data.findings()
            .iter()
            .any(|f| f.message.contains("no string `type`")),
        "{:?}",
        data.findings()
    );
}
