//! NBT: Minecraft's binary serialisation format, and SNBT, its textual form.
//!
//! NBT is a tree of thirteen tag types, written big-endian, with no schema. It
//! is the format of `level.dat`, of every chunk inside a region file, of player
//! and entity data, and — in a variant described below — of the item and
//! component data that travels in packets. A world that vanilla refuses to open
//! and a world produced by a server that is broken look identical from outside,
//! so the details here are not stylistic.
//!
//! # Which dialects this crate implements
//!
//! There are four things called NBT and this crate implements three of them.
//!
//! 1. **Java Edition file NBT.** Big-endian; the root tag carries a name. This
//!    is [`read::from_bytes`] and [`write::to_vec`].
//! 2. **Java Edition network NBT, 1.20.2 and later.** Identical except that the
//!    root's name is *absent* — not empty, absent. 1.21.1 uses this for every
//!    piece of NBT on the wire. This is [`read::from_bytes_network`] and
//!    [`write::to_vec_network`]. The two are a mode the caller chooses and never
//!    a guess the reader makes; [`read::Mode`] explains why a guess is not
//!    merely unreliable but attacker-selectable.
//! 3. **SNBT**, the textual form used by commands and `/data`:
//!    `{Count:1b,id:"minecraft:stone"}`. This is [`snbt::parse`] and
//!    [`snbt::to_string`].
//!
//! The fourth is **Bedrock Edition NBT**, which is little-endian, has a
//! VarInt-length variant for network use, and is not implemented here at all.
//!
//! # What this crate does not do
//!
//! * **No region files.** This crate reads and writes documents; the 4 KiB slot
//!   allocation, the offset table and the timestamp table of an `.mca` file are
//!   `dust-world`'s. What is here is [`Compression::from_region_scheme`], so
//!   that the byte in a chunk's header means the same thing in both crates.
//! * **No schema, and no typed accessors beyond the shape of the format.** A
//!   chunk is a `Compound`; knowing that its `sections` field is a list of
//!   compounds each holding a `block_states` is Minecraft knowledge and lives
//!   with the code that has it.
//! * **No data fixing.** A document from an older world is returned as it was
//!   written. `DataVersion` is a field like any other here.
//! * **No `serde`.** NBT's type distinctions — a byte that means a boolean, six
//!   numeric widths, three array types that are not lists — do not survive a
//!   round trip through a self-describing derive without a schema to put them
//!   back, and a wrong width is a document vanilla reads differently.
//!
//! # What the guards here do not catch
//!
//! The reader is reachable from a packet an attacker controls, and it has three
//! defences: a depth limit, a length check before every allocation, and a heap
//! budget. Between them they bound stack, allocation-from-a-lie, and
//! allocation-from-expansion. None of them bounds *time*: a two-megabyte
//! document of legitimate tags takes as long as it takes, and if that is too
//! long for a network thread the answer is a smaller frame limit, upstream.
//!
//! The heap budget is charged in this crate's sizes rather than the JVM's, so
//! it does not agree with vanilla's `NbtAccounter` to the byte. See
//! [`Limits::max_heap_bytes`].
//!
//! Byte-identity between what is read and what is written is a property of the
//! *document*, and it is tested against real vanilla files. It is not a
//! property of the compressed file: gzip output depends on the deflate
//! implementation, and no Rust one produces the same bytes as the JVM's zlib.
//! Reading a `level.dat` and writing it back gives an equivalent file, not an
//! identical one.
//!
//! Nothing here validates that a document *means* anything. A `Compound` with a
//! `Pos` that is a list of three strings is well-formed NBT.
//!
//! # Reading a file
//!
//! ```
//! use dust_nbt::{compression, read, Compression, Tag};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let level_dat = dust_nbt::write::to_vec("", &Tag::Compound(Default::default()))?;
//! # let level_dat = compression::compress(&level_dat, Compression::Gzip)?;
//! let plain = compression::decompress_detected(&level_dat, compression::DEFAULT_FILE_LIMIT)?;
//! let document = read::from_bytes(&plain)?;
//! assert!(matches!(document.tag, Tag::Compound(_)));
//! # Ok(())
//! # }
//! ```

pub mod compression;
pub mod error;
pub mod mutf8;
pub mod read;
pub mod snbt;
pub mod tag;
pub mod write;

pub use compression::Compression;
pub use error::{Error, Result};
pub use read::{Limits, Mode, Named};
pub use snbt::{NumericStyle, PrintProfile};
pub use tag::{Compound, List, ListError, Tag, TagType};
