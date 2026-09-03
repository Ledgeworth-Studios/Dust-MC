//! The columns the server keeps because somebody is standing near them.
//!
//! # The problem this exists for
//!
//! [D20](../../../../docs/decisions/0020-what-a-movement-check-really-costs-on-a-saved-world.md)
//! measured a movement check on a world read from region files at **8.8
//! microseconds a packet, 97% of which was rebuilding a chunk column out of a
//! region file inside the network path** — about 0.9 ms a build, nineteen
//! builds in a 432-block walk. That is two things at once and only one of them
//! is a number:
//!
//! - **A player feels it.** 0.9 ms of file read, decompress, NBT parse and
//!   light pass happens on the session's own task, between a movement packet
//!   arriving and the reply going out. Walking into new terrain hitches, and a
//!   hundred players walking into new terrain at once is a server that stops
//!   answering.
//! - **It is paid per player.** [`super::collide::Ground`] keeps four built
//!   columns *per session*, so two players standing in the same place hold two
//!   copies of the same megabyte.
//!
//! # What this is instead
//!
//! One copy of a column for the server, held while a player is near it and
//! dropped when none is. A column is an [`Arc<Chunk>`], so a second player
//! arriving costs a refcount and not a build.
//!
//! # Who owns a column, and when it is dropped
//!
//! A session **holds** the [`RESIDENT_RADIUS`] ring around the column it is
//! standing in — nine columns — and releases them when it moves or ends. A
//! column with at least one holder is never dropped. Nine and not the view's
//! several hundred, because this is what the *movement check* needs and it is
//! decided by arithmetic rather than by taste: a player box is 0.6 across, so
//! the cells one check reads span at most two columns on each axis and are
//! always inside the ring around the column the player is in.
//!
//! The ring is held from the player's **current** column, so crossing a
//! boundary is what asks for the five new columns ahead. A player who has just
//! crossed is standing at the edge of their new column and their box is still
//! over the two columns that were already resident; they have to walk the
//! width of a column — sixteen blocks, 1.6 seconds even at the speed limit
//! `dust_guard::SpeedLimit` allows and about 3.7 at a walk — before they can
//! touch one of the new ones. Five builds is 4.5 ms. **The margin is about
//! three hundred to one, and it is bounded by the speed limit rather than by a
//! lock**: the only way to reach a column that is not yet built is to move
//! faster than the server will believe, which is separately refused.
//!
//! A column that loses its last holder is **retired rather than dropped**, and
//! the retired ones go wholesale when there are more than [`RETIRED_CAP`] of
//! them. That is one line of policy for one real player: somebody walking back
//! and forth across a chunk boundary — a corridor, a farm, a doorway — would
//! otherwise rebuild the same column every few seconds. The measurement is in
//! the decision record; a there-and-back walk builds 27 columns without the
//! retired tier and 9 with it.
//!
//! # What it costs
//!
//! Nine columns a player, shared, and zero on a flat world, which lends one
//! template column to every position and has nothing to be resident. Against
//! the four built columns *per session* that [`super::collide::Ground`] holds
//! today, this is fewer columns per player and they stop being per player at
//! all: ten players in one place hold nine columns between them rather than
//! forty.
//!
//! # Thread safety, and what "serialised" means here
//!
//! One `RwLock` over one map, and **the lock is never held across a region
//! read**. What that buys, caller by caller:
//!
//! - **Movement checks**, on every session's task on every tokio worker
//!   thread, take the read lock, clone an `Arc` and drop it. They never build
//!   anything while holding it and never wait on a disk.
//! - **Holds and releases** take the write lock for the length of nine hash
//!   lookups. They do no I/O at all, which is why they are safe to do on the
//!   session's own task: a hold is a refcount, and only the *build* is
//!   expensive.
//! - **[`Residency::fill`]**, from whatever thread built the column, takes the
//!   write lock to insert one entry. The build happened outside it.
//!
//! It is deliberately **not** serialised against [`super::source::AnvilWorld`]'s
//! region-file mutex. Two sessions warming the same cold column will both build
//! it and the second `fill` will find it already there and drop its copy. That
//! is duplicated work, bounded to once per column per simultaneous arrival, and
//! it is the trade taken on purpose: holding this lock across the region mutex
//! would put disk latency inside every movement check on the server.
//!
//! An `Arc<Chunk>` that has already been handed out **outlives eviction**. The
//! last holder leaving drops the map's entry, not the chunk, so a movement
//! check reading a column that was retired mid-read sees the whole column it
//! started with rather than a freed or half-replaced one.
//!
//! # Why this needs no invalidation
//!
//! It holds the column **as generated** — a pure function of its position, from
//! a region file the running server never writes to — which is the same reason
//! [`super::collide::Ground`]'s per-session cache needs none. Everything that
//! changes under a player is an edit, and edits are read live out of
//! [`super::edits::EditedWorld`] ahead of any column, on every lookup.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use dust_world::chunk::Chunk;
use dust_world::coords::ChunkPos;

/// How far around a player's own column the server keeps columns resident, in
/// columns. See the module documentation for why one is enough and what the
/// margin is.
pub const RESIDENT_RADIUS: i32 = 1;

/// How many columns nobody is holding are kept before all of them are dropped.
///
/// A bound, not a working set. The columns anybody is standing near are held
/// and are not in this number at all; this is only how much of a walk that
/// never comes back is remembered on the chance that it does. Sixty-four is
/// about seven players' worth of rings, and it is emptied wholesale rather
/// than a row at a time for the same reason the sky-floor cache is: a set that
/// has passed the cap is mostly columns nobody is near, and the cost of being
/// wrong is building the current ring once.
pub const RETIRED_CAP: usize = 64;

/// One resident column.
struct Resident {
    /// The column as generated, or `None` for a column that is spoken for and
    /// not yet built. The two are separate states because a hold is taken on
    /// the session's own task and the build is not: between the two, the
    /// column is claimed by somebody and there is nothing to read yet.
    chunk: Option<Arc<Chunk>>,
    /// How many players' rings cover this column. Zero is a retired column —
    /// kept, not dropped, until [`RETIRED_CAP`] of them have accumulated.
    holders: u32,
}

/// The columns the server is keeping.
#[derive(Default)]
pub struct Residency {
    columns: RwLock<HashMap<(i32, i32), Resident>>,
}

impl std::fmt::Debug for Residency {
    /// What a reader wants is how many columns are being kept and how many of
    /// them nobody is standing near, which are the two numbers the policy is
    /// about.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let columns = self.columns.read().expect("the residency is never poisoned");
        f.debug_struct("Residency")
            .field("columns", &columns.len())
            .field(
                "retired",
                &columns.values().filter(|c| c.holders == 0).count(),
            )
            .finish()
    }
}

/// Every column within `RESIDENT_RADIUS` of `centre`.
fn ring(centre: ChunkPos) -> impl Iterator<Item = (i32, i32)> {
    (-RESIDENT_RADIUS..=RESIDENT_RADIUS).flat_map(move |dx| {
        (-RESIDENT_RADIUS..=RESIDENT_RADIUS).map(move |dz| (centre.x + dx, centre.z + dz))
    })
}

impl Residency {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The column at `pos`, if the server is keeping one and it has been built.
    ///
    /// A read lock and an `Arc` clone. `None` means "ask the source", never
    /// "there is nothing there": a caller that gets `None` builds the column
    /// itself, which is exactly what every caller did before this existed.
    #[must_use]
    pub fn resident(&self, pos: ChunkPos) -> Option<Arc<Chunk>> {
        self.columns
            .read()
            .expect("the residency is never poisoned")
            .get(&(pos.x, pos.z))?
            .chunk
            .clone()
    }

    /// Claim the ring around `centre` for one player.
    ///
    /// Refcounts and nothing else — no file is opened here, which is what makes
    /// this safe to call from the session task that just read a movement
    /// packet. [`Residency::cold`] and [`Residency::fill`] are the half that
    /// costs something, and they belong on another thread.
    pub fn hold(&self, centre: ChunkPos) {
        let mut columns = self.columns.write().expect("the residency is never poisoned");
        for key in ring(centre) {
            let entry = columns.entry(key).or_insert(Resident {
                chunk: None,
                holders: 0,
            });
            entry.holders += 1;
        }
    }

    /// Give up one player's claim on the ring around `centre`.
    ///
    /// A column that loses its last holder stays in the map with `holders` at
    /// zero — retired, not dropped — until there are more than [`RETIRED_CAP`]
    /// such columns, at which point all of them go at once.
    pub fn release(&self, centre: ChunkPos) {
        let mut columns = self.columns.write().expect("the residency is never poisoned");
        for key in ring(centre) {
            if let Some(entry) = columns.get_mut(&key) {
                entry.holders = entry.holders.saturating_sub(1);
            }
        }
        if columns.values().filter(|c| c.holders == 0).count() > RETIRED_CAP {
            columns.retain(|_, c| c.holders > 0);
        }
    }

    /// The columns in the ring around `centre` that are held and not yet built.
    ///
    /// The list a warming thread works through. Taken as a snapshot under the
    /// read lock and then let go of, because building them is the part that
    /// takes a millisecond each and no lock may be held across it.
    #[must_use]
    pub fn cold(&self, centre: ChunkPos) -> Vec<ChunkPos> {
        let columns = self.columns.read().expect("the residency is never poisoned");
        ring(centre)
            .filter(|key| columns.get(key).is_some_and(|c| c.chunk.is_none()))
            .map(|(x, z)| ChunkPos::new(x, z))
            .collect()
    }

    /// Put a built column in, if anybody is still keeping it.
    ///
    /// Returns whether it was kept. `false` is the ordinary outcome for a
    /// player who walked away while their column was being read, and for the
    /// loser of a race between two sessions warming the same column: the
    /// second one to arrive finds a chunk already there and its own copy is
    /// dropped on the spot. Duplicated work, never duplicated memory.
    pub fn fill(&self, pos: ChunkPos, chunk: Chunk) -> bool {
        let mut columns = self.columns.write().expect("the residency is never poisoned");
        match columns.get_mut(&(pos.x, pos.z)) {
            Some(entry) if entry.chunk.is_none() => {
                entry.chunk = Some(Arc::new(chunk));
                true
            }
            _ => false,
        }
    }

    /// How many columns the server is keeping, held and retired together.
    #[must_use]
    pub fn len(&self) -> usize {
        self.columns
            .read()
            .expect("the residency is never poisoned")
            .len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One player's claim on the columns around them, given up however their
/// session ends.
///
/// A guard rather than a pair of calls, and for the same reason
/// `Counters::player_joined` is one: a session ends by disconnecting, by timing
/// out, by failing to decode a packet and by panicking, and a claim that is
/// only released on the tidy path is a claim that is sometimes never released.
/// A residency that leaks nine columns per crashed session is the memory leak
/// this whole module exists to not be.
pub struct Residence {
    world: super::edits::SharedWorld,
    /// The column this player was last known to be standing in, or `None` for
    /// a session that has not been placed yet.
    centre: Option<ChunkPos>,
}

impl std::fmt::Debug for Residence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Residence")
            .field("centre", &self.centre)
            .finish_non_exhaustive()
    }
}

impl Residence {
    #[must_use]
    pub fn new(world: super::edits::SharedWorld) -> Self {
        Self {
            world,
            centre: None,
        }
    }

    /// Move this player's claim to the ring around `centre`, and say whether
    /// that was a change.
    ///
    /// `false` means the player is still in the column they were in, which is
    /// what all but about one movement packet in seventy says, and it costs a
    /// comparison. `true` means the caller should warm the new ring — **off
    /// this thread**, because warming reads region files and this does not.
    ///
    /// The new ring is claimed *before* the old one is given up, so a column
    /// in both — which is six of the nine, for a step across a boundary —
    /// never falls to zero holders in between and is never retired and rebuilt
    /// for a player who did not leave it.
    pub fn move_to(&mut self, centre: ChunkPos) -> bool {
        if self.centre == Some(centre) {
            return false;
        }
        self.world.hold(centre);
        if let Some(previous) = self.centre.replace(centre) {
            self.world.release(previous);
        }
        true
    }
}

impl Drop for Residence {
    fn drop(&mut self) {
        if let Some(centre) = self.centre {
            self.world.release(centre);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dust_world::heightmap::WorldHeight;

    fn a_column() -> Chunk {
        Chunk::uniform(ChunkPos::new(0, 0), WorldHeight::new(-64, 384), 2, 2, 0, 0)
    }

    fn at(x: i32, z: i32) -> ChunkPos {
        ChunkPos::new(x, z)
    }

    #[test]
    fn a_hold_claims_the_ring_and_nothing_further() {
        let residency = Residency::new();
        residency.hold(at(0, 0));
        assert_eq!(residency.len(), 9);
        assert_eq!(residency.cold(at(0, 0)).len(), 9);
        // Two columns away is outside the ring and was never claimed, so the
        // caller that asks about it gets nothing and builds its own.
        assert!(residency.resident(at(2, 0)).is_none());
    }

    #[test]
    fn a_filled_column_is_shared_rather_than_copied() {
        let residency = Residency::new();
        residency.hold(at(0, 0));
        assert!(residency.fill(at(0, 0), a_column()));
        let one = residency.resident(at(0, 0)).expect("just filled");
        let two = residency.resident(at(0, 0)).expect("still there");
        assert!(Arc::ptr_eq(&one, &two), "two readers, one column");
        // The loser of a race drops its own copy rather than replacing the
        // one that is already being read.
        assert!(!residency.fill(at(0, 0), a_column()));
    }

    #[test]
    fn a_column_nobody_holds_is_only_dropped_once_the_retired_cap_is_passed() {
        let residency = Residency::new();
        residency.hold(at(0, 0));
        residency.release(at(0, 0));
        // Retired, not dropped: the player who walks back over the boundary
        // they just crossed finds their column still there.
        assert_eq!(residency.len(), 9);
        // Enough traffic to pass the cap, and every unheld column goes.
        for x in 0..40 {
            residency.hold(at(x * 4, 0));
            residency.release(at(x * 4, 0));
        }
        assert!(
            residency.len() <= RETIRED_CAP + 9,
            "the retired tier is a bound, not a working set: {}",
            residency.len()
        );
    }

    #[test]
    fn a_column_two_players_hold_survives_one_of_them_leaving() {
        let residency = Residency::new();
        residency.hold(at(0, 0));
        residency.hold(at(1, 0));
        residency.fill(at(1, 0), a_column());
        residency.release(at(0, 0));
        // (1,0) is in both rings, so the first player leaving does not retire
        // it — and the second player's movement check still reads it.
        assert!(residency.resident(at(1, 0)).is_some());
        let columns = residency.columns.read().expect("not poisoned");
        assert_eq!(columns[&(1, 0)].holders, 1);
    }

    #[test]
    fn a_fill_for_a_column_nobody_kept_is_dropped() {
        let residency = Residency::new();
        assert!(!residency.fill(at(5, 5), a_column()));
        assert_eq!(residency.len(), 0);
    }
}
