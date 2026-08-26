//! The zip container end to end: built here with a hand-rolled stored-entry
//! writer, checked against its directory twin, and — when the machine has it —
//! against archives written by the system `zip` binary.
//!
//! That last test is the one that matters most. Everything else in this file
//! is this codebase agreeing with itself; an archive produced by an outside
//! compressor is the only thing that can catch a decompressor bug, because
//! its CRC-32 was computed by somebody else.

mod support;

use dust_data::registry::RegistryId;
use dust_data::zip::ZipError;
use dust_data::{load, LoadOptions, PackError, PackSource, ResourceLocation, Severity, ZipPack};
use support::{write_layouted_zip, PackBuilder, RawZipEntry, ZipEntryLayout};

fn location(text: &str) -> ResourceLocation {
    ResourceLocation::parse(text).expect("valid")
}

#[test]
fn a_zip_loads_exactly_like_its_directory_twin() {
    let builder = PackBuilder::new("twin")
        .resource(
            "minecraft",
            "recipe",
            "shaped",
            r#"{"type":"minecraft:crafting_shaped","result":{"item":"minecraft:x"}}"#,
        )
        .resource(
            "minecraft",
            "tags/block",
            "ores",
            r#"{"values":["minecraft:iron_ore"]}"#,
        )
        .resource(
            "minecraft",
            "advancement",
            "root",
            r#"{"display":{"icon":{"item":"minecraft:compass"}},"criteria":{}}"#,
        );

    let root = support::TempDir::new("zip_twin");
    let dir = dust_data::DirectoryPack::open(builder.build_directory(&root));
    let zip = ZipPack::from_bytes(builder.build_zip_bytes(), "twin", "<zip>").expect("zip");

    let from_dir = load(&[&dir as &dyn PackSource], &LoadOptions::default());
    let from_zip = load(&[&zip as &dyn PackSource], &LoadOptions::default());

    assert_eq!(from_dir.stats(), from_zip.stats());
    assert_eq!(from_dir.findings(), from_zip.findings());
    assert_eq!(from_dir.namespaces(), from_zip.namespaces());
}

#[test]
fn names_that_climb_out_of_the_archive_are_refused_at_open() {
    // Path traversal in entry names. Dust never extracts, so there is no
    // file to overwrite today — but the name becomes resource paths and log
    // lines, and the day an extract command exists is not the day to start
    // checking. Refused at open, loudly.
    let evil = support::write_stored_zip(&[
        ("pack.mcmeta", br#"{"pack":{"pack_format":48}}"#),
        ("../../etc/passwd", b"nope"),
    ]);
    let error =
        ZipPack::from_bytes(evil, "evil", "<zip>").expect_err("a climbing name must not open");
    assert!(matches!(error, PackError::Zip { .. }), "{error}");
}

#[test]
fn a_corrupt_entry_is_a_finding_and_not_a_crash() {
    let builder = PackBuilder::new("corrupt")
        .resource(
            "minecraft",
            "recipe",
            "fine",
            r#"{"type":"minecraft:crafting_shaped"}"#,
        )
        .resource(
            "minecraft",
            "recipe",
            "damaged",
            r#"{"type":"minecraft:crafting_shapeless"}"#,
        );
    let mut bytes = builder.build_zip_bytes();

    // Flip one bit inside the *stored* payload of the second entry. The
    // archive structure stays valid; only the content checksum can notice,
    // which is exactly why every read verifies it.
    let needle = br#"{"type":"minecraft:crafting_shapeless"}"#;
    let position = bytes
        .windows(needle.len())
        .position(|window| window == needle)
        .expect("the payload is in the archive");
    bytes[position] ^= 0x01;

    let zip = ZipPack::from_bytes(bytes, "corrupt", "<zip>").expect("structure still opens");
    let data = load(&[&zip as &dyn PackSource], &LoadOptions::default());

    assert_eq!(data.error_count(), 1, "{:?}", data.findings());
    let finding = &data.findings()[0];
    assert!(finding.message.contains("checksum"), "{}", finding);
    assert!(finding.file.contains("damaged.json"), "{}", finding);

    // The untouched sibling still loaded.
    assert!(data
        .get(&RegistryId::new("recipe"), &location("minecraft:fine"))
        .is_some());
}

#[test]
fn a_declared_size_past_the_cap_is_refused_before_decompressing() {
    // The zip bomb shape: a tiny archive claiming a huge uncompressed size.
    // The reader must trust neither field alone — declared size caps the
    // allocation, actual output must match the declaration, and a lie either
    // way fails.
    let entries = [("data/minecraft/recipe/x.json", &b"{}"[..])];
    let mut bytes = support::write_stored_zip(&entries);

    // Patch both declarations of `uncompressed_size` (local header and
    // central directory) past the cap. Offsets: local header field sits at
    // entry offset + 22; the central record's copy follows its own fixed
    // stride, computed here from the layout the writer produced.
    const LIE: u32 = 500 * 1024 * 1024;
    let body_len = entries[0].1.len() as u32;
    let local_offset = u32::from_le_bytes([
        bytes[bytes.len() - 6..][0],
        bytes[bytes.len() - 6..][1],
        bytes[bytes.len() - 6..][2],
        bytes[bytes.len() - 6..][3],
    ]);
    // The EOCD's central-directory offset field is 12 bytes from the end.
    let directory_at = u32::from_le_bytes([
        bytes[bytes.len() - 10..][0],
        bytes[bytes.len() - 10..][1],
        bytes[bytes.len() - 10..][2],
        bytes[bytes.len() - 10..][3],
    ]) as usize;

    let patch = |bytes: &mut Vec<u8>, at: usize| {
        bytes[at..at + 4].copy_from_slice(&LIE.to_le_bytes());
    };
    patch(&mut bytes, local_offset as usize + 22); // local header
    patch(&mut bytes, directory_at + 24); // central directory

    let _ = body_len; // kept for the arithmetic above
    let zip = ZipPack::from_bytes(bytes, "bomb", "<zip>").expect("structure parses");
    let error = zip
        .read("data/minecraft/recipe/x.json")
        .expect_err("declared size past the cap");
    assert!(matches!(error, PackError::Zip { .. }), "{error}");
}

#[test]
fn local_headers_that_disagree_with_the_directory_are_refused_at_read() {
    // Every field both records carry is checked in its own case below. The
    // point of doing all four: a partial writer that fixed one field but not
    // another must not slip through whichever check happens to be missing.
    let body = br#"{"type":"minecraft:crafting_shaped"}"#;
    let base = RawZipEntry {
        name: b"data/minecraft/recipe/x.json".to_vec(),
        flags: 0,
        method: 0,
        crc: dust_data::zip::crc32(body),
        compressed_size: body.len() as u32,
        uncompressed_size: body.len() as u32,
        body: body.to_vec(),
    };

    let patch_u16 = |bytes: &mut [u8], at: usize, value: u16| {
        bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
    };
    let patch_u32 = |bytes: &mut [u8], at: usize, value: u32| {
        bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    };

    /// One field edit against one entry's records, applied after the writer
    /// has laid the archive out.
    type Patch = Box<dyn Fn(&mut Vec<u8>, &ZipEntryLayout)>;

    struct Case {
        label: &'static str,
        edit: Patch,
    }

    let cases = vec![
        Case {
            label: "a different name",
            edit: Box::new(|bytes, entry| {
                bytes[entry.local_header + 30] = b'X';
            }),
        },
        Case {
            label: "a different method",
            edit: Box::new(move |bytes, entry| {
                patch_u16(bytes, entry.local_header + 8, 8);
            }),
        },
        Case {
            label: "different sizes",
            edit: Box::new(move |bytes, entry| {
                patch_u32(bytes, entry.local_header + 18, 999);
            }),
        },
        Case {
            label: "a different checksum",
            edit: Box::new(move |bytes, entry| {
                patch_u32(bytes, entry.local_header + 14, 0xdead_beef);
            }),
        },
    ];

    for case in cases {
        let entry = base.clone();
        let (mut bytes, layout) = write_layouted_zip(&[entry]);
        (case.edit)(&mut bytes, &layout.entries[0]);
        let pack =
            ZipPack::from_bytes(bytes, "disagree", "<zip>").expect("the archive still opens");
        let error = pack
            .read("data/minecraft/recipe/x.json")
            .expect_err(case.label);
        let label = case.label;
        assert!(error.to_string().contains("disagree"), "{label}: {error}");
    }
}

#[test]
fn the_streaming_flag_excuses_local_fields_that_were_never_filled_in() {
    // Bit 3 means "sizes and checksum arrive after the data" — the local
    // copies are legitimately unfilled, so refusing them would refuse every
    // archive written by a streaming tool. Name and method are known up
    // front and are still checked.
    let body = b"{}";
    let raw = RawZipEntry {
        name: b"data/minecraft/recipe/streamed.json".to_vec(),
        flags: 1 << 3,
        method: 0,
        crc: dust_data::zip::crc32(body),
        compressed_size: body.len() as u32,
        uncompressed_size: body.len() as u32,
        body: body.to_vec(),
    };
    let (mut bytes, layout) = write_layouted_zip(&[raw]);
    // The local copies of checksum and both sizes are zeroed, as a streaming
    // writer would have left them. The name, method and lengths stay put.
    let local = layout.entries[0].local_header;
    bytes[local + 14..local + 26].copy_from_slice(&[0u8; 12]);

    let pack = ZipPack::from_bytes(bytes, "stream", "<zip>").expect("opens");
    let read = pack
        .read("data/minecraft/recipe/streamed.json")
        .expect("reads")
        .expect("present");
    assert_eq!(read, body);
}

#[test]
fn the_unicode_flag_makes_a_non_utf8_name_a_refusal() {
    // The flag promises UTF-8. Bytes that break that promise are corruption,
    // and silently replacing them would invent a resource path no pack wrote.
    let raw = RawZipEntry {
        name: vec![b'd', b'a', b't', b'a', b'/', 0xff],
        flags: 1 << 11,
        method: 0,
        crc: 0,
        compressed_size: 0,
        uncompressed_size: 0,
        body: Vec::new(),
    };
    let (bytes, _) = write_layouted_zip(&[raw]);
    let error = ZipPack::from_bytes(bytes, "badname", "<zip>").expect_err("refused");
    assert!(
        matches!(
            error,
            PackError::Zip {
                source: ZipError::NameNotUtf8 { .. },
                ..
            }
        ),
        "{error}"
    );
}

#[test]
fn unicode_names_survive_both_compression_methods() {
    // A non-ASCII name under the flag, once stored and once deflated through
    // the fixed-Huffman encoder in support. Both copies must come back with
    // the name exactly as written and the payload intact.
    let payload = br#"{"values":["minecraft:stone"]}"#;
    for method in [0u16, 8u16] {
        let name: Vec<u8> = "data/minecraft/tags/block/for\u{ea}ges.json"
            .to_string()
            .into_bytes();
        let body = if method == 8 {
            support::write_deflate_fixed(payload)
        } else {
            payload.to_vec()
        };
        let raw = RawZipEntry {
            name,
            flags: 1 << 11,
            method,
            crc: dust_data::zip::crc32(payload),
            compressed_size: body.len() as u32,
            uncompressed_size: payload.len() as u32,
            body,
        };
        let (bytes, _) = write_layouted_zip(&[raw]);
        let pack = ZipPack::from_bytes(bytes, "unicode", "<zip>").expect("opens");
        let listed = pack.list().expect("lists");
        assert_eq!(
            listed,
            vec![format!("data/minecraft/tags/block/for\u{ea}ges.json")]
        );
        let read = pack
            .read("data/minecraft/tags/block/for\u{ea}ges.json")
            .expect("reads")
            .expect("present");
        assert_eq!(read, payload);
    }
}

#[test]
fn zip64_sentinels_are_refused_wherever_they_sit() {
    let body = b"{}".to_vec();
    let make = || RawZipEntry {
        name: b"data/minecraft/recipe/z.json".to_vec(),
        flags: 0,
        method: 0,
        crc: dust_data::zip::crc32(&body),
        compressed_size: body.len() as u32,
        uncompressed_size: body.len() as u32,
        body: body.clone(),
    };

    // The count sentinel in the end-of-directory record.
    let (mut bytes, layout) = write_layouted_zip(&[make()]);
    bytes[layout.eocd + 10..layout.eocd + 12].copy_from_slice(&0xffff_u16.to_le_bytes());
    assert!(matches!(
        ZipPack::from_bytes(bytes, "z64", "<z>").unwrap_err(),
        PackError::Zip {
            source: ZipError::Zip64,
            ..
        }
    ));

    // The size sentinel in one entry's directory record.
    let (mut bytes, layout) = write_layouted_zip(&[make()]);
    let at = layout.entries[0].central_directory + 24;
    bytes[at..at + 4].copy_from_slice(&0xffff_ffff_u32.to_le_bytes());
    assert!(matches!(
        ZipPack::from_bytes(bytes, "z64", "<z>").unwrap_err(),
        PackError::Zip {
            source: ZipError::Zip64,
            ..
        }
    ));

    // The offset sentinel, also in the directory record.
    let (mut bytes, layout) = write_layouted_zip(&[make()]);
    let at = layout.entries[0].central_directory + 42;
    bytes[at..at + 4].copy_from_slice(&0xffff_ffff_u32.to_le_bytes());
    assert!(matches!(
        ZipPack::from_bytes(bytes, "z64", "<z>").unwrap_err(),
        PackError::Zip {
            source: ZipError::Zip64,
            ..
        }
    ));
}

#[test]
fn a_zip64_record_that_is_not_needed_does_not_stop_the_archive() {
    // Some writers always emit the zip64 end-of-directory locator even when
    // every value fits the ordinary fields. Only the *sentinels* mean zip64
    // is required; an archive whose ordinary fields are truthful opens fine.
    const LOCATOR: u32 = 0x0706_4b50;
    let body = b"{}";
    let raw = RawZipEntry {
        name: b"data/minecraft/recipe/plain.json".to_vec(),
        flags: 0,
        method: 0,
        crc: dust_data::zip::crc32(body),
        compressed_size: body.len() as u32,
        uncompressed_size: body.len() as u32,
        body: body.to_vec(),
    };
    let (mut bytes, layout) = write_layouted_zip(&[raw]);
    // Splice the 20-byte locator in directly before the EOCD.
    let insert_at = layout.eocd;
    let mut locator = LOCATOR.to_le_bytes().to_vec();
    locator.extend_from_slice(&0_u32.to_le_bytes()); // disk
    locator.extend_from_slice(&0_u64.to_le_bytes()); // zip64 eocd offset
    locator.extend_from_slice(&1_u32.to_le_bytes()); // total disks
    bytes.splice(insert_at..insert_at, locator);

    let pack = ZipPack::from_bytes(bytes, "harmless", "<z>").expect("opens");
    assert_eq!(
        pack.read("data/minecraft/recipe/plain.json")
            .unwrap()
            .unwrap(),
        body
    );
}

#[test]
fn the_system_zipper_and_dust_agree() {
    // Archives from an outside compressor: deflate, real CRCs, none of our
    // own bytes anywhere in them. Skipped loudly where `zip` is absent, for
    // the same reason the corpus tests skip — a green that means nothing is
    // worse than no green.
    let zipper = ["/usr/bin/zip", "/bin/zip"]
        .into_iter()
        .find(|path| std::path::Path::new(path).exists());
    let Some(zipper) = zipper else {
        support::skipped(
            "the_system_zipper_and_dust_agree",
            "no system `zip` binary on this machine",
        );
        return;
    };

    let root = support::TempDir::new("system_zip");
    for (name, body) in [
        (
            "pack.mcmeta",
            r#"{"pack":{"pack_format":48,"description":"zipped by the system"}}"#,
        ),
        (
            "data/minecraft/recipe/a.json",
            r#"{"type":"minecraft:crafting_shaped"}"#,
        ),
        (
            "data/minecraft/tags/block/mineable.json",
            r#"{"values":["minecraft:stone","minecraft:dirt"]}"#,
        ),
        ("README.md", "not data, still archived"),
    ] {
        let full = root.path.join(name);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, body).unwrap();
    }
    let status = std::process::Command::new(zipper)
        .arg("-q") // quiet
        .arg("-X") // no extra fields
        .arg("-r") // recurse into data/
        .arg("out.zip")
        .arg("data")
        .arg("README.md")
        .arg("pack.mcmeta")
        .current_dir(&root.path)
        .status()
        .expect("run zip");
    assert!(status.success(), "the system zipper failed");

    let pack = ZipPack::open(root.path.join("out.zip")).expect("opens");
    let listed = pack.list().expect("lists");
    assert_eq!(
        listed,
        vec![
            "README.md".to_owned(),
            "data/minecraft/recipe/a.json".to_owned(),
            "data/minecraft/tags/block/mineable.json".to_owned(),
            "pack.mcmeta".to_owned(),
        ]
    );
    // Deflated contents come back byte-for-byte.
    let recipe = pack.read("data/minecraft/recipe/a.json").unwrap().unwrap();
    assert_eq!(
        String::from_utf8(recipe).unwrap(),
        r#"{"type":"minecraft:crafting_shaped"}"#
    );

    // And through the loader, findings clean.
    let data = load(&[&pack as &dyn PackSource], &LoadOptions::default());
    assert_eq!(
        data.findings()
            .iter()
            .filter(|f| f.severity == Severity::Error)
            .count(),
        0,
        "{:?}",
        data.findings()
    );
}
