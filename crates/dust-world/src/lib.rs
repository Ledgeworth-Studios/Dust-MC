//! Chunk storage: packed bit arrays, paletted containers, region files,
//! heightmaps, light arrays and the chunk itself.
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
//! * [`heightmap`] — the six heightmaps, stored and accessed, plus the
//!   recompute helpers that fill them from a chunk's sections.
//! * [`light::LightArray`] — a section's sky or block light: 4096 four-bit
//!   levels in the 2048 bytes the format stores. Storage only; see its module
//!   documentation for where the light engine will take over.
//! * [`chunk::Chunk`] — one chunk column assembled from all of the above:
//!   sections of block states and biomes, per-section light, the heightmaps,
//!   and block-entity handles. Its serialised form crosses the region layer
//!   through the [`chunk::NbtWriter`] and [`chunk::NbtReader`] traits.
//! * [`coords`] — chunk, region and block positions, and the shifts between
//!   them.
//!
//! # What is not here, and where the seams are
//!
//! A chunk's payload inside a region file is compressed NBT, and there is no
//! NBT crate on this branch — `dust-nbt` forked from an earlier base and will
//! be merged by someone else's commit. The seam is explicit rather than
//! improvised: [`region::ChunkPayload`] is an opaque run of decompressed
//! bytes, [`chunk::Chunk`] is the in-memory shape it decodes into, and
//! [`chunk::NbtWriter`] / [`chunk::NbtReader`] are the two functions between
//! them, waiting for an implementation that owns real tags and
//! Anvil-compatible field names. Nothing in this crate parses a tag, and no
//! call site here changes when that implementation arrives.
//!
//! The line is drawn where it is because everything below it is checkable
//! without knowing what a chunk says. A region file's header, sector runs,
//! declared lengths and compression bytes can all be contradicted by the file
//! itself, which is why every error in [`region::RegionError`] can name the
//! chunk it is about and say what did not add up.
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
//!   have teeth are ones that check against files a real 1.21.1 server wrote,
//!   produced by `tools/generate-corpus.sh`; everything else in the suite is
//!   a self-consistency check and should be read as one.
//! * **A heightmap computed with the wrong predicate.** It is a valid heightmap
//!   of wrong numbers, and this crate cannot know: it does not have the
//!   registry that would let it disagree.
//! * **Light values that were never propagated.** [`light::LightArray`] pins
//!   the encoding; whether fifteen is the right answer for a cell is the
//!   future engine's problem, and nothing here can ask it.
//! * **Ids that mean the wrong thing.** The container checks that an id is *in
//!   range* for the registry size it was given, and nothing checks that the
//!   range is the right registry.
//! * **Durability.** Writes go to the operating system and are not fsynced.
//!   A region file is consistent against a crashed process and not against a
//!   lost power supply.

pub mod bits;
pub mod chunk;
pub mod container;
pub mod coords;
pub mod heightmap;
pub mod light;
pub mod palette;
pub mod region;
pub mod slab;

pub use bits::{BitStorage, BitStorageError};
pub use chunk::{BlockEntityHandle, Chunk, NbtReader, NbtWriter};
pub use container::{ContainerError, PalettedContainer, Strategy};
pub use coords::{BlockPos, ChunkPos, RegionPos};
pub use heightmap::{Heightmap, HeightmapKind, HeightmapSet, WorldHeight};
pub use light::{LightArray, LightArrayError};
pub use palette::{Palette, PaletteKind};
pub use region::{ChunkPayload, Compression, RegionError, RegionFile};
pub use slab::{Slab, SlabError, SlabKey};
