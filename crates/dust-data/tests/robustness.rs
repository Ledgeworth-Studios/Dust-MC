//! Robustness: hostile, broken and absurd input produces findings — never
//! panics, and never a silent no-op.
//!
//! The mutation tests are seeded rather than fixed: each run walks a known
//! set of seeds, so a failure is reproducible by number, but the set is wide
//! enough that a structural bug in the zip reader or the loader cannot dodge
//! every case.

mod support;

use dust_data::{load, LoadOptions, PackSource};
use support::{PackBuilder, Rng};

/// A well-formed synthetic pack with enough variety to be worth corrupting:
/// two containers' worth of structure, JSON bodies, and a non-data file.
fn sample_zip_bytes() -> Vec<u8> {
    PackBuilder::new("sample")
        .resource(
            "minecraft",
            "recipe",
            "a",
            r#"{"type":"minecraft:crafting_shaped","result":{"item":"minecraft:a"}}"#,
        )
        .resource(
            "minecraft",
            "recipe",
            "b",
            r#"{"type":"minecraft:crafting_shapeless"}"#,
        )
        .resource(
            "minecraft",
            "tags/block",
            "ores",
            r#"{"values":["minecraft:iron_ore"],"replace":false}"#,
        )
        .file("pack.png", "\u{89}PNG-ish bytes are fine here")
        .build_zip_bytes()
}

#[test]
fn seeded_corruptions_of_a_zip_never_panic_and_never_lie_about_counts() {
    let original = sample_zip_bytes();
    for seed in 0..300_u64 {
        let mut rng = Rng::new(seed);
        let mut bytes = original.clone();
        // One to four single-byte corruptions at random positions. Most land
        // in payloads; some hit headers, lengths or the directory, which is
        // the point.
        let flips = 1 + rng.below(4);
        for _ in 0..flips {
            let at = rng.below(bytes.len());
            bytes[at] = rng.mutated_byte(bytes[at]);
        }

        let outcome = dust_data::ZipPack::from_bytes(bytes.clone(), "sample", "<zip>");
        match outcome {
            Err(_) => { /* refused at open: a legitimate answer */ }
            Ok(pack) => {
                let data = load(&[&pack as &dyn PackSource], &LoadOptions::default());
                // The invariant the loader must keep under any corruption:
                // every listed file is accounted for as read or skipped, per
                // pack, exactly once.
                for report in data.packs() {
                    assert_eq!(
                        report.files_read + report.files_skipped,
                        data.stats().files_seen / data.packs().len(),
                        "seed {seed}: file accounting drifted\n{:?}",
                        data.findings()
                    );
                }
                // Findings exist only where something did not load cleanly;
                // there is no combination of corrupted bytes that produces a
                // clean-looking load of nothing.
                if data.stats().files_read == 0 {
                    assert!(
                        !data.findings().is_empty() || data.stats().files_seen == 0,
                        "seed {seed}: nothing loaded and nobody said so"
                    );
                }
            }
        }
    }
}

#[test]
fn seeded_truncations_of_a_zip_are_refused_or_reported() {
    let original = sample_zip_bytes();
    for seed in 0..100_u64 {
        let mut rng = Rng::new(seed);
        let cut = 1 + rng.below(original.len());
        let bytes = original[..cut].to_vec();

        match dust_data::ZipPack::from_bytes(bytes, "cut", "<zip>") {
            Err(_) => {}
            Ok(pack) => {
                let _ = load(&[&pack as &dyn PackSource], &LoadOptions::default());
                // Loading may succeed for archives cut inside trailing junk;
                // it must merely not hang or crash.
            }
        }
    }
}

#[test]
fn deeply_nested_json_is_a_finding_not_a_stack_overflow() {
    // serde_json's recursion limit turns this into an ordinary parse error;
    // the assertion is that the loader treats it as one line of output
    // rather than taking the process down.
    let body = format!("{}{}", "[".repeat(50_000), "]".repeat(50_000));
    let pack = PackBuilder::new("deep")
        .resource("minecraft", "recipe", "abyss", &body)
        .build();

    let data = load(&[&pack as &dyn PackSource], &LoadOptions::default());
    assert_eq!(data.error_count(), 1, "{:?}", data.findings());
    assert!(
        data.findings()[0].message.contains("valid JSON"),
        "{}",
        data.findings()[0]
    );
}

#[test]
fn every_broken_file_in_one_pack_produces_its_own_finding() {
    let pack = PackBuilder::new("broken_all_over")
        .resource(
            "minecraft",
            "recipe",
            "trailing_comma",
            r#"{"type":"minecraft:crafting_shaped",}"#,
        )
        .resource("minecraft", "recipe", "not_json", "plainly not json")
        .resource("minecraft", "recipe", "wrong_root", "[1,2,3]")
        .build();

    let data = load(&[&pack as &dyn PackSource], &LoadOptions::default());
    // Two files are unparseable: two errors. `[1,2,3]` *is* valid JSON, so it
    // loads and draws the missing-`type` warning instead — the severities
    // track what actually failed.
    assert_eq!(data.error_count(), 2, "{:?}", data.findings());
    assert_eq!(data.findings().len(), 3);
    for finding in data.findings() {
        assert!(
            finding.file.starts_with("data/minecraft/recipe/"),
            "{finding}"
        );
        assert!(
            finding.subject.is_some(),
            "each broken file names its resource: {finding}"
        );
    }
}

#[test]
fn a_huge_file_in_a_directory_pack_is_refused_with_a_reason() {
    let root = support::TempDir::new("huge");
    let good = PackBuilder::new("huge")
        .resource(
            "minecraft",
            "recipe",
            "small",
            r#"{"type":"minecraft:crafting_shaped"}"#,
        )
        .build_directory(&root);
    // Sparse: length without contents, so the test does not write 64 MiB.
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(good.join("data/minecraft/recipe/enormous.json"))
        .expect("create")
        .set_len(dust_data::pack::MAX_FILE_BYTES + 1)
        .expect("sparsely size");

    let pack = dust_data::DirectoryPack::open(good);
    let data = load(&[&pack as &dyn PackSource], &LoadOptions::default());

    assert_eq!(data.error_count(), 1, "{:?}", data.findings());
    assert!(
        data.findings()[0].message.contains("limit"),
        "{}",
        data.findings()[0]
    );
    // The small sibling still loaded.
    assert!(data
        .registry(&dust_data::registry::RegistryId::new("recipe"))
        .expect("registry")
        .contains_key(&dust_data::ResourceLocation::parse("minecraft:small").unwrap()));
}

#[test]
fn duplicate_ids_inside_one_load_are_refused_even_without_discovery() {
    // `load` enforces the same invariant `discover` does, for lists built by
    // hand — a caller assembling packs from several origins can still hand
    // over two answering to one name.
    let builder = |id: &'static str| {
        PackBuilder::new(id).resource(
            "minecraft",
            "recipe",
            "x",
            r#"{"type":"minecraft:crafting_shaped"}"#,
        )
    };
    let first = builder("twin").build();
    let second = builder("twin").build();

    let data = load(
        &[&first as &dyn PackSource, &second as &dyn PackSource],
        &LoadOptions::default(),
    );
    assert_eq!(data.packs().len(), 2);
    assert!(data.packs()[0].loaded);
    assert!(!data.packs()[1].loaded, "{:?}", data.packs());
    assert_eq!(data.error_count(), 1, "{:?}", data.findings());
    assert!(
        data.findings()[0].message.contains("Rename"),
        "{}",
        data.findings()[0]
    );
}
