//! Biome storage: the four-by-four-by-four paletted containers every chunk
//! section carries alongside its block states.
//!
//! Biomes look like block states that stayed home — same paletted-container
//! machinery, same cell ordering, same promotion logic — and the differences
//! are exactly the parts a single implementation gets wrong when it is
//! written once for blocks and copied:
//!
//! * **Sixty-four cells, not four thousand.** One biome per four-cubed block
//!   region, indexed `y << 4 | z << 2 | x`, which is the block-state index
//!   arithmetic at two bits per axis instead of four.
//! * **A one-bit floor, not a four-bit floor.** A section holding two biomes
//!   really is stored one bit per entry; padding it to four would triple the
//!   size of most sections.
//! * **No hashed tier.** One to three bits is linear; wider than three bits,
//!   the container is the global palette over the whole biome registry — six
//!   bits on 1.21.1 — because sixty-four cells cannot justify an entry list
//!   past eight values.
//!
//! The tests here hold those differences still with hand-computed index
//! vectors, the full promotion ladder for this shape, and round trips through
//! the on-disk form at every rung.

use dust_world::bits::long_count;
use dust_world::palette::{ceil_log2, PaletteKind};
use dust_world::{BitStorage, PalettedContainer, Strategy};

/// Biomes on 1.21.1: small enough that the global palette is narrow, large
/// enough that its width (six bits) exceeds every linear width (one to
/// three).
const BIOMES: u32 = 64;

#[test]
fn a_section_holds_sixty_four_cells_in_a_four_cube() {
    let container = PalettedContainer::filled(Strategy::BIOMES, BIOMES, 3);
    assert_eq!(container.len(), 64);
    assert_eq!(container.strategy().edge(), 4);
    assert_eq!(container.palette_kind(), PaletteKind::Single);
    assert_eq!(container.storage().bits(), 0);
    assert!((0..64).all(|i| container.get(i) == 3));
}

#[test]
fn the_cell_index_is_y_times_sixteen_plus_z_times_four_plus_x() {
    // The encoding spelled out in numbers rather than derived from the shift
    // arithmetic it checks: y varies slowest, x fastest, and the whole space
    // is 0..64. These pairs were computed by hand from `y << 4 | z << 2 | x`.
    let hand_computed: [((u32, u32, u32), usize); 10] = [
        ((0, 0, 0), 0),
        ((1, 0, 0), 1),
        ((3, 0, 0), 3),
        ((0, 0, 1), 4),
        ((2, 0, 2), 10), // 2<<2 | 2 = 10
        ((3, 0, 3), 15),
        ((0, 1, 0), 16),
        ((2, 1, 3), 30), // 16 + 12 + 2
        ((0, 3, 0), 48),
        ((3, 3, 3), 63),
    ];
    for ((x, y, z), want) in hand_computed {
        assert_eq!(
            Strategy::BIOMES.index(x, y, z),
            want,
            "cell ({x}, {y}, {z})"
        );
    }

    // And the encoding is a bijection: every coordinate names one cell, no
    // two coordinates share one, and all sixty-four are named.
    let mut seen = [false; 64];
    for y in 0..4u32 {
        for z in 0..4u32 {
            for x in 0..4u32 {
                let index = Strategy::BIOMES.index(x, y, z);
                assert!(!seen[index], "({x}, {y}, {z}) shares cell {index}");
                seen[index] = true;
            }
        }
    }
    assert!(seen.iter().all(|s| *s), "some cell has no coordinate");
}

#[test]
fn transposed_coordinates_name_different_cells() {
    // The failure mode of getting the axis order wrong: a chunk that is a
    // permutation of itself. Two probes far enough apart that no off-by-one
    // can alias them.
    let mut container = PalettedContainer::filled(Strategy::BIOMES, BIOMES, 0);
    container.set_at(2, 1, 3, 42); // cell 30
    assert_eq!(container.get_at(2, 1, 3), 42);
    assert_eq!(container.get_at(3, 1, 2), 0, "cell 27, not 30");
    assert_eq!(container.get_at(2, 3, 1), 0, "cell 56, not 30");
}

#[test]
fn the_ladder_runs_single_linear_global_and_never_hashed() {
    // Nine distinct biomes exhaust the shape: one value free, two at one bit,
    // up to four at two bits, up to eight at three, and the ninth forces the
    // global palette over the whole registry -- six bits, not four, because
    // the global tier's indices are registry ids.
    let mut container = PalettedContainer::filled(Strategy::BIOMES, BIOMES, 0);
    let mut promotions: Vec<(u32, PaletteKind, u32)> = Vec::new();
    let mut tier = (PaletteKind::Single, 0);

    for distinct in 1..=12u32 {
        if distinct > 1 {
            container.set(distinct as usize - 1, distinct - 1);
        }
        let want = match distinct {
            1 => (PaletteKind::Single, 0),
            2 => (PaletteKind::Linear, 1),
            3..=4 => (PaletteKind::Linear, 2),
            5..=8 => (PaletteKind::Linear, 3),
            _ => (PaletteKind::Global, ceil_log2(BIOMES)),
        };
        assert_eq!(
            container.palette_kind(),
            want.0,
            "{distinct} distinct biomes"
        );
        assert_eq!(
            container.storage().bits(),
            want.1,
            "{distinct} distinct biomes"
        );
        assert_ne!(
            container.palette_kind(),
            PaletteKind::Hashed,
            "{distinct} distinct biomes: biomes have no hashed tier"
        );
        if want != tier {
            promotions.push((distinct, want.0, want.1));
            tier = want;
        }

        // Cell k holds k once it has been written (cells 1..distinct-1 were),
        // and every other cell still holds the fill. Every promotion must
        // have carried them all across.
        for cell in 0..64usize {
            let expected = if cell == 0 || cell >= distinct as usize {
                0
            } else {
                cell as u32
            };
            assert_eq!(
                container.get(cell),
                expected,
                "{distinct} distinct biomes, cell {cell}"
            );
        }
    }

    assert_eq!(
        promotions,
        vec![
            (2, PaletteKind::Linear, 1),
            (3, PaletteKind::Linear, 2),
            (5, PaletteKind::Linear, 3),
            (9, PaletteKind::Global, 6),
        ],
        "the boundaries are one past each power of two"
    );
}

#[test]
fn sixty_four_random_assignments_survive_every_promotion_and_the_disk() {
    // Every cell gets its own pseudo-random biome, so the ladder is climbed
    // while the model fills; then the whole thing goes through the on-disk
    // form and comes back identical.
    for seed in 1..4u64 {
        let mut state = seed | 1;
        let xorshift = |state: &mut u64| {
            *state ^= *state << 13;
            *state ^= *state >> 7;
            *state ^= *state << 17;
            *state
        };
        let mut container = PalettedContainer::filled(Strategy::BIOMES, BIOMES, 0);
        let mut model = vec![0u32; 64];

        for cell in 0..64usize {
            let value = (xorshift(&mut state) % u64::from(BIOMES)) as u32;
            model[cell] = value;
            container.set(cell, value);
            for (index, expected) in model.iter().enumerate().take(cell + 1) {
                assert_eq!(
                    container.get(index),
                    *expected,
                    "seed {seed}: cell {index} after writing cell {cell}"
                );
            }
        }

        let (entries, data) = container.to_parts();
        let rebuilt = PalettedContainer::from_parts(Strategy::BIOMES, BIOMES, &entries, data)
            .expect("its own output");
        for (cell, expected) in model.iter().enumerate() {
            assert_eq!(rebuilt.get(cell), *expected, "seed {seed}, cell {cell}");
        }
    }
}

#[test]
fn two_biomes_pack_into_exactly_one_long_of_alternating_bits() {
    // The floor of the format, checked against a number derived by hand: a
    // section holding two biomes stores one bit per cell, and sixty-four
    // cells are one long. With even cells holding the first-written biome
    // and odd cells the second, the packed word is 0xAAAA...AA -- every odd
    // bit set.
    let mut container = PalettedContainer::filled(Strategy::BIOMES, BIOMES, 7);
    for cell in 0..64usize {
        if cell % 2 == 1 {
            container.set(cell, 3);
        }
    }
    assert_eq!(container.storage().bits(), 1);

    let (entries, data) = container.to_parts();
    assert_eq!(entries, vec![7, 3], "cell 0 was written first");
    assert_eq!(
        data.as_ref().expect("two entries pack an array").len(),
        long_count(64, 1)
    );
    assert_eq!(
        data.as_deref().expect("still there")[0],
        0xaaaa_aaaa_aaaa_aaaa_u64 as i64,
        "every odd cell names entry 1"
    );

    let rebuilt = PalettedContainer::from_parts(Strategy::BIOMES, BIOMES, &entries, data)
        .expect("its own output");
    for cell in 0..64usize {
        assert_eq!(rebuilt.get(cell), if cell % 2 == 1 { 3 } else { 7 });
    }
}

#[test]
fn the_disk_width_stays_on_the_floor_until_the_global_tier() {
    // What a chunk file holds: the palette list beside indices packed wide
    // enough for the list. Up to nine entries that is the floor widths; past
    // eight, the indices point into the list at ceil_log2(entries) bits --
    // not at the global palette's six, which is what the in-memory container
    // uses once rebuilt.
    let s = Strategy::BIOMES;
    assert_eq!(s.disk_bits(2, BIOMES), 1);
    assert_eq!(s.disk_bits(3, BIOMES), 2);
    assert_eq!(s.disk_bits(4, BIOMES), 2);
    assert_eq!(s.disk_bits(5, BIOMES), 3);
    assert_eq!(s.disk_bits(8, BIOMES), 3);
    assert_eq!(
        s.disk_bits(9, BIOMES),
        4,
        "a global-tier file indexes its own list"
    );
    assert_eq!(s.disk_bits(64, BIOMES), 6);

    // Long counts for the interesting rungs, over sixty-four cells. Three
    // and six do not divide sixty-four, so those arrays are longer than a
    // naive `cells * bits / 64` would suggest -- twenty-one values to a
    // three-bit long, ten to a six-bit one.
    assert_eq!(long_count(64, 1), 1);
    assert_eq!(long_count(64, 2), 2);
    assert_eq!(long_count(64, 3), 4);
    assert_eq!(long_count(64, 4), 4);
    assert_eq!(long_count(64, 6), 7);
}

#[test]
fn a_nine_biome_file_has_its_indices_translated_into_registry_ids() {
    // The global-tier subtlety, exercised at the biome size where it is cheap
    // to check by hand: on disk the indices name positions in the palette
    // list, and rebuilding translates them. Read literally they would be
    // biome ids 0..9 -- plausible, and wrong.
    let entries: Vec<u32> = (0..9u32).map(|n| 60 - n * 7).collect();
    let disk_bits = Strategy::BIOMES.disk_bits(entries.len(), BIOMES);
    assert_eq!(disk_bits, 4);

    let mut packed = BitStorage::new(disk_bits, 64);
    for cell in 0..64usize {
        packed.set(cell, (cell % 9) as u32);
    }

    let container = PalettedContainer::from_parts(
        Strategy::BIOMES,
        BIOMES,
        &entries,
        Some(packed.into_longs()),
    )
    .expect("a well-formed global-tier section");

    assert_eq!(container.palette_kind(), PaletteKind::Global);
    assert_eq!(
        container.storage().bits(),
        6,
        "rebuilt onto the whole registry"
    );
    for cell in 0..64usize {
        assert_eq!(container.get(cell), entries[cell % 9], "cell {cell}");
    }
}

#[test]
fn the_same_machinery_serves_both_shapes_and_the_differences_are_real() {
    // Side by side at the counts where the shapes diverge, so a container
    // written for one shape and reused for the other fails here and not in a
    // saved world.
    let mut states = PalettedContainer::filled(Strategy::BLOCK_STATES, 26_684, 0);
    let mut biomes = PalettedContainer::filled(Strategy::BIOMES, BIOMES, 0);

    for n in 1..5u32 {
        states.set(n as usize - 1, n - 1);
        biomes.set(n as usize - 1, n - 1);
        assert_eq!(
            states.storage().bits(),
            match n {
                1 => 0,
                _ => 4,
            },
            "{n} distinct states: single-valued, then pinned at the four-bit floor"
        );
        assert_eq!(
            biomes.storage().bits(),
            match n {
                1 => 0,
                2 => 1,
                _ => 2,
            },
            "{n} distinct biomes use only what they need"
        );
    }

    // Five distinct: biomes are still linear at three bits; block states are
    // still linear too but pinned at four. Seventeen distinct: block states
    // reach the hashed tier; biomes have none to reach and stay linear until
    // their registry-wide jump.
    biomes.set(4, 4);
    assert_eq!(biomes.storage().bits(), 3);

    let mut seventeen = PalettedContainer::filled(Strategy::BLOCK_STATES, 26_684, 0);
    for n in 1..17u32 {
        seventeen.set(n as usize - 1, n - 1);
    }
    assert_eq!(seventeen.storage().bits(), 4);
    seventeen.set(16, 16);
    assert_eq!(seventeen.palette_kind(), PaletteKind::Hashed);
    assert_eq!(seventeen.storage().bits(), 5);

    let mut nine_biomes = PalettedContainer::filled(Strategy::BIOMES, BIOMES, 0);
    for n in 1..9u32 {
        nine_biomes.set(n as usize - 1, n - 1);
    }
    assert_eq!(nine_biomes.storage().bits(), 3);
    nine_biomes.set(8, 8);
    assert_eq!(
        nine_biomes.palette_kind(),
        PaletteKind::Global,
        "no hashed stopover"
    );
    assert_eq!(nine_biomes.storage().bits(), 6);
}
