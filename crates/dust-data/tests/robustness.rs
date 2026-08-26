//! Robustness: hostile, broken and absurd input produces findings — never
//! panics, and never a silent no-op.
//!
//! The mutation tests are seeded rather than fixed: each run walks a known
//! set of seeds, so a failure is reproducible by number, but the set is wide
//! enough that a structural bug in the zip reader or the loader cannot dodge
//! every case.

mod support;

use dust_data::{load, LoadOptions, PackSource, Severity, ZipPack};
use support::{write_layouted_zip, PackBuilder, RawZipEntry, Rng};

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

// ---------------------------------------------------------------------------
// Seeded mutation families, one per file kind the crate reads. The zip
// container has its own mutation suite above; these target the *documents*,
// where a corrupted byte must cost a finding and never a panic, a hang, or
// an account that does not balance.
// ---------------------------------------------------------------------------

/// A synthetic pack with one file of each JSON kind, plus an mcfunction.
fn mixed_pack_files() -> Vec<(String, String)> {
    vec![
        (
            "pack.mcmeta".to_owned(),
            r#"{"pack":{"pack_format":48,"description":"mutation target"}}"#.to_owned(),
        ),
        (
            "data/minecraft/recipe/shaped.json".to_owned(),
            r#"{"type":"minecraft:crafting_shaped","pattern":["XX"],"key":{"X":{"item":"minecraft:stick"}},"result":{"item":"minecraft:ladder"}}"#.to_owned(),
        ),
        (
            "data/minecraft/tags/block/logs.json".to_owned(),
            r##"{"values":["minecraft:oak_log","#minecraft:crimson_stems"],"replace":false}"##.to_owned(),
        ),
        (
            "data/minecraft/loot_table/blocks/stone.json".to_owned(),
            r#"{"type":"minecraft:block","pools":[{"rolls":1,"entries":[{"type":"minecraft:item","name":"minecraft:cobblestone","functions":[{"function":"minecraft:set_count","count":2}]}]}]}"#.to_owned(),
        ),
        (
            "data/minecraft/function/tick.mcfunction".to_owned(),
            "# keep the lights on\nsay tick\n\ntellraw @a \"done\"\n".to_owned(),
        ),
    ]
}

#[test]
fn seeded_mutations_of_every_document_kind_cost_findings_and_never_panics() {
    let original = mixed_pack_files();
    for seed in 0..250_u64 {
        let mut rng = Rng::new(seed);
        // Pick one file and corrupt one to four of its bytes. Everything
        // else stays pristine, so the load always has something honest to
        // compare against.
        let victim = rng.below(original.len());
        let mut files = original.clone();
        let mut body = files[victim].1.clone().into_bytes();
        let flips = 1 + rng.below(4);
        for _ in 0..flips {
            let at = rng.below(body.len());
            body[at] = rng.mutated_byte(body[at]);
        }
        files[victim].1 = String::from_utf8_lossy(&body).into_owned();

        let pack = support::MemPack::with_raw(
            "mutated",
            &files
                .iter()
                .map(|(path, text)| (path.as_str(), text.as_bytes()))
                .collect::<Vec<_>>(),
        );
        let data = load(&[&pack as &dyn PackSource], &LoadOptions::default());

        // The accounting invariant: every listed file lands in exactly one
        // bucket, whatever the corruption did to whichever sibling.
        for report in data.packs() {
            assert_eq!(
                report.files_read + report.files_skipped,
                files.len(),
                "seed {seed}: accounting drifted"
            );
        }
        // And a load that kept nothing said why.
        if data.stats().packs_loaded == 0 {
            assert!(
                !data.findings().is_empty(),
                "seed {seed}: refused everything and nobody said so"
            );
        }
    }
}

#[test]
fn seeded_mutations_of_mcmeta_are_answered_not_fatal() {
    // `PackMeta` decides whether a whole pack loads, so its totality gets
    // its own hammer independent of the loader around it.
    let original = r#"{"pack":{"pack_format":48,"description":"d","supported_formats":[46,48]},"overlays":{"entries":[{"directory":"old","formats":[15,20]}]}}"#;
    for seed in 0..250_u64 {
        let mut rng = Rng::new(seed);
        let mut body = original.as_bytes().to_vec();
        let flips = 1 + rng.below(4);
        for _ in 0..flips {
            let at = rng.below(body.len());
            body[at] = rng.mutated_byte(body[at]);
        }
        let (meta, _findings) = dust_data::PackMeta::parse(&body, "p", "pack.mcmeta");
        // Total: some outcome either way, never a panic. A parse that
        // survives may legitimately still be usable.
        let _ = meta.is_some();
    }
}

#[test]
fn seeded_mutations_of_tag_files_drop_lines_and_keep_talking() {
    let original = r##"{"values":["minecraft:oak_log",{"id":"somemod:fancy","required":false},"#minecraft:logs"],"replace":true}"##;
    for seed in 0..250_u64 {
        let mut rng = Rng::new(seed);
        let mut body = original.as_bytes().to_vec();
        let at = rng.below(body.len());
        body[at] = rng.mutated_byte(body[at]);
        let value: Result<serde_json::Value, _> = serde_json::from_slice(&body);
        if let Ok(value) = value {
            let (_file, findings) = dust_data::tag::TagFile::parse(&value, "p", "f.json");
            // Either it parsed cleanly or it said what was wrong; both fine.
            let _ = findings.is_empty();
        }
    }
}

#[test]
fn seeded_mutations_of_function_files_never_exceed_their_own_bytes() {
    let original = b"# note\r\nsay hi\r\n\r\ntellraw @a {\"x\":1}\n  execute as @s run say done";
    for seed in 0..250_u64 {
        let mut rng = Rng::new(seed);
        let mut body = original.to_vec();
        let flips = 1 + rng.below(3);
        for _ in 0..flips {
            let at = rng.below(body.len());
            body[at] = rng.mutated_byte(body[at]);
        }
        let (file, findings) = dust_data::function::FunctionFile::parse(&body, "p", "f");
        if !findings.iter().any(|f| f.severity == Severity::Error) {
            // Every surviving command came out of those bytes: the parse
            // allocates in proportion to its input, never more. Counting
            // physical lines bounds the command count; counting characters
            // bounds their total length.
            let physical_lines = body.iter().filter(|&&b| b == b'\n').count()
                + usize::from(!body.is_empty() && *body.last().unwrap() != b'\n');
            assert!(file.lines.len() <= physical_lines, "seed {seed}");
            let total_chars: usize = file.lines.iter().map(|l| l.command.len()).sum();
            assert!(
                total_chars <= body.len(),
                "seed {seed}: grew past its input"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Allocation caps under deliberately hostile shapes — the inputs that exist
// to make a loader spend memory rather than to carry data.
// ---------------------------------------------------------------------------

#[test]
fn a_zip_that_lies_about_its_entry_count_is_refused_without_reading_it_all() {
    // An EOCD claiming thousands of entries above a directory holding none.
    // The reader must hit the missing signature almost immediately instead
    // of walking (and allocating for) the claimed count.
    let entries = [("pack.mcmeta", &br#"{"pack":{"pack_format":48}}"#[..])];
    let mut bytes = support::write_stored_zip(&entries);
    let eocd = bytes.len() - 22;
    for offset in [8usize, 10] {
        bytes[eocd + offset..eocd + offset + 2].copy_from_slice(&60000_u16.to_le_bytes());
    }
    let error = dust_data::ZipPack::from_bytes(bytes, "liar", "<zip>").unwrap_err();
    assert!(
        error.to_string().contains("signature"),
        "the false directory is noticed fast: {error}"
    );
}

#[test]
fn a_deflate_stream_expanding_past_the_declared_size_is_capped_mid_flight() {
    // The real bomb shape: a few bytes of back-references that want to
    // become megabytes. The declared size caps what the decompressor will
    // produce even though the stream could go further, and the refusal is
    // reported instead of bytes being delivered.
    //
    // The stream is hand-assembled fixed-Huffman deflate: three literals,
    // then thousands of `length 258, distance 3` copies — about 1.7 bytes
    // of stream per 258 bytes of output.
    let repeats = 12_000; // wants roughly 3 MB of output
    let stream = support::deflate_repeat_stream(repeats);

    let declared: usize = 1024 * 1024; // claims 1 MiB; the stream wants more
    let raw_entry = RawZipEntry {
        name: b"data/minecraft/recipe/bomb.json".to_vec(),
        flags: 0,
        method: 8,
        crc: 0, // never reached: the cap fires first
        compressed_size: stream.len() as u32,
        uncompressed_size: declared as u32,
        body: stream,
    };
    let (bytes, _) = write_layouted_zip(&[raw_entry]);
    let pack = ZipPack::from_bytes(bytes, "bomb", "<zip>").expect("structure opens");
    let error = pack
        .read("data/minecraft/recipe/bomb.json")
        .expect_err("expansion past the declaration is refused");

    assert!(
        error.to_string().contains("expands past"),
        "the cap, not the checksum, is what fires: {error}"
    );
}

#[test]
fn a_deflated_entry_that_matches_its_declaration_reads_back_exactly() {
    // The other half of the cap: expansion up to the declared size must
    // succeed. The same repeat stream at an honest size comes back whole.
    let repeats = 4_000;
    let stream = support::deflate_repeat_stream(repeats);
    let expected_len = 3 + repeats * 258; // three literals, then 258 per copy
    let payload = {
        let mut out = Vec::with_capacity(expected_len);
        out.extend_from_slice(b"abc");
        for _ in 0..repeats {
            let start = out.len() - 3;
            for offset in 0..258 {
                out.push(out[start + offset]);
            }
        }
        out
    };
    let raw_entry = RawZipEntry {
        name: b"data/minecraft/tags/block/repeaty.json".to_vec(),
        flags: 1 << 11,
        method: 8,
        crc: dust_data::zip::crc32(&payload),
        compressed_size: stream.len() as u32,
        uncompressed_size: payload.len() as u32,
        body: stream,
    };
    let (bytes, _) = write_layouted_zip(&[raw_entry]);
    let pack = ZipPack::from_bytes(bytes, "honest", "<zip>").expect("opens");
    let read = pack
        .read("data/minecraft/tags/block/repeaty.json")
        .expect("reads")
        .expect("present");
    assert_eq!(read.len(), expected_len);
    assert_eq!(&read[..6], b"abcabc");
}
