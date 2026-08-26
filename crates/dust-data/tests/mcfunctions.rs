//! Function files end to end: loaded like every other resource, provenance
//! and all, with the file rules from `src/function.rs` holding through real
//! packs in all three containers.
//!
//! The one behavioural surprise worth its own tests is the pre-1.21
//! `functions/` spelling: Minecraft reads only `function/`, so a pack
//! carrying both spellings reaches one name two ways, and what Dust does
//! about that has to be said out loud rather than decided by map order.

mod support;

use dust_data::registry::RegistryId;
use dust_data::{load, LoadOptions, PackSource, ResourceLocation};
use support::PackBuilder;

fn location(text: &str) -> ResourceLocation {
    ResourceLocation::parse(text).expect("valid")
}

fn functions_of(
    data: &dust_data::LoadedData,
) -> &std::collections::BTreeMap<ResourceLocation, dust_data::LoadedFunction> {
    data.functions(&RegistryId::new("function"))
        .expect("function registry present")
}

#[test]
fn a_function_loads_with_line_numbers_counting_every_physical_line() {
    let pack = PackBuilder::new("clockwork")
        .file(
            "data/minecraft/function/tick.mcfunction",
            "# runs often\nsay one\n\n   say two\n",
        )
        .build();
    let data = load(&[&pack as &dyn PackSource], &LoadOptions::default());
    assert_eq!(data.error_count(), 0, "{:?}", data.findings());

    let tick = functions_of(&data)
        .get(&location("minecraft:tick"))
        .expect("loaded");
    assert_eq!(
        tick.file.lines.iter().map(|l| l.number).collect::<Vec<_>>(),
        vec![2, 4],
        "numbers are physical lines, comments and blanks included"
    );
    assert_eq!(data.stats().functions, 1);
    assert_eq!(data.stats().files_read, 1);
}

#[test]
fn a_function_from_a_zip_is_identical_to_its_directory_twin() {
    let builder = PackBuilder::new("twin").file(
        "data/minecraft/function/tick.mcfunction",
        "# comment\r\nsay hi\r\ntellraw @a {\"x\":1}\r\n",
    );

    let root = support::TempDir::new("function_twin");
    let dir = dust_data::DirectoryPack::open(builder.build_directory(&root));
    let zip =
        dust_data::ZipPack::from_bytes(builder.build_zip_bytes(), "twin", "<zip>").expect("zip");

    let from_dir = load(&[&dir as &dyn PackSource], &LoadOptions::default());
    let from_zip = load(&[&zip as &dyn PackSource], &LoadOptions::default());
    assert_eq!(from_dir.stats(), from_zip.stats());
    assert_eq!(from_dir.findings(), from_zip.findings());
}

#[test]
fn a_later_pack_overrides_a_function_and_the_earlier_is_remembered() {
    // The same rule recipes obey, because a function file is a definition of
    // one name exactly as much as a recipe is.
    let base =
        PackBuilder::new("base").file("data/minecraft/function/tick.mcfunction", "say from base\n");
    let over =
        PackBuilder::new("over").file("data/minecraft/function/tick.mcfunction", "say from over\n");

    let data = load(
        &[
            &base.build() as &dyn PackSource,
            &over.build() as &dyn PackSource,
        ],
        &LoadOptions::default(),
    );
    assert_eq!(data.error_count(), 0, "{:?}", data.findings());
    let tick = functions_of(&data)
        .get(&location("minecraft:tick"))
        .expect("loaded");
    assert_eq!(tick.file.lines[0].command, "say from over");
    assert_eq!(tick.overridden, vec!["base".to_owned()]);
    assert_eq!(data.stats().overrides, 1);
}

#[test]
fn both_directory_spellings_in_one_pack_collide_and_the_current_one_wins() {
    // `functions/` is the pre-1.21 name. Vanilla would read only `function/`
    // and never notice the second copy; merging the spellings into one
    // namespace means Dust has to pick, and picking silently is how half a
    // pack goes mysteriously inert.
    let pack = PackBuilder::new("old_and_new")
        .file("data/minecraft/functions/tick.mcfunction", "say legacy\n")
        .file("data/minecraft/function/tick.mcfunction", "say current\n")
        .build();

    let data = load(&[&pack as &dyn PackSource], &LoadOptions::default());
    assert_eq!(data.error_count(), 0, "{:?}", data.findings());
    assert_eq!(data.stats().functions, 1);

    let tick = functions_of(&data)
        .get(&location("minecraft:tick"))
        .expect("one function under the merged name");
    assert_eq!(
        tick.file.lines[0].command, "say current",
        "the copy under the current spelling wins"
    );
    assert_eq!(tick.path, "data/minecraft/function/tick.mcfunction");
    assert!(
        data.findings().iter().any(|f| f
            .message
            .contains("defines the function `minecraft:tick` twice")),
        "{:?}",
        data.findings()
    );
}

#[test]
fn a_legacy_spelling_alone_loads_with_the_rename_warning() {
    let pack = PackBuilder::new("legacy_only")
        .file("data/minecraft/functions/tick.mcfunction", "say hi\n")
        .build();
    let data = load(&[&pack as &dyn PackSource], &LoadOptions::default());
    assert_eq!(data.error_count(), 0, "{:?}", data.findings());
    assert_eq!(data.stats().functions, 1);
    assert!(
        data.findings()
            .iter()
            .any(|f| f.message.contains("pre-1.21")),
        "{:?}",
        data.findings()
    );
}

#[test]
fn a_file_with_the_wrong_extension_under_function_is_one_warning() {
    let pack = PackBuilder::new("sloppy")
        .file("data/minecraft/function/tick.mcfunction", "say fine\n")
        .file("data/minecraft/function/notes.txt", "not a function")
        .file("data/minecraft/function/other.txt", "also not")
        .build();
    let data = load(&[&pack as &dyn PackSource], &LoadOptions::default());

    let about_extension: Vec<_> = data
        .findings()
        .iter()
        .filter(|f| f.message.contains(".mcfunction"))
        .collect();
    assert_eq!(about_extension.len(), 1, "{:?}", data.findings());
    assert!(
        about_extension[0].message.contains("2 file(s)"),
        "{}",
        about_extension[0]
    );
    assert_eq!(data.stats().functions, 1);
}

#[test]
fn an_invalid_function_file_costs_its_own_finding_and_not_the_pack() {
    // The builder takes text only, which is itself the encoding rule at
    // work; the raw constructor exists so this test can hold bytes a text
    // builder must not.
    let pack = support::MemPack::with_raw(
        "mixed_encoding",
        &[
            (
                "pack.mcmeta",
                br#"{"pack":{"pack_format":48,"description":"t"}}"#.as_slice(),
            ),
            ("data/minecraft/function/good.mcfunction", b"say fine\n"),
            (
                "data/minecraft/function/bad.mcfunction",
                &[b's', b'a', b'y', 0xff],
            ),
        ],
    );

    let data = load(&[&pack as &dyn PackSource], &LoadOptions::default());
    assert_eq!(data.error_count(), 1, "{:?}", data.findings());
    assert!(
        data.findings()[0].message.contains("UTF-8"),
        "{}",
        data.findings()[0]
    );
    assert!(functions_of(&data).contains_key(&location("minecraft:good")));
    assert!(!functions_of(&data).contains_key(&location("minecraft:bad")));
}

#[test]
fn an_empty_function_file_still_loads_as_an_empty_function() {
    let pack = PackBuilder::new("quiet")
        .file(
            "data/minecraft/function/silence.mcfunction",
            "# nothing yet\n",
        )
        .build();
    let data = load(&[&pack as &dyn PackSource], &LoadOptions::default());
    assert_eq!(data.error_count(), 0, "{:?}", data.findings());
    let silence = functions_of(&data)
        .get(&location("minecraft:silence"))
        .expect("loaded");
    assert_eq!(silence.file.command_count(), 0);
}
