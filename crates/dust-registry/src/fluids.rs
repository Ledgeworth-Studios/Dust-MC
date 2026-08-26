//! What each fluid fills, what carries it, and what it flows from.
//!
//! The fluid registry itself — five names, five numbers, `minecraft:empty` as
//! the default — is part of the flat registries and reached through
//! [`crate::Fluid`]. This table holds what the registry does not say: the
//! relationships the extractor joined out of three reports at once. A fluid on
//! 1.21.1 is also a block (water *is* `minecraft:water`, one state and all),
//! a still fluid rides in a bucket, and a flowing fluid is the movement
//! of its still partner.
//!
//! # Why the empty fluid knows nothing
//!
//! [`Fluid::block`] returns `None` for `minecraft:empty` because no report says
//! otherwise. Air being "what an empty fluid looks like" is knowledge from
//! outside the reports, and this table's value is that none of it came from
//! outside them. Callers that need an answer for empty say so themselves.
//!
//! # The two tables, and why there are two
//!
//! [`FLUID_DEFS`] is the join; [`FLUID_SAMPLES`] is the same three reports
//! copied as plain text by an extraction pass that shares no reading with the
//! one that built the join. `tests/fluids.rs` checks one against the other,
//! for the reason every golden sample in this repository gives: a table that
//! agrees with itself has proved nothing except that it agrees with itself.

use crate::generated::fluids::{FLUID_DEFS, FLUID_SAMPLES};
use crate::{Block, Fluid, Item};

/// One fluid's relationships, as the generated table holds them.
///
/// Indexed by fluid protocol id: [`FLUID_DEFS`] sits beside the flat registry
/// rather than repeating it, so a row carries only what the registry does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FluidDef {
    /// The block whose states hold this fluid, e.g. `minecraft:lava`, as the
    /// block report lists it. Only still fluids have one on 1.21.1; see the
    /// module header for why the flowing half does not.
    pub block: Option<&'static str>,
    /// The item that carries this fluid, e.g. `minecraft:lava_bucket`. Still
    /// fluids only: nobody picks up flowing water, and the derivation says so
    /// rather than special-casing it.
    pub bucket: Option<&'static str>,
    /// For a flowing fluid, the still fluid it moves; `None` on still ones.
    pub flowing_of: Option<&'static str>,
}

impl Fluid {
    /// This fluid's relationships.
    pub fn def(self) -> FluidDef {
        FLUID_DEFS[self.protocol_id() as usize]
    }

    /// The block whose states hold this fluid.
    ///
    /// `minecraft:empty` has none, on purpose — see the module header.
    pub fn block(self) -> Option<Block> {
        self.def().block.and_then(Block::from_name)
    }

    /// The bucket that carries this fluid around.
    pub fn bucket(self) -> Option<Item> {
        self.def().bucket.and_then(Item::from_name)
    }

    /// The still fluid this one is the movement of, for `flowing_*`.
    pub fn flowing_of(self) -> Option<Fluid> {
        self.def().flowing_of.and_then(Fluid::from_name)
    }
}
