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
//!   levels in the 2048 bytes the format stores.
//! * [`propagation`] — the light engine's walks over that storage: raise
//!   and darken breadth-first passes, sky-light column seeding, a budget
//!   with typed overflow errors, and the [`propagation::LightGraph`] seam
//!   they run through.
//! * [`chunk::Chunk`] — one chunk column assembled from all of the above:
//!   sections of block states and biomes, per-section light, the heightmaps,
//!   and block-entity handles in a generational slab. Its serialised form
//!   crosses the region layer through the [`chunk::NbtWriter`] and
//!   [`chunk::NbtReader`] traits.
//! * [`slab`] — a generational slot array: stable keys over storage that
//!   moves, typed errors for dead ones. Block entities live in one per
//!   chunk; positions ride on the records because those cross files, while
//!   keys never leave the process.
//! * [`coords`] — chunk, region and block positions, and the shifts between
//!   them.
//!
//! # What is not here, and where the seams are
//!
//! Everything listed above is checkable against itself, or against files a
//! real server wrote. Four things are deliberately absent, each waiting on a
//! dependency that does not exist yet, each behind a named seam rather than
//! an improvised one:
//!
//! * **Serialisation** belongs to `dust-nbt`, which forked from an earlier
//!   base and will be merged by someone else's commit. A chunk's payload in
//!   a region file is compressed NBT; [`region::ChunkPayload`] is the opaque
//!   byte run, [`chunk::Chunk`] is the in-memory shape, and
//!   [`chunk::NbtWriter`] / [`chunk::NbtReader`] are the two functions
//!   between them. Nothing here parses a tag, and no call site changes when
//!   real tags arrive.
//! * **Meaning** belongs to `dust-registry`. The same seam three times over:
//!   [`container::PalettedContainer`] indexes *a* registry and is told only
//!   its size; [`heightmap::Heightmap`] takes the "does this state count"
//!   predicate as a closure because all six maps differ in exactly that;
//!   and which block states let light through is answered by whoever wires
//!   [`propagation::LightGraph`], through
//!   [`propagation::OpacityModel`] — which carries either Minecraft's own
//!   level for every state, read from the operator's jar by the light oracle,
//!   or the stand-in that treats everything but an explicit transparent set as
//!   a wall. This crate holds neither table and cannot tell them apart.
//! * **A connected light engine.** [`propagation`] walks levels across any
//!   graph it is handed, including sky seeding above heightmaps, but nothing
//!   yet connects a chunk's blocks to its light arrays: that connection is
//!   the `LightGraph` implementation, which needs the registry's opacity
//!   table to be worth writing.
//! * **Durability.** Region writes reach the operating system unsynced;
//!   fsync policy belongs to whichever layer knows when a save is "done".
//!
//! The line is drawn where it is because everything on this side of it is
//! checkable without knowing what a chunk means. A region file's header,
//! sector runs, declared lengths and compression bytes can all be
//! contradicted by the file itself, which is why every error in
//! [`region::RegionError`] can name the chunk it is about and say what did
//! not add up. The same standard holds for the newer modules: palettes are
//! pinned against vanilla's promotion boundaries, heightmap packing against
//! longs a real server wrote, light walks against a naive reference that
//! cannot share their bugs.
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
//!   the encoding and [`propagation`] walks levels across a graph, but
//!   nothing yet connects a chunk's blocks to its light: whether fifteen is
//!   the right answer for a cell needs the registry-backed
//!   [`propagation::LightGraph`] implementation, and until that lands a
//!   section can read bright while its blocks say dark.
//! * **Ids that mean the wrong thing.** The container checks that an id is *in
//!   range* for the registry size it was given, and nothing checks that the
//!   range is the right registry.
//! * **Durability.** Writes go to the operating system and are not fsynced.
//!   A region file is consistent against a crashed process and not against a
//!   lost power supply.

pub mod anvil;
pub mod bits;
pub mod chunk;
pub mod column_light;
pub mod container;
pub mod coords;
pub mod heightmap;
pub mod light;
pub mod network;
pub mod palette;
pub mod propagation;
pub mod region;
pub mod slab;

pub use bits::{BitStorage, BitStorageError};
pub use chunk::{BlockEntityHandle, Chunk, NbtReader, NbtWriter};
pub use container::{ContainerError, PalettedContainer, Strategy};
pub use coords::{BlockPos, ChunkPos, RegionPos};
pub use heightmap::{Heightmap, HeightmapKind, HeightmapSet, WorldHeight};
pub use light::{LightArray, LightArrayError};
pub use palette::{Palette, PaletteKind};
pub use propagation::{Budget, LightGraph, OpacityModel, PropagationError};
pub use region::{ChunkPayload, Compression, RegionError, RegionFile};
pub use slab::{Slab, SlabError, SlabKey};
