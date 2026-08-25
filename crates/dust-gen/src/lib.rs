//! Worldgen: density functions, biome source, surface rules, carvers, features,
//! structures.
//!
//! Almost none of that exists yet. What does exist is [`ore_density`], the
//! part of ore placement that is arithmetic over the vanilla baseline rather
//! than generation itself, and which is therefore buildable and testable before
//! the engine underneath it is written.

pub mod ore_density;
