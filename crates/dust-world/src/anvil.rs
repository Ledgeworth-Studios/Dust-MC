//! Reading a chunk out of a world Minecraft wrote.
//!
//! # What this is and is not
//!
//! [`region`](crate::region) reads the *container* — the 4 KiB sectors, the
//! offsets, the per-chunk compression — and hands back a decompressed NBT
//! document. This turns that document into a [`Chunk`]. It is the half of
//! Anvil that is about Minecraft rather than about files.
//!
//! **Read only.** Writing an Anvil chunk means answering questions this crate
//! has no way to check the answers to — which of the twenty-odd fields a
//! vanilla server insists on, what a `Status` of `minecraft:full` promises,
//! whether a `block_entities` list may be empty or must be absent — and a
//! writer that guessed would produce worlds that open until the day one does
//! not. Reading is checkable against a world that already exists, which is
//! what `tests/anvil.rs` does.
//!
//! # The layout, from a real 1.21.1 world
//!
//! ```text
//! root compound
//!   DataVersion : Int          3955 for 1.21.1
//!   xPos, zPos  : Int          the column, in chunk coordinates
//!   yPos        : Int          the index of the lowest section, in sections
//!   Status      : String       minecraft:full when it is finished
//!   sections    : List<Compound>
//!     Y            : Byte      the section's own y, in sections, signed
//!     block_states : Compound  { palette: List<Compound>, data: LongArray? }
//!     biomes       : Compound  { palette: List<String>,   data: LongArray? }
//! ```
//!
//! Two things about that shape are worth stating because they are where a
//! reader goes wrong quietly.
//!
//! **`data` is absent when the palette has one entry**, and its absence means
//! "every cell is that entry" rather than "no cells". A reader that treated a
//! missing array as an empty section would turn a solid section of stone into
//! air, and the chunk would still load.
//!
//! **The indices in `data` point into the palette that was written beside
//! them**, not at registry ids, and they are packed at `ceil_log2(palette
//! length)` bits with a floor of four — *not* at the width the container will
//! use in memory. That difference is [`Strategy::disk_bits`], and it exists
//! because on disk a section indexes its own palette while in memory a large
//! one indexes the whole registry.
//!
//! # Blocks are resolved by name, and unknown ones are an error
//!
//! A palette entry is `{ Name: "minecraft:stone", Properties: {...} }`. The
//! name is resolved through a caller-supplied lookup, because which id a block
//! has is `dust-registry`'s business and this crate does not depend on it.
//!
//! Properties are read and passed to the lookup, so a chunk of stairs comes
//! back facing the way it was written. They arrive as `(name, value)` pairs of
//! strings, which is how the file spells them, and turning those into a state
//! id is the registry's job — this crate does not know that `facing` has six
//! values or which of them is which.
//!
//! Even so, **a palette can list one block name twice** and resolve to one id:
//! a lookup may legitimately ignore a property it does not model, and two
//! entries differing only in that property then collapse. The file is not wrong
//! and neither is the reader; the indices still point where they should. See
//! the note in [`read_container`] for why that rules out the fast path through
//! `PalettedContainer::from_parts`.

use std::collections::HashMap;

use dust_nbt::{Compound, Tag};

use crate::chunk::{Chunk, Section};
use crate::container::{PalettedContainer, Strategy};
use crate::coords::ChunkPos;
use crate::heightmap::WorldHeight;
use crate::light::LightArray;

/// The data version 1.21.1 writes.
pub const DATA_VERSION_1_21_1: i32 = 3955;

/// What could not be read.
#[derive(Debug)]
pub enum AnvilError {
    /// A field the format requires is missing or the wrong type.
    Field { name: &'static str },
    /// A block name the caller's lookup does not know.
    UnknownBlock { name: String },
    /// A biome name the caller's lookup does not know.
    UnknownBiome { name: String },
    /// The section list does not fit the world it claims to be part of.
    SectionOutOfRange { y: i32 },
    /// A packed array is the wrong length for its palette.
    BadPacking { cells: usize, longs: usize },
    /// The container refused what the file described. Carried rather than
    /// flattened: "no usable palette" names the field and not the problem, and
    /// the problem is the thing somebody debugging a world needs.
    Container(crate::container::ContainerError),
}

impl std::fmt::Display for AnvilError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Field { name } => write!(f, "the chunk has no usable {name}"),
            Self::UnknownBlock { name } => write!(f, "no block is called {name}"),
            Self::UnknownBiome { name } => write!(f, "no biome is called {name}"),
            Self::SectionOutOfRange { y } => {
                write!(f, "section y={y} is outside the world this chunk is in")
            }
            Self::Container(e) => write!(f, "the section's palette is not usable: {e}"),
            Self::BadPacking { cells, longs } => write!(
                f,
                "{longs} long(s) cannot hold {cells} indices at the width the \
                 palette implies"
            ),
        }
    }
}

impl std::error::Error for AnvilError {}

/// How a caller turns the names in a chunk into ids.
///
/// A trait rather than two closures because the two lookups are always
/// supplied together and always come from the same registry snapshot; a caller
/// that could pass a block table from one version and a biome table from
/// another would be a caller able to build a chunk neither table describes.
pub trait Names {
    /// The state id of a block, by name and property values.
    ///
    /// `properties` is what the palette entry carried, as `(name, value)`
    /// pairs — `("facing", "north")`, `("waterlogged", "false")`. A block with
    /// none has an empty slice, which is the common case and the one the
    /// implementation should be fast for.
    ///
    /// A property the block does not have, or a value it does not take, is the
    /// implementation's to decide about. Refusing the whole state is defensible
    /// and so is ignoring the pair; what is not is silently returning the
    /// default, since that turns one wrong property into a block that looks
    /// right and is not.
    fn block(&self, name: &str, properties: &[(&str, &str)]) -> Option<u32>;
    /// A biome's id, by name. This must be its position in the registry the
    /// *client* was told about, not an internal one — the two are the same
    /// number only if somebody made them so.
    fn biome(&self, name: &str) -> Option<u32>;
    /// How many block states exist, for the containers' bounds.
    fn block_registry_size(&self) -> u32;
    /// How many biomes exist.
    fn biome_registry_size(&self) -> u32;
}

/// Read one chunk from the NBT a region file gave back.
pub fn chunk(root: &Compound, world: WorldHeight, names: &impl Names) -> Result<Chunk, AnvilError> {
    let x = int(root, "xPos")?;
    let z = int(root, "zPos")?;
    let pos = ChunkPos::new(x, z);

    let mut chunk = Chunk::uniform(
        pos,
        world,
        names.block_registry_size(),
        names.biome_registry_size(),
        // Air, which every world agrees is state zero. The sections that
        // exist overwrite this; the ones the file omits are air, and that is
        // the file saying so rather than this guessing.
        0,
        0,
    );

    let sections = match root.get("sections") {
        Some(Tag::List(list)) => list,
        _ => return Err(AnvilError::Field { name: "sections" }),
    };

    let lowest = world.min_y() / 16;
    let highest = lowest + (world.height() / 16) as i32 - 1;

    for entry in sections.iter() {
        let Tag::Compound(section) = entry else {
            return Err(AnvilError::Field { name: "a section" });
        };
        let y = match section.get("Y") {
            Some(Tag::Byte(y)) => i32::from(*y),
            _ => return Err(AnvilError::Field { name: "section Y" }),
        };
        // A world file may carry one section below and one above the world it
        // describes — vanilla writes them so light has somewhere to live at
        // the boundary. They hold no blocks anybody can reach, so they are
        // skipped rather than refused.
        if y < lowest || y > highest {
            continue;
        }

        let states = read_container(
            section.get("block_states"),
            Strategy::BLOCK_STATES,
            names.block_registry_size(),
            |palette| block_ids(palette, names),
        )?;
        let biomes = read_container(
            section.get("biomes"),
            Strategy::BIOMES,
            names.biome_registry_size(),
            |palette| biome_ids(palette, names),
        )?;

        *chunk.section_mut(y * 16) = Section::new(
            states,
            biomes,
            // Light is not read. A chunk's stored light is a cache of what the
            // engine would compute, and this server computes it — reading it
            // would mean trusting a file to agree with an engine that has not
            // run yet.
            LightArray::filled(0),
            LightArray::filled(0),
        );
    }

    Ok(chunk)
}

/// Turn a `block_states` or `biomes` compound into a container.
fn read_container(
    tag: Option<&Tag>,
    strategy: Strategy,
    registry_size: u32,
    ids: impl Fn(&Tag) -> Result<Vec<u32>, AnvilError>,
) -> Result<PalettedContainer, AnvilError> {
    let Some(Tag::Compound(compound)) = tag else {
        // An absent container means the section has none of that thing. For
        // blocks that is air; for biomes it is the first biome. Both are the
        // caller's zero, which is what `Chunk::uniform` already put there.
        return Ok(PalettedContainer::filled(strategy, registry_size, 0));
    };
    let palette = compound
        .get("palette")
        .ok_or(AnvilError::Field { name: "palette" })?;
    let entries = ids(palette)?;

    let data = match compound.get("data") {
        Some(Tag::LongArray(longs)) => Some(longs.clone()),
        // **Absent means uniform, not empty.** A reader that returned an empty
        // section here would turn a solid section of stone into air and the
        // chunk would still load.
        None => None,
        _ => return Err(AnvilError::Field { name: "data" }),
    };

    let Some(longs) = data else {
        // One entry and no array: every cell is that entry.
        let only = *entries
            .first()
            .ok_or(AnvilError::Field { name: "palette" })?;
        return Ok(PalettedContainer::filled(strategy, registry_size, only));
    };

    let bits = strategy.disk_bits(entries.len(), registry_size).max(1);
    let expected = crate::bits::long_count(strategy.len(), bits);
    if longs.len() != expected {
        return Err(AnvilError::BadPacking {
            cells: strategy.len(),
            longs: longs.len(),
        });
    }

    // Unpacked cell by cell rather than handed to `from_parts`, and the reason
    // is a case that only shows up against a real world.
    //
    // A file's palette may list one block *name* twice — two entries of
    // `minecraft:water` at different levels, say — because a palette entry is
    // a block *state* and this reader resolves it to a default state by name.
    // Two entries collapse to one id, and `from_parts` refuses a palette with
    // a repeat, correctly: for a palette that really did repeat, every index
    // past the repeat would mean something other than what the file says.
    //
    // Here the repeat is ours and not the file's. The indices are still right;
    // they just land on equal values. So the indices are followed directly,
    // which costs a write per cell and cannot be wrong about which entry a
    // cell holds.
    let storage =
        crate::bits::BitStorage::from_longs(bits, strategy.len(), longs).map_err(|_| {
            AnvilError::BadPacking {
                cells: strategy.len(),
                longs: expected,
            }
        })?;
    let mut container = PalettedContainer::filled(strategy, registry_size, entries[0]);
    for cell in 0..strategy.len() {
        let index = storage.get(cell) as usize;
        let value = *entries.get(index).ok_or(AnvilError::Field {
            name: "palette index",
        })?;
        container.set(cell, value);
    }
    Ok(container)
}

/// `[{Name: "minecraft:stone", Properties: {...}}, ...]` as state ids.
fn block_ids(palette: &Tag, names: &impl Names) -> Result<Vec<u32>, AnvilError> {
    let Tag::List(list) = palette else {
        return Err(AnvilError::Field {
            name: "block palette",
        });
    };
    let mut out = Vec::with_capacity(list.len());
    for entry in list.iter() {
        let Tag::Compound(block) = entry else {
            return Err(AnvilError::Field {
                name: "block palette entry",
            });
        };
        let Some(Tag::String(name)) = block.get("Name") else {
            return Err(AnvilError::Field {
                name: "block palette Name",
            });
        };

        // `Properties` is absent for a block that has none, which is most of
        // them by count and nearly all of them by volume — stone, dirt, air.
        // The borrow is of the tag rather than a copy, so the common case
        // allocates nothing.
        let mut pairs: Vec<(&str, &str)> = Vec::new();
        if let Some(Tag::Compound(properties)) = block.get("Properties") {
            for (key, value) in properties.iter() {
                let Tag::String(value) = value else {
                    return Err(AnvilError::Field {
                        name: "a block property value",
                    });
                };
                pairs.push((key.as_str(), value.as_str()));
            }
        }

        out.push(
            names
                .block(name, &pairs)
                .ok_or_else(|| AnvilError::UnknownBlock { name: name.clone() })?,
        );
    }
    Ok(out)
}

/// `["minecraft:plains", ...]` as biome ids.
fn biome_ids(palette: &Tag, names: &impl Names) -> Result<Vec<u32>, AnvilError> {
    let Tag::List(list) = palette else {
        return Err(AnvilError::Field {
            name: "biome palette",
        });
    };
    let mut out = Vec::with_capacity(list.len());
    for entry in list.iter() {
        let Tag::String(name) = entry else {
            return Err(AnvilError::Field {
                name: "biome palette entry",
            });
        };
        out.push(
            names
                .biome(name)
                .ok_or_else(|| AnvilError::UnknownBiome { name: name.clone() })?,
        );
    }
    Ok(out)
}

fn int(root: &Compound, name: &'static str) -> Result<i32, AnvilError> {
    match root.get(name) {
        Some(Tag::Int(value)) => Ok(*value),
        _ => Err(AnvilError::Field { name }),
    }
}

/// A [`Names`] backed by two maps, for callers that already have the tables.
#[derive(Debug, Default)]
pub struct NameTables {
    /// Keyed by name alone: this is for tests and for callers that have a flat
    /// table, and it ignores properties by construction.
    pub blocks: HashMap<String, u32>,
    pub biomes: HashMap<String, u32>,
    pub block_registry_size: u32,
    pub biome_registry_size: u32,
}

impl Names for NameTables {
    fn block(&self, name: &str, _properties: &[(&str, &str)]) -> Option<u32> {
        self.blocks.get(name).copied()
    }

    fn biome(&self, name: &str) -> Option<u32> {
        self.biomes.get(name).copied()
    }

    fn block_registry_size(&self) -> u32 {
        self.block_registry_size
    }

    fn biome_registry_size(&self) -> u32 {
        self.biome_registry_size
    }
}
