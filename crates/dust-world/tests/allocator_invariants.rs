//! The sector allocator under random schedules: no overlaps, no leaks, no
//! growth it cannot explain -- with shrinking on failure.
//!
//! A round-trip test exercises one path; an allocator's failure modes live in
//! *schedules*. Freeing a run between two other free runs must merge them,
//! rewriting a chunk that shrank must give sectors back, and hundreds of
//! those in a row must leave exactly as much space as the live chunks need --
//! no more, no less. Those are properties of sequences, and this file drives
//! many deterministic ones through both layers that hand out sectors: the
//! bare [`SectorAllocator`], and a whole
//! [`RegionFile`](dust_world::region::RegionFile) taking random writes and
//! removes.
//!
//! **On randomness:** every schedule comes from a fixed-seed xorshift, so a
//! failure replays exactly. And on failure the harness *shrinks*: it searches
//! for a shorter schedule that still violates an invariant, and panics with
//! that minimal form instead of three hundred operations of noise.
//!
//! **What this does not catch:** an allocator consistent with itself and
//! inconsistent with its file. The two layers here share one model of who owns
//! which sector, so a bug that moved both together -- say, a header written
//! with the same wrong arithmetic -- would pass. Only comparing against files
//! someone else wrote settles that, which is the corpus's job.

use dust_world::region::header::FIRST_DATA_SECTOR;
use dust_world::region::{MemoryStore, RegionFile, SectorAllocator};
use dust_world::{ChunkPayload, ChunkPos, Compression, RegionPos};

const REGION: RegionPos = RegionPos::new(2, -3);

fn chunk(local_x: u32, local_z: u32) -> ChunkPos {
    REGION.chunk_at(local_x, local_z)
}

/// The sixteen slots a schedule may write, laid over the region's grid.
const SLOTS: usize = 16;

fn slot_pos(slot: usize) -> ChunkPos {
    chunk((slot % 13) as u32, ((slot / 13) % 13) as u32)
}

fn xorshift(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

// ---------------------------------------------------------------------------
// The shrinker, and its positive control.
// ---------------------------------------------------------------------------

/// Shrink `ops` against `fail`, which returns `Err(reason)` while the
/// violation reproduces.
///
/// Halving passes first -- a violation that happens early needs nothing after
/// it -- then single-operation deletion left to right until nothing more can
/// go. Not delta debugging's full algorithm, but enough that what lands in a
/// panic message names the bug instead of burying it under the whole schedule
/// that happened to find it.
fn shrink<O: Copy>(ops: &[O], fail: &mut dyn FnMut(&[O]) -> Result<(), String>) -> Vec<O> {
    let mut current: Vec<O> = ops.to_vec();

    if fail(&current).is_err() {
        let mut length = current.len();
        while length > 1 {
            length /= 2;
            if fail(&current[..length]).is_err() {
                current.truncate(length);
            } else {
                break;
            }
        }
    }

    let mut index = 0;
    while index < current.len() {
        let mut candidate = current.clone();
        candidate.remove(index);
        if fail(&candidate).is_err() {
            current = candidate;
        } else {
            index += 1;
        }
    }
    current
}

#[test]
fn the_shrinker_reduces_a_known_failure_to_its_core() {
    // The positive control: a checker that trips once two allocations appear
    // must reduce a long noisy schedule down to exactly those two operations.
    // Without this test the shrinker could be a function that returns its
    // input unchanged, and every panic message below would still print.
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Op {
        Alloc,
        Noise,
    }
    let mut schedule: Vec<Op> = (0..40).map(|_| Op::Noise).collect();
    schedule[10] = Op::Alloc;
    schedule[30] = Op::Alloc;

    let mut fail = |ops: &[Op]| -> Result<(), String> {
        let allocs = ops.iter().filter(|o| **o == Op::Alloc).count();
        if allocs >= 2 {
            Err(format!("{allocs} allocations"))
        } else {
            Ok(())
        }
    };
    let shrunk = shrink(&schedule, &mut fail);
    assert_eq!(shrunk.len(), 2, "{shrunk:?}");
    assert!(shrunk.iter().all(|o| *o == Op::Alloc), "{shrunk:?}");
}

// ---------------------------------------------------------------------------
// Layer one: the allocator against a sector-by-sector occupancy model.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum AllocOp {
    /// Take a run of this many sectors.
    Allocate(u32),
    /// Give back the run at this position in the still-live list.
    Release(usize),
}

/// What the schedule does, generated once per seed so the test and its
/// shrinker see the same stream. Only the number of live runs matters to
/// generation, because that decides which releases name anything.
fn alloc_schedule(seed: u64, steps: usize) -> Vec<AllocOp> {
    let mut state = seed | 1;
    let mut ops = Vec::with_capacity(steps);
    let mut live = 0usize;
    for _ in 0..steps {
        if live == 0 || xorshift(&mut state) % 5 < 3 {
            ops.push(AllocOp::Allocate((xorshift(&mut state) % 8) as u32 + 1));
            live += 1;
        } else {
            ops.push(AllocOp::Release((xorshift(&mut state) as usize) % live));
            live -= 1;
        }
    }
    ops
}

/// Run `ops` against a fresh allocator, checking every invariant after every
/// step against an independent model of who owns which sector.
///
/// A `Release` naming a run that does not exist -- possible only in a shrunken
/// subset, where earlier deletions shifted the numbering -- is skipped, the
/// same way freeing already-free sectors through the public API is.
fn execute_alloc_ops(ops: &[AllocOp]) -> Result<(), String> {
    let mut allocator = SectorAllocator::new(2);
    // The independent bookkeeping. Nothing here shares arithmetic with the
    // allocator beyond "sectors are u32".
    let mut live: Vec<(u32, u32)> = Vec::new();
    let mut occupied: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();

    for (step, op) in ops.iter().enumerate() {
        match *op {
            AllocOp::Allocate(count) => {
                let before_free: Vec<(u32, u32)> = allocator.free_runs();
                let before_span = allocator.sectors();
                let first = allocator.allocate(count);

                if first < FIRST_DATA_SECTOR {
                    return Err(format!(
                        "step {step}: allocate({count}) returned sector {first}, which \
                         is inside or before the header"
                    ));
                }
                // Every handed-out sector was free before the call: either it
                // sat inside a run the free list named, or it lay past the old
                // end of the file.
                for sector in first..first + count {
                    let was_named_free = before_free
                        .iter()
                        .any(|(start, len)| sector >= *start && sector < start + len);
                    let extended_file = u64::from(sector) >= before_span;
                    if !was_named_free && !extended_file {
                        return Err(format!(
                            "step {step}: allocate({count}) returned {first}, but sector \
                             {sector} was neither free nor past the old end"
                        ));
                    }
                    if occupied.contains(&sector) {
                        return Err(format!(
                            "step {step}: allocate({count}) returned {first} across live \
                             sector {sector}"
                        ));
                    }
                }
                for slot in first..first + count {
                    occupied.insert(slot);
                }
                live.push((first, count));
            }
            AllocOp::Release(index) => {
                if index >= live.len() {
                    continue;
                }
                let (first, count) = live[index];
                allocator.free(first, count);
                for slot in first..first + count {
                    occupied.remove(&slot);
                }
                live.swap_remove(index);
            }
        }

        // No leak, no double-count: what the allocator calls used is exactly
        // the header plus the union of the live runs.
        let expected_used = 2 + live
            .iter()
            .map(|(_, c)| usize::try_from(*c).expect("sane sector count"))
            .sum::<usize>();
        if allocator.used_sectors() != expected_used {
            return Err(format!(
                "step {step}: {} sectors reported used, header plus {} live-run sectors \
                 is {expected_used}",
                allocator.used_sectors(),
                expected_used - 2
            ));
        }

        // The free list is precisely the complement of the occupancy over the
        // file's span, header sectors included.
        let span = allocator.sectors() as u32;
        let mut expected_free: Vec<(u32, u32)> = Vec::new();
        let mut cursor = 0u32;
        for sector in 0..span {
            let taken = sector < FIRST_DATA_SECTOR || occupied.contains(&sector);
            if taken {
                if sector > cursor {
                    expected_free.push((cursor, sector - cursor));
                }
                cursor = sector + 1;
            }
        }
        if span > cursor {
            expected_free.push((cursor, span - cursor));
        }
        if allocator.free_runs() != expected_free {
            return Err(format!(
                "step {step}: free list {:?} disagrees with the model's {expected_free:?}",
                allocator.free_runs()
            ));
        }

        // And every live run reports itself occupied through the public door.
        for &(first, count) in &live {
            if allocator.is_free(first, count) {
                return Err(format!(
                    "step {step}: live run {first}+{count} reports itself free"
                ));
            }
        }
    }
    Ok(())
}

#[test]
fn random_alloc_free_schedules_never_overlap_never_leak_and_never_touch_the_header() {
    for seed in 0..150u64 {
        let ops = alloc_schedule(seed, 300);
        if let Err(reason) = execute_alloc_ops(&ops) {
            let minimal = shrink(&ops, &mut execute_alloc_ops);
            panic!(
                "seed {seed}: {reason}\nminimal failing schedule ({}/{} ops): {minimal:?}",
                minimal.len(),
                ops.len()
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Layer two: a whole region file under random writes and removes.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum FileOp {
    /// Overwrite this slot's chunk with a new generation of its payload.
    Write(u8),
    /// Forget this slot's chunk.
    Remove(u8),
}

/// Payload bytes keyed by slot and generation, long and odd-shaped enough
/// that a header pointing at the wrong sectors yields wrong bytes rather than
/// plausible ones.
fn payload_for(slot: u8, generation: u32) -> ChunkPayload {
    let len = 400 + usize::from(slot) * 130 + generation as usize % 7 * 900;
    let mut bytes = Vec::with_capacity(len);
    bytes.extend_from_slice(
        &slot
            .to_be_bytes()
            .into_iter()
            .chain(generation.to_be_bytes())
            .collect::<Vec<u8>>(),
    );
    let mut state = 0x1000_0000_0000_u64 | (u64::from(slot) << 24) | (u64::from(generation) << 32);
    while bytes.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        bytes.push(state as u8);
    }
    ChunkPayload::from_bytes(bytes)
}

/// Replay a write/remove schedule against a fresh in-memory region, checking,
/// after every single step, everything worth knowing about the file: the
/// allocator's books balance against what the header describes, every live
/// chunk reads back its exact bytes, and every removed one reads absent.
///
/// An operation naming a slot beyond [`SLOTS`] is skipped, which again can
/// only arise from a shrunk subset.
fn execute_file_ops(ops: &[FileOp]) -> Result<(), String> {
    let mut file = RegionFile::open(MemoryStore::new(), REGION).expect("an empty store");
    let mut generations = [None::<u32>; SLOTS];

    let accounts_balance = |file: &RegionFile<MemoryStore>,
                            generations: &[Option<u32>; SLOTS]|
     -> Result<(), String> {
        // The sectors the header describes for live chunks, counted slot by
        // slot -- a chunk's position comes from its slot, never from anything
        // else.
        let mut claimed = 0usize;
        for (slot, generation) in generations.iter().enumerate() {
            if generation.is_some() {
                claimed += file.header().location(slot_pos(slot)).sector_count as usize;
            }
        }

        if file.allocator().used_sectors() != claimed + 2 {
            return Err(format!(
                "{} sectors used, but the header describes {claimed} plus its own two",
                file.allocator().used_sectors()
            ));
        }
        Ok(())
    };

    let verify_every_slot = |file: &mut RegionFile<MemoryStore>,
                             generations: &[Option<u32>; SLOTS]|
     -> Result<(), String> {
        accounts_balance(file, generations)?;
        for (slot, generation) in generations.iter().enumerate() {
            let pos = slot_pos(slot);
            match generation {
                None => {
                    if let Some(payload) = file
                        .read_chunk(pos)
                        .map_err(|e| format!("slot {slot}: {e}"))?
                    {
                        return Err(format!(
                            "slot {slot} was removed but reads {} bytes",
                            payload.len()
                        ));
                    }
                }
                Some(generation) => {
                    let want = payload_for(slot as u8, *generation);
                    let got = file
                        .read_chunk(pos)
                        .map_err(|e| format!("slot {slot}: {e}"))?;
                    if got.as_ref() != Some(&want) {
                        return Err(format!(
                            "slot {slot}, generation {generation}: payload differs"
                        ));
                    }
                }
            }
        }
        Ok(())
    };

    for (step, op) in ops.iter().enumerate() {
        match *op {
            FileOp::Write(slot) => {
                let slot = usize::from(slot) % SLOTS;
                let generation = generations[slot].unwrap_or(0) + 1;
                file.write_chunk(
                    slot_pos(slot),
                    &payload_for(slot as u8, generation),
                    Compression::None,
                    i32::try_from(generation).unwrap_or(i32::MAX),
                )
                .map_err(|e| format!("step {step}: write failed: {e}"))?;
                generations[slot] = Some(generation);
            }
            FileOp::Remove(slot) => {
                let slot = usize::from(slot) % SLOTS;
                file.remove_chunk(slot_pos(slot))
                    .map_err(|e| format!("step {step}: remove failed: {e}"))?;
                generations[slot] = None;
            }
        }
        verify_every_slot(&mut file, &generations)
            .map_err(|reason| format!("step {step}: {reason}"))?;
    }
    Ok(())
}

#[test]
fn random_write_remove_schedules_keep_a_whole_region_self_consistent() {
    for seed in 0..60u64 {
        let mut state = seed.wrapping_mul(0xda3e_39cb_94b9_5b04) | 1;
        let ops: Vec<FileOp> = (0..90)
            .map(|_| {
                let slot = (xorshift(&mut state) % SLOTS as u64) as u8;
                if xorshift(&mut state) % 4 == 0 {
                    FileOp::Remove(slot)
                } else {
                    FileOp::Write(slot)
                }
            })
            .collect();
        if let Err(reason) = execute_file_ops(&ops) {
            let minimal = shrink(&ops, &mut execute_file_ops);
            panic!(
                "seed {seed}: {reason}\nminimal failing schedule ({}/{} ops): {minimal:?}",
                minimal.len(),
                ops.len()
            );
        }
    }
}
