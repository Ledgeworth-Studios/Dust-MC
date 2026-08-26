//! Chunk storage: packed bit arrays, paletted containers, region files and
//! heightmaps.
//!
//! # What is here
//!
//! * [`bits::BitStorage`] — N-bit unsigned values packed into `i64`s, in the
//!   post-1.16 layout where no value spans a long boundary.
//! * [`palette`] — the four palette strategies, and the promotion between them.
//! * [`container::PalettedContainer`] — a cube of registry ids, configured by
//!   [`container::Strategy`] as either a section's 4096 block states or its 64
//!   biomes.
//! * [`region`] — `.mca` files: header, sector allocation, per-chunk
//!   compression, external `.mcc` payloads, and both halves of read and write.
//! * [`heightmap`] — the six heightmaps, stored and accessed.
//! * [`coords`] — chunk and region positions, and the shifts between them.
//!
//! # What is not here, and where the seam is
//!
//! A chunk's payload inside a region file is compressed NBT. This crate stops
//! at the compression: [`region::ChunkPayload`] is an opaque run of
//! decompressed bytes, and the layer that reads those bytes into blocks,
//! entities and block entities lives above this crate and does not exist yet.
//!
//! The line is drawn there because everything below it is checkable without
//! knowing what a chunk is. A region file's header, sector runs, declared
//! lengths and compression bytes can all be contradicted by the file itself,
//! which is why every error in [`region::RegionError`] can name the chunk it is
//! about and say what did not add up. One byte further in, the only question
//! left is whether some bytes parse, and a wrong answer there says nothing
//! about whether the container around them was sound.
//!
//! The same seam appears twice more, for the same reason:
//!
//! * [`container::PalettedContainer`] indexes *a* registry and is told how many
//!   ids it has. It never asks what a block is, so it does not depend on the
//!   block table.
//! * [`heightmap::Heightmap`] takes the "does this state count" predicate as a
//!   parameter. There are six heightmaps and they differ only in that
//!   predicate, all six need the block registry to answer it, and none of them
//!   need it to store 256 numbers.
//!
//! # What the guards in this crate do not catch
//!
//! Stated per the rule in `Testing.md`, because a guard's blind spots are a
//! list of the defects it will pass, published in advance:
//!
//! * **Anything inside a payload.** A region file full of intact,
//!   well-compressed nonsense reads without a single error.
//! * **A self-consistently wrong convention.** Encoding then decoding agrees
//!   with itself under any convention, including a wrong one. The tests that
//!   have teeth are the ones in `tests/vanilla_corpus.rs`, which check against
//!   region files a real 1.21.1 server wrote and which Dust did not produce.
//!   Everything else in the suite is a self-consistency check and should be
//!   read as one.
//! * **A heightmap computed with the wrong predicate.** It is a valid heightmap
//!   of wrong numbers, and this crate cannot know: it does not have the
//!   registry that would let it disagree.
//! * **A palette whose entries are not real registry ids.** The container
//!   checks that an id is *in range* for the registry size it was given, and
//!   nothing checks that the range is the right registry.
//! * **Durability.** Writes go to the operating system and are not fsynced.
//!   A region file is consistent against a crashed process and not against a
//!   lost power supply.

pub mod bits;
pub mod container;
pub mod coords;
pub mod heightmap;
pub mod palette;
pub mod region;

pub use bits::{BitStorage, BitStorageError};
pub use container::{ContainerError, PalettedContainer, Strategy};
pub use coords::{ChunkPos, RegionPos};
pub use heightmap::{Heightmap, HeightmapKind, HeightmapSet, WorldHeight};
pub use palette::{Palette, PaletteKind};
pub use region::{ChunkPayload, Compression, RegionError, RegionFile};
