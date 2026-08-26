//! The chunk envelope: everything about a chunk packet that is not the world.
//!
//! # What is implemented, and what is a hook
//!
//! A chunk on the wire has four parts. The coordinates and the light masks are
//! small and fully decoded here. The heightmaps are an NBT compound this crate
//! delimits and does not open — which entries exist and what their long arrays
//! mean is the atlas's business. The section data is a length-prefixed blob of
//! paletted containers: the *envelope* (one VarInt count, then that many
//! bytes) is exact here, and the *contents* are behind [`Section`], the trait
//! `dust-world` implements when it exists.
//!
//! The trait is deliberately tiny. It says how one section's container is read
//! from or written to a cursor and nothing else — no palette types, no block
//! storage maths — because every one of those decisions belongs to whoever
//! stores chunks. What this side guarantees is the part that cannot be fixed
//! later from the world side: sections are walked in y order from the lowest,
//! the run stops when the blob runs out rather than when a count says so (the
//! wire carries no count; trailing all-air sections are implied), and a
//! malformed section is an error naming the section's index.
//!
//! # Why the blob stays bytes at all
//!
//! A paletted container begins with a bits-per-entry VarInt whose meaning
//! changes with its value (single-value, indirect, direct), and getting it
//! wrong does not fail loudly — it silently reindexes every block in the
//! chunk. That is a format this project will implement exactly once, in the
//! crate that owns chunk storage, against real chunk tests. Decoding half of
//! it here would buy nothing and split the truth in two.

use crate::nbt::Nbt;
use crate::types::{Decode, Encode, PrefixedBytes};
use crate::wire::{DecodeError, EncodeError, WireRead, WireWrite};
use crate::{wire_struct, ProtocolVersion};

/// How many bytes of section data one packet may carry before it is refused.
///
/// A worst-case vanilla section is a few kilobytes and a tall world has
/// dozens of them, so four mebibytes is far above anything honest while still
/// bounding the allocation a hostile count could ask for. The bound exists to
/// be generous and finite at the same time; the day real worlds trip it is
/// the day it was wrong, and that would show up as a named error, not a
/// corrupt chunk.
pub const CHUNK_DATA_MAX_BYTES: usize = 4 * 1024 * 1024;

/// How many bytes one section's light array always is: half a byte per block,
/// 16³ blocks.
pub const LIGHT_SECTION_BYTES: usize = 2048;

/// One section's paletted container, as `dust-world` will spell it.
///
/// Implemented by the world crate for whatever its internal section type is;
/// this crate never names the concrete type. The version parameter is carried
/// because paletted containers have changed shape between releases and D3
/// commits this layer to surviving that.
pub trait Section: Sized {
    fn decode_wire<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError>;

    fn encode_wire<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError>;
}

/// The paletted section contents of one chunk column, held as sent.
///
/// Opaque by design; see the module docs for why. Callers either forward the
/// bytes untouched — relaying a chunk from storage to a client needs nothing
/// more — or walk them with [`Self::parse_sections`], which hands each
/// section to the world crate's [`Section`] implementation in wire order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChunkData(pub PrefixedBytes<CHUNK_DATA_MAX_BYTES>);

impl ChunkData {
    /// Walk the blob and decode every section present.
    ///
    /// Sections appear from the bottom of the column upward, and the blob ends
    /// where the non-empty ones stop; the caller supplies how many sections
    /// its world is tall so the result can be padded to a full column with
    /// `None` for "implied air above". A failure inside any section fails the
    /// whole walk with that decoder's own error — this side adds no wrapping,
    /// because whoever debugs a broken chunk wants the palette's complaint and
    /// not a layer of packaging around it.
    pub fn parse_sections<S: Section>(
        &self,
        column_height_sections: usize,
        version: ProtocolVersion,
    ) -> Result<Vec<Option<S>>, DecodeError> {
        let mut input = crate::wire::Reader::new(self.as_bytes());
        let mut sections = Vec::new();
        for _index in 0..column_height_sections {
            if input.remaining() == 0 {
                break;
            }
            sections.push(Some(S::decode_wire(&mut input, version)?));
        }
        // A decoder that overran its section would leave bytes for a next one
        // that never decodes cleanly; one that under-read leaves the rest of
        // the column unexplained. Both are refusals, not shrugs.
        if !input.rest().is_empty() {
            return Err(DecodeError::Nbt {
                why: "section data left over after the last decoded section",
            });
        }
        sections.resize_with(column_height_sections, || None);
        Ok(sections)
    }

    /// The bytes exactly as they were encoded, for pass-through callers.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0 .0
    }
}

impl Encode for ChunkData {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        self.0.encode(out, version)
    }
}

impl Decode for ChunkData {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        PrefixedBytes::<CHUNK_DATA_MAX_BYTES>::decode(input, version).map(Self)
    }
}

wire_struct! {
    /// A block entity baked into a chunk: its slot inside the column, what it
    /// is, and its own NBT.
    ///
    /// The x/z pair arrives pre-packed into one byte, four bits each, because
    /// both are always within the chunk. Unpacking is two shifts and happens
    /// wherever dust-world wants positions instead of slots.
    pub struct BlockEntity {
        packed_xz: u8,
        y: i16,
        kind: crate::types::VarInt,
        data: Nbt,
    }
}

wire_struct! {
    /// The lighting half of a chunk packet.
    ///
    /// Four masks say which of the world's sections carry sky light, block
    /// light, or are known-empty in each; then one 2 KiB array per set bit, in
    /// bit order. The masks lead the arrays precisely so a client can skip
    /// lighting wholesale if it renders without it — which is why this struct
    /// survives intact even though Dust computes none of the values yet.
    pub struct LightData {
        sky_mask: crate::types::BitSet,
        block_mask: crate::types::BitSet,
        empty_sky_mask: crate::types::BitSet,
        empty_block_mask: crate::types::BitSet,
        sky_arrays: Vec<PrefixedBytes<LIGHT_SECTION_BYTES>>,
        block_arrays: Vec<PrefixedBytes<LIGHT_SECTION_BYTES>>,
    }
}
