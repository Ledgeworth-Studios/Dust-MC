//! The seam where "is `minecraft:stobe` a real block?" gets answered.
//!
//! # Why this is a parameter and not a dependency
//!
//! Validating a tag entry needs the item, block, entity and fluid registries.
//! Those live in `dust-registry`, and this crate does not depend on it. That is
//! not a workaround for a scheduling accident; it is the shape the crate should
//! have anyway, for three reasons:
//!
//! 1. **A datapack adds registry entries.** A pack that adds a new
//!    `minecraft:enchantment` and then tags it is doing the ordinary thing, and
//!    a validator baked in at compile time would call the tag entry unknown.
//!    The vocabulary a tag is checked against is the *loaded world's*, which
//!    exists only after this crate has run.
//! 2. **A loader that cannot run without the registries cannot be tested
//!    without them.** Every test in this crate would need 1,060 blocks in scope
//!    to check that a cycle is detected.
//! 3. It keeps the dependency edge pointing the way the architecture says:
//!    generated identifier tables at the bottom, readers of operator data above
//!    them.
//!
//! # The part that is easy to get wrong
//!
//! A vocabulary that knows nothing must not look like a vocabulary that
//! approved everything. [`Unchecked`] answers "I do not know" rather than
//! "yes", and every count of validated entries is reported alongside a count of
//! **unvalidated** ones — see [`crate::tag::TagStats::unvalidated_entries`]. A
//! check that silently did not run is the same failure as a setting that
//! silently does nothing, and this is exactly where it would hide.

use std::collections::{BTreeMap, BTreeSet};

use crate::finding::nearest;
use crate::ResourceLocation;

/// What a registry contains, as far as the caller can say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Known {
    /// The registry has this name.
    Yes,
    /// The registry does not have this name.
    No,
    /// Nothing here can say — this vocabulary does not cover that registry.
    ///
    /// Distinct from [`Known::Yes`] on purpose. Folding the two together is how
    /// a validator ends up reporting that it checked things it never saw.
    Unknown,
}

/// Answers whether a name exists, for the registries the caller has.
///
/// Implement this over `dust-registry` plus whatever the packs themselves
/// defined. [`KnownNames`] is a ready-made implementation for tests and for
/// callers that already have the sets in hand.
pub trait Vocabulary: std::fmt::Debug {
    /// Does `registry` — `block`, `item`, `worldgen/biome` — contain `name`?
    fn contains(&self, registry: &str, name: &ResourceLocation) -> Known;

    /// A close name from the same registry, for a "did you mean".
    ///
    /// The default is no suggestion, so an implementation that cannot enumerate
    /// its registry is still a usable one.
    fn suggest(&self, _registry: &str, _name: &ResourceLocation) -> Option<String> {
        None
    }
}

/// A vocabulary that knows nothing and says so.
///
/// The default. Under it, a tag entry naming a block is neither accepted nor
/// rejected — it is counted as unvalidated and reported as such. What still
/// *is* checked without any vocabulary is every `#tag` reference, because
/// whether a tag exists is a question about the loaded data rather than about
/// the registries.
#[derive(Debug, Clone, Copy, Default)]
pub struct Unchecked;

impl Vocabulary for Unchecked {
    fn contains(&self, _registry: &str, _name: &ResourceLocation) -> Known {
        Known::Unknown
    }
}

/// A vocabulary backed by sets of names held in memory.
#[derive(Debug, Clone, Default)]
pub struct KnownNames {
    registries: BTreeMap<String, BTreeSet<ResourceLocation>>,
}

impl KnownNames {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a registry and everything in it. Adding the same registry twice
    /// merges, so a caller can pour vanilla and a pack's additions in
    /// separately.
    #[must_use]
    pub fn with(
        mut self,
        registry: impl Into<String>,
        names: impl IntoIterator<Item = ResourceLocation>,
    ) -> Self {
        self.registries
            .entry(registry.into())
            .or_default()
            .extend(names);
        self
    }

    pub fn registries(&self) -> impl Iterator<Item = &str> {
        self.registries.keys().map(String::as_str)
    }
}

impl Vocabulary for KnownNames {
    fn contains(&self, registry: &str, name: &ResourceLocation) -> Known {
        match self.registries.get(registry) {
            None => Known::Unknown,
            Some(names) if names.contains(name) => Known::Yes,
            Some(_) => Known::No,
        }
    }

    fn suggest(&self, registry: &str, name: &ResourceLocation) -> Option<String> {
        let names = self.registries.get(registry)?;
        nearest(name.as_str(), names.iter().map(ResourceLocation::as_str)).map(str::to_owned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn location(text: &str) -> ResourceLocation {
        ResourceLocation::parse(text).expect("valid")
    }

    #[test]
    fn knowing_nothing_is_not_the_same_as_approving_everything() {
        assert_eq!(
            Unchecked.contains("block", &location("minecraft:not_a_block")),
            Known::Unknown
        );
    }

    #[test]
    fn a_registry_that_is_present_answers_both_ways() {
        let vocabulary = KnownNames::new().with("block", [location("minecraft:stone")]);
        assert_eq!(
            vocabulary.contains("block", &location("minecraft:stone")),
            Known::Yes
        );
        assert_eq!(
            vocabulary.contains("block", &location("minecraft:stobe")),
            Known::No
        );
    }

    #[test]
    fn a_registry_that_is_absent_is_unknown_rather_than_empty() {
        // The trap: an empty `BTreeSet` and a missing one would both answer
        // "not in there" if this were written with `unwrap_or_default`, and
        // every entry in every item tag would be reported as a typo.
        let vocabulary = KnownNames::new().with("block", [location("minecraft:stone")]);
        assert_eq!(
            vocabulary.contains("item", &location("minecraft:stone")),
            Known::Unknown
        );
    }

    #[test]
    fn a_near_miss_is_suggested() {
        let vocabulary = KnownNames::new().with("block", [location("minecraft:stone")]);
        assert_eq!(
            vocabulary.suggest("block", &location("minecraft:stobe")),
            Some("minecraft:stone".to_owned())
        );
        assert_eq!(
            vocabulary.suggest("item", &location("minecraft:stobe")),
            None
        );
    }
}
