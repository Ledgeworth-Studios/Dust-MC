//! The external corpus: real NBT, written by Minecraft, read from the cache.
//!
//! # Why this file exists and a round-trip test does not replace it
//!
//! Encoding a tag and decoding it back proves the encoder and the decoder agree
//! with *each other*. It passes under any internally consistent convention,
//! including a wrong one — sort every compound, write little-endian, encode
//! strings as standard UTF-8, and a round-trip suite stays green while every
//! file the server writes becomes unopenable. This project has already been
//! bitten by exactly that shape once: swapping the two slowest-varying
//! properties of every block left all 26,684 block states round-tripping
//! perfectly and put every chest at the wrong id, and the argument is written
//! out at length in `xtask/src/extract/codegen.rs`.
//!
//! So the checks here are not round-trips. They are:
//!
//! 1. **Byte-for-byte rewrite** of 1,180 structure files Mojang wrote, all of
//!    which this crate has to reproduce exactly. That check cannot be satisfied
//!    by being self-consistent: the target bytes were produced by a different
//!    implementation, in a different language, years ago.
//! 2. **Values read out of those files and compared against what the format
//!    says they mean** — a structure has a `size` of three ints, a `palette` of
//!    compounds each with a `Name`, a `DataVersion` that must be 1.21.1's.
//! 3. **A check that key order in a real file is not sorted**, which is what
//!    makes decision (1) in `Compound`'s doc comment load-bearing rather than
//!    arbitrary.
//!
//! # Where the corpus comes from
//!
//! `data/minecraft/structure/**.nbt` inside the 1.21.1 server jar: the
//! templates for bastions, ancient cities, trail ruins, igloos, villages and
//! the rest. Each is a gzip-wrapped file-form document. They are Mojang's
//! bytes, they are not committed, and they are read out of the gitignored
//! `.dust-extract/` cache at run time. When the cache is absent every test here
//! prints a visible SKIPPED line and passes, so CI stays green on a machine
//! that has never downloaded a jar.

mod support;

use std::collections::BTreeSet;

use dust_nbt::{compression, read, snbt, write, Compound, Compression, Tag};

/// Load every structure file from the jar, decompressed, as (name, bytes).
fn corpus() -> Option<Vec<(String, Vec<u8>)>> {
    let jar = support::server_jar()?;
    let inner = support::inner_jar(&jar).expect("the inner jar should be readable");
    let archive = support::Zip::open(&inner).expect("the inner jar is a readable zip");
    let mut out = Vec::new();
    for entry in archive.entries_ending_with(".nbt") {
        if !entry.name.starts_with("data/minecraft/structure/") {
            continue;
        }
        let compressed = archive
            .read_entry(entry)
            .unwrap_or_else(|| panic!("{} should inflate", entry.name));
        let plain = compression::decompress_detected(&compressed, compression::DEFAULT_FILE_LIMIT)
            .unwrap_or_else(|e| panic!("{} should decompress: {e}", entry.name));
        out.push((entry.name.clone(), plain.into_owned()));
    }
    Some(out)
}

/// The corpus has to be *there*, and this is the test that says so out loud.
#[test]
fn the_corpus_is_present_or_the_skip_is_visible() {
    let Some(files) = corpus() else {
        support::skip(
            "vanilla",
            "no .dust-extract/server-1.21.1.jar; run `cargo xtask extract` to populate the cache",
        );
        return;
    };
    // A number rather than "not empty": if a future jar reorganises the
    // structure directory, a silent drop to three files would otherwise still
    // pass every test in this file.
    assert!(
        files.len() > 1000,
        "expected over a thousand structure files in the 1.21.1 jar, found {}",
        files.len()
    );
}

/// Every one of Mojang's files, read and written back byte for byte.
///
/// This is the single check in the crate that a self-consistent-but-wrong
/// implementation cannot pass. It exercises, on real data: the big-endian
/// integer encodings, modified UTF-8, `TAG_Byte_Array`, `TAG_Int_Array`,
/// `TAG_List` of compounds, nested compounds, the empty root name, and — most
/// of all — compound field order, because a sorted or hashed map would change
/// the bytes of nearly every file here.
#[test]
fn every_vanilla_structure_rewrites_byte_for_byte() {
    let Some(files) = corpus() else {
        support::skip("vanilla::rewrite", "no server jar in .dust-extract");
        return;
    };
    let mut checked = 0usize;
    for (name, original) in &files {
        let document = read::from_bytes_exact(original)
            .unwrap_or_else(|e| panic!("{name} should parse as file-form NBT: {e}"));
        let rewritten = write::to_vec(&document.name, &document.tag)
            .unwrap_or_else(|e| panic!("{name} should serialise: {e}"));
        if rewritten != *original {
            let at = rewritten
                .iter()
                .zip(original.iter())
                .position(|(a, b)| a != b)
                .unwrap_or_else(|| original.len().min(rewritten.len()));
            panic!(
                "{name} did not rewrite byte for byte: {} bytes in, {} bytes out, first \
                 difference at byte {at}",
                original.len(),
                rewritten.len()
            );
        }
        checked += 1;
    }
    assert_eq!(checked, files.len());
}

/// The same files through the network dialect.
///
/// A file-form document with an empty root name and the same document in
/// network form differ by exactly the two bytes of the absent name length. That
/// is the whole of the 1.20.2 change, and asserting it on real documents is
/// what stops the two modes from drifting into being the same function.
#[test]
fn network_form_is_the_file_form_without_the_two_name_bytes() {
    let Some(files) = corpus() else {
        support::skip("vanilla::network", "no server jar in .dust-extract");
        return;
    };
    for (name, original) in files.iter().take(200) {
        let document = read::from_bytes_exact(original).expect("parses");
        assert_eq!(document.name, "", "{name} should have an empty root name");

        let network = write::to_vec_network(Some(&document.tag)).expect("serialises");
        assert_eq!(
            network.len(),
            original.len() - 2,
            "{name}: network form should be exactly two bytes shorter"
        );
        assert_eq!(network[0], original[0], "{name}: same root tag id");
        assert_eq!(
            network[1..],
            original[3..],
            "{name}: same payload after the id"
        );

        // `Limits::FILE`, not the network default: these are file documents
        // being pushed through the network *encoding*, and the largest of them
        // is a quarter of a megabyte of structure that expands past the 2 MiB
        // packet budget. Vanilla would refuse it in a packet too. The budget is
        // exercised deliberately in `tests/hostile.rs`; here it would only be
        // measuring that a structure file is not a packet.
        let back = read::from_bytes_network_with(&network, dust_nbt::Limits::FILE)
            .expect("network form parses")
            .expect("and is not the absent-NBT byte");
        assert_eq!(back, document.tag, "{name}: same tag through both dialects");
    }
}

/// Read real values out of real files and check them against what the format
/// says they mean, not against what we wrote a moment ago.
#[test]
fn vanilla_structures_hold_the_values_the_format_says_they_do() {
    let Some(files) = corpus() else {
        support::skip("vanilla::values", "no server jar in .dust-extract");
        return;
    };
    // 1.21.1's data version. This is Mojang's number, not ours, and it is in
    // every one of these files; if the crate mis-decoded a `TAG_Int` this would
    // be the first thing to notice.
    const DATA_VERSION_1_21_1: i32 = 3955;

    let mut with_palette = 0usize;
    for (name, bytes) in &files {
        let root = read::from_bytes_exact(bytes)
            .unwrap_or_else(|e| panic!("{name}: {e}"))
            .tag;
        let root = root
            .as_compound()
            .unwrap_or_else(|| panic!("{name}: a structure's root is a compound"));

        match root.get("DataVersion") {
            Some(Tag::Int(version)) => assert_eq!(
                *version, DATA_VERSION_1_21_1,
                "{name}: DataVersion should be 1.21.1's"
            ),
            other => panic!("{name}: DataVersion should be a TAG_Int, found {other:?}"),
        }

        // `size` is three ints: the bounding box the structure occupies. Every
        // one has to be positive, and the number of blocks recorded cannot
        // exceed the volume.
        let size = root
            .get("size")
            .and_then(Tag::as_list)
            .unwrap_or_else(|| panic!("{name}: size should be a TAG_List"));
        assert_eq!(size.len(), 3, "{name}: size should have three entries");
        let dimensions: Vec<i64> = size
            .iter()
            .map(|t| {
                t.as_i64()
                    .unwrap_or_else(|| panic!("{name}: size entries are numbers"))
            })
            .collect();
        assert!(
            dimensions.iter().all(|d| *d > 0),
            "{name}: every dimension should be positive, found {dimensions:?}"
        );
        let volume: i64 = dimensions.iter().product();

        let blocks = root
            .get("blocks")
            .and_then(Tag::as_list)
            .unwrap_or_else(|| panic!("{name}: blocks should be a TAG_List"));
        assert!(
            blocks.len() as i64 <= volume,
            "{name}: {} blocks recorded in a volume of {volume}",
            blocks.len()
        );

        // Each block entry is `{pos:[x,y,z],state:<index into palette>}`, and
        // `pos` is a TAG_List of three TAG_Ints — *not* a TAG_Int_Array, which
        // is what it looks like it ought to be and what this test asserted
        // until the corpus said otherwise. Both encode three integers and they
        // are different tags; a reader that conflated them would pass a
        // round-trip and fail here.
        if let Some(first) = blocks.get(0).and_then(Tag::as_compound) {
            match first.get("pos") {
                Some(Tag::List(pos)) => {
                    assert_eq!(pos.len(), 3, "{name}: a block position is three entries");
                    assert_eq!(
                        pos.element_type(),
                        dust_nbt::TagType::Int,
                        "{name}: a block position is a list of ints"
                    );
                }
                other => panic!("{name}: pos should be a TAG_List, found {other:?}"),
            }
            assert!(
                matches!(first.get("state"), Some(Tag::Int(_))),
                "{name}: state should be a TAG_Int"
            );
        }

        if let Some(palette) = root.get("palette").and_then(Tag::as_list) {
            with_palette += 1;
            for (index, entry) in palette.iter().enumerate() {
                let entry = entry
                    .as_compound()
                    .unwrap_or_else(|| panic!("{name}: palette entries are compounds"));
                let block_name = entry
                    .get("Name")
                    .and_then(Tag::as_str)
                    .unwrap_or_else(|| panic!("{name}: palette[{index}] should have a Name"));
                assert!(
                    block_name.starts_with("minecraft:"),
                    "{name}: palette[{index}] Name is {block_name:?}"
                );
            }
        }
    }
    assert!(
        with_palette > 900,
        "nearly every structure has a palette; only {with_palette} did"
    );
}

/// Which of the twelve value tags the external corpus actually reaches.
///
/// The byte-for-byte test says the corpus is reproduced exactly. It does not
/// say what is *in* it, and a corpus that turned out to contain only compounds
/// and ints would make that check far weaker than it reads. So this counts the
/// types and asserts the coverage — which means the claim in the report is a
/// measurement, and a future jar that stops containing floats fails here rather
/// than quietly narrowing the guarantee.
///
/// **What the corpus does not reach**: `TAG_Byte_Array` and `TAG_Long_Array`.
/// Neither appears anywhere in the 1,180 structure files. They occur in chunk
/// data — light arrays and the packed block-state words — and there is no chunk
/// in the jar. Those two tags are covered only by the generated round-trip in
/// `tests/roundtrip.rs` and by the hand-built fixtures in `tests/binary.rs`,
/// which is a weaker guarantee than the other ten have, and this is the note
/// that says so rather than leaving it to be discovered.
#[test]
fn the_corpus_reaches_ten_of_the_twelve_value_tags() {
    use dust_nbt::TagType;

    let Some(files) = corpus() else {
        support::skip("vanilla::coverage", "no server jar in .dust-extract");
        return;
    };

    fn count(tag: &Tag, seen: &mut std::collections::BTreeMap<TagType, usize>) {
        *seen.entry(tag.tag_type()).or_default() += 1;
        match tag {
            Tag::List(list) => list.iter().for_each(|t| count(t, seen)),
            Tag::Compound(compound) => {
                compound.iter().for_each(|(_, t)| count(t, seen));
            }
            _ => {}
        }
    }

    let mut seen = std::collections::BTreeMap::new();
    for (_, bytes) in &files {
        count(
            &read::from_bytes_exact(bytes).expect("parses").tag,
            &mut seen,
        );
    }

    for tag in [
        TagType::Byte,
        TagType::Short,
        TagType::Int,
        TagType::Long,
        TagType::Float,
        TagType::Double,
        TagType::String,
        TagType::List,
        TagType::Compound,
        TagType::IntArray,
    ] {
        assert!(
            seen.get(&tag).copied().unwrap_or(0) > 0,
            "{tag} does not appear anywhere in the corpus, so the byte-for-byte check \
             says nothing about it; seen: {seen:?}"
        );
    }
    assert_eq!(
        seen.get(&TagType::ByteArray).copied().unwrap_or(0),
        0,
        "a TAG_Byte_Array has appeared in the structure corpus; the note above about \
         this tag being covered only by generated data is now out of date"
    );
    assert_eq!(
        seen.get(&TagType::LongArray).copied().unwrap_or(0),
        0,
        "a TAG_Long_Array has appeared in the structure corpus; the note above about \
         this tag being covered only by generated data is now out of date"
    );
}

/// A real `TAG_Int_Array` from the corpus, checked against what it means.
///
/// An entity's `UUID` is four ints, most significant word first — that is the
/// format's definition, not ours. Finding one and checking its length is the
/// external check on `TAG_Int_Array` specifically, which is otherwise only
/// exercised in bulk by the rewrite test.
#[test]
fn a_real_int_array_is_a_uuid_of_four_words() {
    let Some(files) = corpus() else {
        support::skip("vanilla::int_array", "no server jar in .dust-extract");
        return;
    };

    fn find_uuid(tag: &Tag, out: &mut Vec<Vec<i32>>) {
        match tag {
            Tag::List(list) => list.iter().for_each(|t| find_uuid(t, out)),
            Tag::Compound(compound) => {
                for (key, value) in compound.iter() {
                    if key == "UUID" {
                        if let Tag::IntArray(words) = value {
                            out.push(words.clone());
                        }
                    }
                    find_uuid(value, out);
                }
            }
            _ => {}
        }
    }

    let mut uuids = Vec::new();
    for (_, bytes) in &files {
        find_uuid(
            &read::from_bytes_exact(bytes).expect("parses").tag,
            &mut uuids,
        );
    }
    assert!(
        !uuids.is_empty(),
        "the structure corpus should contain at least one entity UUID"
    );
    for words in &uuids {
        assert_eq!(
            words.len(),
            4,
            "a Minecraft UUID is four ints, found {} in {words:?}",
            words.len()
        );
    }
}

/// Compound order is the file's order, and the file's order is not sorted.
///
/// This is the test that makes `Compound`'s ordering decision falsifiable.
/// Vanilla's `CompoundTag` is a `HashMap` and writes in `keySet()` order, so
/// what is in the file is neither insertion order nor alphabetical. If someone
/// later "tidies" this crate by sorting compound keys, the byte-for-byte test
/// above fails — and this one says why in one line.
#[test]
fn vanilla_files_are_not_written_in_sorted_key_order() {
    let Some(files) = corpus() else {
        support::skip("vanilla::order", "no server jar in .dust-extract");
        return;
    };
    let mut unsorted = 0usize;
    let mut examined = 0usize;
    for (_, bytes) in &files {
        let root = read::from_bytes_exact(bytes).expect("parses").tag;
        let Some(compound) = root.as_compound() else {
            continue;
        };
        if compound.len() < 2 {
            continue;
        }
        examined += 1;
        let keys: Vec<&str> = compound.keys().collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        if keys != sorted {
            unsorted += 1;
        }
    }
    assert!(examined > 1000, "not enough roots examined: {examined}");
    assert!(
        unsorted > 0,
        "every one of {examined} vanilla roots happened to be in sorted key order, which \
         would mean this test can no longer tell a sorting implementation apart from a \
         preserving one"
    );
}

/// The jar's own strings, decoded by this crate's modified-UTF-8 decoder.
///
/// A Java class file stores its string constants in modified UTF-8, so a jar is
/// a corpus of tens of thousands of strings produced by `javac` rather than by
/// anything here. Decoding all of them and getting no error is a real external
/// check on the decoder; finding the ones that standard UTF-8 *rejects* and
/// decoding those correctly is a better one.
#[test]
fn the_jars_own_modified_utf8_decodes() {
    let Some(jar) = support::server_jar() else {
        support::skip("vanilla::mutf8", "no server jar in .dust-extract");
        return;
    };
    let inner = support::inner_jar(&jar).expect("readable");
    let archive = support::Zip::open(&inner).expect("readable zip");

    let mut decoded = 0usize;
    let mut not_standard_utf8 = Vec::new();
    for entry in archive.entries_ending_with(".class") {
        let Some(class) = archive.read_entry(entry) else {
            continue;
        };
        for raw in support::class_constant_strings(&class) {
            let text = dust_nbt::mutf8::decode(raw).unwrap_or_else(|e| {
                panic!(
                    "a constant in {} did not decode as modified UTF-8: {e} (bytes {raw:02x?})",
                    entry.name
                )
            });
            // Re-encoding has to give the original bytes back. `javac` writes
            // the canonical form, so anything else here is an encoder bug.
            assert_eq!(
                dust_nbt::mutf8::encode(&text),
                raw,
                "re-encoding a constant of {} changed it",
                entry.name
            );
            if std::str::from_utf8(raw).is_err() {
                not_standard_utf8.push((entry.name.clone(), raw.to_vec(), text.into_owned()));
            }
            decoded += 1;
        }
    }

    assert!(
        decoded > 100_000,
        "expected a great many string constants in the server jar, found {decoded}"
    );

    // The interesting part. The 1.21.1 server jar contains at least one string
    // that `str::from_utf8` refuses and `readUTF` accepts: a `%s%d` format
    // string with a trailing NUL, written `c0 80`. A crate that used
    // `str::from_utf8` for NBT strings would reject a document containing it.
    assert!(
        !not_standard_utf8.is_empty(),
        "the jar should contain at least one constant that is modified UTF-8 and not \
         standard UTF-8; if it no longer does, this test has stopped checking the thing \
         it was written for"
    );
    for (class, raw, text) in &not_standard_utf8 {
        assert!(
            raw.windows(2).any(|w| w == [0xc0, 0x80]) || raw.contains(&0xed),
            "{class}: a constant that is not standard UTF-8 should be so because of the \
             two-byte NUL or a surrogate pair, found {raw:02x?}"
        );
        assert!(
            text.contains('\0') || text.chars().any(|c| c as u32 > 0xffff),
            "{class}: decoding {raw:02x?} should have produced a NUL or a supplementary \
             character, produced {text:?}"
        );
    }
}

/// SNBT taken from outside this implementation: Mojang's own literals.
///
/// The 1.21.1 jar's constant pools contain thousands of SNBT documents, put
/// there by the DataFixerUpper's block-state renaming tables — things like
/// `{Name:'minecraft:oak_door',Properties:{facing:'east',half:'lower'}}`. They
/// are SNBT that Mojang wrote and Mojang's parser reads, so they are the
/// external anchor the printer's round-trip needs: parsing them checks the
/// parser against text nobody here composed, and re-printing and re-parsing
/// checks that the printer's output survives the parser it was not written
/// against.
#[test]
fn mojangs_own_snbt_literals_parse_and_survive_the_printer() {
    let Some(jar) = support::server_jar() else {
        support::skip("vanilla::snbt", "no server jar in .dust-extract");
        return;
    };
    let inner = support::inner_jar(&jar).expect("readable");
    let archive = support::Zip::open(&inner).expect("readable zip");

    let mut literals = BTreeSet::new();
    for entry in archive.entries_ending_with(".class") {
        let Some(class) = archive.read_entry(entry) else {
            continue;
        };
        for raw in support::class_constant_strings(&class) {
            let Ok(text) = std::str::from_utf8(raw) else {
                continue;
            };
            // A compound literal: starts `{`, ends `}`, has a `key:` in it,
            // and contains neither a control character nor a space. The pool
            // also holds `String.format` templates with `\x01` placeholders and
            // log messages like `"{} lost connection: {}"`, both of which look
            // like this test's quarry and are not SNBT.
            if text.len() > 2
                && text.starts_with('{')
                && text.ends_with('}')
                && text.contains(':')
                && !text.chars().any(|c| c.is_control() || c == ' ')
            {
                literals.insert(text.to_owned());
            }
        }
    }

    assert!(
        literals.len() > 5000,
        "expected thousands of SNBT literals in the jar, found {}",
        literals.len()
    );

    for literal in &literals {
        let parsed = snbt::parse(literal)
            .unwrap_or_else(|e| panic!("Mojang's own SNBT {literal:?} did not parse: {e}"));
        assert!(
            matches!(parsed, Tag::Compound(_)),
            "{literal:?} should parse as a compound"
        );

        let printed = snbt::to_string(&parsed);
        let reparsed = snbt::parse(&printed).unwrap_or_else(|e| {
            panic!("our own printed form {printed:?} of {literal:?} did not re-parse: {e}")
        });
        assert_eq!(
            reparsed, parsed,
            "printing and re-parsing {literal:?} changed it (printed {printed:?})"
        );
    }

    // One of them by hand, so that the sweep above cannot pass by parsing
    // everything into the same empty compound.
    let known = "{Name:'minecraft:stone'}";
    assert!(
        literals.contains(known),
        "the jar should contain {known}; if it no longer does, pick another and say so"
    );
    let mut expected = Compound::new();
    expected.insert("Name", Tag::String("minecraft:stone".to_owned()));
    assert_eq!(snbt::parse(known).unwrap(), Tag::Compound(expected));
}

/// The three region-file compression schemes, exercised on a real document.
///
/// The gzip half is not a self-consistency check: these files arrive from the
/// jar gzip-compressed by Java's `GZIPOutputStream`, and inflating them is this
/// crate reading a stream another implementation produced.
#[test]
fn all_three_compression_schemes_carry_a_real_document() {
    let Some(files) = corpus() else {
        support::skip("vanilla::compression", "no server jar in .dust-extract");
        return;
    };
    let (_, plain) = &files[0];
    for scheme in [Compression::None, Compression::Gzip, Compression::Zlib] {
        let wrapped = compression::compress(plain, scheme).expect("compresses");
        assert_eq!(
            Compression::detect(&wrapped),
            scheme,
            "detection should recognise what we just wrote as {scheme:?}"
        );
        assert_eq!(
            Compression::from_region_scheme(scheme.region_scheme()),
            Some(scheme)
        );
        let back = compression::decompress(&wrapped, scheme, compression::DEFAULT_FILE_LIMIT)
            .expect("decompresses");
        assert_eq!(&*back, plain.as_slice(), "{scheme:?} did not round-trip");
    }
}
