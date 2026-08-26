//! Parse throughput on a synthetic compound, measured with a stopwatch.
//!
//! # Why this is not criterion
//!
//! A benchmark framework brings statistics and a dozen dependencies to answer
//! "how fast", which for a serialiser is usually the wrong question. The
//! questions worth asking here are comparative and coarse: does a change make
//! chunk parsing twice as slow, does the reader stay linear as documents grow,
//! how does SNBT text compare against binary decode on the same tree. A fixed
//! workload timed with [`std::time::Instant`] answers those in one run of
//! `cargo bench -p dust-nbt` with nothing added to the lockfile.
//!
//! The document is shaped like real data rather than like noise — block
//! entities with ids, position lists, UUID int arrays, light byte arrays —
//! because a codec's speed on `{a:1}` proves little.
//!
//! Run it: `cargo bench -p dust-nbt`.

use std::time::Instant;

use dust_nbt::{read, snbt, write, Compound, List, Tag};

/// A synthetic chunk-ish root: 4 KiB of packed block-state words plus `n`
/// entities each carrying strings, a position list, a UUID and a light array.
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

        let mut light = Vec::with_capacity(2048);
        for _ in 0..2048 {
            light.push((index & 0xff) as i8);
        }
        entity.insert("SkyLight", Tag::ByteArray(light));

        let _ = entity_list.push(Tag::Compound(entity));
    }
    root.insert("entities", Tag::List(entity_list));
    root
}

fn measure(label: &str, rounds: u32, f: impl Fn(u32)) {
    // One untimed round to warm caches and fault the allocator's pages.
    f(0);
    let start = Instant::now();
    f(rounds);
    let elapsed = start.elapsed();
    println!(
        "{label:<24} {rounds:>3} rounds in {elapsed:.3?}  ({:.2} µs/round)",
        elapsed.as_secs_f64() / f64::from(rounds) * 1e6
    );
}

fn main() {
    const ROUNDS: u32 = 100;
    // SNBT printing and parsing run tens of times slower per byte than the
    // binary codec — text always does — so they get fewer rounds.
    const TEXT_ROUNDS: u32 = 10;
    let tag = Tag::Compound(synthetic_document(2_000));
    let bytes = write::to_vec("", &tag).expect("serialises");
    let network_bytes = write::to_vec_network(Some(&tag)).expect("serialises");
    let printed = snbt::to_string(&tag);

    println!(
        "document: {} entities, {} bytes binary, {} bytes SNBT",
        tag.as_compound()
            .and_then(|c| c.get("entities"))
            .and_then(Tag::as_list)
            .map(List::len)
            .unwrap_or_default(),
        bytes.len(),
        printed.len()
    );

    measure("binary write (file)", ROUNDS, |rounds| {
        for _ in 0..rounds {
            drop(write::to_vec("", &tag));
        }
    });
    measure("binary parse (file)", ROUNDS, |rounds| {
        for _ in 0..rounds {
            drop(read::from_bytes_exact(&bytes));
        }
    });
    measure("binary write (network)", ROUNDS, |rounds| {
        for _ in 0..rounds {
            drop(write::to_vec_network(Some(&tag)));
        }
    });
    measure("binary parse (network)", ROUNDS, |rounds| {
        for _ in 0..rounds {
            drop(read::from_bytes_network_with(&network_bytes, dust_nbt::Limits::FILE));
        }
    });
    measure("snbt print", TEXT_ROUNDS, |rounds| {
        for _ in 0..rounds {
            drop(snbt::to_string(&tag));
        }
    });
    measure("snbt parse", TEXT_ROUNDS, |rounds| {
        for _ in 0..rounds {
            drop(snbt::parse(&printed));
        }
    });
}
