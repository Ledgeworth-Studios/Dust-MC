//! Everything a region file can be wrong about, named.
//!
//! Every variant carries the chunk or region it is about. That is the whole
//! design rule here and it comes from the same place as `dust-config`'s
//! findings: "corrupt region file" sends an operator to delete a world, and
//! "chunk (-37, 12) claims 3 sectors starting at sector 91, and the file has
//! 88" sends them to a backup of one chunk. The second also tells whoever wrote
//! the code which branch produced it.
//!
//! **What this does not catch:** every variant here is about the *container*.
//! A region file whose structure is perfect and whose payloads are nonsense
//! produces no error from this module, because nothing here looks inside a
//! payload — that is the NBT layer's job and it does not exist yet. The one
//! exception is decompression, which fails loudly, so a payload that is not
//! even a valid deflate stream is caught.

use std::io;

use crate::coords::{ChunkPos, RegionPos};
use crate::region::compression::{Compression, UnsupportedScheme};

/// A region file that cannot be read, or a chunk in it that cannot.
#[derive(Debug)]
pub enum RegionError {
    /// The underlying bytes could not be read or written.
    Io {
        region: RegionPos,
        doing: &'static str,
        source: io::Error,
    },
    /// The file is not long enough to hold the 8 KiB header.
    HeaderTruncated { region: RegionPos, length: u64 },
    /// A chunk claims a sector inside the header.
    SectorInHeader { chunk: ChunkPos, first_sector: u32 },
    /// A chunk has an offset but occupies no sectors.
    EmptySectorRun { chunk: ChunkPos, first_sector: u32 },
    /// A chunk's sectors run past the end of the file.
    ChunkPastEnd {
        chunk: ChunkPos,
        first_sector: u32,
        sector_count: u32,
        file_sectors: u64,
    },
    /// Two chunks claim the same sector.
    OverlappingChunks {
        chunk: ChunkPos,
        other: ChunkPos,
        sector: u32,
    },
    /// A chunk's payload declares a length longer than the sectors it was
    /// given.
    StreamPastSectors {
        chunk: ChunkPos,
        declared: u32,
        available: u32,
    },
    /// A chunk's payload declares a negative length.
    NegativeStreamLength { chunk: ChunkPos, declared: i32 },
    /// A chunk has sectors allocated and nothing in them.
    EmptyStream { chunk: ChunkPos },
    /// A compression byte this crate will not decode.
    UnsupportedCompression {
        chunk: ChunkPos,
        source: UnsupportedScheme,
    },
    /// A chunk says its payload is in a `.mcc` file that is not there.
    ExternalChunkMissing { chunk: ChunkPos, file: String },
    /// A chunk says its payload is external and carries inline bytes too.
    ExternalChunkAlsoInline { chunk: ChunkPos, inline_bytes: u32 },
    /// The payload is not a valid stream of the compression it claims.
    Decompress {
        chunk: ChunkPos,
        scheme: Compression,
        source: io::Error,
    },
    /// A chunk was asked of a region file that does not hold it.
    NotInRegion { chunk: ChunkPos, region: RegionPos },
}

impl RegionError {
    /// The chunk this is about, when it is about one.
    #[must_use]
    pub fn chunk(&self) -> Option<ChunkPos> {
        match self {
            Self::Io { .. } | Self::HeaderTruncated { .. } => None,
            Self::SectorInHeader { chunk, .. }
            | Self::EmptySectorRun { chunk, .. }
            | Self::ChunkPastEnd { chunk, .. }
            | Self::OverlappingChunks { chunk, .. }
            | Self::StreamPastSectors { chunk, .. }
            | Self::NegativeStreamLength { chunk, .. }
            | Self::EmptyStream { chunk }
            | Self::UnsupportedCompression { chunk, .. }
            | Self::ExternalChunkMissing { chunk, .. }
            | Self::ExternalChunkAlsoInline { chunk, .. }
            | Self::Decompress { chunk, .. }
            | Self::NotInRegion { chunk, .. } => Some(*chunk),
        }
    }
}

impl std::fmt::Display for RegionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io {
                region,
                doing,
                source,
            } => write!(f, "{region}: {doing}: {source}"),
            Self::HeaderTruncated { region, length } => write!(
                f,
                "{region} is {length} bytes, and the header alone is 8192; the file was cut short"
            ),
            Self::SectorInHeader {
                chunk,
                first_sector,
            } => write!(
                f,
                "{chunk} starts at sector {first_sector}, which is inside the header; \
                 chunk data starts at sector 2"
            ),
            Self::EmptySectorRun {
                chunk,
                first_sector,
            } => write!(
                f,
                "{chunk} points at sector {first_sector} and claims zero sectors, so the header \
                 says both that it exists and that it has no room"
            ),
            Self::ChunkPastEnd {
                chunk,
                first_sector,
                sector_count,
                file_sectors,
            } => write!(
                f,
                "{chunk} claims {sector_count} sector{} from sector {first_sector}, and the file \
                 holds {file_sectors}",
                if *sector_count == 1 { "" } else { "s" }
            ),
            Self::OverlappingChunks {
                chunk,
                other,
                sector,
            } => write!(
                f,
                "{chunk} claims sector {sector}, which {other} already claims; one of the two \
                 is reading the other's bytes"
            ),
            Self::StreamPastSectors {
                chunk,
                declared,
                available,
            } => write!(
                f,
                "{chunk} declares a payload of {declared} bytes and was given room for \
                 {available}"
            ),
            Self::NegativeStreamLength { chunk, declared } => write!(
                f,
                "{chunk} declares a payload length of {declared}, which is negative"
            ),
            Self::EmptyStream { chunk } => write!(
                f,
                "{chunk} has sectors allocated to it and declares a payload of no bytes at all"
            ),
            Self::UnsupportedCompression { chunk, source } => write!(f, "{chunk}: {source}"),
            Self::ExternalChunkMissing { chunk, file } => write!(
                f,
                "{chunk} says its payload is in {file}, and that file is not beside the region"
            ),
            Self::ExternalChunkAlsoInline {
                chunk,
                inline_bytes,
            } => write!(
                f,
                "{chunk} says its payload is in an external file and carries {inline_bytes} \
                 bytes inline as well; there is no way to tell which is the chunk"
            ),
            Self::Decompress {
                chunk,
                scheme,
                source,
            } => write!(
                f,
                "{chunk} does not decompress as {}: {source}",
                scheme.name()
            ),
            Self::NotInRegion { chunk, region } => {
                write!(f, "{chunk} belongs in {}, not in {region}", chunk.region())
            }
        }
    }
}

impl std::error::Error for RegionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } | Self::Decompress { source, .. } => Some(source),
            Self::UnsupportedCompression { source, .. } => Some(source),
            _ => None,
        }
    }
}
