//! The loot tables vanilla ships, as an inventory and a vocabulary.
//!
//! 1,178 tables on 1.21.1: one per block that drops anything, one per entity,
//! the chests, shearing, fishing, barter. What lives in this crate is which
//! tables exist, how they group, and which condition, function and pool-entry
//! types they are written with — the grammar of loot, ahead of any need to
//! speak it. No drop amount, roll or result survives extraction; those are
//! Mojang's data and stay on the machine that read them.
//!
//! # Two readings of one tree
//!
//! [`VOCABULARY`] comes from a walk that knows the format's positions: an
//! entry type is the `type` key of an object inside `entries` or `children`,
//! not the `type` of a number-provider argument buried in a function.
//! [`SOURCE_COUNTS`] comes from a pass with no position rules at all — every
//! string under `"condition"` or `"function"`, counted wherever it sits. The
//! two must agree exactly for those kinds; where they differ, one reading of
//! the tree misread it, and `tests/loot.rs` names the disagreement.

use crate::generated::loot::{CATEGORIES, SOURCE_COUNTS, TABLES, VOCABULARY};

/// Which kind of loot vocabulary a name belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Condition,
    Function,
    Entry,
}

impl Kind {
    /// The name the generated table spells this kind with.
    pub fn name(self) -> &'static str {
        match self {
            Self::Condition => "condition",
            Self::Function => "function",
            Self::Entry => "entry",
        }
    }
}

/// Whether a loot table with that id exists in the vanilla set.
///
/// The full name is required — `minecraft:blocks/stone`, not
/// `blocks/stone` — because every id in Dust is namespaced and a lookup that
/// accepted two spellings would be two lookups wearing one signature.
pub fn table_exists(id: &str) -> bool {
    TABLES.binary_search(&id).is_ok()
}

/// Every table id, sorted.
pub fn tables() -> impl Iterator<Item = &'static str> {
    TABLES.iter().copied()
}

/// Tables per top-level directory, sorted by directory name.
pub fn categories() -> &'static [(&'static str, u32)] {
    CATEGORIES
}

/// How many times the vanilla tables use a given vocabulary item.
///
/// `None` when the name is unknown *or* belongs to another kind; asking how
/// many times `minecraft:set_count` appears as a condition is a question with
/// no answer rather than one with zero.
pub fn uses(kind: Kind, name: &str) -> Option<u32> {
    let index = VOCABULARY
        .binary_search_by(|(k, n, _)| (*k).cmp(kind.name()).then_with(|| (*n).cmp(name)))
        .ok()?;
    Some(VOCABULARY[index].2)
}

/// The same tally from the structureless second pass, for the checks that
/// compare them.
pub fn source_uses(kind: Kind, name: &str) -> Option<u32> {
    let index = SOURCE_COUNTS
        .binary_search_by(|(k, n, _)| (*k).cmp(kind.name()).then_with(|| (*n).cmp(name)))
        .ok()?;
    Some(SOURCE_COUNTS[index].2)
}

/// Every vocabulary item of one kind, as `(name, uses)`, in name order.
pub fn vocabulary(kind: Kind) -> impl Iterator<Item = (&'static str, u32)> {
    VOCABULARY
        .iter()
        .filter(move |(k, _, _)| *k == kind.name())
        .map(|(_, n, u)| (*n, *u))
}
