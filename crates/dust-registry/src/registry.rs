//! The generic way in to any flat registry, by registry id.
//!
//! A flat registry is a list of namespaced names with a protocol id attached to
//! each — items, entity types, particles, sound events, the other seventy-odd.
//! The three that most of the server touches have first-class types in
//! [`crate::flat`]; this is what the rest are reached through, and what those
//! types are built on, so there is one table and not two readings of it.
//!
//! Lookups are by namespaced id in both directions and neither is a scan:
//! `name -> id` binary-searches the name-ordered table, `id -> name` indexes the
//! reverse array. The extractor is what makes the second one an index rather
//! than a search — it refuses to emit a registry whose protocol ids are not
//! `0..n`.

use crate::generated::registries::REGISTRIES;

/// One flat registry, as the generated table holds it.
///
/// The three slices are parallel in a particular way that the generated code
/// guarantees and `tests/registries.rs` re-checks against the emitted table:
/// `names` is sorted by name, `ids[i]` is the protocol id of `names[i]`, and
/// `by_id[id]` is the position in `names` of the entry with that protocol id.
/// `ids` and `by_id` are inverse permutations of `0..names.len()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegistryDef {
    /// Namespaced registry id, e.g. `minecraft:item`.
    pub name: &'static str,
    /// The registry's own id in Minecraft's root registry.
    pub protocol_id: u16,
    /// Entry names, sorted by name.
    pub names: &'static [&'static str],
    /// The protocol id of each entry in `names`, in the same order.
    pub ids: &'static [u16],
    /// Indexed by protocol id: the position of that entry in `names`.
    pub by_id: &'static [u16],
    /// The entry Minecraft falls back to when a name does not resolve, for the
    /// registries that declare one.
    pub default: Option<&'static str>,
}

/// One of the vanilla flat registries.
///
/// `minecraft:block` is not among them. Blocks have a state space as well as a
/// name, so they are [`crate::Block`] and a table of their own; a block's
/// protocol id is its position in that table, which the extractor checks
/// against this report rather than assuming.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Registry(u8);

impl Registry {
    /// Look a registry up by its namespaced id, e.g. `minecraft:particle_type`.
    ///
    /// A bare name is not accepted, for the same reason [`crate::Block`] does
    /// not accept one: `item` and `minecraft:item` are the same registry to a
    /// person and different strings to a lookup, and taking both would leave
    /// every caller downstream unsure which it holds.
    pub fn from_name(name: &str) -> Option<Self> {
        REGISTRIES
            .binary_search_by(|def| def.name.cmp(name))
            .ok()
            .map(|index| Self(index as u8))
    }

    /// The registry at a position in the generated table.
    ///
    /// Only the generated `index` constants are positions, which is what keeps
    /// a first-class type from drifting onto another registry's table when a
    /// release adds a registry and the name order shifts.
    pub(crate) const fn at(index: usize) -> Self {
        // A `Registry` is a u8 index and there are 77 registries, so this
        // cannot narrow today. It is checked anyway because the failure it
        // would be is silent: a wrapped index is a valid `Registry`, just not
        // the one that was asked for.
        assert!(index < REGISTRIES.len(), "no registry at that index");
        Self(index as u8)
    }

    pub fn name(self) -> &'static str {
        self.def().name
    }

    /// The registry's own id in Minecraft's root registry.
    ///
    /// Not contiguous across [`Registry::all`]: `minecraft:block` holds one of
    /// these ids and is not in this table.
    pub fn protocol_id(self) -> u16 {
        self.def().protocol_id
    }

    pub fn entry_count(self) -> usize {
        self.def().names.len()
    }

    /// The entry Minecraft falls back to, for the registries that declare one.
    pub fn default_entry(self) -> Option<&'static str> {
        self.def().default
    }

    /// The name of the entry with this protocol id, or `None` if the registry
    /// has no such id.
    pub fn entry_name(self, protocol_id: u32) -> Option<&'static str> {
        let def = self.def();
        let position = *def.by_id.get(usize::try_from(protocol_id).ok()?)?;
        Some(def.names[position as usize])
    }

    /// The protocol id of the entry with this namespaced name.
    pub fn entry_id(self, name: &str) -> Option<u32> {
        let def = self.def();
        let position = def.names.binary_search(&name).ok()?;
        Some(u32::from(def.ids[position]))
    }

    /// Every entry, in protocol-id order — the order the wire numbers them.
    pub fn entries(self) -> impl Iterator<Item = (u32, &'static str)> {
        let def = self.def();
        def.by_id
            .iter()
            .enumerate()
            .map(|(id, &position)| (id as u32, def.names[position as usize]))
    }

    /// Every entry name, in name order.
    pub fn names(self) -> impl Iterator<Item = &'static str> {
        self.def().names.iter().copied()
    }

    /// Every flat registry, in name order.
    pub fn all() -> impl Iterator<Item = Self> {
        (0..REGISTRIES.len() as u8).map(Self)
    }

    fn def(self) -> &'static RegistryDef {
        &REGISTRIES[self.0 as usize]
    }
}
