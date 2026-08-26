//! The datapack registries a server sends a client during configuration.
//!
//! # What this is for
//!
//! Since 1.20.5 a joining client must be told the contents of every datapack
//! registry before it enters the world, because a datapack may have changed
//! them and there is no other channel that would say so. Both ends then share
//! one name-to-id mapping — built from the *order* the server sent the entries
//! in — and every later packet that names a biome, a damage type or a chat type
//! uses a number that only means anything against that mapping.
//!
//! # Why the entries carry no data
//!
//! The sync packet's per-entry payload is optional, and vanilla omits it for
//! every entry whenever the client has acknowledged the server's known packs.
//! A vanilla client acknowledges `minecraft:core`, so a vanilla-content server
//! sends names and nothing else — captured from a real 1.21.1 server, all
//! eleven registries, every entry, no data.
//!
//! That is the difference between this table and one several hundred kilobytes
//! larger, and it is a licensing difference too: names are facts about an
//! interface, and a biome's noise parameters are Mojang's content. When Dust
//! supports datapacks that change these registries, the changed entries will
//! need their data — from the datapack, which the operator supplied, not from
//! here.
//!
//! # Order is the content
//!
//! An entry's position in this list is its id for the rest of a session. The
//! generated table writes the order out rather than sorting at run time,
//! because a sort whose result depended on a locale, a hash seed or a file
//! system would give two builds of this server two different meanings for the
//! same number, with a diff that looks like a reordering.

pub mod generated {
    //! Re-exported from [`crate::generated::synced`] so callers have one path.
    pub use crate::generated::synced::*;
}

/// One registry as it goes on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncedRegistry {
    /// The registry's namespaced id, e.g. `minecraft:worldgen/biome`.
    pub name: &'static str,
    /// Entry names, namespaced, in the order they are sent — which is the
    /// order that assigns their ids.
    pub entries: &'static [&'static str],
}

impl SyncedRegistry {
    /// The id this registry gives `entry`, or `None` if it has no such entry.
    ///
    /// A linear scan, deliberately. These are read when a player joins, over
    /// tables of at most sixty-four rows, and an index would be a second
    /// structure that could disagree with the first about the thing this
    /// crate exists to be certain of.
    pub fn id_of(&self, entry: &str) -> Option<usize> {
        self.entries.iter().position(|name| *name == entry)
    }
}

/// Every datapack registry, in the order a server sends them.
pub fn all() -> &'static [SyncedRegistry] {
    generated::SYNCED
}

/// One registry by name.
pub fn by_name(name: &str) -> Option<&'static SyncedRegistry> {
    all().iter().find(|registry| registry.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The eleven registries and their entry counts, as a real 1.21.1 server
    /// sent them.
    ///
    /// A fixture read off the wire, not a computation over this table: a table
    /// generated from the wrong directory would agree with itself perfectly.
    /// The counts are what a client uses to build its own id mapping, so a
    /// registry short by one entry shifts every id after it and turns a plains
    /// biome into a swamp with nothing to see anywhere.
    const CAPTURED: &[(&str, usize)] = &[
        ("minecraft:worldgen/biome", 64),
        ("minecraft:chat_type", 7),
        ("minecraft:trim_pattern", 18),
        ("minecraft:trim_material", 10),
        ("minecraft:wolf_variant", 9),
        ("minecraft:painting_variant", 50),
        ("minecraft:dimension_type", 4),
        ("minecraft:damage_type", 47),
        ("minecraft:banner_pattern", 43),
        ("minecraft:enchantment", 42),
        ("minecraft:jukebox_song", 19),
    ];

    #[test]
    fn the_registries_and_their_counts_are_the_ones_a_real_server_sent() {
        assert_eq!(all().len(), CAPTURED.len(), "eleven registries");
        for (registry, (name, count)) in all().iter().zip(CAPTURED) {
            assert_eq!(&registry.name, name, "in the order the server sent them");
            assert_eq!(
                registry.entries.len(),
                *count,
                "{name} entry count against the capture"
            );
        }
    }

    #[test]
    fn every_entry_is_namespaced_and_no_registry_repeats_one() {
        for registry in all() {
            let mut seen = std::collections::BTreeSet::new();
            for entry in registry.entries {
                assert!(
                    entry.contains(':'),
                    "{entry} in {} is not namespaced",
                    registry.name
                );
                assert!(
                    seen.insert(*entry),
                    "{} lists {entry} twice, so two ids name one thing",
                    registry.name
                );
            }
        }
    }

    #[test]
    fn the_four_dimension_types_include_the_three_a_client_is_told_about() {
        // The play-state join packet names the dimensions a player may be in,
        // and every one of them has to resolve here. A capture from a real
        // server listed overworld, the_end and the_nether; the fourth is
        // overworld_caves, which exists as a type and is not offered as a
        // world.
        let dimensions = by_name("minecraft:dimension_type").expect("a synced registry");
        for expected in [
            "minecraft:overworld",
            "minecraft:the_end",
            "minecraft:the_nether",
            "minecraft:overworld_caves",
        ] {
            assert!(
                dimensions.id_of(expected).is_some(),
                "{expected} must be a dimension type"
            );
        }
    }

    #[test]
    fn plains_keeps_the_id_its_position_gives_it() {
        // Not an assertion that plains is id 0-something in particular; an
        // assertion that id_of and the slice agree, which is the property
        // every packet that names a biome by number depends on.
        let biomes = by_name("minecraft:worldgen/biome").expect("a synced registry");
        let id = biomes.id_of("minecraft:plains").expect("plains exists");
        assert_eq!(biomes.entries[id], "minecraft:plains");
        assert_eq!(biomes.id_of("minecraft:not_a_biome"), None);
    }
}
