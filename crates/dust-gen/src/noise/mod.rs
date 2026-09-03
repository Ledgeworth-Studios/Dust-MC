//! Noise, and the language Minecraft writes worldgen in.
//!
//! Three pieces, bottom up: [`rng`] is the generator every noise is seeded
//! from, [`perlin`] is the noise itself, and [`density`] is the evaluator for
//! the density-function graph that wires noises into the six climate values a
//! biome is chosen from. [`build`] compiles that graph out of a data pack.
//!
//! None of it holds a number of Mojang's. The amplitudes, the octaves, the
//! splines and the graph shape all arrive at run time from the copy of
//! Minecraft the operator already has — the same rule decision records 0006,
//! 0007 and 0008 set for block constants and item mappings.

pub mod blended;
pub mod build;
pub mod density;
pub mod perlin;
pub mod rng;
