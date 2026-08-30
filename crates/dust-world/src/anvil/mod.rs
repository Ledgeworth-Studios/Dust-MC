//! Anvil: the chunks inside the files Minecraft's worlds are made of.
//!
//! # What this is and is not
//!
//! [`region`](crate::region) reads and writes the *container* — the 4 KiB
//! sectors, the offsets, the per-chunk compression — and deals in an opaque run
//! of decompressed bytes. This module is the layer above: it turns the NBT
//! document inside a payload into a [`Chunk`](crate::chunk::Chunk), and a
//! `Chunk` back into one. It is the half of Anvil that is about Minecraft
//! rather than about files.
//!
//! [`read`] and [`write`] are not each other's mirror image and the difference
//! is the point of this module's design. Reading is checkable against a world
//! that already exists; writing is only checkable by handing the result to
//! Minecraft, so the writer's contract is narrower on purpose — see
//! [`write`]'s own documentation for what it declines to invent.
//!
//! # The layout, from a real 1.21.1 world
//!
//! Twenty-five chunks of a seed-0 world were read field by field before either
//! half of this module was written. Every one of them carried exactly these
//! fifteen root fields and no others:
//!
//! ```text
//! root compound (the NBT root's own name is empty)
//!   DataVersion    : Int          3955 for 1.21.1
//!   xPos, zPos     : Int          the column, in chunk coordinates
//!   yPos           : Int          the index of the lowest section, in sections
//!   Status         : String       minecraft:full when it is finished
//!   sections       : List<Compound>
//!     Y            : Byte         the section's own y, in sections, signed
//!     block_states : Compound     { palette: List<Compound>, data: LongArray? }
//!     biomes       : Compound     { palette: List<String>,   data: LongArray? }
//!     SkyLight     : ByteArray?   2048 bytes, present only where it varies
//!     BlockLight   : ByteArray?   2048 bytes, likewise
//!   Heightmaps     : Compound     four LongArrays, not six — see below
//!   isLightOn      : Byte         whether the stored light may be trusted
//!   block_entities : List         present and empty far more often than not
//!   block_ticks    : List
//!   fluid_ticks    : List
//!   PostProcessing : List<List>   one list per section
//!   structures     : Compound     { References: Compound, starts: Compound }
//!   InhabitedTime  : Long
//!   LastUpdate     : Long
//! ```
//!
//! Four facts in there are worth stating outright, because each is a place an
//! implementation written from memory goes wrong quietly.
//!
//! **`data` is absent when the palette has one entry**, and its absence means
//! "every cell is that entry" rather than "no cells". A reader that treated a
//! missing array as an empty section would turn a solid section of stone into
//! air, and the chunk would still load.
//!
//! **The indices in `data` point into the palette that was written beside
//! them**, not at registry ids, and they are packed at `ceil_log2(palette
//! length)` bits with a floor that differs per container — *not* at the width
//! the container will use in memory. That difference is
//! [`Strategy::disk_bits`](crate::container::Strategy::disk_bits).
//!
//! **`Heightmaps` carries four maps and not six.** `WORLD_SURFACE`,
//! `OCEAN_FLOOR`, `MOTION_BLOCKING` and `MOTION_BLOCKING_NO_LEAVES` are on
//! disk; the two `_WG` variants are worldgen scaffolding and are not saved.
//! [`HeightmapKind::persisted`](crate::heightmap::HeightmapKind::persisted)
//! is the list, and it is one list rather than two so that a reader and a
//! writer cannot disagree about it.
//!
//! **`block_entities` is present and empty** rather than absent, in twenty-three
//! of those twenty-five chunks. An empty NBT list carries element type `End`,
//! which is what an empty `List<Compound>` and an empty `List<String>` both
//! serialise to, so nothing downstream may infer an element type from one.
//!
//! # Blocks are resolved by name, in both directions
//!
//! A palette entry is `{ Name: "minecraft:stone", Properties: {...} }`. Which
//! id that is, and which name an id has, is `dust-registry`'s business and this
//! crate does not depend on it — so both directions go through a caller-supplied
//! lookup: [`Names`] to read, [`Ids`] to write.

mod read;
pub mod write;

use std::collections::HashMap;

pub use read::chunk;
pub use write::{Carried, WriteError};

/// The data version 1.21.1 writes.
pub const DATA_VERSION_1_21_1: i32 = 3955;

/// The `Status` of a chunk that is finished. Every chunk Dust holds is one:
/// the partially generated states exist inside a generator and never leave it.
pub const STATUS_FULL: &str = "minecraft:full";

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

/// How a caller turns the ids in a [`Chunk`](crate::chunk::Chunk) into the
/// names a file spells them with — the inverse of [`Names`].
///
/// # Why this is a second trait and not two more methods on [`Names`]
///
/// Because reading and writing are separately reachable. A server generating
/// its own world writes chunks it never read, and a tool that inspects a world
/// reads chunks it never writes; requiring either to supply the other direction
/// would be requiring a table for a question it does not ask. The pair that
/// *does* have to travel together is each trait's own two lookups, for the
/// reason [`Names`] gives, and that is why each is a trait rather than closures.
///
/// A caller that does both — which the server is — implements both on one type,
/// and then the snapshot argument holds across the pair by construction.
pub trait Ids {
    /// The name and property values of a block state.
    ///
    /// The pairs are what the file will spell out under `Properties`; a block
    /// with none returns an empty vector and the writer omits the compound
    /// entirely, which is what vanilla does and what the reader expects.
    ///
    /// `None` means this id names no block in the caller's table. That is not
    /// something a writer may paper over — see [`write`] — because the
    /// alternative is a file that claims some other block stands there.
    fn block_name(&self, id: u32) -> Option<(&str, Vec<(&str, &str)>)>;
    /// A biome's name, by the same id [`Names::biome`] returns.
    fn biome_name(&self, id: u32) -> Option<&str>;
}

/// A [`Names`] and [`Ids`] backed by two maps, for callers that already have
/// the tables.
///
/// Keyed by name alone in both directions: this is for tests and for callers
/// with a flat table, and it ignores block properties by construction. A real
/// server resolves through `dust-registry`, where a state id is a block *and*
/// its properties, and gets stairs that face the way they were written.
#[derive(Debug, Default)]
pub struct NameTables {
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

impl Ids for NameTables {
    /// Linear in the table, because this type is a test fixture and the maps
    /// it holds are the wrong shape for the reverse question. A caller writing
    /// real worlds implements [`Ids`] on something that can answer it.
    fn block_name(&self, id: u32) -> Option<(&str, Vec<(&str, &str)>)> {
        self.blocks
            .iter()
            .find(|(_, v)| **v == id)
            .map(|(name, _)| (name.as_str(), Vec::new()))
    }

    fn biome_name(&self, id: u32) -> Option<&str> {
        self.biomes
            .iter()
            .find(|(_, v)| **v == id)
            .map(|(name, _)| name.as_str())
    }
}
