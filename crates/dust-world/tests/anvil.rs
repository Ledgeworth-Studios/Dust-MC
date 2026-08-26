//! Reading chunks out of a world Minecraft actually wrote.
//!
//! # Why this is `#[ignore]` and how to run it
//!
//! It needs a real world on disk, which this repository does not and will not
//! carry: a generated world is Mojang's content, and the project's provenance
//! rule is that nothing of theirs is committed. So the test is skipped unless
//! `DUST_ANVIL_WORLD` points at a `region` directory, and CI does not set it.
//!
//! ```text
//! cargo xtask harness provision --version 1.21.1 --seed 0 --yes
//! cargo xtask harness capture --version 1.21.1 --seed 0 --radius 2
//! DUST_ANVIL_WORLD=<cache>/servers/1.21.1/seed-0/world/region \
//!   cargo test -p dust-world --test anvil -- --ignored
//! ```
//!
//! That is the same shape as the rest of the differential work: the machinery
//! is committed, the data is fetched, and the test says out loud when it did
//! not run rather than passing vacuously. A test that silently skipped would
//! be worse than none, because it would appear in a green run.

use std::collections::HashMap;
use std::path::PathBuf;

use dust_world::anvil::write::Carried;
use dust_world::anvil::{self, NameTables};
use dust_world::coords::{ChunkPos, RegionPos};
use dust_world::heightmap::WorldHeight;
use dust_world::region::RegionFile;

fn world_dir() -> Option<PathBuf> {
    std::env::var_os("DUST_ANVIL_WORLD").map(PathBuf::from)
}

/// Name tables built from the names the file itself uses.
///
/// The real server resolves through `dust-registry`; this crate must not
/// depend on it, and a test that did would be testing the registry rather than
/// the parser. Numbering the names in the order they are met is enough to
/// check the *structure*: which cell holds which palette entry, and that a
/// section with no `data` is uniform rather than empty.
fn tables_for(root: &dust_nbt::Compound) -> NameTables {
    let mut tables = NameTables {
        blocks: HashMap::new(),
        biomes: HashMap::new(),
        block_registry_size: 1 << 20,
        biome_registry_size: 1 << 12,
    };
    let Some(dust_nbt::Tag::List(sections)) = root.get("sections") else {
        return tables;
    };
    for entry in sections.iter() {
        let dust_nbt::Tag::Compound(section) = entry else {
            continue;
        };
        if let Some(dust_nbt::Tag::Compound(states)) = section.get("block_states") {
            if let Some(dust_nbt::Tag::List(palette)) = states.get("palette") {
                for block in palette.iter() {
                    if let dust_nbt::Tag::Compound(block) = block {
                        if let Some(dust_nbt::Tag::String(name)) = block.get("Name") {
                            let next = tables.blocks.len() as u32;
                            tables.blocks.entry(name.clone()).or_insert(next);
                        }
                    }
                }
            }
        }
        if let Some(dust_nbt::Tag::Compound(biomes)) = section.get("biomes") {
            if let Some(dust_nbt::Tag::List(palette)) = biomes.get("palette") {
                for biome in palette.iter() {
                    if let dust_nbt::Tag::String(name) = biome {
                        let next = tables.biomes.len() as u32;
                        tables.biomes.entry(name.clone()).or_insert(next);
                    }
                }
            }
        }
    }
    tables
}

#[test]
#[ignore = "needs a real world; set DUST_ANVIL_WORLD to a region directory"]
fn every_chunk_of_a_real_region_reads() {
    let Some(dir) = world_dir() else {
        panic!("DUST_ANVIL_WORLD is not set; see this file's own documentation");
    };

    let mut region = RegionFile::open_in(&dir, RegionPos::new(0, 0)).expect("open the region");
    let positions: Vec<ChunkPos> = region.chunk_positions().collect();
    assert!(
        positions.len() > 100,
        "a pregenerated region should hold hundreds of chunks, not {}",
        positions.len()
    );

    let mut read = 0usize;
    let mut solid_sections = 0usize;
    for pos in positions {
        let payload = region
            .read_chunk(pos)
            .expect("read the chunk")
            .expect("the header said it was there");
        let named = dust_nbt::read::from_bytes(payload.as_bytes()).expect("the chunk is NBT");
        let dust_nbt::Tag::Compound(root) = &named.tag else {
            panic!("a chunk's root is a compound");
        };

        // The version the format was written by. Checked rather than assumed,
        // because every field below is a claim about *this* version's layout.
        assert_eq!(
            root.get("DataVersion"),
            Some(&dust_nbt::Tag::Int(anvil::DATA_VERSION_1_21_1)),
            "this test's layout knowledge is 1.21.1's"
        );

        let tables = tables_for(root);
        let chunk = anvil::chunk(root, WorldHeight::OVERWORLD, &tables)
            .unwrap_or_else(|e| panic!("chunk {pos:?}: {e}"));

        assert_eq!(chunk.pos(), pos, "the chunk knows where it is");

        // Somewhere in an overworld column there is stone. A parser that
        // returned air everywhere would satisfy every structural check above
        // and fail this.
        let stone = tables.blocks.get("minecraft:stone").copied();
        if let Some(stone) = stone {
            let found = (0..16)
                .any(|x| (0..16).any(|z| (-64..64).any(|y| chunk.get_block(x, y, z) == stone)));
            assert!(found, "chunk {pos:?} has no stone in it anywhere");
        }

        for section in chunk.sections() {
            if section.states().palette().kind() != dust_world::palette::PaletteKind::Single {
                solid_sections += 1;
            }
        }
        read += 1;
    }

    assert!(read > 100, "read {read} chunks");
    // If every section came back single-valued, the packed arrays were never
    // unpacked and the whole world would be uniform slabs.
    assert!(
        solid_sections > read,
        "only {solid_sections} sections across {read} chunks had more than one \
         block in them; the packed indices are not being read"
    );
}

#[test]
#[ignore = "needs a real world; set DUST_ANVIL_WORLD to a region directory"]
fn a_section_with_no_data_array_is_uniform_rather_than_empty() {
    // The single most likely way to read Anvil wrongly and have it still load.
    // `data` is absent when the palette has one entry, and its absence means
    // "every cell is that entry" — a reader that treated it as an empty
    // section would turn solid stone into air.
    let Some(dir) = world_dir() else {
        panic!("DUST_ANVIL_WORLD is not set; see this file's own documentation");
    };
    let mut region = RegionFile::open_in(&dir, RegionPos::new(0, 0)).expect("open");

    let mut checked = 0usize;
    for pos in region.chunk_positions().collect::<Vec<_>>() {
        let payload = region.read_chunk(pos).expect("read").expect("present");
        let named = dust_nbt::read::from_bytes(payload.as_bytes()).expect("nbt");
        let dust_nbt::Tag::Compound(root) = &named.tag else {
            continue;
        };
        let tables = tables_for(root);
        let chunk = anvil::chunk(root, WorldHeight::OVERWORLD, &tables).expect("parse");

        let Some(dust_nbt::Tag::List(sections)) = root.get("sections") else {
            continue;
        };
        for entry in sections.iter() {
            let dust_nbt::Tag::Compound(section) = entry else {
                continue;
            };
            let Some(dust_nbt::Tag::Byte(y)) = section.get("Y") else {
                continue;
            };
            let y = i32::from(*y);
            if !(-4..20).contains(&y) {
                continue;
            }
            let Some(dust_nbt::Tag::Compound(states)) = section.get("block_states") else {
                continue;
            };
            if states.get("data").is_some() {
                continue;
            }
            // No data array: every cell must be the palette's one entry.
            let Some(dust_nbt::Tag::List(palette)) = states.get("palette") else {
                continue;
            };
            let dust_nbt::Tag::Compound(only) = palette.get(0).expect("one entry") else {
                continue;
            };
            let Some(dust_nbt::Tag::String(name)) = only.get("Name") else {
                continue;
            };
            let expected = tables.blocks[name];
            let section = chunk.section(y * 16);
            for cell in 0..section.states().len() {
                assert_eq!(
                    section.states().get(cell),
                    expected,
                    "chunk {pos:?} section {y} cell {cell} should be all {name}"
                );
            }
            checked += 1;
        }
    }
    assert!(
        checked > 0,
        "no section in this world had a palette of one and no data array, so \
         the case this test exists for was never exercised"
    );
}

/// Every chunk of a real region, written back out and read again.
///
/// # What this proves, stated narrowly on purpose
///
/// That the writer and the reader agree, over real input rather than over
/// fixtures somebody here invented — which is more than a synthetic round trip
/// and is still **not** the question. Both halves are this crate's, so they
/// agree with each other under any self-consistent convention, including one
/// where every packed index is off by one in the same direction. That is the
/// same argument Phase 0.5 made about registry round trips and it holds here.
///
/// The check that does answer the question is `cargo xtask harness rewrite`,
/// which hands the bytes to Minecraft. This exists because it fails in seconds
/// and that one takes seven minutes, so a broken writer should not have to wait
/// for a JVM to say so.
#[test]
#[ignore = "needs a real world; set DUST_ANVIL_WORLD to a region directory"]
fn a_real_chunk_written_and_read_again_is_the_chunk_it_was() {
    let Some(dir) = world_dir() else {
        panic!("DUST_ANVIL_WORLD is not set; see this file's own documentation");
    };
    let mut region = RegionFile::open_in(&dir, RegionPos::new(0, 0)).expect("open the region");

    let mut checked = 0usize;
    let mut with_entities = 0usize;
    for pos in region.chunk_positions().collect::<Vec<_>>() {
        let payload = region.read_chunk(pos).expect("read").expect("present");
        let named = dust_nbt::read::from_bytes(payload.as_bytes()).expect("nbt");
        let dust_nbt::Tag::Compound(root) = &named.tag else {
            continue;
        };
        let tables = tables_for(root);
        let before = anvil::chunk(root, WorldHeight::OVERWORLD, &tables).expect("parse");

        let carried = Carried::read_from(root);
        let written = anvil::write::chunk(&before, &tables, &carried)
            .unwrap_or_else(|e| panic!("{pos:?}: {e}"));

        // Through bytes, not through the compound. Serialising and parsing is
        // where a palette entry's type or a long array's length would go wrong,
        // and comparing the compound to itself would skip exactly that.
        let bytes = dust_nbt::write::to_vec("", &dust_nbt::Tag::Compound(written)).expect("write");
        let again = dust_nbt::read::from_bytes(&bytes).expect("read back");
        let dust_nbt::Tag::Compound(again) = &again.tag else {
            panic!("the root this wrote is a compound");
        };
        let after = anvil::chunk(again, WorldHeight::OVERWORLD, &tables)
            .unwrap_or_else(|e| panic!("{pos:?} on the way back: {e}"));

        assert_eq!(after.pos(), before.pos(), "{pos:?} moved");
        assert_eq!(
            after.sections().len(),
            before.sections().len(),
            "{pos:?} changed height"
        );
        for (index, (a, b)) in after.sections().iter().zip(before.sections()).enumerate() {
            assert!(
                a.states().equivalent(b.states()),
                "{pos:?} section {index} holds different blocks"
            );
            assert!(
                a.biomes().equivalent(b.biomes()),
                "{pos:?} section {index} holds different biomes"
            );
        }

        // The heightmaps are written from the chunk rather than recomputed on
        // the way back in, so this compares what the file carried against what
        // the file carries now.
        for kind in dust_world::heightmap::HeightmapKind::ALL {
            if !kind.persisted() {
                continue;
            }
            assert_eq!(
                after.heightmaps().get(kind).as_longs(),
                before.heightmaps().get(kind).as_longs(),
                "{pos:?} {kind:?} changed"
            );
        }

        // Carrying is the whole reason `Carried` exists, and a test that only
        // ever ran over chunks with nothing to carry would be green for the
        // wrong reason. The count below is what makes that visible.
        assert_eq!(
            Carried::read_from(again),
            carried,
            "{pos:?} lost what a chunk cannot model"
        );
        if carried
            .block_entities
            .as_ref()
            .is_some_and(|l| !l.is_empty())
        {
            with_entities += 1;
        }
        checked += 1;
    }

    assert!(checked > 100, "only {checked} chunks were written back");
    // A pregenerated overworld region has dungeons in it, and a dungeon has a
    // chest. Zero here does not mean the carrying is broken — it means this
    // world could not have told the difference, which is worth failing over
    // rather than passing quietly.
    assert!(
        with_entities > 0,
        "none of the {checked} chunks carried a block entity, so nothing here \
         exercised the one field a `Chunk` provably cannot reconstruct"
    );
}
