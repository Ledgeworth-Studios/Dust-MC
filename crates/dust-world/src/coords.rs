//! Chunk and region coordinates, and the arithmetic between them.
//!
//! The conversions here are two lines each and are in one place anyway,
//! because both of them are wrong in the same way if written with `/` and `%`:
//! chunk coordinates are signed and go negative, and `-1 / 32` is `0` while
//! `-1 >> 5` is `-1`. A region file with a negative coordinate in its name is
//! the ordinary case, not an edge case, so the arithmetic that gets it wrong
//! puts a quarter of the world in the wrong file.

/// Chunks per region, along one axis. A region file holds 32 x 32 = 1024.
pub const CHUNKS_PER_REGION: i32 = 32;

/// A chunk's position in the world, in chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChunkPos {
    pub x: i32,
    pub z: i32,
}

impl ChunkPos {
    #[must_use]
    pub const fn new(x: i32, z: i32) -> Self {
        Self { x, z }
    }

    /// The region file this chunk lives in.
    ///
    /// An arithmetic shift, not a division: `-1 / 32` rounds towards zero and
    /// would put chunk -1 in region 0, where nothing would ever find it again.
    #[must_use]
    pub const fn region(self) -> RegionPos {
        RegionPos {
            x: self.x >> 5,
            z: self.z >> 5,
        }
    }

    /// This chunk's x within its region, `0..32`.
    #[must_use]
    pub const fn local_x(self) -> u32 {
        (self.x & 31) as u32
    }

    /// This chunk's z within its region, `0..32`.
    #[must_use]
    pub const fn local_z(self) -> u32 {
        (self.z & 31) as u32
    }

    /// This chunk's slot in a region file's header tables, `0..1024`.
    ///
    /// `x` varies fastest, which is the opposite of the paletted container's
    /// ordering. There is no reason for them to differ and they differ anyway.
    #[must_use]
    pub const fn header_slot(self) -> usize {
        (self.local_x() + self.local_z() * 32) as usize
    }

    /// The name of the file an oversized chunk's payload is moved to.
    #[must_use]
    pub fn external_file_name(self) -> String {
        format!("c.{}.{}.mcc", self.x, self.z)
    }
}

impl std::fmt::Display for ChunkPos {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "chunk ({}, {})", self.x, self.z)
    }
}

/// A region file's position, in regions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RegionPos {
    pub x: i32,
    pub z: i32,
}

impl RegionPos {
    #[must_use]
    pub const fn new(x: i32, z: i32) -> Self {
        Self { x, z }
    }

    /// The chunk at a slot within this region, `0..32` on each axis.
    ///
    /// # Panics
    ///
    /// If either local coordinate is 32 or more.
    #[must_use]
    pub const fn chunk_at(self, local_x: u32, local_z: u32) -> ChunkPos {
        assert!(
            local_x < 32 && local_z < 32,
            "coordinate outside the region"
        );
        ChunkPos {
            x: (self.x << 5) + local_x as i32,
            z: (self.z << 5) + local_z as i32,
        }
    }

    /// The chunk at a header slot, `0..1024`.
    ///
    /// # Panics
    ///
    /// If `slot` is 1024 or more.
    #[must_use]
    pub const fn chunk_at_slot(self, slot: usize) -> ChunkPos {
        assert!(slot < 1024, "slot outside the region header");
        self.chunk_at((slot % 32) as u32, (slot / 32) as u32)
    }

    /// Every chunk position in this region, in header-slot order.
    pub fn chunks(self) -> impl Iterator<Item = ChunkPos> {
        (0..1024).map(move |slot| self.chunk_at_slot(slot))
    }

    /// The file name a world save uses for this region.
    #[must_use]
    pub fn file_name(self) -> String {
        format!("r.{}.{}.mca", self.x, self.z)
    }

    /// The region a file name refers to, or `None` if the name is not one.
    ///
    /// Used to walk a `region/` directory. Deliberately strict about the shape:
    /// a directory listing is untrusted input, and a name that nearly parses is
    /// how a stray `r.0.0.mca.bak` becomes a region Dust believes in.
    #[must_use]
    pub fn from_file_name(name: &str) -> Option<Self> {
        let rest = name.strip_prefix("r.")?.strip_suffix(".mca")?;
        let (x, z) = rest.split_once('.')?;
        Some(Self {
            x: x.parse().ok()?,
            z: z.parse().ok()?,
        })
    }

    /// Whether a chunk belongs in this region file.
    #[must_use]
    pub const fn contains(self, pos: ChunkPos) -> bool {
        pos.region().x == self.x && pos.region().z == self.z
    }
}

impl std::fmt::Display for RegionPos {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "region ({}, {})", self.x, self.z)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negative_chunks_land_in_negative_regions() {
        // The whole reason this module exists. Written out rather than derived,
        // because a shared formula would be the same mistake twice.
        for (chunk, region) in [
            ((0, 0), (0, 0)),
            ((31, 31), (0, 0)),
            ((32, 32), (1, 1)),
            ((-1, -1), (-1, -1)),
            ((-32, -32), (-1, -1)),
            ((-33, -33), (-2, -2)),
            ((-1, 40), (-1, 1)),
        ] {
            assert_eq!(
                ChunkPos::new(chunk.0, chunk.1).region(),
                RegionPos::new(region.0, region.1),
                "chunk {chunk:?}"
            );
        }
    }

    #[test]
    fn local_coordinates_are_always_in_range_and_come_back() {
        for x in -70i32..70 {
            for z in -70i32..70 {
                let chunk = ChunkPos::new(x, z);
                assert!(chunk.local_x() < 32 && chunk.local_z() < 32, "{chunk}");
                assert!(chunk.header_slot() < 1024, "{chunk}");
                assert_eq!(
                    chunk.region().chunk_at(chunk.local_x(), chunk.local_z()),
                    chunk
                );
                assert_eq!(chunk.region().chunk_at_slot(chunk.header_slot()), chunk);
            }
        }
    }

    #[test]
    fn a_region_lists_its_thousand_and_twenty_four_chunks_once_each() {
        for region in [RegionPos::new(0, 0), RegionPos::new(-2, 3)] {
            let chunks: Vec<ChunkPos> = region.chunks().collect();
            assert_eq!(chunks.len(), 1024);
            let mut sorted = chunks.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted.len(), 1024, "{region} lists a chunk twice");
            for chunk in chunks {
                assert!(region.contains(chunk), "{chunk} is not in {region}");
            }
        }
    }

    #[test]
    fn file_names_round_trip_and_near_misses_do_not_parse() {
        for region in [
            RegionPos::new(0, 0),
            RegionPos::new(-1, 2),
            RegionPos::new(1234, -5678),
        ] {
            let name = region.file_name();
            assert_eq!(RegionPos::from_file_name(&name), Some(region), "{name}");
        }
        assert_eq!(RegionPos::new(-1, 2).file_name(), "r.-1.2.mca");
        for name in [
            "r.0.0.mca.bak",
            "r.0.mca",
            "r..0.mca",
            "region.0.0.mca",
            "r.0.0.mcr",
            "r.0.0.0.mca",
            "",
        ] {
            assert_eq!(
                RegionPos::from_file_name(name),
                None,
                "{name} should not parse as a region"
            );
        }
    }

    #[test]
    fn external_files_are_named_by_absolute_chunk_coordinates() {
        // Not by the local ones. A `.mcc` sits in the same directory as every
        // other region's, so a name built from local coordinates would collide
        // with the same slot in the region next door.
        assert_eq!(ChunkPos::new(-33, 40).external_file_name(), "c.-33.40.mcc");
        assert_eq!(ChunkPos::new(0, 0).external_file_name(), "c.0.0.mcc");
    }

    #[test]
    fn the_header_slot_varies_x_fastest() {
        assert_eq!(ChunkPos::new(0, 0).header_slot(), 0);
        assert_eq!(ChunkPos::new(1, 0).header_slot(), 1);
        assert_eq!(ChunkPos::new(0, 1).header_slot(), 32);
        assert_eq!(ChunkPos::new(31, 31).header_slot(), 1023);
    }
}
