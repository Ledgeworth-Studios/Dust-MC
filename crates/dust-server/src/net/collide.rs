//! The world, answering the one question a movement check has for it.
//!
//! # What this is
//!
//! [`dust_guard::Solidity`], implemented over the world a session is playing
//! in. The rule itself — what may be walked into and what may not — is in
//! `dust-guard` and knows nothing about a chunk; this is the half that knows
//! about chunks and nothing about the rule.
//!
//! # Why the cache, and why it needs no invalidation
//!
//! A movement packet arrives about twenty times a second per player, and
//! answering one means reading up to eight block cells. On a flat world that is
//! free: the source lends one template column to every position and reading a
//! cell out of it is an array index. On a world served from region files it is
//! not free at all — `Source` deliberately caches no parsed column, so every
//! ask rebuilds one from the file, and the module documentation there says why:
//! a column is about a megabyte and a view distance of ten is four hundred of
//! them.
//!
//! So this keeps four. Four is not a guess: a player box is 0.6 across, so the
//! cells one movement check looks at span at most two columns on x and two on
//! z, and a player standing on a chunk corner is asking about exactly four. One
//! entry would be enough for a player in the middle of a column and would
//! thrash for as long as a player stood on a boundary, which is a thing players
//! do for hours.
//!
//! **The cache never needs invalidating**, and that is the reason it is safe to
//! hold at all. What it holds is the column *as generated* — a pure function of
//! its position, from a flat template or from a region file the running server
//! never writes to. Everything that changes underneath a player is an edit, and
//! edits are read live out of [`EditedWorld`] on every single lookup, ahead of
//! the cache. A block placed into a player's path is seen by the next packet.
//!
//! # What it costs
//!
//! At most four built columns per online player, against the four hundred a
//! view distance of ten would have been, and **none at all on a flat world**,
//! where the source lends its template and there is nothing to own.
//!
//! # What is still wrong here
//!
//! A cache miss on a region-file world is a file read, a decompress, an NBT
//! parse and a light pass, inside the movement path. The cache turns that from
//! every packet into every chunk boundary a player crosses, which is about once
//! every four seconds at a walk — but the real answer is that a server should
//! keep the columns its players are standing in, and Dust does not keep any
//! column at all. That is chunk residency and it is not this.

use dust_guard::Solidity;
use dust_protocol::types::Position;
use dust_registry::constants::Flag;
use dust_registry::BlockConstants;
use dust_world::chunk::Chunk;
use dust_world::coords::ChunkPos;

use super::edits::{EditedWorld, Edits};
use super::source::Column;

/// The name of the column in `dust-constants.tsv` that says which block states
/// a player cannot stand inside.
///
/// Written by `xtask/oracle/dustoracle/BlockOracle.java` from Minecraft's own
/// `isCollisionShapeFullBlock`. A table extracted before that column existed
/// does not have it, which is a state this server runs in rather than an error:
/// see [`Ground::of`].
pub const FULL_COLLISION: &str = "full_collision";

/// How many generated columns one player keeps. See the module documentation.
const CACHED_COLUMNS: usize = 4;

/// The world a session's movement is checked against.
#[derive(Debug)]
pub struct Ground<'a> {
    world: &'a EditedWorld,
    constants: &'a BlockConstants,
    solid: Flag,
    /// The columns as generated, where the source had to build them. A flat
    /// world lends its template and never fills this.
    built: [Option<(ChunkPos, Chunk)>; CACHED_COLUMNS],
    /// Which entry the next build replaces. Round robin rather than least
    /// recently used: with four entries and at most four columns in play there
    /// is nothing for a better policy to be better than.
    next: usize,
}

impl<'a> Ground<'a> {
    /// The ground under a player, or `None` if this server cannot say what is
    /// solid.
    ///
    /// `None` is a real state and not a failure. The block constants are
    /// extracted from the operator's own jar and never shipped, so a server can
    /// legitimately be running without them, and one extracted before the
    /// `full_collision` column existed has every other column and not that one.
    /// Asking the table whether it *knows* — rather than reading what it
    /// answers when it does not — is what keeps an old table from quietly
    /// meaning "nothing is solid, so nothing is refused" in one place and
    /// "everything is solid, so nobody may move" in another.
    pub fn of(world: &'a EditedWorld, constants: Option<&'a BlockConstants>) -> Option<Self> {
        let constants = constants?;
        let solid = constants.flag(FULL_COLLISION)?;
        Some(Self {
            world,
            constants,
            solid,
            built: [const { None }; CACHED_COLUMNS],
            next: 0,
        })
    }

    /// The state at a cell, from the edits if one has touched it and from the
    /// column as generated otherwise.
    ///
    /// The caller has already resolved the column and taken the edit map's
    /// read lock, which are the two expensive parts; this is the per-cell
    /// remainder. It used to take that lock itself, once per cell, which for
    /// one movement packet was up to twelve acquisitions of the same lock —
    /// see [`super::edits::EditedWorld::edits_now`].
    fn state_at(edits: &Edits<'_>, chunk: &Chunk, x: i32, y: i32, z: i32) -> u32 {
        if !edits.is_empty() {
            if let Some(state) = edits.at(Position { x, y, z }) {
                return state;
            }
        }
        chunk.get_block((x & 15) as u32, y, (z & 15) as u32)
    }
}

impl Solidity for Ground<'_> {
    fn first_solid(&mut self, lo: (i32, i32, i32), hi: (i32, i32, i32)) -> Option<(i32, i32, i32)> {
        // A player below the floor or above the ceiling of the world is not
        // inside anything: there is no block there to be inside. Clamped rather
        // than refused, because a falling player passes through the floor of
        // the world on their way out of it and that is not a cheat — and
        // because `Chunk::get_block` panics outside the world's height.
        let height = self.world.height();
        let low = lo.1.max(height.min_y());
        let high = hi.1.min(height.max_y_exclusive() - 1);
        if low > high {
            return None;
        }
        // `&'a EditedWorld` is Copy, so this borrow outlives the `&mut self`
        // ones below it and a lent column can be read while the cache is being
        // written.
        let world = self.world;
        // Once for the whole box, rather than once for each of its cells. The
        // guard is held for the length of this call, so an edit lands either
        // side of the box and never in the middle of it.
        let edits = world.edits_now();
        for cx in (lo.0 >> 4)..=(hi.0 >> 4) {
            for cz in (lo.2 >> 4)..=(hi.2 >> 4) {
                let pos = ChunkPos::new(cx, cz);
                let mut lent = None;
                if !self.built.iter().flatten().any(|(at, _)| *at == pos) {
                    match world.template(pos) {
                        Column::Shared(chunk) => lent = Some(chunk),
                        Column::Built(chunk) => {
                            self.built[self.next] = Some((pos, chunk));
                            self.next = (self.next + 1) % CACHED_COLUMNS;
                        }
                    }
                }
                let chunk = match lent {
                    Some(chunk) => chunk,
                    None => match self
                        .built
                        .iter()
                        .flatten()
                        .find(|(at, _)| *at == pos)
                        .map(|(_, chunk)| chunk)
                    {
                        Some(chunk) => chunk,
                        // Unreachable: the column was either lent or built
                        // above. Skipping is the direction to be wrong in — a
                        // cell nobody could read is not a cell to refuse a
                        // player over.
                        None => continue,
                    },
                };
                // y outermost and x innermost, because that is the order a
                // section stores its states in and the run along x is the one
                // that is contiguous.
                for y in low..=high {
                    for z in lo.2.max(cz * 16)..=hi.2.min(cz * 16 + 15) {
                        for x in lo.0.max(cx * 16)..=hi.0.min(cx * 16 + 15) {
                            let state = Self::state_at(&edits, chunk, x, y, z);
                            if self.constants.is_set(self.solid, state) {
                                return Some((x, y, z));
                            }
                        }
                    }
                }
            }
        }
        None
    }
}

/// How many block states a player cannot stand inside, or `None` where the
/// table has no [`FULL_COLLISION`] column at all.
///
/// The `Option` is the whole point of the function: a table that does not carry
/// the column answers "not solid" for every state, and a boot message that
/// printed `0` for that would be telling an operator a fact about Minecraft
/// rather than a fact about their own file.
#[must_use]
pub fn solid_states(constants: &BlockConstants) -> Option<usize> {
    let solid = constants.flag(FULL_COLLISION)?;
    Some(
        (0..constants.len())
            .filter(|state| constants.is_set(solid, *state as u32))
            .count(),
    )
}
