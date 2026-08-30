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
//!
//! # The stored form is not the wire form
//!
//! A client is sent a flat list of numeric ids per tag, with the references
//! already followed. [`wire`] is that conversion, and what says it is right is
//! a comparison against a real 1.21.1 server rather than any property of this
//! table: **all thirteen registries, all 514 tags and all 6,362 ids match what
//! vanilla put on the wire, exactly.** The two byte streams are the same
//! length and not the same bytes, because vanilla emits tags in its own map's
//! order and this emits them sorted — the client builds a set either way, and
//! an order that varied between two builds of this server would be a diff
//! nobody could read.

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

/// One tag as `update_tags` carries it: a name and the numeric ids in it.
///
/// The wire form of a tag is not the stored form. Two things happen on the way
/// out and both of them are why this exists rather than a `map` at the call
/// site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireTag {
    /// Namespaced tag id, e.g. `minecraft:mineable/pickaxe`.
    pub id: &'static str,
    /// Registry ids, ascending and without repeats.
    pub entries: Vec<u32>,
}

/// Why a tag could not be put on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireError {
    /// A member names something its registry has no id for. Impossible from
    /// the generated baseline, which is checked at extraction — but this
    /// function is the one place the two tables meet, and a version skew
    /// between them would show up exactly here.
    UnknownMember {
        /// The tag holding it.
        tag: &'static str,
        /// The member.
        member: &'static str,
    },
    /// A `#` reference that names no tag of the same registry.
    DanglingReference {
        /// The tag holding it.
        tag: &'static str,
        /// What it pointed at.
        target: &'static str,
    },
    /// A tag that reaches itself through references. Vanilla has none; a
    /// datapack could write one, and a flattening walk that met one without
    /// noticing would not return.
    Cycle {
        /// The tag the walk came back to.
        tag: &'static str,
    },
}

impl core::fmt::Display for WireError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnknownMember { tag, member } => {
                write!(f, "{tag} names {member}, which its registry has no id for")
            }
            Self::DanglingReference { tag, target } => {
                write!(f, "{tag} references {target}, which is not a tag")
            }
            Self::Cycle { tag } => write!(f, "{tag} reaches itself through its references"),
        }
    }
}

impl std::error::Error for WireError {}

/// Every tag of one registry, flattened and resolved to ids.
///
/// # Flattened, because that is what a client is sent
///
/// A tag file may name another tag — `minecraft:logs` is four other tags and
/// nothing else — and the client is not sent that structure. It is sent a flat
/// list of ids, so the references are followed here, transitively, and the
/// result deduplicated: `minecraft:logs` goes out as the ids of every log,
/// which is what makes an axe work on all of them.
///
/// # Where the ids come from, and why it is three places
///
/// `minecraft:block` numbers its entries in the block report;
/// `minecraft:item` and five others in the registry report; and the five
/// datapack registries have no protocol id at all, so their entries are
/// numbered by their position in the *sync* the server sent this session. That
/// last one is the load-bearing part: the ids in a biome tag and the ids in a
/// chunk's biome container both come from `synced`, so they cannot disagree,
/// and taking them from anywhere else would give the client two meanings for
/// the number 37.
pub fn wire(registry: TagRegistry) -> Result<Vec<WireTag>, WireError> {
    by_registry(registry)
        .map(|tag| {
            let mut entries = Vec::with_capacity(tag.members.len());
            flatten(registry, tag, &mut entries, &mut Vec::new())?;
            // Ascending and unique. Two tags reaching the same member through
            // different references is ordinary — `logs` and `logs_that_burn`
            // overlap — and a set is what the client builds anyway.
            entries.sort_unstable();
            entries.dedup();
            Ok(WireTag {
                id: tag.id,
                entries,
            })
        })
        .collect()
}

/// Follow one tag's members, pushing ids and recursing into references.
///
/// `seen` is the path from the root, not everything visited: a diamond — two
/// references reaching one tag — is legal and common, and only a tag that
/// reaches *itself* is a cycle. The dedup afterwards is what makes the diamond
/// free.
fn flatten(
    registry: TagRegistry,
    tag: &'static TagDef,
    out: &mut Vec<u32>,
    seen: &mut Vec<&'static str>,
) -> Result<(), WireError> {
    if seen.contains(&tag.id) {
        return Err(WireError::Cycle { tag: tag.id });
    }
    seen.push(tag.id);
    for member in tag.members {
        if let Some(target) = member.strip_prefix('#') {
            let referenced = from_id(registry, target).ok_or(WireError::DanglingReference {
                tag: tag.id,
                target,
            })?;
            flatten(registry, referenced, out, seen)?;
        } else {
            out.push(id_of(registry, member).ok_or(WireError::UnknownMember {
                tag: tag.id,
                member,
            })?);
        }
    }
    seen.pop();
    Ok(())
}

/// The id a registry gives one of its entries.
fn id_of(registry: TagRegistry, member: &str) -> Option<u32> {
    match registry {
        // The blocks table is its own; see `Block::protocol_id`.
        TagRegistry::Block => crate::Block::from_name(member).map(crate::Block::protocol_id),
        // Everything with a protocol id compiled into the game.
        TagRegistry::Item
        | TagRegistry::Fluid
        | TagRegistry::EntityType
        | TagRegistry::GameEvent
        | TagRegistry::PointOfInterestType
        | TagRegistry::CatVariant
        | TagRegistry::Instrument => crate::Registry::from_name(registry.name())?.entry_id(member),
        // The datapack registries, numbered by the order they were synced.
        TagRegistry::Biome
        | TagRegistry::Enchantment
        | TagRegistry::DamageType
        | TagRegistry::BannerPattern
        | TagRegistry::PaintingVariant => crate::synced::by_name(registry.name())
            .and_then(|r| r.id_of(member))
            .map(|position| position as u32),
    }
}
