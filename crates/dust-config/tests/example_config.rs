//! The example configuration has to be a configuration.
//!
//! `dust.toml.example` is the first file most operators will edit, and an
//! example that no longer parses is worse than no example — it teaches a syntax
//! the server rejects. It is also the file most likely to be forgotten when a
//! setting is renamed, because nothing else refers to it.

use std::collections::BTreeSet;
use std::path::PathBuf;

use dust_config::ore::{OreGroup, VANILLA_ORE_GROUPS};
use dust_config::DustConfig;

fn example_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("dust.toml.example")
}

fn example() -> DustConfig {
    let text = std::fs::read_to_string(example_path()).expect("dust.toml.example exists");
    DustConfig::from_toml_and_env(&text, "dust.toml.example", [])
        .unwrap_or_else(|e| panic!("dust.toml.example does not load:\n{e}"))
}

#[test]
fn the_example_configuration_loads() {
    // Not just parsed — actually carrying the values it appears to carry.
    let config = example();
    assert_eq!(
        config
            .worldgen
            .ores
            .resolve_group(&OreGroup::new("diamond"))
            .frequency,
        3.0
    );
}

#[test]
fn every_ore_the_example_names_is_a_real_ore() {
    // The example is also documentation, and an example naming an ore that does
    // not exist teaches the wrong name to everyone who copies it.
    let vanilla: BTreeSet<OreGroup> = VANILLA_ORE_GROUPS
        .iter()
        .map(|g| OreGroup::new(*g))
        .collect();
    let findings = example()
        .worldgen
        .ores
        .validate_against(&vanilla, "worldgen.ores");
    assert!(findings.is_empty(), "{findings:?}");
}
