//! Writing region files: sector accounting, the external payload path, and the
//! round trip through a real file on disk.
//!
//! **What these tests are worth on their own: not much.** A write followed by a
//! read agrees with itself under any convention, including a wrong one, so
//! nothing here can tell whether Dust's region files are region files. That
//! question is answered in `vanilla_corpus.rs`, by reading files Dust did not
//! write and by handing files Dust did write back to the server that defines
//! the format.
//!
//! What these tests *can* answer is the question that has nothing to do with
//! the format: whether the sector allocator is self-consistent. A file that
//! leaks a sector on every rewrite grows without bound while remaining
//! perfectly readable, so no round-trip against vanilla would ever catch it.

use std::path::{Path, PathBuf};

use dust_world::region::{
    Compression, MemoryStore, RegionFile, RegionStore, MAX_SECTORS, SECTOR_BYTES,
};
use dust_world::{ChunkPayload, ChunkPos, RegionPos};

const REGION: RegionPos = RegionPos::new(-1, 2);

fn chunk(local_x: u32, local_z: u32) -> ChunkPos {
    REGION.chunk_at(local_x, local_z)
}

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

/// The sectors the header accounts for, worked out from the header alone.
///
/// Deliberately independent of the allocator: comparing the allocator against
/// itself would pass however wrong it was.
fn sectors_claimed_by_header<S: RegionStore>(file: &RegionFile<S>) -> usize {
    file.chunk_positions()
        .map(|pos| file.header().location(pos).sector_count as usize)
        .sum::<usize>()
        + 2
}

fn assert_accounts_balance<S: RegionStore>(file: &RegionFile<S>, note: &str) {
    assert_eq!(
        file.allocator().used_sectors(),
        sectors_claimed_by_header(file),
        "{note}: the allocator and the header disagree about how much is in use"
    );
}

#[test]
fn a_payload_survives_every_compression_scheme() {
    for scheme in [Compression::Gzip, Compression::Zlib, Compression::None] {
        let payload = ChunkPayload::from_bytes(incompressible(50_000));
        let mut file = RegionFile::open(MemoryStore::new(), REGION).expect("opens");
        file.write_chunk(chunk(3, 4), &payload, scheme, 42)
            .expect("writes");
        let mut reopened = RegionFile::open(
            MemoryStore::from_bytes(file.into_store().into_bytes()),
            REGION,
        )
        .expect("reopens");
        assert_eq!(
            reopened.read_chunk(chunk(3, 4)).expect("reads"),
            Some(payload),
            "{}",
            scheme.name()
        );
        assert_eq!(reopened.timestamp(chunk(3, 4)), Some(42));
        let raw = reopened
            .read_chunk_raw(chunk(3, 4))
            .expect("reads")
            .expect("present");
        assert_eq!(raw.compression, scheme);
        assert!(!raw.external);
    }
}

#[test]
fn an_empty_region_file_is_a_region_file_with_no_chunks() {
    let mut file = RegionFile::open(MemoryStore::new(), REGION).expect("opens");
    assert_eq!(file.chunk_count(), 0);
    assert_eq!(file.read_chunk(chunk(0, 0)).expect("reads"), None);
    assert_eq!(file.timestamp(chunk(0, 0)), None);
    assert_eq!(
        file.into_store().into_bytes().len(),
        0,
        "opening a file must not write to it"
    );
}

#[test]
fn a_chunk_that_grew_moves_and_gives_its_old_sectors_back() {
    let mut file = RegionFile::open(MemoryStore::new(), REGION).expect("opens");
    let target = chunk(0, 0);
    let neighbour = chunk(1, 0);

    file.write_chunk(
        target,
        &ChunkPayload::from_bytes(incompressible(1000)),
        Compression::None,
        1,
    )
    .expect("writes");
    file.write_chunk(
        neighbour,
        &ChunkPayload::from_bytes(incompressible(1000)),
        Compression::None,
        2,
    )
    .expect("writes");
    let before = file.header().location(target);
    assert_eq!(before.sector_count, 1);
    assert_accounts_balance(&file, "two small chunks");

    // Four sectors' worth. It cannot stay where it is: the neighbour is next.
    let grown = ChunkPayload::from_bytes(incompressible(4 * SECTOR_BYTES - 5));
    file.write_chunk(target, &grown, Compression::None, 3)
        .expect("writes");
    let after = file.header().location(target);
    assert_eq!(after.sector_count, 4);
    assert_ne!(after.first_sector, before.first_sector, "it had to move");
    assert!(
        file.allocator().is_free(before.first_sector, 1),
        "the sector it vacated is back on the free list"
    );
    assert_accounts_balance(&file, "after growing");
    assert_eq!(file.read_chunk(target).expect("reads"), Some(grown));
    assert_eq!(file.timestamp(target), Some(3));
}

#[test]
fn a_chunk_that_shrank_does_not_leak_the_sectors_it_gave_up() {
    // The leak this catches is invisible: the file reads perfectly and grows by
    // three sectors every time a chunk shrinks, forever.
    let mut file = RegionFile::open(MemoryStore::new(), REGION).expect("opens");
    let target = chunk(2, 2);

    file.write_chunk(
        target,
        &ChunkPayload::from_bytes(incompressible(4 * SECTOR_BYTES - 5)),
        Compression::None,
        1,
    )
    .expect("writes");
    assert_eq!(file.header().location(target).sector_count, 4);

    let small = ChunkPayload::from_bytes(incompressible(100));
    file.write_chunk(target, &small, Compression::None, 2)
        .expect("writes");
    assert_eq!(file.header().location(target).sector_count, 1);
    assert_accounts_balance(&file, "after shrinking");
    assert_eq!(
        file.allocator().used_sectors(),
        3,
        "the header's two sectors and one chunk"
    );
    assert_eq!(file.read_chunk(target).expect("reads"), Some(small));
}

#[test]
fn rewriting_the_same_chunk_a_hundred_times_does_not_grow_the_file() {
    // Every size in turn, over and over. A file that grows here is leaking.
    let mut file = RegionFile::open(MemoryStore::new(), REGION).expect("opens");
    let target = chunk(7, 7);
    let sizes = [100usize, 9000, 300, 20_000, 50, 12_000];

    for round in 0..100 {
        let size = sizes[round % sizes.len()];
        let payload = ChunkPayload::from_bytes(incompressible(size));
        file.write_chunk(target, &payload, Compression::None, round as i32)
            .expect("writes");
        assert_eq!(file.read_chunk(target).expect("reads"), Some(payload));
        assert_accounts_balance(&file, &format!("round {round}"));
    }

    let biggest = sizes.iter().max().expect("not empty") + 5;
    let ceiling = 2 + biggest.div_ceil(SECTOR_BYTES);
    assert!(
        file.allocator().sectors() as usize <= ceiling + 4,
        "the file reached {} sectors, and the largest chunk needs {ceiling}",
        file.allocator().sectors()
    );
}

#[test]
fn removing_a_chunk_frees_it_and_the_space_is_reused() {
    let mut file = RegionFile::open(MemoryStore::new(), REGION).expect("opens");
    for slot in 0..4u32 {
        file.write_chunk(
            chunk(slot, 0),
            &ChunkPayload::from_bytes(incompressible(1000)),
            Compression::None,
            slot as i32,
        )
        .expect("writes");
    }
    assert_eq!(file.chunk_count(), 4);
    let vacated = file.header().location(chunk(1, 0)).first_sector;

    assert!(file.remove_chunk(chunk(1, 0)).expect("removes"));
    assert!(!file.remove_chunk(chunk(1, 0)).expect("already gone"));
    assert_eq!(file.chunk_count(), 3);
    assert_eq!(file.timestamp(chunk(1, 0)), None);
    assert_accounts_balance(&file, "after removing");

    file.write_chunk(
        chunk(9, 9),
        &ChunkPayload::from_bytes(incompressible(1000)),
        Compression::None,
        9,
    )
    .expect("writes");
    assert_eq!(
        file.header().location(chunk(9, 9)).first_sector,
        vacated,
        "the freed sector was reused rather than the file being extended"
    );
}

#[test]
fn a_chunk_too_large_for_the_header_moves_to_an_external_file() {
    // A sector count is one byte, so 256 sectors cannot be described. The
    // payload here is uncompressed on purpose: the threshold is about the bytes
    // that reach the file, and driving deflate over a megabyte to prove it
    // would only make the test slow.
    let mut file = RegionFile::open(MemoryStore::new(), REGION).expect("opens");
    let target = chunk(4, 4);
    let huge = ChunkPayload::from_bytes(incompressible(MAX_SECTORS as usize * SECTOR_BYTES + 1));

    file.write_chunk(target, &huge, Compression::None, 77)
        .expect("writes");
    let location = file.header().location(target);
    assert_eq!(
        location.sector_count, 1,
        "only the five-byte stub stays in the region"
    );
    assert_accounts_balance(&file, "with an external chunk");

    let raw = file
        .read_chunk_raw(target)
        .expect("reads")
        .expect("present");
    assert!(raw.external);
    assert_eq!(file.read_chunk(target).expect("reads"), Some(huge.clone()));
    assert_eq!(file.timestamp(target), Some(77));

    // The stub is what vanilla writes: a declared length of one, and the
    // compression byte with the high bit set.
    let store = file.into_store();
    let at = location.first_sector as usize * SECTOR_BYTES;
    let bytes = store.bytes();
    assert_eq!(bytes[at..at + 4], 1u32.to_be_bytes());
    assert_eq!(bytes[at + 4], Compression::None.to_byte() | 0x80);

    // And it reopens from nothing but the bytes and the sibling file.
    let mut reopened = RegionFile::open(store, REGION).expect("reopens");
    assert_eq!(reopened.read_chunk(target).expect("reads"), Some(huge));
}

#[test]
fn a_chunk_that_stops_being_huge_leaves_no_external_file_behind() {
    // A stale .mcc is the size of a chunk, nothing reads it, and it stays
    // forever. Nothing in the format says it is wrong, which is why it needs a
    // test rather than a check.
    let mut file = RegionFile::open(MemoryStore::new(), REGION).expect("opens");
    let target = chunk(4, 4);
    file.write_chunk(
        target,
        &ChunkPayload::from_bytes(incompressible(MAX_SECTORS as usize * SECTOR_BYTES + 1)),
        Compression::None,
        1,
    )
    .expect("writes");

    let small = ChunkPayload::from_bytes(incompressible(500));
    file.write_chunk(target, &small, Compression::None, 2)
        .expect("writes");
    assert_eq!(file.read_chunk(target).expect("reads"), Some(small));

    let mut store = file.into_store();
    assert_eq!(
        store.read_external(target).expect("in memory"),
        None,
        "the external file outlived the chunk that needed it"
    );
}

#[test]
fn every_slot_in_a_region_can_hold_a_chunk_and_read_back() {
    // 1024 chunks, each with a payload that depends on its slot, so a header
    // entry pointing at the wrong sectors is a wrong payload rather than a
    // missing one.
    let mut file = RegionFile::open(MemoryStore::new(), REGION).expect("opens");
    for (slot, pos) in REGION.chunks().enumerate() {
        let payload = ChunkPayload::from_bytes(format!("chunk in slot {slot}").into_bytes());
        file.write_chunk(pos, &payload, Compression::Zlib, slot as i32)
            .expect("writes");
    }
    assert_eq!(file.chunk_count(), 1024);
    assert_accounts_balance(&file, "a full region");

    let mut reopened = RegionFile::open(
        MemoryStore::from_bytes(file.into_store().into_bytes()),
        REGION,
    )
    .expect("reopens");
    assert_eq!(reopened.chunk_count(), 1024);
    for (slot, pos) in REGION.chunks().enumerate() {
        assert_eq!(
            reopened.read_chunk(pos).expect("reads"),
            Some(ChunkPayload::from_bytes(
                format!("chunk in slot {slot}").into_bytes()
            )),
            "{pos}"
        );
        assert_eq!(reopened.timestamp(pos), Some(slot as i32), "{pos}");
    }
}

/// A directory that deletes itself, so a failing test does not leave one.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "dust-world-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a scratch directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_region_file_on_disk_round_trips_through_a_real_file() {
    // The memory store is the same code path with a different set of syscalls,
    // and "the same code path" is exactly the assumption worth checking once.
    let scratch = Scratch::new("disk");
    let target = chunk(6, 6);
    let payload = ChunkPayload::from_bytes(incompressible(30_000));

    {
        let mut file = RegionFile::open_in(scratch.path(), REGION).expect("creates");
        file.write_chunk(target, &payload, Compression::Zlib, 1234)
            .expect("writes");
    }

    let on_disk = scratch.path().join(REGION.file_name());
    assert!(on_disk.is_file(), "r.-1.2.mca was not created");
    let length = std::fs::metadata(&on_disk).expect("stat").len();
    assert_eq!(
        length % SECTOR_BYTES as u64,
        0,
        "a region file is a whole number of sectors"
    );

    let mut reopened = RegionFile::open_in(scratch.path(), REGION).expect("reopens");
    assert_eq!(reopened.read_chunk(target).expect("reads"), Some(payload));
    assert_eq!(reopened.timestamp(target), Some(1234));
}

#[test]
fn an_external_chunk_on_disk_writes_and_removes_its_sibling_file() {
    let scratch = Scratch::new("external");
    let target = chunk(8, 8);
    let sibling = scratch.path().join(target.external_file_name());
    let huge = ChunkPayload::from_bytes(incompressible(MAX_SECTORS as usize * SECTOR_BYTES + 1));

    {
        let mut file = RegionFile::open_in(scratch.path(), REGION).expect("creates");
        file.write_chunk(target, &huge, Compression::None, 1)
            .expect("writes");
    }
    assert!(sibling.is_file(), "{} was not written", sibling.display());
    assert_eq!(
        std::fs::metadata(&sibling).expect("stat").len() as usize,
        huge.len(),
        "the external file holds the payload and no header"
    );
    assert!(
        std::fs::metadata(scratch.path().join(REGION.file_name()))
            .expect("stat")
            .len()
            < 100_000,
        "the region file should hold a stub, not the chunk"
    );

    {
        let mut file = RegionFile::open_in(scratch.path(), REGION).expect("reopens");
        assert_eq!(file.read_chunk(target).expect("reads"), Some(huge));
        file.write_chunk(
            target,
            &ChunkPayload::from_bytes(incompressible(200)),
            Compression::None,
            2,
        )
        .expect("writes");
    }
    assert!(
        !sibling.exists(),
        "{} outlived the chunk that needed it",
        sibling.display()
    );
    assert!(
        !scratch.path().join("c.-24.72.mcc.tmp").exists(),
        "the temporary file used for the atomic write was left behind"
    );
}
