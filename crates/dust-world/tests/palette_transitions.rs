//! The palette transition property suite: every storage-format boundary a
//! container can cross, crossed.
//!
//! The off-by-one in this crate's history lives where a palette changes its
//! width. A container that promoted one entry too early or one too late
//! writes a section whose packed indices are a different width than the
//! reader expects, and the reader does not fail — it decodes plausible blocks
//! from the wrong slots. So this file drives containers across every boundary
//! as entries are inserted, and holds four things still:
//!
//! * **The schedule.** Single-valued, then indirect at 4 bits, then indirect
//!   through 5, 6, 7 and 8 bits, then direct over the whole registry — with
//!   the minimum and maximum bits per entry of each format pinned exactly,
//!   including the four-bit floor that makes a two-value section cost as much
//!   as a sixteen-value one.
//! * **The fallback.** When the palette list would outgrow the hashed tier it
//!   becomes the global palette, whose width is `ceil_log2(registry)`, and no
//!   further growth is possible or needed.
//! * **Growth and shrink.** A container promotes eagerly and only when an
//!   insert does not fit, never demotes in memory, and shrinks *only* at
//!   serialisation, where vanilla re-palettes so a file names just the values
//!   still present.
//! * **Remapping.** Compaction renumbers: entries come out in first-appearance
//!   order over the cells, not the order history happened to insert them, so
//!   two containers holding identical contents serialise identically however
//!   differently they were built. This is also where hash-map iteration would
//!   leak into saved bytes if the writer were careless; the ordering tests
//!   here are what say it cannot.
//!
//! Everything is driven by fixed seeds through xorshift, because a property
//! test that fails on one seed in fifty is a test nobody trusts.

use dust_world::bits::long_count;
use dust_world::palette::{ceil_log2, PaletteKind};
use dust_world::{BitStorage, ContainerError, PalettedContainer, Strategy};

/// The number of block states on 1.21.1, written down rather than imported;
/// see the same constant in `container.rs`.
const REGISTRY: u32 = 26_684;

fn xorshift(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

/// The tier a block-state container must be in while holding `distinct`
/// values, straight from `Strategy.SECTION_STATES`.
///
/// Written as a table rather than derived, because a formula shared with the
/// implementation would be the mistake checked by itself.
fn expected_tier(distinct: u32) -> (PaletteKind, u32) {
    match distinct {
        0 | 1 => (PaletteKind::Single, 0),
        2..=16 => (PaletteKind::Linear, 4),
        17..=32 => (PaletteKind::Hashed, 5),
        33..=64 => (PaletteKind::Hashed, 6),
        65..=128 => (PaletteKind::Hashed, 7),
        129..=256 => (PaletteKind::Hashed, 8),
        _ => (PaletteKind::Global, ceil_log2(REGISTRY)),
    }
}

#[test]
fn inserting_entries_walks_every_boundary_in_order_and_never_skips_one() {
    let mut container = PalettedContainer::filled(Strategy::BLOCK_STATES, REGISTRY, 0);
    let mut promotions: Vec<(u32, PaletteKind, u32)> = Vec::new();
    let mut tier = expected_tier(1);

    for distinct in 1..=300u32 {
        // Value n goes into cell n - 1; value 0 was already there from the
        // fill. Every distinct count is visited, so every boundary is.
        container.set(distinct as usize - 1, distinct - 1);

        let want = expected_tier(distinct);
        assert_eq!(
            container.palette_kind(),
            want.0,
            "{distinct} distinct block states"
        );
        assert_eq!(
            container.storage().bits(),
            want.1,
            "{distinct} distinct block states: bits per entry"
        );
        assert_eq!(
            container.storage().as_longs().len(),
            long_count(4096, want.1),
            "{distinct} distinct block states: longs for the width"
        );
        assert!(container.storage().padding_is_zero());

        if want != tier {
            promotions.push((distinct, want.0, want.1));
            tier = want;
        }
    }

    assert_eq!(
        promotions,
        vec![
            (2, PaletteKind::Linear, 4),
            (17, PaletteKind::Hashed, 5),
            (33, PaletteKind::Hashed, 6),
            (65, PaletteKind::Hashed, 7),
            (129, PaletteKind::Hashed, 8),
            (257, PaletteKind::Global, 15),
        ],
        "the boundaries are where the bits run out, to the entry"
    );
}

#[test]
fn each_format_holds_its_minimum_and_maximum_bits_per_entry_across_its_whole_band() {
    // Not just at the edges: a width that wobbled anywhere inside a band
    // would produce files a vanilla server reads at the wrong width. The
    // bands are swept end to end and the width must be flat throughout.
    let mut container = PalettedContainer::filled(Strategy::BLOCK_STATES, REGISTRY, 0);
    for distinct in 1..=300u32 {
        container.set(distinct as usize - 1, distinct - 1);
        let want = expected_tier(distinct);
        assert_eq!(container.storage().bits(), want.1, "{distinct} distinct");
    }

    // The floor: a two-entry section pays four bits per entry, because the
    // format says indirect block data starts at four -- even though one bit
    // would have been enough for the two of them.
    let mut sparse = PalettedContainer::filled(Strategy::BLOCK_STATES, REGISTRY, 0);
    sparse.set(0, 1);
    assert_eq!(
        sparse.palette_kind(),
        PaletteKind::Linear,
        "two values are indirect"
    );
    assert_eq!(
        sparse.storage().bits(),
        4,
        "and indirect starts at the floor"
    );
    assert_eq!(
        sparse.storage().as_longs().len(),
        long_count(4096, 4),
        "256 longs, not the 128 a three-bit width would give"
    );

    // Rewriting everything back to one value leaves the tier alone in memory
    // and collapses only at serialisation.
    for cell in 0..4096usize {
        sparse.set(cell, 0);
    }
    assert_eq!(
        sparse.palette_kind(),
        PaletteKind::Linear,
        "no demotion in memory"
    );
    assert_eq!(sparse.to_parts().0, vec![0]);
    assert_eq!(sparse.to_parts().1, None, "but it writes as single-valued");

    // The direct format's width is the registry's, not "as many as needed":
    // 257 distinct values are already global at fifteen bits even though nine
    // bits could name them all.
    let mut full = PalettedContainer::filled(Strategy::BLOCK_STATES, REGISTRY, 0);
    for cell in 0..257usize {
        full.set(cell, cell as u32);
    }
    assert_eq!(full.palette_kind(), PaletteKind::Global);
    assert_eq!(full.storage().bits(), 15);
    assert_eq!(full.palette().len(), REGISTRY as usize);
}

#[test]
fn scrambled_insertion_orders_reach_the_same_tiers_and_keep_every_cell() {
    // The ladder above inserts values in ascending order into consecutive
    // cells. Here the same three hundred values land in cells in a
    // pseudo-random *permutation* -- every value keeps a cell of its own, so
    // the number present is exact and the tier schedule must fire at the very
    // same counts as it did in order. What differs is the order the packed
    // array's cells were written in, which is what a re-indexing bug would
    // trip over.
    for seed in 0..5u64 {
        let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;

        // Fisher-Yates over the cell indices: value `extra` gets the cell at
        // position `extra - 1` in the shuffled order.
        let mut cells: Vec<usize> = (0..4096).collect();
        for i in (1..cells.len()).rev() {
            let j = (xorshift(&mut state) % (i as u64 + 1)) as usize;
            cells.swap(i, j);
        }

        let mut container = PalettedContainer::filled(Strategy::BLOCK_STATES, REGISTRY, 0);
        let mut model = vec![0u32; 4096];
        let mut promotions: Vec<(u32, PaletteKind, u32)> = Vec::new();
        let mut tier = expected_tier(1);

        // 0 stays where the fill put it; extras 1..=300 each take one cell.
        for extra in 1..=300u32 {
            let cell = cells[(extra - 1) as usize];
            model[cell] = extra;
            container.set(cell, extra);

            let distinct = extra + 1;
            let want = expected_tier(distinct);
            assert_eq!(
                container.palette_kind(),
                want.0,
                "seed {seed}: {distinct} distinct"
            );
            assert_eq!(
                container.storage().bits(),
                want.1,
                "seed {seed}: {distinct} distinct"
            );
            if want.0 != PaletteKind::Global {
                assert_eq!(
                    container.palette().len(),
                    distinct as usize,
                    "seed {seed}: nothing has been overwritten, so the palette holds all \
                     of them"
                );
            }
            if want != tier {
                promotions.push((distinct, want.0, want.1));
                tier = want;
            }
        }
        assert_eq!(
            promotions,
            vec![
                (2, PaletteKind::Linear, 4),
                (17, PaletteKind::Hashed, 5),
                (33, PaletteKind::Hashed, 6),
                (65, PaletteKind::Hashed, 7),
                (129, PaletteKind::Hashed, 8),
                (257, PaletteKind::Global, 15),
            ],
            "seed {seed}: scrambled cells do not move the boundaries"
        );

        // A thousand arbitrary rewrites at the top of the ladder: global is
        // the last tier and cannot grow again, and whatever is overwritten,
        // the model still says what every cell must read.
        for round in 0..1000 {
            let cell = (xorshift(&mut state) % 4096) as usize;
            let value = (xorshift(&mut state) % u64::from(REGISTRY)) as u32;
            model[cell] = value;
            container.set(cell, value);
            assert_eq!(
                container.palette_kind(),
                PaletteKind::Global,
                "seed {seed} round {round}"
            );
            assert_eq!(container.storage().bits(), 15, "seed {seed} round {round}");
        }
        for (cell, value) in model.iter().enumerate() {
            assert_eq!(container.get(cell), *value, "seed {seed}, cell {cell}");
        }
    }
}

#[test]
fn a_promotion_prunes_the_values_no_cell_holds_anymore() {
    // Discovered by the scrambled suite above and pinned here on purpose:
    // between promotions the palette carries entries no cell references any
    // more -- that is what keeps edits cheap -- but rebuilding for a
    // promotion re-encodes from the live cells alone. What a container grows
    // into therefore depends on what it *holds*, not on everything that was
    // ever written to it; vanilla behaves the same way because it too
    // re-palettes from contents.
    let mut container = PalettedContainer::filled(Strategy::BLOCK_STATES, REGISTRY, 0);

    // Ten distinct values beside the fill: eleven palette entries, five short
    // of a four-bit linear palette's sixteen.
    for cell in 1..=10usize {
        container.set(cell, cell as u32);
    }
    assert_eq!(container.palette_kind(), PaletteKind::Linear);
    assert_eq!(container.storage().bits(), 4);

    // Six of them are overwritten away -- but there is still room, so the
    // writes succeed without a promotion, and the dead entries stay.
    for cell in 3..=8usize {
        container.set(cell, 11);
    }
    assert_eq!(container.palette_kind(), PaletteKind::Linear);
    assert_eq!(
        container.palette().len(),
        12,
        "six dead entries ride along in memory"
    );

    // Four fresh distinct values fill the palette to exactly its capacity...
    for step in 0..4u32 {
        container.set(11 + step as usize, 12 + step);
    }
    assert_eq!(
        container.palette().len(),
        16,
        "one short of forcing the issue"
    );
    assert_eq!(container.storage().bits(), 4);

    // ...and one more cannot fit. The promotion re-encodes every cell through
    // the old palette and into a fresh one built from what the cells hold,
    // which drops the six dead entries and renumbers everything else.
    container.set(20, 16);
    assert_eq!(container.palette_kind(), PaletteKind::Hashed);
    assert_eq!(
        container.palette().entries(),
        Some(&[0u32, 1, 2, 11, 9, 10, 12, 13, 14, 15, 16][..]),
        "first-appearance order over the cells, then the value that caused it"
    );
    assert_eq!(container.storage().bits(), 5);

    assert_eq!(container.get(1), 1);
    assert_eq!(container.get(2), 2);
    for cell in 3..=8 {
        assert_eq!(container.get(cell), 11, "cell {cell}");
    }
    assert_eq!(container.get(9), 9);
    assert_eq!(container.get(10), 10);
    assert_eq!(container.get(20), 16);
}

#[test]
fn the_global_fallback_holds_any_registry_id_without_growing_again() {
    let mut container = PalettedContainer::filled(Strategy::BLOCK_STATES, REGISTRY, 0);
    // Cells 0..256 end up holding their own index: 256 distinct values, the
    // top of the eight-bit hashed tier.
    let mut model = vec![0u32; 4096];
    for (cell, stored) in model.iter_mut().take(256).enumerate() {
        container.set(cell, cell as u32);
        *stored = cell as u32;
    }
    assert_eq!(container.storage().bits(), 8);

    // The 257th distinct value tips it into the global palette...
    container.set(256, 999);
    model[256] = 999;
    assert_eq!(container.palette_kind(), PaletteKind::Global);
    assert_eq!(container.storage().bits(), 15);

    // ...and after that nothing grows, however wide the ids get. The global
    // palette is the registry; there is no tier above it to promote into.
    let mut state = 0x1234_5678_9abc_def0u64;
    for round in 0..2000 {
        let cell = (xorshift(&mut state) % 4096) as usize;
        let value = (xorshift(&mut state) % u64::from(REGISTRY)) as u32;
        model[cell] = value;
        container.set(cell, value);
        assert_eq!(
            container.palette_kind(),
            PaletteKind::Global,
            "round {round}"
        );
        assert_eq!(container.storage().bits(), 15, "round {round}");
        assert_eq!(container.get(cell), value, "round {round}");
    }
    for (cell, value) in model.iter().enumerate() {
        assert_eq!(container.get(cell), *value, "cell {cell}");
    }
}

#[test]
fn a_registry_of_one_value_stays_single_valued_and_refuses_everything_else() {
    // The degenerate case: a global palette over one id needs zero bits and
    // has no room for any other id, so the container must refuse writes
    // instead of wrapping them onto id 0.
    let mut container = PalettedContainer::filled(Strategy::BLOCK_STATES, 1, 0);
    assert_eq!(container.palette_kind(), PaletteKind::Single);
    for cell in [0usize, 1, 2048, 4095] {
        container.set(cell, 0);
    }
    assert!((0..4096).all(|i| container.get(i) == 0));

    let err = container
        .try_set(0, 1)
        .expect_err("a registry of one holds only 0");
    assert_eq!(err.value, 1);
    assert_eq!(err.registry_size, 1);
    assert_eq!(container.get(0), 0, "nothing was stored");

    let (entries, data) = container.to_parts();
    assert_eq!(entries, vec![0]);
    assert_eq!(data, None, "zero bits pack no longs");
}

#[test]
fn compacting_the_palette_remaps_every_index_into_first_appearance_order() {
    // Two hundred distinct values reach the hashed tier; half of them are
    // then overwritten away. Serialisation must keep only the survivors, in
    // first-appearance order over the cells, and the packed indices must be
    // remapped to follow -- because the alternative, keeping dead entries and
    // stale numbering, is a file whose tier and bit width differ from what a
    // vanilla server writes for the same contents.
    let mut container = PalettedContainer::filled(Strategy::BLOCK_STATES, REGISTRY, 0);
    for cell in 0..4096usize {
        container.set(cell, (cell % 200) as u32 * 3 + 1);
    }
    assert_eq!(container.palette_kind(), PaletteKind::Hashed);

    let survivor = 7u32;
    for cell in 0..4096usize {
        if container.get(cell) != survivor && cell % 2 == 0 {
            container.set(cell, survivor);
        }
    }

    let (entries, data) = container.to_parts();
    let live: std::collections::BTreeSet<u32> = (0..4096).map(|cell| container.get(cell)).collect();
    assert_eq!(
        entries.len(),
        live.len(),
        "the file names only the values still present"
    );

    // First-appearance order, verified directly: scanning the cells and
    // collecting unseen values must reproduce the entry list exactly.
    let mut replay: Vec<u32> = Vec::new();
    for cell in 0..4096 {
        let value = container.get(cell);
        if !replay.contains(&value) {
            replay.push(value);
        }
    }
    assert_eq!(entries, replay);

    // And every packed index points back at the right value through the new
    // numbering.
    let disk_bits = Strategy::BLOCK_STATES.disk_bits(entries.len(), REGISTRY);
    let packed = BitStorage::from_longs(
        disk_bits,
        4096,
        data.expect("more than one value packs an array"),
    )
    .expect("its own output");
    for cell in 0..4096 {
        let index = packed.get(cell);
        assert!(
            (index as usize) < entries.len(),
            "cell {cell} names entry {index} of {}",
            entries.len()
        );
        assert_eq!(entries[index as usize], container.get(cell), "cell {cell}");
    }
}

#[test]
fn shrinking_happens_at_serialisation_and_nowhere_else() {
    // The decided behaviour, pinned: memory keeps the tier it reached --
    // re-palettng on every write would make edits quadratic -- and the write
    // path is the single place a container collapses back to what it holds.
    let mut container = PalettedContainer::filled(Strategy::BLOCK_STATES, REGISTRY, 0);
    for cell in 0..4096usize {
        container.set(cell, (cell % 200) as u32 * 5 + 2);
    }
    assert_eq!(container.palette_kind(), PaletteKind::Hashed);

    for cell in 0..4096 {
        container.set(cell, 12);
    }
    assert_eq!(
        container.palette_kind(),
        PaletteKind::Hashed,
        "no demotion in memory"
    );

    let (entries, data) = container.to_parts();
    assert_eq!(entries, vec![12], "the write names one value");
    assert_eq!(data, None);

    let rebuilt = PalettedContainer::from_parts(Strategy::BLOCK_STATES, REGISTRY, &entries, data)
        .expect("its own output");
    assert_eq!(
        rebuilt.palette_kind(),
        PaletteKind::Single,
        "and reads back as such"
    );
    assert!((0..4096).all(|i| rebuilt.get(i) == 12));
}

#[test]
fn a_full_ladder_round_trips_through_parts_at_every_rung() {
    for distinct in [
        1usize, 2, 15, 16, 17, 31, 32, 33, 100, 255, 256, 257, 300, 4096,
    ] {
        let mut container = PalettedContainer::filled(Strategy::BLOCK_STATES, REGISTRY, 0);
        // A permutation of the cells decides which value lands where, so the
        // fixture is not a stripe; `p % distinct` over a full permutation
        // covers every residue, so exactly `distinct` values end up present.
        let values: Vec<u32> = (0..4096)
            .map(|cell| ((cell * 7919) % 4096) as u32 % distinct as u32 + 1)
            .collect();
        for (cell, value) in values.iter().enumerate() {
            container.set(cell, *value);
        }

        let (entries, data) = container.to_parts();
        assert_eq!(entries.len(), distinct, "{distinct} distinct values");
        assert_eq!(
            data.is_none(),
            distinct == 1,
            "only a single-valued section omits the array"
        );
        if let Some(longs) = &data {
            assert_eq!(
                longs.len(),
                long_count(4096, Strategy::BLOCK_STATES.disk_bits(distinct, REGISTRY)),
                "{distinct} distinct values: longs at the disk width"
            );
        }

        let rebuilt =
            PalettedContainer::from_parts(Strategy::BLOCK_STATES, REGISTRY, &entries, data.clone())
                .expect("its own output");
        for (cell, value) in values.iter().enumerate() {
            assert_eq!(
                rebuilt.get(cell),
                *value,
                "{distinct} distinct, cell {cell}"
            );
        }
    }
}

#[test]
fn a_single_valued_section_accepts_and_ignores_a_packed_array() {
    // Files other server software writes sometimes carry an array beside a
    // one-entry palette. Every index in it can only be zero, so refusing
    // would discard a sound chunk over bytes that carry no information.
    let lone = PalettedContainer::from_parts(Strategy::BLOCK_STATES, REGISTRY, &[42], None)
        .expect("a plain single-valued section");

    let padded = BitStorage::new(4, 4096).into_longs();
    let with_data =
        PalettedContainer::from_parts(Strategy::BLOCK_STATES, REGISTRY, &[42], Some(padded))
            .expect("the array is redundant, not wrong");

    assert_eq!(lone, with_data, "the array changes nothing");
    assert!((0..4096).all(|i| with_data.get(i) == 42));
}

#[test]
fn parts_do_not_depend_on_the_history_that_built_them() {
    // The determinism proof under everything else: two containers with
    // identical contents but wildly different construction histories -- one
    // set directly, one dragged up the whole ladder and back down again --
    // must serialise to the same bytes. If they did not, saved worlds would
    // depend on how a chunk was edited, and on whatever order a hash map
    // happened to visit its entries along the way.
    let pattern: Vec<u32> = (0..4096)
        .map(|cell| (cell * 31 % 50) as u32 * 11 + 3)
        .collect();

    let mut direct = PalettedContainer::filled(Strategy::BLOCK_STATES, REGISTRY, 0);
    for (cell, value) in pattern.iter().enumerate() {
        direct.set(cell, *value);
    }

    let mut scenic = PalettedContainer::filled(Strategy::BLOCK_STATES, REGISTRY, 0);
    // Drag it past every tier first: three hundred distinct values reach the
    // global palette, whose internal numbering owes nothing to insertion
    // order.
    for cell in 0..300usize {
        scenic.set(cell, cell as u32 * 13 + 5_000);
    }
    assert_eq!(scenic.palette_kind(), PaletteKind::Global);
    // Then overwrite everything with the target pattern, in a different cell
    // order than `direct` used.
    for step in 0..4096usize {
        let cell = (step * 2897 + 1301) % 4096;
        scenic.set(cell, pattern[cell]);
    }

    assert_eq!(
        direct.to_parts(),
        scenic.to_parts(),
        "same contents, same parts"
    );
}

#[test]
fn rebuilding_from_parts_that_cannot_agree_is_named() {
    // The suite's negative control, restated at the seams it crosses: an
    // index past the palette is caught at read time by from_parts, not left
    // to panic thousands of cells later.
    let five_entries: Vec<u32> = (1..=5).collect();
    let mut packed = BitStorage::new(Strategy::BLOCK_STATES.disk_bits(5, REGISTRY), 4096);
    packed.set(9, 7);
    let err = PalettedContainer::from_parts(
        Strategy::BLOCK_STATES,
        REGISTRY,
        &five_entries,
        Some(packed.into_longs()),
    )
    .expect_err("index 7 names nothing");
    assert!(
        matches!(
            err,
            ContainerError::IndexNotInPalette {
                cell: 9,
                index: 7,
                ..
            }
        ),
        "{err}"
    );
}
