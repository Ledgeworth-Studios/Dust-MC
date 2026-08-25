//! First-class types over the three flat registries the server touches most.
//!
//! [`Item`], [`EntityType`] and [`Fluid`] are the same shape as
//! [`crate::Block`]: a namespaced-id lookup, a name, a protocol id, and a way
//! to walk the whole registry. Everything else is reached through
//! [`Registry`], which is also what these are built on — one table, read one
//! way. What a type here buys over `Registry::from_name("minecraft:item")` is
//! that an item cannot be passed where an entity type belongs, which is a class
//! of mistake a protocol implementation makes constantly.
//!
//! Each is a newtype over the protocol id itself, which is only sound because
//! the ids of a registry run `0..n` with no hole — every id is an entry and
//! every entry an id. That is not assumed: the extractor refuses to emit a
//! sparse registry rather than leave one of these holding a number that decodes
//! to nothing. See `xtask/src/extract/registries.rs`.
//!
//! `u16` rather than `u32` because these are stored per item stack and per
//! entity, and the widest of them has 1,611 entries; the extractor refuses a
//! registry that would not fit.

use crate::generated::registries::index;
use crate::registry::Registry;

macro_rules! flat_registry {
    ($(#[$meta:meta])* $name:ident, $index:expr) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(u16);

        impl $name {
            /// The registry this type is the whole of.
            pub const fn registry() -> Registry {
                Registry::at($index)
            }

            /// Look one up by its namespaced id.
            ///
            /// A bare name is not accepted. `stone` and `minecraft:stone` are
            /// the same thing to a player and different strings to a lookup,
            /// and accepting both here would mean every caller downstream is
            /// unsure which it holds.
            pub fn from_name(name: &str) -> Option<Self> {
                let id = Self::registry().entry_id(name)?;
                // The extractor refuses a registry with more entries than a
                // u16 holds, so this cannot narrow. `try_from` rather than a
                // cast so that if that ever changed this would stop, instead
                // of wrapping around to some other entry.
                Some(Self(u16::try_from(id).expect("the extractor caps this at u16")))
            }

            pub fn name(self) -> &'static str {
                Self::registry()
                    .entry_name(u32::from(self.0))
                    .expect("built from an id this registry has")
            }

            /// The number that goes on the wire.
            pub fn protocol_id(self) -> u32 {
                u32::from(self.0)
            }

            /// The entry with this protocol id, or `None` if the registry has
            /// no such id.
            pub fn from_protocol_id(id: u32) -> Option<Self> {
                let count = Self::registry().entry_count() as u32;
                (id < count).then_some(Self(id as u16))
            }

            /// Every entry, in protocol-id order.
            pub fn all() -> impl Iterator<Item = Self> {
                (0..Self::registry().entry_count() as u16).map(Self)
            }
        }
    };
}

flat_registry!(
    /// An item — `minecraft:diamond_sword`, the thing a stack is made of.
    ///
    /// Every block that can be held is also an item, and the two have different
    /// numbers: on 1.21.1, 913 of the 1,060 blocks share a name with an item
    /// and not one of them shares its id. [`crate::Block::from_name`] and
    /// [`Item::from_name`] on the same string are two different answers, both
    /// right.
    Item,
    index::ITEM
);

flat_registry!(
    /// A kind of entity — `minecraft:zombie`.
    EntityType,
    index::ENTITY_TYPE
);

flat_registry!(
    /// A fluid — `minecraft:water`, and the flowing variant beside it.
    ///
    /// Five of them, `minecraft:empty` included: a block that holds no fluid
    /// holds this one rather than nothing, which is why the registry has a
    /// default at all.
    Fluid,
    index::FLUID
);
