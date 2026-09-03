//! What survives a restart.
//!
//! # What is saved, and what is not
//!
//! The world is *generated plus edits*, so the generated half needs no saving:
//! it comes back identically from the same six lines. What cannot be
//! regenerated is the part players made — the blocks they changed, and where
//! each of them was standing.
//!
//! That is the whole file. It is **not** the Anvil format and does not pretend
//! to be: a vanilla server cannot open a Dust world and Dust cannot open one of
//! vanilla's. Region files are Phase 7 and `dust-world` already reads and
//! writes the container for them; what does not exist is the chunk NBT that
//! goes inside, which is a large format worth doing against a differential
//! rather than guessing at. Saying so here is the point — a save file that
//! looked like a world save and was not would be discovered by an operator, not
//! by a test.
//!
//! # Why blocks are saved by name
//!
//! A block state id is a position in a generated table, and that table is
//! regenerated per Minecraft version. Saving ids would make a version bump turn
//! every saved block into a different one — silently, because every id would
//! still be valid. Names cost more bytes and mean the same thing next year.
//!
//! A name the current table does not have is dropped, with a count reported
//! rather than a failure: a world that refuses to load because one block was
//! renamed is worse than a world that loads with a hole and says so.
//!
//! # Why an inventory is saved by name too, and what it does not promise
//!
//! The same argument, one register further: an item's protocol id is its
//! position in a generated table, so a saved id would survive a version bump
//! by turning into a different item. The name is written, and the count beside
//! it, and the slot number is vanilla's own `0..=45`.
//!
//! **What it promises:** the forty-six slots, the item in each, and how many —
//! plus which hotbar slot was in hand. Those come back exactly, and an item
//! this build has no entry for is dropped and named the way a block is.
//!
//! **What it does not promise:** components. A stack's name is written and its
//! data components are not, because Dust does not model them — a renamed
//! block, an enchanted tool and a shulker box with things inside it all save
//! and reload as the plain item. That is not a saving decision; it is
//! `dust_protocol::types::Slot`'s wall, and this format cannot record what
//! nothing upstream of it can read. It is written here rather than discovered
//! later, because a saved record that quietly means less than it looks like it
//! means is worse than one that refuses to be written.
//!
//! # Writing
//!
//! Written to a temporary file, flushed, and renamed over the original, because
//! a save interrupted halfway is otherwise a save file that parses to half a
//! world. Rename is atomic on every platform this runs on; a partial temporary
//! file is left behind and ignored.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use dust_protocol::types::Position;
use dust_registry::Block;
use serde::{Deserialize, Serialize};

/// One changed block, as it is written down.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedBlock {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    /// The block's namespaced name, e.g. `minecraft:grass_block`. Not its state
    /// id; see the module docs.
    pub block: String,
}

/// One stack in one slot, as it is written down.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedStack {
    /// Vanilla's own container numbering, `0..=45`. See
    /// [`crate::net::inventory`] for what each range is.
    pub slot: u8,
    /// The item's namespaced name, e.g. `minecraft:cobblestone`. Not its
    /// protocol id; see the module docs.
    pub item: String,
    pub count: u8,
    /// The stack's data-component patch, as lowercase hex of the canonical
    /// wire bytes, or absent when the stack has none — which is almost all of
    /// them. See [`Save::components`] for what this promises.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub components: Option<String>,
}

/// Where a player was when they left, and what they were carrying.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedPlayer {
    /// The profile id, hyphenated, which is what every other tool in this
    /// ecosystem uses and what an operator can paste somewhere.
    pub id: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    /// Only the slots that hold something, in slot order. An empty inventory
    /// writes an empty list rather than forty-six nulls.
    #[serde(default)]
    pub inventory: Vec<SavedStack>,
    /// Which hotbar slot was in hand, `0..9`.
    #[serde(default)]
    pub selected: u8,
}

/// The whole file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Save {
    /// Bumped when the meaning of a field changes. A file from a future
    /// version is refused rather than read hopefully.
    pub version: u32,
    #[serde(default)]
    pub blocks: Vec<SavedBlock>,
    #[serde(default)]
    pub players: Vec<SavedPlayer>,
    /// Which Minecraft version's component encoding the `components` fields
    /// are written in, or absent when no stack in the file has any.
    ///
    /// **This is the whole of what the component half promises, and it is
    /// deliberately narrow.** A component patch is stored as the protocol
    /// bytes that arrived, and those bytes only mean something against one
    /// version's component registry: the same eleven bytes are an enchantment
    /// in one version and a food value in the next, because the type ids are
    /// positions in a table Minecraft regenerates. So the version is written
    /// beside them and checked on the way back in. A file whose components
    /// were written by another version loads with its items and **without**
    /// their components, and says how many it dropped.
    ///
    /// It does not promise that a component means the same thing next year. It
    /// promises that a component this reader cannot vouch for is dropped
    /// loudly rather than handed back as a different one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub components: Option<String>,
}

/// The version this build writes and is willing to read.
///
/// Bumped to 2 when player inventories joined the file. A version 1 file still
/// loads — the new fields default to an empty inventory, which is exactly what
/// a save written before there were any means — and it is only the other
/// direction that is refused.
///
/// Components did **not** bump it to 3. A version 2 file has no `components`
/// keys and reads as forty-six componentless stacks, which is what it means;
/// a version 2 reader meeting a version 2 file that has them would ignore keys
/// it does not know, which is the one case this reasoning does not cover and
/// is why the encoding version is written in the file rather than inferred.
pub const SAVE_VERSION: u32 = 2;

/// The file's name inside the world directory.
pub const SAVE_FILE: &str = "dust-edits.json";

/// Why a save could not be read.
#[derive(Debug)]
pub enum SaveError {
    Io(std::io::Error),
    Parse(String),
    /// Written by a newer Dust. Refused rather than read: an older reader
    /// guessing at a newer file is how a save gets quietly truncated.
    FromTheFuture {
        found: u32,
        understood: u32,
    },
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Parse(why) => write!(f, "the save file is not readable: {why}"),
            Self::FromTheFuture { found, understood } => write!(
                f,
                "the save file is version {found} and this server understands \
                 {understood}; a newer Dust wrote it"
            ),
        }
    }
}

impl std::error::Error for SaveError {}

/// The save file inside `world_dir`.
pub fn path_in(world_dir: &Path) -> PathBuf {
    world_dir.join(SAVE_FILE)
}

/// Read the save, or `None` if there is not one yet.
///
/// A missing file is not an error: a world that has never been played is the
/// ordinary first case, and treating it as a failure would make a fresh install
/// look broken.
pub fn load(world_dir: &Path) -> Result<Option<Save>, SaveError> {
    let path = path_in(world_dir);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(SaveError::Io(e)),
    };
    let save: Save = serde_json::from_slice(&bytes).map_err(|e| SaveError::Parse(e.to_string()))?;
    if save.version > SAVE_VERSION {
        return Err(SaveError::FromTheFuture {
            found: save.version,
            understood: SAVE_VERSION,
        });
    }
    Ok(Some(save))
}

/// Write the save, atomically.
pub fn store(world_dir: &Path, save: &Save) -> Result<(), SaveError> {
    let path = path_in(world_dir);
    let temporary = path.with_extension("json.tmp");

    let bytes = serde_json::to_vec_pretty(save).map_err(|e| SaveError::Parse(e.to_string()))?;
    {
        let mut file = std::fs::File::create(&temporary).map_err(SaveError::Io)?;
        file.write_all(&bytes).map_err(SaveError::Io)?;
        // Flushed before the rename, not after. A rename that beat its own
        // data to disk would leave a file that exists, is the right size, and
        // holds whatever was in those blocks before.
        file.sync_all().map_err(SaveError::Io)?;
    }
    std::fs::rename(&temporary, &path).map_err(SaveError::Io)
}

/// Turn saved blocks into `(position, state)` pairs the world can take.
///
/// Returns the pairs and the names that could not be resolved, so a caller can
/// report the second rather than discover it as missing blocks.
pub fn resolve(blocks: &[SavedBlock]) -> (Vec<(Position, u32)>, Vec<String>) {
    let mut resolved = Vec::with_capacity(blocks.len());
    let mut unknown = Vec::new();
    for saved in blocks {
        match Block::from_name(&saved.block) {
            Some(block) => resolved.push((
                Position {
                    x: saved.x,
                    y: saved.y,
                    z: saved.z,
                },
                block.default_state().id(),
            )),
            None => {
                if !unknown.contains(&saved.block) {
                    unknown.push(saved.block.clone());
                }
            }
        }
    }
    (resolved, unknown)
}

/// The name to write down for a block state.
///
/// The block's name, not the state's: a state carries property values — which
/// way a stair faces — and this save format does not keep them.
///
/// **It is still lossless, and the reason it is has changed.** It used to be
/// that one block could be placed and that block had no properties. Now any of
/// the 925 blocks Minecraft has an item for can go down — stairs, logs, slabs,
/// every one of them property-carrying — and what keeps this honest is that
/// they all go down in their **default** state, because `held_block` has no
/// placement context to compute another one from.
///
/// So the day that gains a context is the day this becomes a real loss: a
/// player's stairs would come back from a restart all facing north. This is the
/// function that has to grow then, and the growth is a property map beside the
/// name plus a save version bump, so that an older build refuses the file
/// rather than silently flattening a world.
pub fn name_of(state: u32) -> Option<&'static str> {
    dust_registry::BlockState::from_id(state).map(|s| s.block().name())
}

/// Profile ids in the spelling everything else uses.
pub fn hyphenated(id: &[u8; 16]) -> String {
    let hex: String = id.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Where each player was, by raw profile id.
///
/// Keyed by the sixteen bytes rather than by the hyphenated spelling, because
/// this map is written on every movement packet — twenty a second per
/// player — and a string key means an allocation each time to look up
/// something the caller already has as an array. The hyphenated form is for
/// the file and for logs, and the conversion happens at those two edges.
pub type Positions = HashMap<[u8; 16], (f64, f64, f64)>;

/// The shared, live version of [`Positions`]: written by a session as it ends,
/// read by the next one that starts.
pub type SharedPositions = std::sync::Arc<std::sync::Mutex<Positions>>;

/// Where players were, by profile id.
pub fn positions(save: &Save) -> Positions {
    save.players
        .iter()
        .filter_map(|p| Some((parse_id(&p.id)?, (p.x, p.y, p.z))))
        .collect()
}

/// What each player was carrying, by raw profile id.
///
/// A separate map from [`Positions`] on purpose, and the reason is the access
/// pattern rather than tidiness. Positions are written twenty times a second
/// per player; an inventory is written when a player clicks, which is a few
/// times a minute at most. Putting them in one map would mean copying
/// forty-six slots on every movement packet to record that nothing about the
/// inventory changed.
pub type Inventories = HashMap<[u8; 16], Carried>;

/// The live, shared version of [`Inventories`]: written by a session when a
/// slot moves and when it ends, read by the next session that starts.
pub type SharedInventories = std::sync::Arc<std::sync::Mutex<Inventories>>;

/// One player's container, as the live map holds it.
///
/// A fixed array and a byte. Names are only spelled out at the file's edge.
///
/// No longer `Copy`: a stack carries a refcounted component patch, so cloning
/// this is forty-six branches and one refcount bump per stack that has
/// components. It is cloned when a session starts and when one ends, not per
/// packet.
#[derive(Debug, Clone)]
pub struct Carried {
    pub slots: crate::net::inventory::Slots,
    pub selected: u8,
}

/// What each player was carrying, read out of a save.
///
/// An item name this build has no entry for is dropped and collected into the
/// second return value, so a caller can name it rather than let the player
/// discover a missing slot. The same trade as a renamed block, for the same
/// reason: a world that refuses to load because one item was renamed is worse
/// than one that loads and says what it lost.
pub fn inventories(save: &Save) -> (Inventories, Vec<String>, usize) {
    let mut carried = Inventories::new();
    let mut unknown = Vec::new();
    let mut dropped_components = 0usize;
    // Components are only read back when the file says they were written for
    // the version this build speaks. Anything else is another version's bytes
    // and would decode to a different component, not to a missing one.
    let components_readable = save
        .components
        .as_deref()
        .is_some_and(|version| version == dust_registry::generated::registries::DATA_VERSION);
    for player in &save.players {
        let Some(id) = parse_id(&player.id) else {
            continue;
        };
        if player.inventory.is_empty() && player.selected == 0 {
            continue;
        }
        let mut slots: crate::net::inventory::Slots = std::array::from_fn(|_| None);
        for saved in &player.inventory {
            let index = usize::from(saved.slot);
            if index >= crate::net::inventory::SLOTS || saved.count == 0 {
                continue;
            }
            match dust_registry::Item::from_name(&saved.item) {
                Some(item) => {
                    let components = match saved.components.as_deref() {
                        None => dust_protocol::components::ComponentPatch::EMPTY,
                        Some(hex) if components_readable => {
                            match dust_protocol::components::ComponentPatch::from_hex(hex) {
                                Ok(patch) => patch,
                                Err(_) => {
                                    dropped_components += 1;
                                    dust_protocol::components::ComponentPatch::EMPTY
                                }
                            }
                        }
                        Some(_) => {
                            dropped_components += 1;
                            dust_protocol::components::ComponentPatch::EMPTY
                        }
                    };
                    slots[index] = Some(crate::net::inventory::Stack::with_components(
                        item,
                        saved.count,
                        components,
                    ));
                }
                None => {
                    if !unknown.contains(&saved.item) {
                        unknown.push(saved.item.clone());
                    }
                }
            }
        }
        carried.insert(
            id,
            Carried {
                slots,
                selected: player.selected,
            },
        );
    }
    (carried, unknown, dropped_components)
}

/// The slots that hold something, in slot order, ready to be written down.
pub fn stacks_of(carried: &Carried) -> Vec<SavedStack> {
    carried
        .slots
        .iter()
        .enumerate()
        .filter_map(|(index, stack)| {
            let stack = stack.as_ref()?;
            Some(SavedStack {
                slot: index as u8,
                item: stack.item.name().to_owned(),
                count: stack.count,
                components: stack.components.to_hex(),
            })
        })
        .collect()
}

/// The version to stamp a save's components with, or `None` when no stack in
/// it has any.
///
/// `None` rather than always the version, so that a world nobody has put a
/// named item in stays a file with no component key in it at all.
#[must_use]
pub fn components_version(players: &[SavedPlayer]) -> Option<&'static str> {
    players
        .iter()
        .flat_map(|player| player.inventory.iter())
        .any(|stack| stack.components.is_some())
        .then_some(dust_registry::generated::registries::DATA_VERSION)
}

/// Read a hyphenated profile id back into its bytes.
///
/// `None` for anything that is not one. A save hand-edited into an id that is
/// not an id loses that player's position rather than failing the boot — the
/// same trade as an unknown block name, for the same reason.
pub fn parse_id(text: &str) -> Option<[u8; 16]> {
    let hex: String = text.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 {
        return None;
    }
    let mut bytes = [0u8; 16];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "dust-save-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn a_profile_id_survives_the_trip_through_its_written_form() {
        let id = [
            0xf3, 0xd2, 0x8c, 0xb0, 0x72, 0x25, 0x3c, 0xb1, 0xba, 0xeb, 0x2d, 0xad, 0xd2, 0xbe,
            0x89, 0xae,
        ];
        let written = hyphenated(&id);
        assert_eq!(written, "f3d28cb0-7225-3cb1-baeb-2dadd2be89ae");
        assert_eq!(parse_id(&written), Some(id));
        // And anything that is not an id is a `None` rather than a panic: a
        // hand-edited save loses one player's position instead of failing the
        // boot, the same trade as an unknown block name.
        assert_eq!(parse_id("not an id"), None);
        assert_eq!(parse_id(""), None);
        assert_eq!(parse_id(&"z".repeat(32)), None);
    }

    #[test]
    fn a_world_that_has_never_been_played_is_not_an_error() {
        let dir = temp_dir("fresh");
        assert!(load(&dir).expect("a missing file is fine").is_none());
    }

    #[test]
    fn what_is_written_is_what_comes_back() {
        let dir = temp_dir("roundtrip");
        let save = Save {
            version: SAVE_VERSION,
            components: None,
            blocks: vec![SavedBlock {
                x: -1,
                y: -60,
                z: 4000,
                block: "minecraft:stone".to_owned(),
            }],
            players: vec![SavedPlayer {
                id: "f3d28cb0-7225-3cb1-baeb-2dadd2be89ae".to_owned(),
                x: 0.5,
                y: -59.0,
                z: -12.5,
                inventory: vec![
                    SavedStack {
                        slot: 9,
                        item: "minecraft:cobblestone".to_owned(),
                        count: 17,
                            components: None,
                    },
                    SavedStack {
                        slot: 45,
                        item: "minecraft:bucket".to_owned(),
                        count: 1,
                            components: None,
                    },
                ],
                selected: 4,
            }],
        };
        store(&dir, &save).expect("written");
        let back = load(&dir).expect("read").expect("present");
        assert_eq!(back.blocks.len(), 1);
        assert_eq!(back.blocks[0].block, "minecraft:stone");
        assert_eq!(back.blocks[0].z, 4000);
        assert_eq!(back.players[0].z, -12.5);

        // And the inventory comes back as slots, not as a list: the slot
        // number is the record, and a save that renumbered them on the way
        // through would put a player's things in the wrong hand.
        let (carried, unknown, _) = inventories(&back);
        assert!(unknown.is_empty(), "{unknown:?}");
        let id = parse_id("f3d28cb0-7225-3cb1-baeb-2dadd2be89ae").expect("an id");
        let mine = carried.get(&id).expect("this player carried something");
        assert_eq!(mine.selected, 4);
        assert_eq!(
            mine.slots[9].as_ref().map(|s| (s.item.name(), s.count)),
            Some(("minecraft:cobblestone", 17))
        );
        assert_eq!(
            mine.slots[45].as_ref().map(|s| (s.item.name(), s.count)),
            Some(("minecraft:bucket", 1))
        );
        assert!(mine.slots[10].is_none());
        // And back out again, in slot order and holding only what is there.
        let written = stacks_of(mine);
        assert_eq!(written.len(), 2);
        assert_eq!((written[0].slot, written[0].count), (9, 17));
        assert_eq!(written[1].slot, 45);
    }

    #[test]
    fn a_version_one_file_loads_with_an_empty_inventory_rather_than_failing() {
        // The forward half of the version bump. A save written before players
        // carried anything is not a broken save; it is a save from before, and
        // reading it as an empty inventory is what it means.
        let dir = temp_dir("v1");
        let json = r#"{
            "version": 1,
            "blocks": [],
            "players": [
                { "id": "f3d28cb0-7225-3cb1-baeb-2dadd2be89ae", "x": 1.0, "y": 2.0, "z": 3.0 }
            ]
        }"#;
        std::fs::write(path_in(&dir), json).expect("written");
        let back = load(&dir).expect("read").expect("present");
        assert_eq!(back.version, 1);
        assert_eq!(back.players[0].x, 1.0);
        assert!(back.players[0].inventory.is_empty());
        let (carried, unknown, _) = inventories(&back);
        assert!(carried.is_empty() && unknown.is_empty());
    }

    #[test]
    fn an_unknown_item_name_is_dropped_and_reported() {
        // The same trade as an unknown block, one register up. An operator who
        // changed version needs to know which item their players lost.
        let save = Save {
            version: SAVE_VERSION,
            components: None,
            blocks: Vec::new(),
            players: vec![SavedPlayer {
                id: "f3d28cb0-7225-3cb1-baeb-2dadd2be89ae".to_owned(),
                x: 0.0,
                y: 0.0,
                z: 0.0,
                inventory: vec![
                    SavedStack {
                        slot: 9,
                        item: "minecraft:cobblestone".to_owned(),
                        count: 3,
                            components: None,
                    },
                    SavedStack {
                        slot: 10,
                        item: "minecraft:unobtainium".to_owned(),
                        count: 1,
                            components: None,
                    },
                ],
                selected: 0,
            }],
        };
        let (carried, unknown, _) = inventories(&save);
        assert_eq!(unknown, vec!["minecraft:unobtainium".to_owned()]);
        let id = parse_id("f3d28cb0-7225-3cb1-baeb-2dadd2be89ae").expect("an id");
        let mine = carried.get(&id).expect("present");
        assert!(mine.slots[9].is_some());
        assert!(mine.slots[10].is_none(), "the unknown one, and only it");
    }

    #[test]
    fn a_file_from_a_newer_dust_is_refused_rather_than_guessed_at() {
        let dir = temp_dir("future");
        let save = Save {
            version: SAVE_VERSION + 1,
            components: None,
            ..Save::default()
        };
        store(&dir, &save).expect("written");
        let err = load(&dir).expect_err("a newer file must be refused");
        assert!(err.to_string().contains("newer Dust"), "{err}");
    }

    #[test]
    fn an_unknown_block_name_is_dropped_and_reported() {
        // A world that refuses to load because one block was renamed is worse
        // than a world that loads with a hole and says which name it lost.
        let blocks = vec![
            SavedBlock {
                x: 0,
                y: 0,
                z: 0,
                block: "minecraft:stone".to_owned(),
            },
            SavedBlock {
                x: 1,
                y: 0,
                z: 0,
                block: "minecraft:definitely_not_a_block".to_owned(),
            },
        ];
        let (resolved, unknown) = resolve(&blocks);
        assert_eq!(resolved.len(), 1);
        assert_eq!(unknown, vec!["minecraft:definitely_not_a_block"]);
    }

    #[test]
    fn a_state_id_round_trips_through_its_name() {
        // The property the format stands on: what `name_of` writes,
        // `resolve` reads back as the same state — for blocks with no
        // properties, which is all that can be placed today. A block with
        // properties would come back as its default, which is the loss the
        // function's own documentation names.
        let stone = Block::from_name("minecraft:stone").expect("stone exists");
        let id = stone.default_state().id();
        let name = name_of(id).expect("a state has a block");
        let (resolved, unknown) = resolve(&[SavedBlock {
            x: 0,
            y: 0,
            z: 0,
            block: name.to_owned(),
        }]);
        assert!(unknown.is_empty());
        assert_eq!(resolved[0].1, id);
    }

    #[test]
    fn a_half_written_file_never_replaces_a_whole_one() {
        // The temporary is a sibling, so a crash between create and rename
        // leaves the previous save untouched. Simulated by writing the
        // temporary by hand and checking the real file is still the old one.
        let dir = temp_dir("atomic");
        let good = Save {
            version: SAVE_VERSION,
            components: None,
            blocks: vec![SavedBlock {
                x: 7,
                y: 7,
                z: 7,
                block: "minecraft:stone".to_owned(),
            }],
            players: Vec::new(),
        };
        store(&dir, &good).expect("written");
        std::fs::write(path_in(&dir).with_extension("json.tmp"), b"{ truncated")
            .expect("a torn temporary");
        let back = load(&dir).expect("read").expect("present");
        assert_eq!(back.blocks[0].x, 7, "the previous save is intact");
    }
}
