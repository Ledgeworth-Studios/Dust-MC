//! The vanilla tags, as the baseline a datapack loader overlays.
//!
//! A tag is a named group over one registry — `minecraft:mineable/pickaxe`
//! over blocks, `minecraft:arrows` over items — and the game resolves it
//! wherever a single id would be too specific: what pickaxes break, what
//! floats, what survives falling. This table is vanilla's own answer, all five
//! registries of it, extracted with every membership checked against the
//! tables this crate already holds.
//!
//! # The baseline layer, and the layer above it
//!
//! What is here is what vanilla defines. A datapack at load time adds members
//! to these groups or replaces them outright; that semantics lives in the
//! loader, not in these rows. Nothing here carries `replace` because vanilla's
//! own files never set it — the extraction refuses one that does, on the
//! reasoning that overlay vocabulary inside the baseline would mean somebody
//! shipped a datapack into the jar.
//!
//! Members beginning with `#` are references to other tags of the same
//! registry, stored fully namespaced (`"#minecraft:logs_that_burn"`) however
//! vanilla spelled them relative. They resolve inside this table; nothing
//! dangles.

use crate::generated::tags::TAGS;

/// Which registry a tag groups.
///
/// Thirteen, which is what a real 1.21.1 server sends `update_tags` for —
/// counted off the wire, not off a list. Every one of them is checked at
/// extraction against a table extracted separately: the block report, the
/// registry report, or the datapack registry names. A fourteenth directory in
/// a future version stops the extractor rather than arriving as unchecked
/// rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TagRegistry {
    Block,
    Item,
    Fluid,
    EntityType,
    GameEvent,
    Biome,
    PointOfInterestType,
    Enchantment,
    DamageType,
    BannerPattern,
    CatVariant,
    Instrument,
    PaintingVariant,
}

impl TagRegistry {
    /// Every registry the baseline groups.
    ///
    /// In the order a real server sent them. Nothing on the wire depends on
    /// it — each registry names itself in the packet — but an order somebody
    /// chose is one two people can disagree about, and this one has an answer.
    pub const ALL: [Self; 13] = [
        Self::Block,
        Self::EntityType,
        Self::Biome,
        Self::GameEvent,
        Self::Item,
        Self::PointOfInterestType,
        Self::Enchantment,
        Self::Fluid,
        Self::DamageType,
        Self::BannerPattern,
        Self::CatVariant,
        Self::Instrument,
        Self::PaintingVariant,
    ];

    /// The registry id this tag groups, which is also how the generated rows
    /// spell their registry.
    pub fn name(self) -> &'static str {
        match self {
            Self::Block => "minecraft:block",
            Self::Item => "minecraft:item",
            Self::Fluid => "minecraft:fluid",
            Self::EntityType => "minecraft:entity_type",
            Self::GameEvent => "minecraft:game_event",
            Self::Biome => "minecraft:worldgen/biome",
            Self::PointOfInterestType => "minecraft:point_of_interest_type",
            Self::Enchantment => "minecraft:enchantment",
            Self::DamageType => "minecraft:damage_type",
            Self::BannerPattern => "minecraft:banner_pattern",
            Self::CatVariant => "minecraft:cat_variant",
            Self::Instrument => "minecraft:instrument",
            Self::PaintingVariant => "minecraft:painting_variant",
        }
    }

    /// The registry with this id, or `None` if tags do not group it.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.name() == name)
    }
}

/// One tag, as the baseline holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TagDef {
    pub registry: TagRegistry,
    /// Namespaced tag id, e.g. `minecraft:mineable/pickaxe`.
    pub id: &'static str,
    /// Members sorted. Plain entries are namespaced ids of [`Self::registry`];
    /// entries starting with `#` name other tags of the same registry,
    /// resolved within this table.
    pub members: &'static [&'static str],
}

impl TagDef {
    /// Whether this plain id is a member of the tag.
    ///
    /// A binary search, which is why the members are sorted and why that is
    /// checked rather than trusted.
    pub fn contains(&self, id: &str) -> bool {
        self.members.binary_search(&id).is_ok()
    }

    /// The tag ids this tag references, with the `#` kept.
    pub fn references(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.members.iter().copied().filter(|m| m.starts_with('#'))
    }
}

/// Every tag, sorted by (registry, id).
pub fn all() -> impl Iterator<Item = &'static TagDef> {
    TAGS.iter()
}

/// Every tag of one registry, sorted by id.
pub fn by_registry(registry: TagRegistry) -> impl Iterator<Item = &'static TagDef> {
    TAGS.iter().filter(move |tag| tag.registry == registry)
}

/// Look a tag up by its namespaced id within one registry.
pub fn from_id(registry: TagRegistry, id: &str) -> Option<&'static TagDef> {
    let index = TAGS
        .binary_search_by(|tag| {
            tag.registry
                .name()
                .cmp(registry.name())
                .then_with(|| tag.id.cmp(id))
        })
        .ok()?;
    Some(&TAGS[index])
}
