//! The grammar of vanilla crafting, smelting and smithing.
//!
//! A recipe file on 1.21.1 is a JSON object whose `type` names one of 23
//! serialisers, and the serialiser decides what every other key means. This
//! table is that vocabulary: which shapes the vanilla data uses, which keys
//! every recipe of a shape carries, which keys only some carry, and how many
//! of each shape there are.
//!
//! # What this table is deliberately not
//!
//! It is not the 1,290 recipes. No pattern grid, no ingredient list, no result
//! stack is in here — those are the contents, and committing them would put
//! Mojang's data in the repository by a route the provenance line does not
//! allow. The counts and key sets are the same kind of fact as "how many
//! packets exist": structure, not substance.
//!
//! The thirteen `crafting_special_*` shapes are the proof this table was worth
//! building from data instead of from memory: on 1.21.1 they are not missing,
//! as one might guess, but present as single marker files carrying nothing but
//! `type` and `category`. The catalogue records them at one use each with two
//! required keys, which is what the files say and precisely unlike what
//! someone typing the vocabulary would have written down.
//!
//! What the table buys a caller: a `/recipe` UI can lay out its slots per
//! shape without reading any recipe; a validator can reject a recipe whose
//! keys do not fit its declared shape before anything tries to craft with it;
//! and `optional` documents the gap between the two — `group` appears on some
//! smelting recipes and not others, and `show_notification` appears on exactly
//! one shaped recipe out of 634. A reader that assumed uniformity would treat
//! the optional half as missing.

use crate::generated::recipes::{RECIPE_COUNT, RECIPE_SHAPES};

/// One recipe shape, as the generated catalogue holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecipeShape {
    /// The recipe's `type` value and the serialiser's registry name, e.g.
    /// `minecraft:crafting_shaped`.
    pub serializer: &'static str,
    /// How many vanilla recipes use this shape.
    pub uses: usize,
    /// Keys present on *every* recipe of this shape, sorted.
    pub required: &'static [&'static str],
    /// Keys present on at least one but not all, sorted. Empty for the shapes
    /// vanilla writes uniformly.
    pub optional: &'static [&'static str],
}

impl RecipeShape {
    /// Whether this key belongs to the shape — either half of the catalogue,
    /// because a key seen once is as legal as one seen everywhere.
    pub fn carries(&self, key: &str) -> bool {
        self.required.contains(&key) || self.optional.contains(&key)
    }
}

/// Every shape, name-sorted by serialiser.
pub fn all() -> impl Iterator<Item = &'static RecipeShape> {
    RECIPE_SHAPES.iter()
}

/// Look a shape up by its serialiser id.
pub fn from_serializer(serializer: &str) -> Option<&'static RecipeShape> {
    RECIPE_SHAPES
        .binary_search_by(|shape| shape.serializer.cmp(serializer))
        .ok()
        .map(|index| &RECIPE_SHAPES[index])
}

/// How many recipe files the catalogue accounts for, across every shape.
pub const RECIPE_TOTAL: usize = RECIPE_COUNT;
