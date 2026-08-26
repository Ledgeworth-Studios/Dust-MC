//! The 8 KiB at the front of every region file.
//!
//! Two tables of 1024 entries each, in header-slot order. The first says where
//! a chunk's bytes are; the second says when it was last written. Both are
//! fixed-size and always present, so a region file with no chunks in it is
//! still 8192 bytes.

use crate::coords::ChunkPos;

/// Bytes per sector, and the size of each of the header's two tables.
pub const SECTOR_BYTES: usize = 4096;

/// The header's size: one sector of locations, one of timestamps.
pub const HEADER_BYTES: usize = SECTOR_BYTES * 2;

/// Chunk slots per region file.
pub const SLOTS: usize = 1024;

/// The first sector a chunk's payload may occupy. Sectors 0 and 1 are the
/// header itself.
pub const FIRST_DATA_SECTOR: u32 = 2;

/// The largest sector run a location entry can describe, because the count is
/// one byte.
///
/// A payload needing more than this is moved out to a `.mcc` file. The
/// threshold is `>= 256` rather than `> 255` in vanilla's own arithmetic, which
/// is the same thing said less clearly.
pub const MAX_SECTORS: u32 = 255;

/// Where one chunk's payload is, in sectors.
///
/// `first_sector == 0 && sector_count == 0` means "no chunk here", which is
/// what a whole zeroed table says, and is why sector 0 can never hold data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Location {
    pub first_sector: u32,
    pub sector_count: u32,
}

impl Location {
    /// Whether the slot is empty.
    ///
    /// Vanilla treats the whole packed word being zero as absent, so a count of
    /// zero with a nonzero offset is *not* absent — it is a damaged entry, and
    /// this returns false for it so that the caller reports it rather than
    /// quietly skipping a chunk that exists.
    #[must_use]
    pub const fn is_absent(&self) -> bool {
        self.first_sector == 0 && self.sector_count == 0
    }

    /// One past the last sector this chunk occupies.
    #[must_use]
    pub const fn end_sector(&self) -> u64 {
        self.first_sector as u64 + self.sector_count as u64
    }
}

/// A region file's location and timestamp tables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    locations: Box<[Location; SLOTS]>,
    timestamps: Box<[i32; SLOTS]>,
}

impl Default for Header {
    fn default() -> Self {
        Self::empty()
    }
}

impl Header {
    /// A header with no chunks in it.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            locations: Box::new([Location::default(); SLOTS]),
            timestamps: Box::new([0; SLOTS]),
        }
    }

    /// Decode 8192 bytes.
    ///
    /// # Panics
    ///
    /// If `bytes` is not exactly [`HEADER_BYTES`] long. Callers read a fixed
    /// buffer, so a short one is a bug here rather than a damaged file; the
    /// damaged-file case is a read that could not fill the buffer at all, and
    /// it is reported before this is called.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Self {
        assert_eq!(bytes.len(), HEADER_BYTES, "a region header is 8192 bytes");
        let mut header = Self::empty();
        for slot in 0..SLOTS {
            let at = slot * 4;
            // Three bytes of sector offset, big-endian, then one of count.
            header.locations[slot] = Location {
                first_sector: u32::from(bytes[at]) << 16
                    | u32::from(bytes[at + 1]) << 8
                    | u32::from(bytes[at + 2]),
                sector_count: u32::from(bytes[at + 3]),
            };
            let at = SECTOR_BYTES + slot * 4;
            header.timestamps[slot] =
                i32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
        }
        header
    }

    /// Encode 8192 bytes.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = vec![0u8; HEADER_BYTES];
        for slot in 0..SLOTS {
            let location = self.locations[slot];
            let at = slot * 4;
            bytes[at] = (location.first_sector >> 16) as u8;
            bytes[at + 1] = (location.first_sector >> 8) as u8;
            bytes[at + 2] = location.first_sector as u8;
            bytes[at + 3] = location.sector_count as u8;
            let at = SECTOR_BYTES + slot * 4;
            bytes[at..at + 4].copy_from_slice(&self.timestamps[slot].to_be_bytes());
        }
        bytes
    }

    #[must_use]
    pub fn location(&self, pos: ChunkPos) -> Location {
        self.locations[pos.header_slot()]
    }

    #[must_use]
    pub fn location_at_slot(&self, slot: usize) -> Location {
        self.locations[slot]
    }

    pub fn set_location(&mut self, pos: ChunkPos, location: Location) {
        self.locations[pos.header_slot()] = location;
    }

    /// When the chunk was last written, as a Unix time in seconds.
    ///
    /// Signed, and stored as written rather than as a `SystemTime`: it is four
    /// bytes of somebody else's bookkeeping that Dust preserves, and turning it
    /// into a time type would invite normalising a value that must survive a
    /// rewrite unchanged.
    #[must_use]
    pub fn timestamp(&self, pos: ChunkPos) -> i32 {
        self.timestamps[pos.header_slot()]
    }

    pub fn set_timestamp(&mut self, pos: ChunkPos, seconds: i32) {
        self.timestamps[pos.header_slot()] = seconds;
    }
}
