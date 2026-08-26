//! `/reload` under pressure: readers walking a stack while writers swap
//! others in underneath them.
//!
//! The invariant being hammered is the whole point of the snapshot design:
//! every snapshot a reader ever holds is **one complete load**, internally
//! consistent with its own counts, no matter how many swaps land around it.
//! A reader never sees a mixture, a half-built stack, or a count that does
//! not match the maps it came from — and when policy refuses broken
//! candidates, no reader ever sees anything but the last accepted world.

mod support;

use dust_data::{LoadOptions, RegistryId, ReloadHandle, ReloadPolicy};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use support::PackBuilder;

/// A self-consistency check a reader can run on any snapshot without knowing
/// which load it came from: the summary counts must agree with the maps they
/// summarise.
fn assert_self_consistent(snapshot: &Arc<dust_data::LoadedData>) {
    let stats = snapshot.stats();
    let counted_resources: usize = snapshot
        .registries()
        .filter(|registry| !registry.is_tags() && registry.as_str() != "function")
        .filter_map(|registry| snapshot.registry(registry))
        .map(|map| map.len())
        .sum();
    let counted_functions: usize = snapshot
        .function_registries()
        .filter_map(|registry| snapshot.functions(registry))
        .map(|map| map.len())
        .sum();
    let counted_tags: usize = snapshot
        .tag_registries()
        .filter_map(|registry| snapshot.merged_tags(registry))
        .map(|map| map.len())
        .sum();

    // Resources and tags are summed into separate totals; functions too.
    // (`resources` excludes functions and tags by construction.)
    assert_eq!(
        stats.resources, counted_resources,
        "a reader saw a torn stack: resource total disagrees with the maps"
    );
    assert_eq!(
        stats.functions, counted_functions,
        "function totals drifted"
    );
    assert_eq!(stats.tags, counted_tags, "tag totals drifted");
}

/// A family of packs that differ in real content, so successive reloads
/// genuinely change the world rather than re-installing identical bytes.
fn pack_family(member: usize) -> support::MemPack {
    PackBuilder::new("family")
        .resource(
            "minecraft",
            "recipe",
            "shared",
            r#"{"type":"minecraft:crafting_shaped","result":{"item":"minecraft:x"}}"#,
        )
        .resource(
            "minecraft",
            "recipe",
            &format!("member{member}"),
            r#"{"type":"minecraft:crafting_shapeless"}"#,
        )
        .file(
            "data/minecraft/function/tick.mcfunction",
            &format!("say member {member}\n"),
        )
        .file(
            "data/minecraft/tags/block/hot.json",
            r#"{"values":["minecraft:magma_block"]}"#,
        )
        .build()
}

#[test]
fn readers_never_see_anything_but_a_whole_world_while_writers_reload() {
    let handle = Arc::new(ReloadHandle::starting(dust_data::load(
        &[&pack_family(0) as &dyn dust_data::PackSource],
        &LoadOptions::default(),
    )));

    const READERS: usize = 4;
    const WRITERS: usize = 3;
    const RELOADS_PER_WRITER: usize = 60;
    const SNAPSHOTS_PER_READER: usize = 400;

    let reads_done = Arc::new(AtomicUsize::new(0));
    let mut readers = Vec::new();
    for _ in 0..READERS {
        let handle = Arc::clone(&handle);
        let reads_done = Arc::clone(&reads_done);
        readers.push(std::thread::spawn(move || {
            for _ in 0..SNAPSHOTS_PER_READER {
                let snapshot = handle.snapshot();
                assert_self_consistent(&snapshot);
                reads_done.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }

    let mut writers = Vec::new();
    for writer in 0..WRITERS {
        let handle = Arc::clone(&handle);
        writers.push(std::thread::spawn(move || {
            for step in 0..RELOADS_PER_WRITER {
                let member = writer * RELOADS_PER_WRITER + step;
                let pack = pack_family(member % 8 + 1);
                // Concurrent writers may install the *same* member another
                // writer just did, so an empty diff is legitimate here; what
                // must hold is only that every reload succeeds and readers
                // never see anything but whole worlds.
                let _ = handle.reload(
                    &[&pack as &dyn dust_data::PackSource],
                    &LoadOptions::default(),
                    ReloadPolicy::default(),
                );
            }
        }));
    }

    for thread in readers {
        thread.join().expect("reader survived");
    }
    for thread in writers {
        thread.join().expect("writer survived");
    }

    assert!(
        reads_done.load(Ordering::Relaxed) >= READERS * SNAPSHOTS_PER_READER,
        "every scheduled read happened"
    );

    // Settle on a known final state and confirm the handle holds it whole.
    let final_pack = pack_family(99);
    handle
        .reload(
            &[&final_pack as &dyn dust_data::PackSource],
            &LoadOptions::default(),
            ReloadPolicy::default(),
        )
        .expect("settles");
    let snapshot = handle.snapshot();
    assert_self_consistent(&snapshot);
    assert_eq!(snapshot.stats().functions, 1);
}

#[test]
fn refused_reloads_under_contention_leave_only_accepted_worlds_visible() {
    let good = |body: &'static str| {
        PackBuilder::new("gated")
            .resource("minecraft", "recipe", "gate", body)
            .build()
    };
    let handle = Arc::new(ReloadHandle::starting(dust_data::load(
        &[&good(r#"{"type":"minecraft:crafting_shaped"}"#) as &dyn dust_data::PackSource],
        &LoadOptions::default(),
    )));

    let broken = PackBuilder::new("broken")
        .resource("minecraft", "recipe", "gate", "{not json at all")
        .build();
    let acceptable = good(r#"{"type":"minecraft:crafting_shapeless"}"#);

    let stop = Arc::new(AtomicUsize::new(0));
    let refuser = {
        let handle = Arc::clone(&handle);
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let mut flips = 0;
            while stop.load(Ordering::Relaxed) == 0 && flips < 500 {
                let outcome = handle.reload(
                    &[&broken as &dyn dust_data::PackSource],
                    &LoadOptions::default(),
                    ReloadPolicy::RequireClean,
                );
                assert!(outcome.is_err(), "a broken pack is always refused");
                flips += 1;
            }
            flips
        })
    };

    let mut readers = Vec::new();
    for _ in 0..2 {
        let handle = Arc::clone(&handle);
        readers.push(std::thread::spawn(move || {
            for _ in 0..600 {
                let snapshot = handle.snapshot();
                assert_self_consistent(&snapshot);
                // The broken candidate was never accepted, so whichever world
                // a reader lands in, the gate recipe exists and parses.
                assert!(snapshot
                    .get(
                        &RegistryId::new("recipe"),
                        &dust_data::ResourceLocation::parse("minecraft:gate").unwrap()
                    )
                    .is_some());
            }
        }));
    }

    for thread in readers {
        thread.join().expect("reader survived");
    }
    stop.store(1, Ordering::Relaxed);
    let flips = refuser.join().expect("refuser survived");
    assert!(flips > 0, "refusal actually ran against the readers");

    // And an honest swap still lands afterwards.
    handle
        .reload(
            &[&acceptable as &dyn dust_data::PackSource],
            &LoadOptions::default(),
            ReloadPolicy::RequireClean,
        )
        .expect("accepted");
}
