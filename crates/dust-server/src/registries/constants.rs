//! The tables the oracle wrote, read from `[data] path`.
//!
//! Two of them: `dust-constants.tsv`, which says what every block state does to
//! light and to a heightmap and what it sounds like going down, and
//! `dust-items.tsv`, which says which block each item puts down. One route,
//! argued once below, because they are the same kind of value arriving from the
//! same jar in the same run.
//!
//! # The route, and why it is this one
//!
//! How much light a block state costs to enter and how much it gives off are
//! Java code in Minecraft. Decision record 0008 is the whole of that problem;
//! `cargo xtask extract --only constants` is the oracle that asks the operator's
//! own jar for the answer. What was open until now was not where the numbers
//! come from but how they reach a **server operator**, and the record listed
//! four routes: a new `dust` subcommand, the server running the oracle at boot,
//! a standalone jar beside each release, and this one.
//!
//! This one is a file in the directory the operator already populates. It costs
//! no new command grammar, puts no JDK on the boot path, and turns no class of
//! Java failure into a class of server-start failure. It is also exactly how
//! `[data] path` already behaves: read at boot if present, absent otherwise,
//! and the server says which it got.
//!
//! ```text
//! <[data] path>/
//!   dust-constants.tsv     ← this
//!   dust-items.tsv         ← and this
//!   minecraft/
//!     worldgen/biome/…
//!     dimension_type/…
//! ```
//!
//! The `dust-` prefix says who wrote them and who reads them. Everything else
//! under `[data] path` is Minecraft's own output in Minecraft's own layout, and
//! a bare `light.tsv` sitting beside `minecraft/` would look like one more of
//! them.
//!
//! # Nothing here ships
//!
//! Same rule as D6 and D7 and for the same reason. The repository holds the
//! question, the oracle that asks it and the reader for the answer; the answer
//! itself is Mojang's and lives on the operator's disk. `xtask extract --only
//! light` prints the one command that puts it there.
//!
//! # Absent is not an error
//!
//! A server without a table lights the way it always has — air passes light and
//! every other block is a wall — and says so at boot. That is wrong and is
//! measured: `cargo xtask harness light` puts it at 0.6% of cells on an inland
//! world and 3.5% on an ocean one, against 0.03% and nothing at all with a
//! table. But it is a server that runs, and a light table is not something an
//! operator can be expected to have before they have read about one.
//!
//! The same for the item table, and the same *shape* of consequence: without it
//! a player holding cobblestone puts down the world's own surface block,
//! because what a player is holding cannot be turned into a block without
//! Minecraft's answer for which block that is. Guessing by name would be right
//! about nine hundred items and wrong about sixteen — `minecraft:wheat` places
//! nothing at all, and the crop of that name comes from the seeds — which is
//! the kind of wrong that gets discovered by a player and not by a test.

use std::path::{Path, PathBuf};

use dust_registry::{BlockConstants, ItemBlocks};

/// What the block-state table is called inside `[data] path`.
pub const FILE: &str = "dust-constants.tsv";

/// What the item-to-block table is called inside `[data] path`.
pub const ITEMS_FILE: &str = "dust-items.tsv";

/// Why a light table beside the data could not be used.
#[derive(Debug)]
pub struct ConstantsFileError {
    /// The file it was reading.
    pub path: PathBuf,
    /// What was wrong with it.
    pub detail: String,
}

impl std::fmt::Display for ConstantsFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.detail)
    }
}

impl std::error::Error for ConstantsFileError {}

/// Read the light table beside a data directory, if there is one.
///
/// `root` is `[data] path` — the directory holding `minecraft/`.
///
/// # Errors
///
/// [`ConstantsFileError`] when the file exists and cannot be used. A file that is
/// there and wrong stops the server, because the alternative is a server that
/// runs with lighting quietly worse than the operator asked for. A file that
/// is not there is `Ok(None)`.
pub fn beside(root: impl AsRef<Path>) -> Result<Option<BlockConstants>, ConstantsFileError> {
    let path = root.as_ref().join(FILE);
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path).map_err(|e| ConstantsFileError {
        path: path.clone(),
        detail: e.to_string(),
    })?;
    let table = BlockConstants::parse(&text).map_err(|e| ConstantsFileError {
        path,
        detail: e.to_string(),
    })?;
    Ok(Some(table))
}

/// Read the item-to-block table beside a data directory, if there is one.
///
/// `root` is `[data] path` — the directory holding `minecraft/`.
///
/// # Errors
///
/// [`ConstantsFileError`], by the same rule [`beside`] follows and for the same
/// reason: a file that is there and wrong stops the server, because the
/// alternative is a server that read the operator's file, put it down, and
/// serves a world where half the blocks a player places are the wrong ones. A
/// file that is not there is `Ok(None)`.
pub fn items_beside(root: impl AsRef<Path>) -> Result<Option<ItemBlocks>, ConstantsFileError> {
    let path = root.as_ref().join(ITEMS_FILE);
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path).map_err(|e| ConstantsFileError {
        path: path.clone(),
        detail: e.to_string(),
    })?;
    let table = ItemBlocks::parse(&text).map_err(|e| ConstantsFileError {
        path,
        detail: e.to_string(),
    })?;
    Ok(Some(table))
}
