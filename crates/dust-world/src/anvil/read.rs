//! Turning the NBT a region file holds into a [`Chunk`].
//!
//! The format this reads is written down once, in [the module
//! documentation](super), together with the four facts about it that a reader
//! gets wrong quietly. What is here is the walk itself, and the decisions about
//! what to do when the file says something this build was not expecting.
//!
//! # Light is not read, and heightmaps are
//!
//! Both are derived data — a cache of an answer some engine computed about the
//! blocks — so the two being treated differently needs a reason, and it is this:
//! **Dust can reproduce the light and cannot reproduce the heightmaps.**
//!
//! A chunk's stored `SkyLight` and `BlockLight` are a cache of what a light
//! engine produced, and this server has its own engine. Reading them would mean
//! serving light no code here can reproduce, and being unable to tell a stale
//! cache from a fresh one. So every section is loaded dark and lit again.
//!
//! A heightmap is a cache of a *predicate*, and vanilla's four maps use four
//! different ones: `WORLD_SURFACE` is not-air, `OCEAN_FLOOR` is blocks-motion,
//! `MOTION_BLOCKING` is blocks-motion-or-fluid, and `MOTION_BLOCKING_NO_LEAVES`
//! is that with leaves taken out. Dust's recompute takes a *single* closure and
//! every caller in the tree passes not-air — right for the first and wrong for
//! the other three. Discarding the file's maps and substituting that would not
//! be recovering the data; it would be replacing four answers with one, three
//! times over. So they are read, and a caller with a better predicate is free
//! to recompute.
//!
//! **The differential found this, not a unit test.** The first version of this
//! reader ignored `Heightmaps` entirely, and every chunk came back carrying the
//! default map — which is invisible in-process, because the one caller that
//! serves chunks recomputes before sending. It surfaced the moment a written
//! chunk went to a real server: blocks and biomes identical on all 25 chunks,
//! heightmaps different on all 25. It is worth naming the shape: this was not
//! code that was wrong, it was **code that was never written**, and no test over
//! the code that exists can find a field nobody read.

use dust_nbt::{Compound, Tag};

use crate::chunk::{Chunk, Section};
use crate::container::{PalettedContainer, Strategy};
use crate::coords::ChunkPos;
use crate::heightmap::{Heightmap, HeightmapKind, WorldHeight, COLUMNS};
use crate::light::LightArray;

use super::{AnvilError, Names};

/// Read one chunk from the NBT a region file gave back.
pub fn chunk(root: &Compound, world: WorldHeight, names: &impl Names) -> Result<Chunk, AnvilError> {
    let x = int(root, "xPos")?;
    let z = int(root, "zPos")?;
    let pos = ChunkPos::new(x, z);

    let mut chunk = Chunk::uniform(
        pos,
        world,
        names.block_registry_size(),
        names.biome_registry_size(),
        // Air, which every world agrees is state zero. The sections that
        // exist overwrite this; the ones the file omits are air, and that is
        // the file saying so rather than this guessing.
        0,
        0,
    );

    let sections = match root.get("sections") {
        Some(Tag::List(list)) => list,
        _ => return Err(AnvilError::Field { name: "sections" }),
    };

    let lowest = world.min_y() / 16;
    let highest = lowest + (world.height() / 16) as i32 - 1;

    for entry in sections.iter() {
        let Tag::Compound(section) = entry else {
            return Err(AnvilError::Field { name: "a section" });
        };
        let y = match section.get("Y") {
            Some(Tag::Byte(y)) => i32::from(*y),
            _ => return Err(AnvilError::Field { name: "section Y" }),
        };
        // A world file may carry one section below and one above the world it
        // describes — vanilla writes them so light has somewhere to live at
        // the boundary. They hold no blocks anybody can reach, so they are
        // skipped rather than refused.
        if y < lowest || y > highest {
            continue;
        }

        let states = read_container(
            section.get("block_states"),
            Strategy::BLOCK_STATES,
            names.block_registry_size(),
            |palette| block_ids(palette, names),
        )?;
        let biomes = read_container(
            section.get("biomes"),
            Strategy::BIOMES,
            names.biome_registry_size(),
            |palette| biome_ids(palette, names),
        )?;

        *chunk.section_mut(y * 16) = Section::new(
            states,
            biomes,
            // Light is not read. A chunk's stored light is a cache of what the
            // engine would compute, and this server computes it — reading it
            // would mean trusting a file to agree with an engine that has not
            // run yet.
            LightArray::filled(0),
            LightArray::filled(0),
        );
    }

    // Absent, and a key that is absent from a present compound, both leave the
    // default in place: a chunk that has not been generated far enough to have
    // heightmaps is a real state and not a damaged file. A key that is *there*
    // and unreadable is the opposite, and stops the read.
    if let Some(Tag::Compound(maps)) = root.get("Heightmaps") {
        for (key, value) in maps.iter() {
            // An unrecognised key is skipped rather than refused. The two `_WG`
            // maps are not written by a full chunk and a datapack-shaped mod
            // may add its own; neither is a reason to reject a world.
            let Some(kind) = HeightmapKind::from_nbt_key(key) else {
                continue;
            };
            let Tag::LongArray(longs) = value else {
                return Err(AnvilError::Field {
                    name: "a heightmap",
                });
            };
            *chunk.heightmaps_mut().get_mut(kind) =
                Heightmap::from_longs(kind, world, longs.clone()).map_err(|_| {
                    AnvilError::BadPacking {
                        cells: COLUMNS,
                        longs: longs.len(),
                    }
                })?;
        }
    }

    Ok(chunk)
}

/// Turn a `block_states` or `biomes` compound into a container.
fn read_container(
    tag: Option<&Tag>,
    strategy: Strategy,
    registry_size: u32,
    ids: impl Fn(&Tag) -> Result<Vec<u32>, AnvilError>,
) -> Result<PalettedContainer, AnvilError> {
    let Some(Tag::Compound(compound)) = tag else {
        // An absent container means the section has none of that thing. For
        // blocks that is air; for biomes it is the first biome. Both are the
        // caller's zero, which is what `Chunk::uniform` already put there.
        return Ok(PalettedContainer::filled(strategy, registry_size, 0));
    };
    let palette = compound
        .get("palette")
        .ok_or(AnvilError::Field { name: "palette" })?;
    let entries = ids(palette)?;

    let data = match compound.get("data") {
        Some(Tag::LongArray(longs)) => Some(longs.clone()),
        // **Absent means uniform, not empty.** A reader that returned an empty
        // section here would turn a solid section of stone into air and the
        // chunk would still load.
        None => None,
        _ => return Err(AnvilError::Field { name: "data" }),
    };

    let Some(longs) = data else {
        // One entry and no array: every cell is that entry.
        let only = *entries
            .first()
            .ok_or(AnvilError::Field { name: "palette" })?;
        return Ok(PalettedContainer::filled(strategy, registry_size, only));
    };

    let bits = strategy.disk_bits(entries.len(), registry_size).max(1);
    let expected = crate::bits::long_count(strategy.len(), bits);
    if longs.len() != expected {
        return Err(AnvilError::BadPacking {
            cells: strategy.len(),
            longs: longs.len(),
        });
    }

    // Unpacked cell by cell rather than handed to `from_parts`, and the reason
    // is a case that only shows up against a real world.
    //
    // A file's palette may list one block *name* twice — two entries of
    // `minecraft:water` at different levels, say — because a palette entry is
    // a block *state* and this reader resolves it to a default state by name.
    // Two entries collapse to one id, and `from_parts` refuses a palette with
    // a repeat, correctly: for a palette that really did repeat, every index
    // past the repeat would mean something other than what the file says.
    //
    // Here the repeat is ours and not the file's. The indices are still right;
    // they just land on equal values. So the indices are followed directly,
    // which costs a write per cell and cannot be wrong about which entry a
    // cell holds.
    let storage =
        crate::bits::BitStorage::from_longs(bits, strategy.len(), longs).map_err(|_| {
            AnvilError::BadPacking {
                cells: strategy.len(),
                longs: expected,
            }
        })?;
    let mut container = PalettedContainer::filled(strategy, registry_size, entries[0]);
    for cell in 0..strategy.len() {
        let index = storage.get(cell) as usize;
        let value = *entries.get(index).ok_or(AnvilError::Field {
            name: "palette index",
        })?;
        container.set(cell, value);
    }
    Ok(container)
}

/// `[{Name: "minecraft:stone", Properties: {...}}, ...]` as state ids.
fn block_ids(palette: &Tag, names: &impl Names) -> Result<Vec<u32>, AnvilError> {
    let Tag::List(list) = palette else {
        return Err(AnvilError::Field {
            name: "block palette",
        });
    };
    let mut out = Vec::with_capacity(list.len());
    for entry in list.iter() {
        let Tag::Compound(block) = entry else {
            return Err(AnvilError::Field {
                name: "block palette entry",
            });
        };
        let Some(Tag::String(name)) = block.get("Name") else {
            return Err(AnvilError::Field {
                name: "block palette Name",
            });
        };

        // `Properties` is absent for a block that has none, which is most of
        // them by count and nearly all of them by volume — stone, dirt, air.
        // The borrow is of the tag rather than a copy, so the common case
        // allocates nothing.
        let mut pairs: Vec<(&str, &str)> = Vec::new();
        if let Some(Tag::Compound(properties)) = block.get("Properties") {
            for (key, value) in properties.iter() {
                let Tag::String(value) = value else {
                    return Err(AnvilError::Field {
                        name: "a block property value",
                    });
                };
                pairs.push((key.as_str(), value.as_str()));
            }
        }

        out.push(
            names
                .block(name, &pairs)
                .ok_or_else(|| AnvilError::UnknownBlock { name: name.clone() })?,
        );
    }
    Ok(out)
}

/// `["minecraft:plains", ...]` as biome ids.
fn biome_ids(palette: &Tag, names: &impl Names) -> Result<Vec<u32>, AnvilError> {
    let Tag::List(list) = palette else {
        return Err(AnvilError::Field {
            name: "biome palette",
        });
    };
    let mut out = Vec::with_capacity(list.len());
    for entry in list.iter() {
        let Tag::String(name) = entry else {
            return Err(AnvilError::Field {
                name: "biome palette entry",
            });
        };
        out.push(
            names
                .biome(name)
                .ok_or_else(|| AnvilError::UnknownBiome { name: name.clone() })?,
        );
    }
    Ok(out)
}

fn int(root: &Compound, name: &'static str) -> Result<i32, AnvilError> {
    match root.get(name) {
        Some(Tag::Int(value)) => Ok(*value),
        _ => Err(AnvilError::Field { name }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anvil::NameTables;
    use dust_nbt::{List, TagType};

    fn tables() -> NameTables {
        let mut tables = NameTables {
            block_registry_size: 32,
            biome_registry_size: 8,
            ..NameTables::default()
        };
        tables.blocks.insert("minecraft:air".into(), 0);
        tables.biomes.insert("minecraft:plains".into(), 1);
        tables
    }

    /// The smallest root the reader accepts: a position and an empty section
    /// list. Everything the tests below vary is added on top.
    fn bare_root() -> Compound {
        let mut root = Compound::new();
        root.insert("xPos", Tag::Int(0));
        root.insert("zPos", Tag::Int(0));
        root.insert("sections", Tag::List(List::new(TagType::End)));
        root
    }

    /// 256 columns at nine bits, packed seven to a long with the top bit of
    /// each long unused — 37 longs, which is what a real file carries and what
    /// 36 would be if the packing straddled.
    fn heightmap_longs(value: i64) -> Vec<i64> {
        let mut longs = vec![0i64; 37];
        for column in 0..COLUMNS {
            let long = column / 7;
            let shift = (column % 7) * 9;
            longs[long] |= value << shift;
        }
        longs
    }

    /// The regression the differential found: a reader that ignores
    /// `Heightmaps` loses them silently, because the only in-process caller
    /// recomputes before it uses them.
    #[test]
    fn the_heightmaps_a_file_carries_are_read_and_not_discarded() {
        let mut maps = Compound::new();
        maps.insert("MOTION_BLOCKING", Tag::LongArray(heightmap_longs(70)));
        let mut root = bare_root();
        root.insert("Heightmaps", Tag::Compound(maps));

        let chunk = chunk(&root, WorldHeight::OVERWORLD, &tables()).expect("read");
        let map = chunk.heightmaps().get(HeightmapKind::MotionBlocking);
        // `first_available` is the row above the highest taken one, and the
        // stored number is that row's offset from the world's floor: 70 above
        // y=-64 is y=6.
        assert_eq!(map.first_available(0, 0), 6);
        assert_eq!(map.first_available(15, 15), 6);
        // A map the file did not carry keeps the default, rather than being
        // filled in with a neighbour's numbers.
        assert_eq!(
            chunk
                .heightmaps()
                .get(HeightmapKind::WorldSurface)
                .first_available(0, 0),
            -64
        );
    }

    #[test]
    fn a_chunk_with_no_heightmaps_at_all_reads_with_the_defaults() {
        let chunk = chunk(&bare_root(), WorldHeight::OVERWORLD, &tables()).expect("read");
        for kind in HeightmapKind::ALL {
            assert_eq!(
                chunk.heightmaps().get(kind).first_available(0, 0),
                -64,
                "{kind:?}"
            );
        }
    }

    /// Absent is a state; present and unreadable is a contradiction. The two
    /// are treated differently on purpose, and this is the difference.
    #[test]
    fn a_heightmap_that_is_there_and_unreadable_stops_the_read() {
        let mut maps = Compound::new();
        maps.insert("MOTION_BLOCKING", Tag::String("about eighty".into()));
        let mut root = bare_root();
        root.insert("Heightmaps", Tag::Compound(maps));
        assert!(matches!(
            chunk(&root, WorldHeight::OVERWORLD, &tables()),
            Err(AnvilError::Field {
                name: "a heightmap"
            })
        ));

        let mut maps = Compound::new();
        maps.insert("OCEAN_FLOOR", Tag::LongArray(vec![0; 12]));
        let mut root = bare_root();
        root.insert("Heightmaps", Tag::Compound(maps));
        assert!(matches!(
            chunk(&root, WorldHeight::OVERWORLD, &tables()),
            Err(AnvilError::BadPacking { longs: 12, .. })
        ));
    }

    /// The two `_WG` maps are absent from every finished chunk, and a datapack
    /// may add a key of its own. Neither is a damaged world.
    #[test]
    fn a_heightmap_key_this_build_does_not_know_is_skipped_rather_than_refused() {
        let mut maps = Compound::new();
        maps.insert("WORLD_SURFACE", Tag::LongArray(heightmap_longs(70)));
        maps.insert("SOMEBODYS_OWN_MAP", Tag::LongArray(vec![0; 3]));
        let mut root = bare_root();
        root.insert("Heightmaps", Tag::Compound(maps));

        let chunk = chunk(&root, WorldHeight::OVERWORLD, &tables()).expect("read");
        assert_eq!(
            chunk
                .heightmaps()
                .get(HeightmapKind::WorldSurface)
                .first_available(0, 0),
            6
        );
    }
}
