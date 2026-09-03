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

// ---------------------------------------------------------------------------
// The second shelf of mutations. Same rule as above -- a typed error that
// names the chunk and never a panic -- aimed at the cases the first suite
// did not build: offsets at the edges of what three bytes can say, runs that
// grow into their neighbours, tables that disagree with each other, and
// compressed streams damaged in their first bytes.
// ---------------------------------------------------------------------------

/// The location entry of `pos`, as the file currently says it.
#[test]
fn the_largest_offset_three_bytes_can_express_is_still_checked() {
    // The offset field is three bytes; a reader that unpacked it into a u32
    // and trusted it would compute sector 16_777_215 times 4096 and go
    // looking there. The check is against the file's actual length, so the
    // biggest possible lie is refused with the number in it.
    let mut bytes = sound_region();
    let target = chunk(0, 0);
    set_location(&mut bytes, target, 0xff_ff_ff, 255);
    let err = open_err(bytes);
    names(&err, target);
    match err {
        RegionError::ChunkPastEnd {
            first_sector,
            sector_count,
            file_sectors,
            ..
        } => {
            assert_eq!(first_sector, 0xff_ff_ff);
            assert_eq!(sector_count, 255);
            assert!(
                file_sectors < 100,
                "the sound region is a handful of sectors, not {file_sectors}"
            );
        }
        other => panic!("{other}"),
    }
    assert!(err.to_string().contains("16777215"), "{err}");
}

#[test]
fn a_run_grown_by_one_sector_reaches_into_the_next_chunk() {
    // Distinct from pointing a chunk somewhere random: the header is honest
    // about where the run starts and lies only about how long it is, so the
    // damage lands exactly on the boundary sector the neighbour owns. This
    // is what an interrupted resize leaves behind.
    let mut bytes = sound_region();
    let small = chunk(0, 0);
    let large = chunk(5, 7);
    let (large_first, _) = location(&bytes, large);
    let (small_first, small_count) = location(&bytes, small);
    assert_eq!(
        small_first + small_count,
        large_first,
        "the fixture: back to back"
    );

    set_location(&mut bytes, small, small_first, small_count + 1);
    let err = open_err(bytes);
    // The small chunk sits in an earlier slot, so it claims the stolen
    // sector first and the error falls on the large one.
    names(&err, large);
    match err {
        RegionError::OverlappingChunks { other, sector, .. } => {
            assert_eq!(other, small);
            assert_eq!(sector, large_first, "the shared boundary sector is named");
        }
        other => panic!("{other}"),
    }
}

#[test]
fn two_chunks_claiming_the_identical_run_are_refused_and_one_is_recoverable() {
    // The laziest possible double allocation: both entries point at the same
    // place. Strictly this is a refusal; through the lenient door exactly one
    // of the twins is dropped and the survivor still reads, which is the
    // difference between "restore from backup" and "delete one chunk".
    let mut bytes = sound_region();
    let original = chunk(0, 0);
    let twin = chunk(9, 9);
    let (first, count) = location(&bytes, original);
    set_location(&mut bytes, twin, first, count);

    let err = open_err(bytes.clone());
    names(&err, twin);
    match err {
        RegionError::OverlappingChunks { other, sector, .. } => {
            assert_eq!(other, original);
            assert_eq!(sector, first);
        }
        other => panic!("{other}"),
    }

    let (mut file, damage) =
        RegionFile::open_dropping_damage(MemoryStore::from_bytes(bytes), REGION)
            .expect("the header itself is readable");
    assert_eq!(damage.len(), 1);
    names(&damage[0], twin);
    assert_eq!(
        file.chunk_count(),
        2,
        "the twin was dropped, the original kept"
    );
    assert_eq!(
        file.read_chunk(original).expect("reads").map(|p| p.len()),
        Some(b"the first chunk".repeat(200).len())
    );
}

#[test]
fn timestamp_damage_alone_does_not_make_a_file_unsound() {
    // Timestamps are bookkeeping, not structure: nothing in a region file
    // cross-checks them, so filling the table with garbage must open, read
    // and preserve it byte for byte. A reader that validated them would
    // refuse worlds whose clocks were simply wrong.
    let mut bytes = sound_region();
    for byte in &mut bytes[SECTOR_BYTES..2 * SECTOR_BYTES] {
        *byte = 0xaa;
    }
    let mut file = RegionFile::open(MemoryStore::from_bytes(bytes), REGION).expect("opens");
    assert_eq!(file.chunk_count(), 2);
    assert_eq!(
        file.read_chunk(chunk(0, 0)).expect("reads"),
        Some(ChunkPayload::from_bytes(b"the first chunk".repeat(200)))
    );
    let garbage = i32::from_be_bytes([0xaa; 4]);
    assert!(garbage.is_negative());
    assert_eq!(
        file.timestamp(chunk(0, 0)),
        Some(garbage),
        "the value comes back as it is stored, sign and all"
    );

    // And one deliberately impossible-looking stamp survives the same way.
    let mut bytes = sound_region();
    let at = SECTOR_BYTES + chunk(5, 7).header_slot() * 4;
    bytes[at..at + 4].copy_from_slice(&(-1i32).to_be_bytes());
    let file = RegionFile::open(MemoryStore::from_bytes(bytes), REGION).expect("opens");
    assert_eq!(file.timestamp(chunk(5, 7)), Some(-1));
}

#[test]
fn a_timestamp_without_a_location_is_not_a_chunk() {
    // The tables disagreeing in one direction: the timestamp table remembers
    // a write the location table no longer describes. Presence is decided by
    // location alone, so the slot reads as empty and nothing counts it --
    // including the chunk count.
    let mut bytes = sound_region();
    let ghost = chunk(3, 3);
    let at = ghost.header_slot() * 4;
    assert_eq!((bytes[at], bytes[at + 3]), (0, 0), "the slot starts absent");
    let stamp_at = SECTOR_BYTES + at;
    bytes[stamp_at..stamp_at + 4].copy_from_slice(&1_700_000_999i32.to_be_bytes());

    let mut file =
        RegionFile::open(MemoryStore::from_bytes(bytes), REGION).expect("a stamp alone is fine");
    assert_eq!(file.chunk_count(), 2, "no phantom chunk was counted");
    assert!(!file.contains(ghost));
    assert_eq!(file.read_chunk(ghost).expect("absent, not broken"), None);
    assert_eq!(
        file.timestamp(ghost),
        None,
        "a timestamp without a chunk is not reported"
    );
}

#[test]
fn a_location_whose_timestamp_was_lost_is_still_a_chunk() {
    // ...and in the other: a zeroed timestamp does not unmake a chunk whose
    // sectors are described. The two tables are checked independently, which
    // is why damaging either alone costs nothing.
    let mut bytes = sound_region();
    let target = chunk(5, 7);
    let at = SECTOR_BYTES + target.header_slot() * 4;
    bytes[at..at + 4].copy_from_slice(&0i32.to_be_bytes());

    let mut file = RegionFile::open(MemoryStore::from_bytes(bytes), REGION).expect("opens");
    assert_eq!(file.chunk_count(), 2);
    assert_eq!(file.timestamp(target), Some(0));
    assert_eq!(
        file.read_chunk(target).expect("reads"),
        Some(ChunkPayload::from_bytes(incompressible(20_000)))
    );
}

#[test]
fn a_zlib_payload_with_a_flipped_magic_byte_is_refused_by_scheme() {
    // One byte, the first: zlib's header carries its own checksum, so the
    // stream is rejected before a single payload byte is decoded. The error
    // names the scheme because "corrupt" and "this world was written by
    // something else" lead an operator to very different backups.
    let mut bytes = sound_region();
    let target = chunk(0, 0);
    let (first, _) = location(&bytes, target);
    bytes[first as usize * SECTOR_BYTES + 5] ^= 0xff;
    let err = read_err(bytes, target);
    names(&err, target);
    assert!(matches!(err, RegionError::Decompress { .. }), "{err}");
    assert!(err.to_string().contains("zlib"), "{err}");

    // Flipping the same byte back restores the chunk bit for bit, so the
    // mutation is proven to be the whole difference between sound and not.
    let restored = {
        let mut bytes = sound_region();
        bytes[first as usize * SECTOR_BYTES + 5] ^= 0xff;
        bytes[first as usize * SECTOR_BYTES + 5] ^= 0xff;
        bytes
    };
    let mut file = RegionFile::open(MemoryStore::from_bytes(restored), REGION).expect("opens");
    assert_eq!(
        file.read_chunk(target).expect("reads"),
        Some(ChunkPayload::from_bytes(b"the first chunk".repeat(200)))
    );
}

#[test]
fn a_gzip_payload_with_a_flipped_magic_byte_is_refused_by_scheme() {
    // gzip's magic is 0x1f 0x8b, and the decoder checks it before anything
    // else -- so this is the cheapest corruption to detect and the most
    // important not to mislabel. The region is rebuilt with gzip rather than
    // mutated, because zlib is what the rest of the suite writes.
    let gzip_region = || -> Vec<u8> {
        let mut file = RegionFile::open(MemoryStore::new(), REGION).expect("an empty store");
        file.write_chunk(
            chunk(0, 0),
            &ChunkPayload::from_bytes(incompressible(3_000)),
            Compression::Gzip,
            7,
        )
        .expect("writes");
        file.into_store().into_bytes()
    };

    let mut bytes = gzip_region();
    let (first, _) = location(&bytes, chunk(0, 0));
    bytes[first as usize * SECTOR_BYTES + 5] ^= 0xff;
    let err = read_err(bytes, chunk(0, 0));
    names(&err, chunk(0, 0));
    assert!(matches!(err, RegionError::Decompress { .. }), "{err}");
    assert!(err.to_string().contains("gzip"), "{err}");
    assert!(!err.to_string().contains("zlib"), "{err}");

    let intact = gzip_region();
    let mut file = RegionFile::open(MemoryStore::from_bytes(intact), REGION).expect("opens");
    let payload = file
        .read_chunk(chunk(0, 0))
        .expect("reads")
        .expect("present");
    assert_eq!(payload.len(), 3_000);
}

// ---------------------------------------------------------------------------
// The tail Minecraft leaves, which is the case in this file that is *not*
// damage. Vanilla writes a chunk's four-byte length, its compression byte and
// its stream, and then stops: the last chunk of a region file it wrote sits in
// a sector that was never padded out, so the file ends mid-sector with every
// byte of the stream present. Measured on ten region files from two worlds the
// harness generated — in all ten, the bytes after the last chunk's offset were
// its declared length plus the four-byte prefix, exactly.
//
// Dust read those files as damaged and, because `open` refuses a file with any
// damage in it, threw away all 1,024 chunks of the region and served its flat
// fallback there instead. Nothing caught it: Dust's own writer pads, and every
// test in this suite round-trips Dust to Dust.
// ---------------------------------------------------------------------------

/// Where the last chunk of a file starts, and how many bytes its stream
/// declares — read out of the file rather than assumed, so this follows the
/// writer wherever it puts things.
fn last_stream(bytes: &[u8]) -> (usize, usize) {
    let mut last = (0u32, 0u32);
    for slot in 0..1024 {
        let at = slot * 4;
        let first =
            u32::from(bytes[at]) << 16 | u32::from(bytes[at + 1]) << 8 | u32::from(bytes[at + 2]);
        let count = u32::from(bytes[at + 3]);
        if first != 0 && first + count > last.0 + last.1 {
            last = (first, count);
        }
    }
    let at = last.0 as usize * SECTOR_BYTES;
    let declared =
        u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize;
    (at, declared)
}

#[test]
fn a_file_that_stops_where_minecraft_stops_writing_keeps_every_chunk() {
    let mut bytes = sound_region();
    let (at, declared) = last_stream(&bytes);
    bytes.truncate(at + 4 + declared);
    assert_ne!(
        bytes.len() % SECTOR_BYTES,
        0,
        "the point is an unpadded tail"
    );

    let mut file = RegionFile::open(MemoryStore::from_bytes(bytes), REGION).expect("opens");
    assert_eq!(file.chunk_count(), 2);
    assert_eq!(
        file.read_chunk(chunk(0, 0)).expect("reads"),
        Some(ChunkPayload::from_bytes(b"the first chunk".repeat(200)))
    );
    assert_eq!(
        file.read_chunk(chunk(5, 7)).expect("reads"),
        Some(ChunkPayload::from_bytes(incompressible(20_000)))
    );
}

#[test]
fn one_byte_less_than_minecraft_would_have_written_is_still_refused() {
    // The negative control for the test above, and the reason rounding up is
    // not the same as trusting the file: a stream cut one byte short is caught
    // against the bytes that are there, and the count in the message is a byte
    // count rather than a sector count.
    let mut bytes = sound_region();
    let (at, declared) = last_stream(&bytes);
    bytes.truncate(at + 4 + declared - 1);
    let target = chunk(5, 7);
    let err = read_err(bytes, target);
    names(&err, target);
    match err {
        RegionError::StreamPastSectors {
            declared: said,
            available,
            ..
        } => {
            assert_eq!(
                said as usize,
                declared - 1,
                "the stream's own length, less its compression byte"
            );
            assert_eq!(available as usize, declared - 2, "one byte short of it");
        }
        other => panic!("{other}"),
    }
}
