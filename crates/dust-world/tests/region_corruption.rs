//! Every way a region file can contradict itself, and the error each produces.
//!
//! The rule these tests enforce is one rule: **a damaged region file produces
//! an error that names the chunk and says what did not add up.** Never a panic,
//! never a zeroed chunk, never a plausible chunk built from the wrong bytes.
//! The reason is what an operator does next. "Region file corrupt" gets a world
//! deleted; "chunk (-30, 65) claims 3 sectors from sector 91, and the file
//! holds 6" gets one chunk restored from a backup, and tells whoever wrote the
//! code exactly which branch fired.
//!
//! Every case is built by writing a *sound* region file and then changing the
//! bytes that make it unsound, so the file is realistic in every other respect
//! and the test cannot pass because something unrelated failed first.
//!
//! Dust is stricter here than vanilla, which logs a warning, zeroes the
//! offending header entry and carries on. That silently discards a chunk that
//! may have been recoverable. `open_dropping_damage` offers vanilla's behaviour
//! to whoever wants it, and the last test in this file is the one that says the
//! good chunks in a damaged file are still reachable.

use dust_world::region::{Compression, MemoryStore, RegionError, RegionFile, SECTOR_BYTES};
use dust_world::{ChunkPayload, ChunkPos, RegionPos};

/// A region with negative coordinates, so that every error message in this file
/// is checked against the case where the arithmetic could have gone wrong.
const REGION: RegionPos = RegionPos::new(-1, 2);

fn chunk(local_x: u32, local_z: u32) -> ChunkPos {
    REGION.chunk_at(local_x, local_z)
}

/// Bytes deflate cannot shrink, so that a payload of a given size occupies a
/// predictable number of sectors.
///
/// A repetitive payload compresses to nothing and lands in one sector however
/// large it is, which would leave the multi-sector cases in this file untested.
fn incompressible(len: usize) -> Vec<u8> {
    let mut state = 0x2545_f491_4f6c_dd1du64;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

/// A sound region file holding two chunks of very different sizes.
fn sound_region() -> Vec<u8> {
    let mut file = RegionFile::open(MemoryStore::new(), REGION).expect("an empty store opens");
    file.write_chunk(
        chunk(0, 0),
        &ChunkPayload::from_bytes(b"the first chunk".repeat(200)),
        Compression::Zlib,
        1_700_000_000,
    )
    .expect("writes");
    file.write_chunk(
        chunk(5, 7),
        &ChunkPayload::from_bytes(incompressible(20_000)),
        Compression::Zlib,
        1_700_000_001,
    )
    .expect("writes");
    file.into_store().into_bytes()
}

/// Overwrite a chunk's location entry.
fn set_location(bytes: &mut [u8], pos: ChunkPos, first_sector: u32, sector_count: u32) {
    let at = pos.header_slot() * 4;
    bytes[at] = (first_sector >> 16) as u8;
    bytes[at + 1] = (first_sector >> 8) as u8;
    bytes[at + 2] = first_sector as u8;
    bytes[at + 3] = sector_count as u8;
}

fn location(bytes: &[u8], pos: ChunkPos) -> (u32, u32) {
    let at = pos.header_slot() * 4;
    (
        u32::from(bytes[at]) << 16 | u32::from(bytes[at + 1]) << 8 | u32::from(bytes[at + 2]),
        u32::from(bytes[at + 3]),
    )
}

/// Open a file that is expected to be refused at the door.
fn open_err(bytes: Vec<u8>) -> RegionError {
    match RegionFile::open(MemoryStore::from_bytes(bytes), REGION) {
        Ok(_) => panic!("a damaged region file was opened without complaint"),
        Err(e) => e,
    }
}

/// Open a file that is sound enough to open, and fail on one chunk.
fn read_err(bytes: Vec<u8>, pos: ChunkPos) -> RegionError {
    let mut file =
        RegionFile::open(MemoryStore::from_bytes(bytes), REGION).expect("the header is sound");
    match file.read_chunk(pos) {
        Ok(payload) => panic!("a damaged chunk read as {payload:?}"),
        Err(e) => e,
    }
}

/// Every error must name the chunk it is about, in the text an operator sees.
fn names(err: &RegionError, pos: ChunkPos) {
    assert_eq!(err.chunk(), Some(pos), "{err}");
    assert!(
        err.to_string().contains(&pos.to_string()),
        "the message does not name {pos}: {err}"
    );
}

#[test]
fn the_sound_file_this_suite_damages_is_actually_sound() {
    // The positive control. Without it every test below could be passing
    // because the file was already broken, and the damage nobody applied.
    let mut file =
        RegionFile::open(MemoryStore::from_bytes(sound_region()), REGION).expect("opens");
    assert_eq!(file.chunk_count(), 2);
    assert_eq!(
        file.read_chunk(chunk(0, 0)).expect("reads"),
        Some(ChunkPayload::from_bytes(b"the first chunk".repeat(200)))
    );
    assert_eq!(file.timestamp(chunk(5, 7)), Some(1_700_000_001));
    assert_eq!(file.read_chunk(chunk(9, 9)).expect("reads"), None);
}

#[test]
fn a_file_too_short_for_its_header_is_refused() {
    let mut bytes = sound_region();
    bytes.truncate(100);
    let err = open_err(bytes);
    assert!(
        matches!(err, RegionError::HeaderTruncated { length: 100, .. }),
        "{err}"
    );
    assert!(err.to_string().contains("region (-1, 2)"), "{err}");
    assert!(err.to_string().contains("8192"), "{err}");
}

#[test]
fn a_file_cut_through_a_chunk_names_the_chunk_it_lost() {
    // Distinct from an offset past the end: the header is untouched and honest,
    // and the file simply stops. This is what a crash during a write leaves.
    let mut bytes = sound_region();
    let target = chunk(5, 7);
    let (first, count) = location(&bytes, target);
    bytes.truncate((first as usize + count as usize - 1) * SECTOR_BYTES);
    let err = open_err(bytes);
    names(&err, target);
    match err {
        RegionError::ChunkPastEnd {
            first_sector,
            sector_count,
            file_sectors,
            ..
        } => {
            assert_eq!(first_sector, first);
            assert_eq!(sector_count, count);
            assert_eq!(file_sectors, u64::from(first + count - 1));
        }
        other => panic!("{other}"),
    }
}

#[test]
fn an_offset_past_the_end_of_the_file_is_refused() {
    let mut bytes = sound_region();
    let target = chunk(0, 0);
    set_location(&mut bytes, target, 9999, 1);
    let err = open_err(bytes);
    names(&err, target);
    assert!(matches!(err, RegionError::ChunkPastEnd { .. }), "{err}");
    assert!(err.to_string().contains("9999"), "{err}");
}

#[test]
fn an_offset_inside_the_header_is_refused() {
    // Sector 1 is the timestamp table. A chunk that claimed it would read four
    // kilobytes of timestamps as a payload, and writing there would destroy
    // every other chunk's location in the file.
    let mut bytes = sound_region();
    let target = chunk(0, 0);
    set_location(&mut bytes, target, 1, 1);
    let err = open_err(bytes);
    names(&err, target);
    assert!(
        matches!(
            err,
            RegionError::SectorInHeader {
                first_sector: 1,
                ..
            }
        ),
        "{err}"
    );
}

#[test]
fn two_chunks_over_the_same_sectors_are_refused_and_both_are_named() {
    let mut bytes = sound_region();
    let small = chunk(0, 0);
    let large = chunk(5, 7);
    let (large_sector, large_count) = location(&bytes, large);
    assert!(
        large_count > 2,
        "the large chunk should span several sectors"
    );
    // Point the small chunk into the middle of the large one's run, so the two
    // ranges overlap without either containing the other. The small chunk sits
    // in an earlier header slot, so it claims its sectors first and the error
    // is raised against the large one.
    set_location(&mut bytes, small, large_sector + 1, 2);

    let err = open_err(bytes);
    names(&err, large);
    match err {
        RegionError::OverlappingChunks { other, sector, .. } => {
            assert_eq!(
                other, small,
                "the message must name the chunk already there"
            );
            assert_eq!(sector, large_sector + 1);
        }
        other => panic!("{other}"),
    }
    let text = err.to_string();
    assert!(text.contains(&small.to_string()), "{text}");
    assert!(text.contains(&large.to_string()), "{text}");
}

#[test]
fn a_sector_count_of_zero_with_a_real_offset_is_refused() {
    // The header says both "this chunk exists" and "it has no room". Vanilla
    // drops the entry; the distinction from an absent chunk matters, because an
    // absent chunk is regenerated and a dropped one is lost.
    let mut bytes = sound_region();
    let target = chunk(0, 0);
    set_location(&mut bytes, target, 2, 0);
    let err = open_err(bytes);
    names(&err, target);
    assert!(
        matches!(
            err,
            RegionError::EmptySectorRun {
                first_sector: 2,
                ..
            }
        ),
        "{err}"
    );
}

#[test]
fn a_declared_length_longer_than_the_sectors_it_was_given_is_refused() {
    let mut bytes = sound_region();
    let target = chunk(0, 0);
    let (first, count) = location(&bytes, target);
    let at = first as usize * SECTOR_BYTES;
    // A length that would need far more room than the entry describes.
    bytes[at..at + 4].copy_from_slice(&(count * SECTOR_BYTES as u32 + 1).to_be_bytes());
    let err = read_err(bytes, target);
    names(&err, target);
    match err {
        RegionError::StreamPastSectors {
            declared,
            available,
            ..
        } => {
            assert_eq!(declared, count * SECTOR_BYTES as u32);
            assert_eq!(available, count * SECTOR_BYTES as u32 - 5);
        }
        other => panic!("{other}"),
    }
}

#[test]
fn a_negative_declared_length_is_refused() {
    // Four bytes read as a signed integer, because that is what vanilla writes
    // and what the file means. Read as unsigned it would be an enormous length
    // and produce a less useful error a long way from the cause.
    let mut bytes = sound_region();
    let target = chunk(0, 0);
    let (first, _) = location(&bytes, target);
    let at = first as usize * SECTOR_BYTES;
    bytes[at..at + 4].copy_from_slice(&(-5i32).to_be_bytes());
    let err = read_err(bytes, target);
    names(&err, target);
    assert!(
        matches!(err, RegionError::NegativeStreamLength { declared: -5, .. }),
        "{err}"
    );
}

#[test]
fn a_chunk_with_sectors_and_no_payload_is_refused() {
    let mut bytes = sound_region();
    let target = chunk(0, 0);
    let (first, _) = location(&bytes, target);
    let at = first as usize * SECTOR_BYTES;
    bytes[at..at + 4].copy_from_slice(&0u32.to_be_bytes());
    let err = read_err(bytes, target);
    names(&err, target);
    assert!(matches!(err, RegionError::EmptyStream { .. }), "{err}");
}

#[test]
fn a_compression_byte_that_is_not_a_scheme_is_refused() {
    let mut bytes = sound_region();
    let target = chunk(0, 0);
    let (first, _) = location(&bytes, target);
    bytes[first as usize * SECTOR_BYTES + 4] = 9;
    let err = read_err(bytes, target);
    names(&err, target);
    match &err {
        RegionError::UnsupportedCompression { source, .. } => {
            assert_eq!(source.byte, 9);
            assert_eq!(source.known_name(), None);
        }
        other => panic!("{other}"),
    }
    assert!(err.to_string().contains("1 is gzip"), "{err}");
}

#[test]
fn a_scheme_minecraft_has_and_dust_does_not_is_named_rather_than_called_corrupt() {
    // 1.20.5 added lz4 as a region compression scheme. A server started with
    // -Dminecraft.regionFileCompressionType=lz4 writes worlds this crate cannot
    // read, and telling the operator their world is corrupt would be a lie that
    // costs them the world.
    let mut bytes = sound_region();
    let target = chunk(0, 0);
    let (first, _) = location(&bytes, target);
    bytes[first as usize * SECTOR_BYTES + 4] = 4;
    let err = read_err(bytes, target);
    names(&err, target);
    assert!(err.to_string().contains("lz4"), "{err}");
    assert!(
        !err.to_string().contains("not a scheme"),
        "lz4 is a scheme: {err}"
    );
}

#[test]
fn a_payload_that_does_not_decompress_is_refused() {
    // The one thing in this suite that is about the payload rather than the
    // container, and it is caught only because deflate carries a checksum.
    let mut bytes = sound_region();
    let target = chunk(0, 0);
    let (first, _) = location(&bytes, target);
    let at = first as usize * SECTOR_BYTES + 5;
    for byte in &mut bytes[at..at + 64] {
        *byte ^= 0xff;
    }
    let err = read_err(bytes, target);
    names(&err, target);
    assert!(matches!(err, RegionError::Decompress { .. }), "{err}");
    assert!(err.to_string().contains("zlib"), "{err}");
}

#[test]
fn an_external_chunk_whose_file_is_missing_is_refused_by_name() {
    // The silent-corruption case if the high bit is ignored: the stub is five
    // bytes of header and nothing else, so a reader that masked the flag off
    // would hand an empty payload to a decompressor and, for scheme 3, succeed.
    let mut bytes = sound_region();
    let target = chunk(0, 0);
    let (first, _) = location(&bytes, target);
    let at = first as usize * SECTOR_BYTES;
    bytes[at..at + 4].copy_from_slice(&1u32.to_be_bytes());
    bytes[at + 4] = Compression::Zlib.to_byte() | 0x80;
    let err = read_err(bytes, target);
    names(&err, target);
    match &err {
        RegionError::ExternalChunkMissing { file, .. } => {
            assert_eq!(file, "c.-32.64.mcc", "named by absolute chunk coordinates");
        }
        other => panic!("{other}"),
    }
}

#[test]
fn an_external_chunk_that_also_carries_inline_bytes_is_refused() {
    // The file says the payload is in two places. Vanilla logs a warning and
    // takes the external one; there is no way to know which is the chunk, and
    // guessing right is worth less than saying so.
    let mut bytes = sound_region();
    let target = chunk(0, 0);
    let (first, _) = location(&bytes, target);
    let at = first as usize * SECTOR_BYTES;
    bytes[at + 4] = Compression::Zlib.to_byte() | 0x80;
    let err = read_err(bytes, target);
    names(&err, target);
    assert!(
        matches!(err, RegionError::ExternalChunkAlsoInline { .. }),
        "{err}"
    );
}

#[test]
fn a_chunk_from_another_region_is_refused_rather_than_wrapped_into_range() {
    // The dangerous alternative is masking the coordinates to 0..32, which
    // silently reads or overwrites a different chunk in this file.
    let mut file =
        RegionFile::open(MemoryStore::from_bytes(sound_region()), REGION).expect("opens");
    let elsewhere = ChunkPos::new(0, 0);
    let err = file.read_chunk(elsewhere).expect_err("not in this region");
    names(&err, elsewhere);
    assert!(err.to_string().contains("region (0, 0)"), "{err}");

    let err = file
        .write_chunk(
            elsewhere,
            &ChunkPayload::from_bytes(vec![1, 2, 3]),
            Compression::Zlib,
            0,
        )
        .expect_err("not in this region");
    names(&err, elsewhere);
}

#[test]
fn the_lenient_open_drops_only_what_is_damaged() {
    // The repair path. One chunk's entry is destroyed; the other must still
    // read, and the caller must be told exactly what was thrown away.
    let mut bytes = sound_region();
    let broken = chunk(0, 0);
    let intact = chunk(5, 7);
    set_location(&mut bytes, broken, 9999, 1);

    let (mut file, damage) =
        RegionFile::open_dropping_damage(MemoryStore::from_bytes(bytes), REGION)
            .expect("the header itself is readable");
    assert_eq!(damage.len(), 1);
    names(&damage[0], broken);

    assert_eq!(file.chunk_count(), 1);
    assert!(!file.contains(broken));
    assert_eq!(
        file.read_chunk(intact).expect("still readable"),
        Some(ChunkPayload::from_bytes(incompressible(20_000)))
    );
    assert_eq!(
        file.read_chunk(broken).expect("absent, not damaged"),
        None,
        "a dropped entry reads as absent"
    );
}

#[test]
fn every_damaged_entry_is_reported_and_not_just_the_first() {
    let mut bytes = sound_region();
    set_location(&mut bytes, chunk(1, 1), 9999, 1);
    set_location(&mut bytes, chunk(2, 2), 1, 1);
    set_location(&mut bytes, chunk(3, 3), 2, 0);

    let (_, damage) =
        RegionFile::open_dropping_damage(MemoryStore::from_bytes(bytes), REGION).expect("readable");
    assert_eq!(damage.len(), 3, "{damage:?}");
    let named: Vec<ChunkPos> = damage.iter().filter_map(RegionError::chunk).collect();
    assert!(named.contains(&chunk(1, 1)), "{named:?}");
    assert!(named.contains(&chunk(2, 2)), "{named:?}");
    assert!(named.contains(&chunk(3, 3)), "{named:?}");
}
