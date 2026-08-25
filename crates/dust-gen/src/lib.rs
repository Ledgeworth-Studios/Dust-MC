//! Worldgen: density functions, biome source, surface rules, carvers, features,
//! structures.
//!
//! Almost none of that exists yet. What does exist is [`ore_density`], the
//! part of ore placement that is arithmetic over the vanilla baseline rather
//! than generation itself, and which is therefore buildable and testable before
//! the engine underneath it is written — and [`vanilla_ores`], the extracted
//! baseline it is arithmetic *over*.
//!
//! The two are separate on purpose. `ore_density` never reaches for a vanilla
//! constant, so it is right on a modded world as well as a vanilla one;
//! `vanilla_ores` is one caller that happens to supply vanilla's numbers.

pub mod generated;
pub mod ore_density;
pub mod vanilla_ores;
