//! Region files: `r.<x>.<z>.mca`, the container 1024 chunks are stored in.
//!
//! # The layout
//!
//! ```text
//! sector 0    1024 x 4 bytes   location: 3-byte sector offset, 1-byte count
//! sector 1    1024 x 4 bytes   timestamp: seconds, big-endian, signed
//! sector 2+   the payloads, each starting on a sector boundary:
//!             4 bytes   length, big-endian, counting the byte that follows
//!             1 byte    compression scheme, high bit set if external
//!             n bytes   the compressed chunk
//!             padding   to the end of the last sector
//! ```
//!
//! A sector is 4096 bytes. The location's count is one byte, so a payload
//! needing 256 sectors or more cannot be described and is moved to a sibling
//! `c.<x>.<z>.mcc` file, leaving a five-byte stub behind with the high bit of
//! the compression byte set. That case is rare and it is not hypothetical —
//! a chunk with a very large amount of block-entity data reaches it — and a
//! reader that has not implemented it either refuses a good chunk or, worse,
//! masks the flag off and hands the stub to a decompressor.
//!
//! # Where the NBT seam is, and why it is here
//!
//! Everything above is arithmetic over bytes. Nothing in it needs to know that
//! a payload is NBT: the header, the sector allocation, the timestamps and the
//! compression are the same whatever the payload turns out to be. So this
//! module stops at [`ChunkPayload`], an opaque run of decompressed bytes, and
//! the crate that parses those bytes into blocks is a layer above.
//!
//! The seam is drawn there rather than a step earlier or later for a reason
//! that is worth stating: it is the last place where the format is still
//! self-describing. The length, the scheme and the sector run are all checkable
//! against each other and against the file's size, so every error this module
//! raises is a contradiction *within the file* — which is why every one of them
//! can name the chunk it is about. One byte further in, the first question is
//! "is this a valid NBT compound", and a wrong answer there says nothing about
//! whether the region file was intact.
//!
//! When the NBT layer lands it adds a function from [`ChunkPayload`] to a
//! chunk. It does not change a call site here, and this module does not gain a
//! dependency.
//!
//! # What these guards do not catch
//!
//! * A payload that is intact, decompresses, and is not a chunk. Nothing here
//!   looks inside one.
//! * A header that is internally consistent and describes the wrong chunks —
//!   a region file whose 1024 entries were all shifted by one slot passes every
//!   check in this module and puts every chunk in the wrong place.
//! * Bit rot inside a payload that still decompresses. Deflate has a checksum
//!   and gzip has a stronger one, so this is unlikely rather than impossible,
//!   and scheme 3 has none at all.

pub mod allocator;
pub mod compression;
pub mod error;
pub mod header;
pub mod store;

use std::path::Path;

pub use allocator::SectorAllocator;
pub use compression::{Compression, UnsupportedScheme, EXTERNAL_FLAG};
pub use error::RegionError;
pub use header::{Header, Location, HEADER_BYTES, MAX_SECTORS, SECTOR_BYTES, SLOTS};
pub use store::{FileStore, MemoryStore, RegionStore};

use crate::coords::{ChunkPos, RegionPos};

/// One chunk's decompressed bytes, before anything has read them.
///
/// This is the boundary type. A region file yields these and takes these back;
/// what they contain is a gzip-free, sector-free run of NBT that this crate
/// does not parse. Keeping it a named type rather than a bare `Vec<u8>` is not
/// ceremony: it is the thing that lets the NBT layer arrive later as a
/// `TryFrom<ChunkPayload>` without a single call site here changing, and it
/// stops a caller passing the *compressed* bytes to something expecting the
/// decompressed ones, which is a mistake a `Vec<u8>` parameter invites.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkPayload {
    bytes: Vec<u8>,
}

impl ChunkPayload {
    /// Wrap the decompressed bytes an [`NbtWriter`](crate::chunk::NbtWriter)
    /// produced.
    #[must_use]
    pub const fn from_bytes(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }

    /// The decompressed payload, as compression wants it.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The decompressed payload, taken.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// How many bytes the payload holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Whether the payload is empty, which no real chunk is.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

/// A chunk's payload as it sits in the file: still compressed.
///
/// Returned by [`RegionFile::read_chunk_raw`], which exists for the jobs that
/// move a chunk without looking at it — copying a region file, streaming a
/// chunk to a client that will decompress it anyway — where decompressing and
/// recompressing is both slower and lossy in file size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawChunk {
    /// The scheme byte, which decides how the payload is decoded.
    pub compression: Compression,
    /// Whether the bytes came from a `.mcc` file rather than from the region.
    pub external: bool,
    /// The compressed bytes exactly as stored.
    pub data: Vec<u8>,
}

/// One region file, open.
#[derive(Debug)]
pub struct RegionFile<S: RegionStore> {
    store: S,
    region: RegionPos,
    header: Header,
    allocator: SectorAllocator,
}

impl RegionFile<FileStore> {
    /// Open `r.<x>.<z>.mca` in a `region` directory, creating it if absent.
    pub fn open_in(directory: impl AsRef<Path>, region: RegionPos) -> Result<Self, RegionError> {
        let store = FileStore::open(directory, region).map_err(|source| RegionError::Io {
            region,
            doing: "opening the file",
            source,
        })?;
        Self::open(store, region)
    }
}

impl<S: RegionStore> RegionFile<S> {
    /// Read and validate a region file's header.
    ///
    /// Strict: the first structural problem is returned and the file is not
    /// opened. Vanilla is lenient here — it logs a warning, zeroes the offending
    /// entry and carries on, which silently deletes a chunk that may have been
    /// recoverable. Dust refuses, so that the decision to discard a chunk is
    /// made by a person with a backup. [`RegionFile::open_dropping_damage`] is
    /// the lenient path for the repair tool that will want it.
    pub fn open(store: S, region: RegionPos) -> Result<Self, RegionError> {
        let (file, damage) = Self::open_dropping_damage(store, region)?;
        match damage.into_iter().next() {
            Some(first) => Err(first),
            None => Ok(file),
        }
    }

    /// Open a region file, dropping the entries that are damaged and returning
    /// what was wrong with each.
    ///
    /// An I/O failure is still an error: it is a statement about the machine
    /// rather than about the file, and continuing past one would report damage
    /// that is not there.
    pub fn open_dropping_damage(
        mut store: S,
        region: RegionPos,
    ) -> Result<(Self, Vec<RegionError>), RegionError> {
        let length = store.length().map_err(|source| RegionError::Io {
            region,
            doing: "measuring the file",
            source,
        })?;

        if length == 0 {
            // A file of no bytes is a region with no chunks. It becomes 8192
            // bytes the first time something is written to it, not now: opening
            // a file must not modify it.
            return Ok((
                Self {
                    store,
                    region,
                    header: Header::empty(),
                    allocator: SectorAllocator::new(0),
                },
                Vec::new(),
            ));
        }
        if length < HEADER_BYTES as u64 {
            return Err(RegionError::HeaderTruncated { region, length });
        }

        let mut bytes = vec![0u8; HEADER_BYTES];
        store
            .read_at(0, &mut bytes)
            .map_err(|source| RegionError::Io {
                region,
                doing: "reading the header",
                source,
            })?;
        let mut header = Header::decode(&bytes);

        // Sectors the file's bytes reach into, rounded *up*.
        //
        // Rounded down would be the tidier rule and it is wrong against the
        // only writer that matters: **Minecraft does not pad the last chunk it
        // writes out to a sector boundary.** Every region file in a world
        // vanilla generated ends mid-sector, with the final chunk's stream
        // complete and the padding after it simply absent — measured on ten of
        // them, where the bytes present were the declared length plus its
        // four-byte prefix every single time.
        //
        // Rounding down made the last chunk of every such file `ChunkPastEnd`,
        // and `open` refuses a file with any damage in it, so **one unpadded
        // tail discarded all 1,024 chunks of the region** and the server served
        // its flat fallback there instead. No test caught it because Dust's own
        // writer pads and every test round-tripped Dust to Dust: a differential
        // cannot catch a rule that is wrong on both sides.
        //
        // What is given up by rounding up is caught a layer in rather than
        // lost: [`RegionFile::read_chunk_raw`] reads only the bytes the file
        // holds and compares the declared stream length against those, so a
        // file that really was cut through a chunk still fails, and fails with
        // the byte counts rather than the sector counts.
        let file_sectors = length.div_ceil(SECTOR_BYTES as u64);
        let mut allocator = SectorAllocator::new(file_sectors);
        let mut owners: Vec<Option<ChunkPos>> = vec![None; file_sectors as usize];
        let mut damage = Vec::new();

        for slot in 0..SLOTS {
            let location = header.location_at_slot(slot);
            if location.is_absent() {
                continue;
            }
            let chunk = region.chunk_at_slot(slot);

            let problem = Self::inspect(chunk, location, file_sectors, &mut allocator, &mut owners);
            match problem {
                None => {
                    for sector in
                        location.first_sector..location.first_sector + location.sector_count
                    {
                        if let Some(slot) = owners.get_mut(sector as usize) {
                            *slot = Some(chunk);
                        }
                    }
                }
                Some(problem) => {
                    header.set_location(chunk, Location::default());
                    header.set_timestamp(chunk, 0);
                    damage.push(problem);
                }
            }
        }

        Ok((
            Self {
                store,
                region,
                header,
                allocator,
            },
            damage,
        ))
    }

    /// Everything wrong with one location entry, or `None`.
    ///
    /// Claims the sectors as a side effect when the entry is sound, because the
    /// overlap check *is* the claim: the second chunk to ask for a sector is
    /// the one that finds it taken.
    fn inspect(
        chunk: ChunkPos,
        location: Location,
        file_sectors: u64,
        allocator: &mut SectorAllocator,
        owners: &mut [Option<ChunkPos>],
    ) -> Option<RegionError> {
        if location.first_sector < header::FIRST_DATA_SECTOR {
            return Some(RegionError::SectorInHeader {
                chunk,
                first_sector: location.first_sector,
            });
        }
        if location.sector_count == 0 {
            return Some(RegionError::EmptySectorRun {
                chunk,
                first_sector: location.first_sector,
            });
        }
        if location.end_sector() > file_sectors {
            return Some(RegionError::ChunkPastEnd {
                chunk,
                first_sector: location.first_sector,
                sector_count: location.sector_count,
                file_sectors,
            });
        }
        if let Err(taken) = allocator.claim(location.first_sector, location.sector_count) {
            return Some(RegionError::OverlappingChunks {
                chunk,
                other: owners
                    .get(taken.sector as usize)
                    .copied()
                    .flatten()
                    .unwrap_or(chunk),
                sector: taken.sector,
            });
        }
        None
    }

    /// Which region file this is.
    #[must_use]
    pub const fn region(&self) -> RegionPos {
        self.region
    }

    /// The location and timestamp tables, as they currently sit.
    #[must_use]
    pub const fn header(&self) -> &Header {
        &self.header
    }

    /// The sector bookkeeping, as it currently sits.
    #[must_use]
    pub const fn allocator(&self) -> &SectorAllocator {
        &self.allocator
    }

    /// Whether this file holds a chunk at that position.
    #[must_use]
    pub fn contains(&self, pos: ChunkPos) -> bool {
        self.region.contains(pos) && !self.header.location(pos).is_absent()
    }

    /// Every chunk in this file, in header-slot order.
    pub fn chunk_positions(&self) -> impl Iterator<Item = ChunkPos> + '_ {
        self.region
            .chunks()
            .filter(|pos| !self.header.location(*pos).is_absent())
    }

    /// How many chunks this file holds.
    #[must_use]
    pub fn chunk_count(&self) -> usize {
        self.chunk_positions().count()
    }

    /// When a chunk was last written, or `None` if it is not here.
    #[must_use]
    pub fn timestamp(&self, pos: ChunkPos) -> Option<i32> {
        self.contains(pos).then(|| self.header.timestamp(pos))
    }

    /// A chunk's decompressed payload, or `None` if the file does not hold it.
    pub fn read_chunk(&mut self, pos: ChunkPos) -> Result<Option<ChunkPayload>, RegionError> {
        let Some(raw) = self.read_chunk_raw(pos)? else {
            return Ok(None);
        };
        let bytes =
            raw.compression
                .decompress(&raw.data)
                .map_err(|source| RegionError::Decompress {
                    chunk: pos,
                    scheme: raw.compression,
                    source,
                })?;
        Ok(Some(ChunkPayload::from_bytes(bytes)))
    }

    /// A chunk's payload as stored, still compressed.
    pub fn read_chunk_raw(&mut self, pos: ChunkPos) -> Result<Option<RawChunk>, RegionError> {
        self.require_in_region(pos)?;
        let location = self.header.location(pos);
        if location.is_absent() {
            return Ok(None);
        }

        // Only as far as the file goes. The last chunk of a region file
        // Minecraft wrote sits in a sector that was never padded out, so the
        // run this location describes can be a few hundred bytes shorter on
        // disk than it is in sectors, with every byte of the stream present.
        // `read_at` refuses a short read — correctly, it is filling a structure
        // of known size — so the size has to be known here.
        let length = self.store.length().map_err(|source| RegionError::Io {
            region: self.region,
            doing: "measuring the file",
            source,
        })?;
        let at = location.first_sector as u64 * SECTOR_BYTES as u64;
        let run = u64::from(location.sector_count) * SECTOR_BYTES as u64;
        let held = length.saturating_sub(at).min(run) as usize;
        if held < 5 {
            // The file stops inside the five bytes that say how long the stream
            // is, so there is no declared length to compare anything against.
            // That is a short read by any other name, and it is reported as one.
            return Err(RegionError::Io {
                region: self.region,
                doing: "reading a chunk's stream header",
                source: std::io::Error::from(std::io::ErrorKind::UnexpectedEof),
            });
        }
        let mut sectors = vec![0u8; held];
        self.store
            .read_at(at, &mut sectors)
            .map_err(|source| RegionError::Io {
                region: self.region,
                doing: "reading a chunk's sectors",
                source,
            })?;

        let declared = i32::from_be_bytes([sectors[0], sectors[1], sectors[2], sectors[3]]);
        if declared < 0 {
            return Err(RegionError::NegativeStreamLength {
                chunk: pos,
                declared,
            });
        }
        if declared == 0 {
            return Err(RegionError::EmptyStream { chunk: pos });
        }
        let (compression, external) = compression::Compression::from_byte(sectors[4])
            .map_err(|source| RegionError::UnsupportedCompression { chunk: pos, source })?;

        // The declared length counts the compression byte, which is why every
        // comparison from here on is against `declared - 1`. Reading it as the
        // payload length is an off-by-one that truncates the last byte of every
        // chunk in the world, and deflate notices, so it fails loudly — which
        // is the only reason it is not on the list of silent mistakes.
        let inline = declared as u32 - 1;

        if external {
            if inline != 0 {
                return Err(RegionError::ExternalChunkAlsoInline {
                    chunk: pos,
                    inline_bytes: inline,
                });
            }
            let data = self
                .store
                .read_external(pos)
                .map_err(|source| RegionError::Io {
                    region: self.region,
                    doing: "reading an external chunk file",
                    source,
                })?
                .ok_or_else(|| RegionError::ExternalChunkMissing {
                    chunk: pos,
                    file: pos.external_file_name(),
                })?;
            return Ok(Some(RawChunk {
                compression,
                external: true,
                data,
            }));
        }

        let available = (sectors.len() - 5) as u32;
        if inline > available {
            return Err(RegionError::StreamPastSectors {
                chunk: pos,
                declared: inline,
                available,
            });
        }
        Ok(Some(RawChunk {
            compression,
            external: false,
            data: sectors[5..5 + inline as usize].to_vec(),
        }))
    }

    /// Compress a payload and store it.
    pub fn write_chunk(
        &mut self,
        pos: ChunkPos,
        payload: &ChunkPayload,
        compression: Compression,
        timestamp: i32,
    ) -> Result<(), RegionError> {
        let data = compression
            .compress(payload.as_bytes())
            .map_err(|source| RegionError::Io {
                region: self.region,
                doing: "compressing a chunk",
                source,
            })?;
        self.write_chunk_raw(pos, compression, &data, timestamp)
    }

    /// Store a payload that is already compressed.
    ///
    /// # The order of operations
    ///
    /// New sectors are allocated *before* the old ones are freed, and the old
    /// ones are freed *after* the header has been written. Both matter:
    ///
    /// * Freeing first lets the allocator hand back the run the chunk is
    ///   currently stored in, and a chunk that shrank would then be written
    ///   over the bytes being replaced — which is fine right up to the moment
    ///   the write fails halfway.
    /// * Freeing before the header is written leaves a window in which the
    ///   header points at sectors the allocator believes are available, so the
    ///   next chunk written in the same session lands on top of a live one.
    ///
    /// Neither is theoretical; both are what "must not leak sectors" turns into
    /// once the file is also expected to survive being interrupted.
    pub fn write_chunk_raw(
        &mut self,
        pos: ChunkPos,
        compression: Compression,
        data: &[u8],
        timestamp: i32,
    ) -> Result<(), RegionError> {
        self.require_in_region(pos)?;
        let previous = self.header.location(pos);
        let needed = (data.len() + 5).div_ceil(SECTOR_BYTES);
        let external = needed > MAX_SECTORS as usize;

        if external {
            self.store
                .write_external(pos, data)
                .map_err(|source| RegionError::Io {
                    region: self.region,
                    doing: "writing an external chunk file",
                    source,
                })?;
            let first = self.allocator.allocate(1);
            let mut stub = vec![0u8; SECTOR_BYTES];
            stub[..4].copy_from_slice(&1i32.to_be_bytes());
            stub[4] = compression.to_byte() | EXTERNAL_FLAG;
            self.write_sectors(first, &stub)?;
            self.header.set_location(
                pos,
                Location {
                    first_sector: first,
                    sector_count: 1,
                },
            );
        } else {
            let first = self.allocator.allocate(needed as u32);
            let mut buffer = vec![0u8; needed * SECTOR_BYTES];
            buffer[..4].copy_from_slice(&(data.len() as i32 + 1).to_be_bytes());
            buffer[4] = compression.to_byte();
            buffer[5..5 + data.len()].copy_from_slice(data);
            self.write_sectors(first, &buffer)?;
            self.header.set_location(
                pos,
                Location {
                    first_sector: first,
                    sector_count: needed as u32,
                },
            );
        }

        self.header.set_timestamp(pos, timestamp);
        self.write_header()?;

        if !previous.is_absent() {
            self.allocator
                .free(previous.first_sector, previous.sector_count);
        }
        if !external {
            // A chunk that used to be too big and no longer is leaves a `.mcc`
            // behind. Nothing reads it, and it is the size of a chunk, so it
            // stays on disk forever unless it is removed here.
            self.store
                .remove_external(pos)
                .map_err(|source| RegionError::Io {
                    region: self.region,
                    doing: "removing a stale external chunk file",
                    source,
                })?;
        }
        Ok(())
    }

    /// Forget a chunk, freeing its sectors.
    ///
    /// Returns whether there was one.
    pub fn remove_chunk(&mut self, pos: ChunkPos) -> Result<bool, RegionError> {
        self.require_in_region(pos)?;
        let location = self.header.location(pos);
        if location.is_absent() {
            return Ok(false);
        }
        self.header.set_location(pos, Location::default());
        self.header.set_timestamp(pos, 0);
        self.write_header()?;
        self.allocator
            .free(location.first_sector, location.sector_count);
        self.store
            .remove_external(pos)
            .map_err(|source| RegionError::Io {
                region: self.region,
                doing: "removing an external chunk file",
                source,
            })?;
        Ok(true)
    }

    /// The store, for a caller that needs the bytes back.
    #[must_use]
    pub fn into_store(self) -> S {
        self.store
    }

    fn write_header(&mut self) -> Result<(), RegionError> {
        let bytes = self.header.encode();
        self.store
            .write_at(0, &bytes)
            .map_err(|source| RegionError::Io {
                region: self.region,
                doing: "writing the header",
                source,
            })
    }

    fn write_sectors(&mut self, first: u32, data: &[u8]) -> Result<(), RegionError> {
        self.store
            .write_at(first as u64 * SECTOR_BYTES as u64, data)
            .map_err(|source| RegionError::Io {
                region: self.region,
                doing: "writing a chunk",
                source,
            })
    }

    fn require_in_region(&self, pos: ChunkPos) -> Result<(), RegionError> {
        if self.region.contains(pos) {
            Ok(())
        } else {
            Err(RegionError::NotInRegion {
                chunk: pos,
                region: self.region,
            })
        }
    }
}
