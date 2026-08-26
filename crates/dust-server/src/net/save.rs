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

/// Where a player was when they left.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedPlayer {
    /// The profile id, hyphenated, which is what every other tool in this
    /// ecosystem uses and what an operator can paste somewhere.
    pub id: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
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
}

/// The version this build writes and is willing to read.
pub const SAVE_VERSION: u32 = 1;

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
/// way a stair faces — and this save format does not keep them. That is a real
/// loss and it is bounded by what can currently be placed, which is one block
/// with no properties. It is named here so the day placing gains properties,
/// this is the function that has to grow rather than the bug that has to be
/// found.
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

/// Where each player was, by hyphenated profile id.
///
/// Named because it crosses three module boundaries and a tuple of three
/// floats behind two smart pointers is not a type anybody should have to read
/// twice.
pub type Positions = HashMap<String, (f64, f64, f64)>;

/// The shared, live version of [`Positions`]: written by a session as it ends,
/// read by the next one that starts.
pub type SharedPositions = std::sync::Arc<std::sync::Mutex<Positions>>;

/// Where players were, by profile id.
pub fn positions(save: &Save) -> Positions {
    save.players
        .iter()
        .map(|p| (p.id.clone(), (p.x, p.y, p.z)))
        .collect()
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
    fn a_world_that_has_never_been_played_is_not_an_error() {
        let dir = temp_dir("fresh");
        assert!(load(&dir).expect("a missing file is fine").is_none());
    }

    #[test]
    fn what_is_written_is_what_comes_back() {
        let dir = temp_dir("roundtrip");
        let save = Save {
            version: SAVE_VERSION,
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
            }],
        };
        store(&dir, &save).expect("written");
        let back = load(&dir).expect("read").expect("present");
        assert_eq!(back.blocks.len(), 1);
        assert_eq!(back.blocks[0].block, "minecraft:stone");
        assert_eq!(back.blocks[0].z, 4000);
        assert_eq!(back.players[0].z, -12.5);
    }

    #[test]
    fn a_file_from_a_newer_dust_is_refused_rather_than_guessed_at() {
        let dir = temp_dir("future");
        let save = Save {
            version: SAVE_VERSION + 1,
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
