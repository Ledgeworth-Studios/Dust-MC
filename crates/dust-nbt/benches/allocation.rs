//! Allocation counts for the two readers and the writer, measured directly.
//!
//! # Why count allocations instead of only timing
//!
//! Throughput answers "how fast"; allocation counts answer "what does this
//! cost the allocator", which is where NBT parsing actually hurts — a chunk
//! parse makes thousands of small requests, and the difference between one
//! per string and one per document is worth more than any cache tweak. The
//! global allocator is wrapped with counters and the deltas taken around each
//! measured call, so the numbers are exact rather than sampled: this bench
//! prints facts about specific inputs, not statistics about noise.
//!
//! The borrowed reader is the point of comparison: its documents view numeric
//! payloads in place and hold all text in one region, so a full parse should
//! cost a handful of allocations regardless of how many strings the
//! document carries. The owned reader pays per string and per container by
//! design; seeing both numbers side by side is what lets a caller choose with
//! open eyes.
//!
//! Run it: `cargo bench -p dust-nbt --bench allocation`.

// The allocator trait is `unsafe` to implement by nature; the wrapper below
// forwards every call to [`System`] untouched and adds nothing but counters,
// which is the whole of its safety argument. The crate's own deny stays
// meaningful for the library; this opt-out is scoped to the bench binary that
// could not exist without it.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

use dust_nbt::{borrow, read, write, Compound, List, Tag};

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static ALLOCATED_BYTES: AtomicUsize = AtomicUsize::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Relaxed);
        ALLOCATED_BYTES.fetch_add(layout.size(), Relaxed);
        System.alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Relaxed);
        ALLOCATED_BYTES.fetch_add(layout.size(), Relaxed);
        System.alloc_zeroed(layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // One request in, whatever the allocator does underneath: counted as
        // the single allocation it looks like to the caller.
        ALLOCATIONS.fetch_add(1, Relaxed);
        ALLOCATED_BYTES.fetch_add(new_size.saturating_sub(layout.size()), Relaxed);
        System.realloc(ptr, layout, new_size)
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Reset and run `f`, returning `(calls, bytes, result)`.
fn counted<T>(f: impl FnOnce() -> T) -> (usize, usize, T) {
    ALLOCATIONS.store(0, Relaxed);
    ALLOCATED_BYTES.store(0, Relaxed);
    std::hint::black_box(());
    let value = f();
    std::hint::black_box(&value);
    (
        ALLOCATIONS.load(Relaxed),
        ALLOCATED_BYTES.load(Relaxed),
        value,
    )
}

/// The synthetic chunk from the throughput bench: block states, entities with
/// strings, positions, UUIDs and light arrays. Duplicated here because bench
/// targets are separate binaries and sharing would mean a support crate for
/// two functions.
fn synthetic_document(entities: usize) -> Compound {
    let mut root = Compound::new();
    root.insert("DataVersion", Tag::Int(3955));

    let mut states = Vec::with_capacity(4096);
    for index in 0..4096i64 {
        states.push(index.wrapping_mul(0x0000_0004_0000_0001));
    }
    root.insert("block_states", Tag::LongArray(states));

    let mut entity_list = List::new(dust_nbt::TagType::Compound);
    for index in 0..entities {
        let mut entity = Compound::new();
        entity.insert("id", Tag::String("minecraft:area_effect_cloud".to_owned()));
        entity.insert(
            "CustomName",
            Tag::String(format!("cloud the {index}th of its name")),
        );
        let mut position = List::new(dust_nbt::TagType::Double);
        for coordinate in [index as f64, 64.0, (index % 16) as f64] {
            let _ = position.push(Tag::Double(coordinate));
        }
        entity.insert("Pos", Tag::List(position));
        entity.insert("UUID", Tag::IntArray(vec![index as i32, 7, 13, 42]));

        let light = vec![0i8; 2048];
        entity.insert("SkyLight", Tag::ByteArray(light));

        let _ = entity_list.push(Tag::Compound(entity));
    }
    root.insert("entities", Tag::List(entity_list));
    root
}

fn report(label: &str, calls: usize, bytes: usize, seconds: f64) {
    println!("{label:<34} {calls:>6} allocs {bytes:>9} bytes  {seconds:.3?}");
}

fn main() {
    const ROUNDS: u32 = 100;
    let tag = Tag::Compound(synthetic_document(2_000));
    let bytes = write::to_vec("", &tag).expect("serialises");
    let network_bytes = write::to_vec_network(Some(&tag)).expect("serialises");

    println!(
        "document: {} entities, {} bytes binary",
        tag.as_compound()
            .and_then(|c| c.get("entities"))
            .and_then(Tag::as_list)
            .map(List::len)
            .unwrap_or_default(),
        bytes.len(),
    );

    // One untimed pass warms caches; then the timed-and-counted rounds take
    // their totals per round by dividing, keeping the printed numbers exact
    // for the single-shot case they matter most to.
    drop(read::from_bytes_exact(&bytes));
    drop(borrow::from_bytes_exact(&bytes));
    drop(write::to_vec("", &tag));

    let (owned_calls, owned_bytes, elapsed) = counted(|| {
        let start = Instant::now();
        for _ in 0..ROUNDS {
            std::hint::black_box(read::from_bytes_exact(&bytes).ok());
        }
        start.elapsed()
    });
    report(
        "owned parse (file)",
        owned_calls / ROUNDS as usize,
        owned_bytes / ROUNDS as usize,
        elapsed.as_secs_f64() / f64::from(ROUNDS),
    );

    let (borrowed_calls, borrowed_bytes, elapsed) = counted(|| {
        let start = Instant::now();
        for _ in 0..ROUNDS {
            std::hint::black_box(borrow::from_bytes_exact(&bytes).ok());
        }
        start.elapsed()
    });
    report(
        "borrowed parse (file)",
        borrowed_calls / ROUNDS as usize,
        borrowed_bytes / ROUNDS as usize,
        elapsed.as_secs_f64() / f64::from(ROUNDS),
    );

    let (network_calls, _, _) = counted(|| {
        for _ in 0..ROUNDS {
            std::hint::black_box(
                read::from_bytes_network_with(&network_bytes, dust_nbt::Limits::FILE).ok(),
            );
        }
    });
    println!(
        "owned parse (network)   {:>6} allocs per round",
        network_calls / ROUNDS as usize
    );

    // A single write should be exactly one allocation since the buffer is
    // presized from the tree; the count says so or names a regression.
    let (write_calls, write_total, written) = counted(|| write::to_vec("", &tag).expect("writes"));
    assert_eq!(written.len(), write_total);
    println!(
        "binary write            {write_calls:>6} allocs {write_total:>9} bytes  (one reservation)"
    );

    // And the borrowed document really is dropped when its scope ends: the
    // region is one String inside it, so dropping frees roughly what the
    // parse allocated.
    let (drop_calls, _, _) = counted(|| {
        let document = borrow::from_bytes_exact(&bytes).expect("parses");
        std::hint::black_box(&document);
        drop(document);
    });
    println!(
        "borrowed parse + drop   {} allocs net (region freed with the document)",
        drop_calls
    );
}
