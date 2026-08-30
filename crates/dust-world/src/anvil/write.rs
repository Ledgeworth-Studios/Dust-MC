//! Turning a [`Chunk`] into the NBT a region file holds.
//!
//! # The writer's contract is narrower than the reader's, on purpose
//!
//! Reading is checkable against a world that already exists: point the parser
//! at a real region file and every disagreement is a bug with an address.
//! Writing has no such check available from inside this crate. The only
//! authority on whether a file is well-formed is Minecraft, and reaching it
//! means booting one — which is what `cargo xtask harness rewrite` does and
//! what Phase 2's exit criterion asks for.
//!
//! So the rule here is: **write what was measured, carry what is not modelled,
//! and refuse the rest.** Each of the three is a section below.
//!
//! # What was measured
//!
//! The fifteen root fields in [the module documentation](super) are not a
//! recollection of the format — they are the fields twenty-five chunks of a
//! real seed-0 1.21.1 world carried, counted. Every chunk had all fifteen and
//! none had a sixteenth, so this writes all fifteen and nothing else.
//!
//! Two of the measurements are worth repeating because they contradict what a
//! writer would otherwise do:
//!
//! * **`sections` holds every section of the world, not every non-empty one.**
//!   All twenty-five chunks carried exactly twenty-four sections, y=-4 through
//!   y=19, including the sixteen consecutive sections of pure air above the
//!   terrain. Writing only the interesting ones would produce a shorter file
//!   that is not the file vanilla writes, and the difference is not obviously
//!   harmless.
//! * **`block_entities` is present and empty**, as `List<End>`, in the
//!   twenty-three chunks that have none. Absent is not the same tag, and the
//!   reader's open question — "whether a `block_entities` list may be empty or
//!   must be absent" — is answered by the data: empty, and present.
//!
//! # What is not modelled is carried, not defaulted
//!
//! Seven of the fifteen fields hold things `dust-world` has no representation
//! for: a chest's loot table, a scheduled fluid tick, a village's structure
//! references. [`BlockEntityHandle`](crate::chunk::BlockEntityHandle) is
//! explicit that it is a placeholder holding a position and a state id and no
//! payload, so a chunk that was read and is written again **cannot reconstruct
//! them from the `Chunk`**. It can only copy them.
//!
//! That is what [`Carried`] is. It is a required argument rather than a
//! defaulted one because the alternative — writing the empty forms whenever a
//! caller says nothing — is a save that deletes every chest in the world and
//! reopens without complaint. A criterion that says *vanilla reopens it* would
//! be met by that, which is precisely why the loss has to be nameable:
//! [`Carried::dropped`] is spelled the way it is so that a call site choosing
//! it is visible in review.
//!
//! **What carrying does not fix.** A carried block entity is a record at a
//! position. If Dust broke the block at that position, the record now describes
//! a block that is not there, and vanilla will drop it and log about it on
//! load. Keeping the two in step means modelling block entities, which is a
//! later phase's work; until then the honest statement is that a round trip
//! preserves what it did not touch.
//!
//! # Light is not written
//!
//! `isLightOn` is written as **0** and no `SkyLight` or `BlockLight` array is
//! emitted, so vanilla relights the chunk when it loads it.
//!
//! This is the same position [`read`](super::read) takes from the other side,
//! and for the same reason: stored light is a cache of some engine's output,
//! Dust has its own engine, and a file that claims light Dust computed is a
//! file asserting the two engines agree. They have never been compared. Setting
//! the flag to 0 costs the loading server a relight and says something true;
//! setting it to 1 would be faster and would be a claim nothing here can back.
//!
//! The measured worlds do write light — 62 sections carried `SkyLight` and 71
//! carried `BlockLight` across those twenty-five chunks, present only where the
//! value varies within the section. Matching that is possible and is not the
//! same job as being correct about it, so it waits for the differential that
//! could tell the difference.
//!
//! # What is refused
//!
//! An id with no name. [`Ids::block_name`] returning `None` stops the write
//! ([`WriteError::UnknownBlockId`]) rather than substituting air or the default
//! state, because both of those produce a file that says a block is somewhere
//! it is not — and unlike a missing field, nothing downstream can notice.

use dust_nbt::{Compound, List, Tag, TagType};

use crate::chunk::Chunk;
use crate::container::PalettedContainer;
use crate::heightmap::HeightmapKind;

use super::{Ids, DATA_VERSION_1_21_1, STATUS_FULL};

/// Why a chunk could not be written.
///
/// Separate from [`AnvilError`](super::AnvilError) because the two failure sets
/// share nothing: a reader fails on what a file says, a writer on what a caller
/// holds. One enum covering both would have every call site matching on
/// variants it can never see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteError {
    /// A block state id the caller's table cannot name.
    UnknownBlockId { id: u32 },
    /// A biome id the caller's table cannot name.
    UnknownBiomeId { id: u32 },
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownBlockId { id } => {
                write!(f, "block state {id} has no name in this registry")
            }
            Self::UnknownBiomeId { id } => write!(f, "biome {id} has no name in this registry"),
        }
    }
}

impl std::error::Error for WriteError {}

/// The parts of a chunk file `dust-world` does not model, kept so that writing
/// a chunk back does not delete them.
///
/// See the module documentation for why this is a required argument. The seven
/// fields here are exactly the root fields that survive a round trip only by
/// being copied; everything else in a chunk file is derived from the
/// [`Chunk`] itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Carried {
    /// `block_entities`, verbatim. The records themselves, tags and all.
    pub block_entities: Option<List>,
    /// `block_ticks` — blocks scheduled to update, with their delays.
    pub block_ticks: Option<List>,
    /// `fluid_ticks`, likewise.
    pub fluid_ticks: Option<List>,
    /// `PostProcessing` — per-section lists of positions vanilla revisits after
    /// generation. One list per section, in section order.
    pub post_processing: Option<List>,
    /// `structures`, with its `References` and `starts`.
    pub structures: Option<Compound>,
    /// `InhabitedTime` — ticks a player has spent in this chunk, which drives
    /// local difficulty. Nothing in Dust advances it yet, so a chunk that is
    /// read and written keeps the number it arrived with rather than resetting
    /// somebody's hard-won difficulty to zero.
    pub inhabited_time: i64,
    /// `LastUpdate` — the game tick at which the chunk was saved.
    pub last_update: i64,
}

impl Carried {
    /// Take everything a [`Chunk`] cannot hold out of a chunk's root compound.
    ///
    /// A field that is absent or the wrong type is carried as `None` and
    /// written back in its empty form. That is deliberately lenient: this is
    /// the salvage path, and a world written by something other than vanilla is
    /// exactly when it runs.
    #[must_use]
    pub fn read_from(root: &Compound) -> Self {
        fn list(root: &Compound, name: &str) -> Option<List> {
            match root.get(name) {
                Some(Tag::List(list)) => Some(list.clone()),
                _ => None,
            }
        }
        fn long(root: &Compound, name: &str) -> i64 {
            match root.get(name) {
                Some(Tag::Long(value)) => *value,
                _ => 0,
            }
        }
        Self {
            block_entities: list(root, "block_entities"),
            block_ticks: list(root, "block_ticks"),
            fluid_ticks: list(root, "fluid_ticks"),
            post_processing: list(root, "PostProcessing"),
            structures: match root.get("structures") {
                Some(Tag::Compound(compound)) => Some(compound.clone()),
                _ => None,
            },
            inhabited_time: long(root, "InhabitedTime"),
            last_update: long(root, "LastUpdate"),
        }
    }

    /// Nothing to carry: write the empty forms.
    ///
    /// **Named for what it costs.** For a chunk Dust generated this is simply
    /// the truth — there was never a chest to lose. For a chunk that came off
    /// disk it silently deletes every block entity, every scheduled tick and
    /// every structure reference in it, and the resulting world opens perfectly.
    /// Use [`Carried::read_from`] there.
    #[must_use]
    pub fn dropped() -> Self {
        Self {
            block_entities: None,
            block_ticks: None,
            fluid_ticks: None,
            post_processing: None,
            structures: None,
            inhabited_time: 0,
            last_update: 0,
        }
    }
}

/// Render one chunk as the root compound a region file's payload holds.
///
/// The NBT root's own name is the empty string; that is the caller's to supply
/// when it serialises this, and `dust_nbt::write::to_vec("", &tag)` is what
/// vanilla's own files decode as.
pub fn chunk(chunk: &Chunk, ids: &impl Ids, carried: &Carried) -> Result<Compound, WriteError> {
    let world = chunk.world();
    let pos = chunk.pos();
    let lowest = world.min_y() / 16;

    let mut sections = List::new(TagType::Compound);
    for (index, section) in chunk.sections().iter().enumerate() {
        let y = lowest + index as i32;
        let mut compound = Compound::new();
        // `Y` is a Byte and the overworld's range is -4..=19, so nothing here
        // truncates. A world deep enough to overflow a byte is one vanilla
        // could not write either, and it would be the world's error rather than
        // this cast's.
        compound.insert("Y", Tag::Byte(y as i8));
        compound.insert(
            "block_states",
            container(section.states(), |id| {
                let (name, properties) = ids
                    .block_name(id)
                    .ok_or(WriteError::UnknownBlockId { id })?;
                let mut entry = Compound::new();
                entry.insert("Name", Tag::String(name.to_owned()));
                if !properties.is_empty() {
                    let mut bag = Compound::new();
                    for (property, value) in properties {
                        bag.insert(property, Tag::String(value.to_owned()));
                    }
                    entry.insert("Properties", Tag::Compound(bag));
                }
                Ok(Tag::Compound(entry))
            })?,
        );
        compound.insert(
            "biomes",
            container(section.biomes(), |id| {
                let name = ids
                    .biome_name(id)
                    .ok_or(WriteError::UnknownBiomeId { id })?;
                Ok(Tag::String(name.to_owned()))
            })?,
        );
        // No SkyLight, no BlockLight. See the module documentation: the flag
        // below says so, and the two statements have to agree.
        sections
            .push(Tag::Compound(compound))
            .expect("every element is a Compound");
    }

    let mut heightmaps = Compound::new();
    for kind in HeightmapKind::ALL {
        if !kind.persisted() {
            continue;
        }
        heightmaps.insert(
            kind.nbt_key(),
            Tag::LongArray(chunk.heightmaps().get(kind).as_longs().to_vec()),
        );
    }

    let mut root = Compound::new();
    root.insert("DataVersion", Tag::Int(DATA_VERSION_1_21_1));
    root.insert("xPos", Tag::Int(pos.x));
    root.insert("yPos", Tag::Int(lowest));
    root.insert("zPos", Tag::Int(pos.z));
    root.insert("Status", Tag::String(STATUS_FULL.to_owned()));
    root.insert("sections", Tag::List(sections));
    root.insert("Heightmaps", Tag::Compound(heightmaps));
    // 0, and no arrays above. A relight on load is the price of not claiming
    // two light engines agree.
    root.insert("isLightOn", Tag::Byte(0));
    root.insert(
        "block_entities",
        Tag::List(carried.block_entities.clone().unwrap_or_else(empty_list)),
    );
    root.insert(
        "block_ticks",
        Tag::List(carried.block_ticks.clone().unwrap_or_else(empty_list)),
    );
    root.insert(
        "fluid_ticks",
        Tag::List(carried.fluid_ticks.clone().unwrap_or_else(empty_list)),
    );
    root.insert(
        "PostProcessing",
        Tag::List(match carried.post_processing.clone() {
            Some(list) => list,
            // One empty list per section, which is the shape every measured
            // chunk had. Built rather than left empty because a reader that
            // indexes it by section would be reading off the end of a list
            // that is merely shorter, and nothing about that is loud.
            None => post_processing_for(chunk.sections().len()),
        }),
    );
    root.insert(
        "structures",
        Tag::Compound(carried.structures.clone().unwrap_or_else(|| {
            let mut structures = Compound::new();
            structures.insert("References", Tag::Compound(Compound::new()));
            structures.insert("starts", Tag::Compound(Compound::new()));
            structures
        })),
    );
    root.insert("InhabitedTime", Tag::Long(carried.inhabited_time));
    root.insert("LastUpdate", Tag::Long(carried.last_update));
    Ok(root)
}

/// A `block_states` or `biomes` compound: a palette of whatever the entries
/// spell as, and the indices, absent when there is one entry.
///
/// The palette and the packing both come from
/// [`PalettedContainer::to_parts`], which is already defined as the *disk*
/// form — it re-palettes to the values actually present and packs at
/// [`Strategy::disk_bits`](crate::container::Strategy::disk_bits). Reproducing
/// either here would be a second answer to a question this crate had already
/// answered once.
fn container(
    container: &PalettedContainer,
    entry: impl Fn(u32) -> Result<Tag, WriteError>,
) -> Result<Tag, WriteError> {
    let (values, data) = container.to_parts();
    let mut palette = List::new(TagType::End);
    for value in values {
        let tag = entry(value)?;
        if palette.is_empty() {
            palette = List::new(tag.tag_type());
        }
        // An NBT list holds one element type, and every tag here came from one
        // closure: a block palette is compounds throughout and a biome palette
        // is strings throughout. The list was created from the first tag's own
        // type above, so a refusal would mean the closure changed its mind
        // mid-palette, which is not a failure a caller can cause.
        palette.push(tag).expect("one closure, one element type");
    }

    let mut compound = Compound::new();
    compound.insert("palette", Tag::List(palette));
    // Absent, not empty, when every cell holds the same entry. The reader's
    // note on this is the other half: absence means uniform.
    if let Some(longs) = data {
        compound.insert("data", Tag::LongArray(longs));
    }
    Ok(Tag::Compound(compound))
}

/// `List<End>` — what an empty NBT list of any element type serialises to.
fn empty_list() -> List {
    List::new(TagType::End)
}

fn post_processing_for(sections: usize) -> List {
    let mut outer = List::new(TagType::List);
    for _ in 0..sections {
        outer
            .push(Tag::List(empty_list()))
            .expect("every element is a List");
    }
    outer
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anvil::NameTables;
    use crate::coords::ChunkPos;
    use crate::heightmap::WorldHeight;

    /// Three blocks and two biomes, numbered so that no id is its own index in
    /// any palette a test builds — an off-by-one in the packing then moves a
    /// block rather than landing on the same value.
    fn tables() -> NameTables {
        let mut tables = NameTables {
            block_registry_size: 32,
            biome_registry_size: 8,
            ..NameTables::default()
        };
        tables.blocks.insert("minecraft:air".into(), 0);
        tables.blocks.insert("minecraft:stone".into(), 5);
        tables.blocks.insert("minecraft:dirt".into(), 9);
        tables.biomes.insert("minecraft:plains".into(), 1);
        tables.biomes.insert("minecraft:desert".into(), 3);
        tables
    }

    fn air_chunk() -> Chunk {
        Chunk::uniform(ChunkPos::new(2, -3), WorldHeight::OVERWORLD, 32, 8, 0, 1)
    }

    fn written(chunk: &Chunk) -> Compound {
        super::chunk(chunk, &tables(), &Carried::dropped()).expect("every id is in the tables")
    }

    fn sections_of(root: &Compound) -> &List {
        match root.get("sections") {
            Some(Tag::List(list)) => list,
            other => panic!("sections is {other:?}"),
        }
    }

    fn section_field<'a>(root: &'a Compound, index: usize, field: &str) -> &'a Compound {
        let Some(Tag::Compound(section)) = sections_of(root).get(index) else {
            panic!("section {index} is not a compound")
        };
        match section.get(field) {
            Some(Tag::Compound(compound)) => compound,
            other => panic!("section {index}'s {field} is {other:?}"),
        }
    }

    /// The fifteen fields twenty-five chunks of a real seed-0 world carried.
    ///
    /// Asserted as a *set equality* and not as fifteen `contains` checks,
    /// because the half that matters more is the second one: a writer that
    /// added a sixteenth field would pass every containment check ever written
    /// for it, and a field vanilla does not expect is exactly the kind of thing
    /// that opens until the day it does not.
    #[test]
    fn a_chunk_carries_the_fifteen_root_fields_a_real_world_does_and_no_others() {
        let root = written(&air_chunk());
        let mut got: Vec<&str> = root.keys().collect();
        got.sort_unstable();
        assert_eq!(
            got,
            [
                "DataVersion",
                "Heightmaps",
                "InhabitedTime",
                "LastUpdate",
                "PostProcessing",
                "Status",
                "block_entities",
                "block_ticks",
                "fluid_ticks",
                "isLightOn",
                "sections",
                "structures",
                "xPos",
                "yPos",
                "zPos",
            ]
        );
    }

    #[test]
    fn the_column_and_the_worlds_floor_are_written_as_the_file_spells_them() {
        let root = written(&air_chunk());
        assert_eq!(root.get("xPos"), Some(&Tag::Int(2)));
        assert_eq!(root.get("zPos"), Some(&Tag::Int(-3)));
        // yPos is the index of the lowest section, in sections, not in blocks.
        // -64 blocks is section -4, and writing -64 here would put every chunk
        // sixty sections below the world.
        assert_eq!(root.get("yPos"), Some(&Tag::Int(-4)));
        assert_eq!(root.get("DataVersion"), Some(&Tag::Int(3955)));
        assert_eq!(
            root.get("Status"),
            Some(&Tag::String("minecraft:full".into()))
        );
    }

    /// Twenty-four, matching every measured chunk — not "as many as hold
    /// something", which for a world of air would be none.
    #[test]
    fn every_section_of_the_world_is_written_even_where_there_is_nothing_in_it() {
        let root = written(&air_chunk());
        let sections = sections_of(&root);
        assert_eq!(sections.len(), 24);
        let ys: Vec<i8> = (0..sections.len())
            .map(|index| {
                let Some(Tag::Compound(section)) = sections.get(index) else {
                    panic!("section {index} is not a compound")
                };
                match section.get("Y") {
                    Some(Tag::Byte(y)) => *y,
                    other => panic!("section {index}'s Y is {other:?}"),
                }
            })
            .collect();
        assert_eq!(ys, (-4..=19).collect::<Vec<i8>>());
    }

    /// The reader's rule from the writing side: absent means uniform, so a
    /// section where every cell agrees must not carry an array at all.
    ///
    /// Writing an empty array instead would be the mirror of the reader bug
    /// this format punishes — and it would round-trip perfectly through Dust,
    /// because Dust's own reader would then see a palette of one and never look.
    #[test]
    fn a_section_of_one_block_writes_a_palette_and_no_data() {
        let root = written(&air_chunk());
        let states = section_field(&root, 0, "block_states");
        assert_eq!(states.keys().collect::<Vec<_>>(), ["palette"]);
        match states.get("palette") {
            Some(Tag::List(list)) => assert_eq!(list.len(), 1),
            other => panic!("palette is {other:?}"),
        }
        assert!(
            states.get("data").is_none(),
            "a uniform section has no data"
        );
    }

    #[test]
    fn a_section_of_two_blocks_writes_the_indices_at_the_width_the_palette_implies() {
        let mut chunk = air_chunk();
        chunk.set_block(3, -60, 4, 5);
        let root = written(&chunk);
        let states = section_field(&root, 0, "block_states");
        match states.get("palette") {
            Some(Tag::List(list)) => assert_eq!(list.len(), 2),
            other => panic!("palette is {other:?}"),
        }
        // Two entries need one bit, but a block-state container's disk floor is
        // four, so 4096 cells occupy 4096 * 4 / 64 longs. A writer that packed
        // at the width the entry count alone implies would produce a quarter of
        // this and every chunk would be unreadable.
        match states.get("data") {
            Some(Tag::LongArray(longs)) => assert_eq!(longs.len(), 256),
            other => panic!("data is {other:?}"),
        }
    }

    /// Biomes are the case that proves the floor is per-container and not a
    /// constant: 64 cells over a two-entry palette is one bit and one long,
    /// where the block container's floor of four would give four.
    #[test]
    fn biomes_pack_at_their_own_floor_and_not_the_block_containers() {
        let mut chunk = air_chunk();
        chunk.set_biome(0, -60, 0, 3);
        let root = written(&chunk);
        match section_field(&root, 0, "biomes").get("data") {
            Some(Tag::LongArray(longs)) => assert_eq!(longs.len(), 1),
            other => panic!("biome data is {other:?}"),
        }
    }

    /// The flag and the arrays have to agree, and this asserts both halves
    /// rather than the flag alone — `isLightOn = 0` beside a stored `SkyLight`
    /// is a file that contradicts itself, and so is the reverse.
    #[test]
    fn no_light_is_written_and_the_flag_says_so() {
        let root = written(&air_chunk());
        assert_eq!(root.get("isLightOn"), Some(&Tag::Byte(0)));
        let sections = sections_of(&root);
        for index in 0..sections.len() {
            let Some(Tag::Compound(section)) = sections.get(index) else {
                panic!("section {index} is not a compound")
            };
            let mut keys: Vec<&str> = section.keys().collect();
            keys.sort_unstable();
            assert_eq!(
                keys,
                ["Y", "biomes", "block_states"],
                "section {index} carries light"
            );
        }
    }

    /// Four, not six. The two `_WG` maps are worldgen scaffolding and a file
    /// vanilla writes does not have them.
    #[test]
    fn only_the_four_persisted_heightmaps_are_written() {
        let root = written(&air_chunk());
        let Some(Tag::Compound(maps)) = root.get("Heightmaps") else {
            panic!("Heightmaps is not a compound")
        };
        let mut keys: Vec<&str> = maps.keys().collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "MOTION_BLOCKING",
                "MOTION_BLOCKING_NO_LEAVES",
                "OCEAN_FLOOR",
                "WORLD_SURFACE",
            ]
        );
    }

    /// Present and empty, which is what twenty-three of the twenty-five
    /// measured chunks had. An empty NBT list carries element type `End`
    /// whatever it would have held, so this is the tag vanilla writes too.
    #[test]
    fn the_lists_a_chunk_has_nothing_for_are_present_and_empty() {
        let root = written(&air_chunk());
        for field in ["block_entities", "block_ticks", "fluid_ticks"] {
            match root.get(field) {
                Some(Tag::List(list)) => {
                    assert!(list.is_empty(), "{field} is not empty");
                    assert_eq!(list.element_type(), TagType::End, "{field}'s element type");
                }
                other => panic!("{field} is {other:?}"),
            }
        }
    }

    /// One per section. A list that is merely shorter is the failure this
    /// guards: a reader indexing it by section number would read off the end,
    /// and nothing about that is loud.
    #[test]
    fn post_processing_has_one_list_per_section() {
        let root = written(&air_chunk());
        match root.get("PostProcessing") {
            Some(Tag::List(outer)) => {
                assert_eq!(outer.len(), 24);
                for index in 0..outer.len() {
                    match outer.get(index) {
                        Some(Tag::List(inner)) => assert!(inner.is_empty()),
                        other => panic!("PostProcessing[{index}] is {other:?}"),
                    }
                }
            }
            other => panic!("PostProcessing is {other:?}"),
        }
    }

    #[test]
    fn structures_is_written_with_both_of_its_halves() {
        let root = written(&air_chunk());
        let Some(Tag::Compound(structures)) = root.get("structures") else {
            panic!("structures is not a compound")
        };
        let mut keys: Vec<&str> = structures.keys().collect();
        keys.sort_unstable();
        assert_eq!(keys, ["References", "starts"]);
    }

    /// The refusal the module documentation promises. Substituting air would
    /// produce a file that says a block is somewhere it is not, and unlike a
    /// missing field nothing downstream could notice.
    #[test]
    fn an_id_with_no_name_stops_the_write_rather_than_becoming_something_else() {
        let mut chunk = air_chunk();
        chunk.set_block(0, -60, 0, 11);
        assert_eq!(
            super::chunk(&chunk, &tables(), &Carried::dropped()),
            Err(WriteError::UnknownBlockId { id: 11 })
        );

        let mut chunk = air_chunk();
        chunk.set_biome(0, -60, 0, 6);
        assert_eq!(
            super::chunk(&chunk, &tables(), &Carried::dropped()),
            Err(WriteError::UnknownBiomeId { id: 6 })
        );
    }

    /// `Carried` exists so that a save does not delete what `Chunk` cannot
    /// hold. This is that claim, over the one field where the loss would be a
    /// player's chest.
    #[test]
    fn what_a_chunk_cannot_model_survives_being_taken_out_and_put_back() {
        let mut chest = Compound::new();
        chest.insert("id", Tag::String("minecraft:chest".into()));
        chest.insert("x", Tag::Int(62));
        chest.insert("y", Tag::Int(-12));
        chest.insert("z", Tag::Int(18));
        chest.insert(
            "LootTable",
            Tag::String("minecraft:chests/simple_dungeon".into()),
        );
        let mut entities = List::new(TagType::Compound);
        entities
            .push(Tag::Compound(chest.clone()))
            .expect("one compound");

        let mut source = Compound::new();
        source.insert("block_entities", Tag::List(entities));
        source.insert("InhabitedTime", Tag::Long(4_200));
        source.insert("LastUpdate", Tag::Long(9_001));

        let carried = Carried::read_from(&source);
        let root = super::chunk(&air_chunk(), &tables(), &carried).expect("ids are known");

        assert_eq!(root.get("InhabitedTime"), Some(&Tag::Long(4_200)));
        assert_eq!(root.get("LastUpdate"), Some(&Tag::Long(9_001)));
        match root.get("block_entities") {
            Some(Tag::List(list)) => {
                assert_eq!(list.len(), 1);
                assert_eq!(list.get(0), Some(&Tag::Compound(chest)));
            }
            other => panic!("block_entities is {other:?}"),
        }
    }

    /// The other half of the claim, and the reason [`Carried::dropped`] is
    /// spelled the way it is: choosing it really does delete the chest, and a
    /// reader of this test can see that it does.
    #[test]
    fn dropping_what_is_carried_is_a_loss_and_not_a_no_op() {
        let mut chest = Compound::new();
        chest.insert("id", Tag::String("minecraft:chest".into()));
        let mut entities = List::new(TagType::Compound);
        entities.push(Tag::Compound(chest)).expect("one compound");
        let mut source = Compound::new();
        source.insert("block_entities", Tag::List(entities));

        let kept = super::chunk(&air_chunk(), &tables(), &Carried::read_from(&source))
            .expect("ids are known");
        let dropped =
            super::chunk(&air_chunk(), &tables(), &Carried::dropped()).expect("ids are known");
        assert_ne!(kept.get("block_entities"), dropped.get("block_entities"));
    }

    /// A field that is absent from the source, or is there with the wrong type,
    /// carries as nothing rather than stopping the salvage. This is the path
    /// that runs over a world written by something other than vanilla, which is
    /// exactly when leniency is worth more than a refusal.
    #[test]
    fn a_source_that_is_missing_or_malformed_carries_as_empty() {
        let mut source = Compound::new();
        source.insert("block_entities", Tag::Int(7));
        source.insert("InhabitedTime", Tag::String("soon".into()));
        assert_eq!(Carried::read_from(&source), Carried::dropped());
    }
}
