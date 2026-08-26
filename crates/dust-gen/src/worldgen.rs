//! The worldgen vocabulary Phase 6 will build terrain on top of.
//!
//! Before any terrain generates, the engine needs to know the *language* of
//! vanilla's terrain definitions: which density-function types exist and what
//! arguments they take, which slots a noise router wires, what the seven
//! multi-noise parameters are. This module is that language, extracted from
//! vanilla's own data pack and its biome-parameter report.
//!
//! # What is deliberately absent
//!
//! The overworld's biome-parameter expansion — 7,593 ranged entries over 53
//! biomes — is not here, and should stay out. It is world-generation *data*:
//! a real server reads it from the world's datapacks at boot, exactly like
//! recipe contents and loot drops, both of which this pipeline also declines
//! to bake in. A datapack that reshapes the overworld would make a baked copy
//! wrong; nothing makes a vocabulary wrong. The nether's five points are here
//! because five rows are a worked example, not a dataset.
//!
//! The ore baseline in [`crate::vanilla_ores`] is the deliberate exception to
//! that rule, and D6 is where that exception's reasoning lives: operators
//! configure Dust against vanilla's ores specifically, so those numbers have
//! to ship.

use crate::generated::worldgen::{
    BIOME_PARAMETER_NAMES, DENSITY_FUNCTION_TYPES, NETHER_BIOME_POINTS,
};

pub use crate::generated::worldgen::BIOME_PARAMETER_DIMENSIONS;

/// One density-function type, as vanilla uses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DensityFunctionType {
    /// Namespaced type id, e.g. `minecraft:add`.
    pub name: &'static str,
    /// How many objects of this type appear across vanilla's density functions,
    /// nested appearances included.
    pub uses: usize,
    /// Top-level argument keys objects of this type carry, `type` excluded,
    /// sorted.
    pub arguments: &'static [&'static str],
}

/// Per-dimension summary of the biome-parameter report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BiomeParameterDimension {
    pub dimension: &'static str,
    /// Parameter entries written for this dimension — for the overworld, the
    /// full ranged expansion.
    pub entries: usize,
    /// Entries carrying `[min, max]` ranges rather than single values.
    pub ranged_entries: usize,
    pub distinct_biomes: usize,
}

/// Every density-function type in use, name-sorted.
pub fn density_function_types() -> impl Iterator<Item = &'static DensityFunctionType> {
    DENSITY_FUNCTION_TYPES.iter()
}

/// Look up a density-function type by its namespaced id.
pub fn density_function_type(name: &str) -> Option<&'static DensityFunctionType> {
    DENSITY_FUNCTION_TYPES
        .binary_search_by(|function| function.name.cmp(name))
        .ok()
        .map(|index| &DENSITY_FUNCTION_TYPES[index])
}

/// The seven multi-noise parameters, alphabetically.
pub const PARAMETER_NAMES: &[&str] = BIOME_PARAMETER_NAMES;

/// A biome's position in the multi-noise space, all seven parameters at once.
///
/// Values follow [`PARAMETER_NAMES`] order. Only point-shaped dimensions can
/// be held exactly; the nether is one, and is the table's golden sample.
pub fn nether_biome_points() -> impl Iterator<Item = (&'static str, [f64; 7])> {
    NETHER_BIOME_POINTS
        .iter()
        .map(|(biome, values)| (*biome, *values))
}

/// The parameter value at `name`'s position, for readers that want one axis.
pub fn parameter_value(point: [f64; 7], name: &str) -> Option<f64> {
    let index = PARAMETER_NAMES.binary_search(&name).ok()?;
    Some(point[index])
}
