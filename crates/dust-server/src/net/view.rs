//! What a player currently has loaded, and what changes when they move.
//!
//! # The job
//!
//! A client holds the columns the server sent it and forgets the ones the
//! server tells it to forget. Between those two the server has to keep an
//! honest record, because sending a column twice wastes a chunk packet and
//! forgetting one that was never sent makes the client discard a column it is
//! standing on.
//!
//! So this is a set, and the whole of the design is that the set is the
//! server's *statement about the client*, not about the world. It is updated
//! only alongside the packets that make it true.
//!
//! # Why the set is exact rather than derived
//!
//! The columns in range of a position are computable, so a tempting
//! implementation recomputes the old range and the new one and diffs them. That
//! is right exactly until the first time something else sends or forgets a
//! column — a teleport, a respawn, a dimension change — and then the derived
//! answer and the client's actual contents disagree with nothing to see. Ten
//! columns of memory per player buys a record that cannot drift.

use std::collections::BTreeSet;

use dust_world::coords::ChunkPos;

/// The columns one client holds.
#[derive(Debug, Default)]
pub struct View {
    loaded: BTreeSet<(i32, i32)>,
    centre: Option<ChunkPos>,
    /// How far this view reaches, in columns. Settable, because a player may
    /// change their render distance without reconnecting; see
    /// [`View::set_radius`].
    ///
    /// Held here rather than passed to every [`View::move_to`], because a view
    /// that could be moved at a radius other than the one it was built for is
    /// a view that can disagree with itself about what it holds — the
    /// `loaded` set would then describe a shape no single radius produces, and
    /// the columns to forget would be wrong forever after.
    radius: i32,
}

/// What moving to a new centre requires.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ViewChange {
    /// Columns to send, nearest to the new centre first.
    ///
    /// Nearest first because a client renders what it has: the column under
    /// the player's feet arriving before the far corner is the difference
    /// between walking forward and waiting.
    pub send: Vec<ChunkPos>,
    /// Columns to tell the client to forget.
    pub forget: Vec<ChunkPos>,
    /// Whether the centre itself moved, and so whether the client needs a new
    /// `set_chunk_cache_center`.
    pub recentre: bool,
}

impl View {
    /// An empty view that reaches `radius` columns in every direction.
    #[must_use]
    pub fn with_radius(radius: u32) -> Self {
        Self {
            loaded: BTreeSet::new(),
            centre: None,
            // Clamped rather than cast: the configuration bounds this to 32
            // and the client's request is bounded by the configuration, so a
            // value that needed clamping would be a bug elsewhere — and the
            // one number this must never be is negative, because the loops
            // below would then produce an empty view that forgets everything.
            radius: i32::try_from(radius.clamp(1, 32)).expect("clamped to 1..=32"),
        }
    }

    /// Change how far this view reaches.
    ///
    /// Nothing is sent or forgotten here. The next [`View::move_to`] does both,
    /// from the same difference it already computes — which is the whole reason
    /// this is one line: a shrinking view forgets what fell outside it and a
    /// growing one sends what came in, and neither is a special case.
    ///
    /// Clamped like [`View::with_radius`], for the same reason.
    pub fn set_radius(&mut self, radius: u32) {
        self.radius = i32::try_from(radius.clamp(1, 32)).expect("clamped to 1..=32");
    }

    /// Move the view to `centre`, and say what that costs.
    ///
    /// Idempotent: calling it twice with the same centre returns an empty
    /// change the second time. That is what makes it safe to call on every
    /// movement packet, which arrive twenty times a second and almost never
    /// cross a chunk boundary.
    pub fn move_to(&mut self, centre: ChunkPos) -> ViewChange {
        self.move_to_limited(centre, None)
    }

    /// Move the view, taking at most `limit` of the columns it wants.
    ///
    /// **The record is updated only for what is actually returned**, which is
    /// what makes a partial pass composable: the columns left behind are still
    /// wanted, and the next call to either method returns them. A version that
    /// marked the whole square loaded and handed back a slice would leave this
    /// set describing a client that holds columns it was never sent, and the
    /// forget list wrong from then on.
    ///
    /// The columns to *forget* are never limited. They are cheap — a
    /// coordinate pair each — and a client left holding a column the server
    /// has stopped tracking is the one disagreement this type exists to
    /// prevent.
    pub fn move_to_limited(&mut self, centre: ChunkPos, limit: Option<usize>) -> ViewChange {
        let recentre = self.centre != Some(centre);
        self.centre = Some(centre);

        let radius = self.radius;
        let mut wanted = BTreeSet::new();
        for dx in -radius..=radius {
            for dz in -radius..=radius {
                wanted.insert((centre.x + dx, centre.z + dz));
            }
        }

        let mut send: Vec<ChunkPos> = wanted
            .difference(&self.loaded)
            .map(|(x, z)| ChunkPos::new(*x, *z))
            .collect();
        send.sort_by_key(|pos| distance_squared(*pos, centre));
        if let Some(limit) = limit {
            send.truncate(limit);
        }

        let forget: Vec<ChunkPos> = self
            .loaded
            .difference(&wanted)
            .map(|(x, z)| ChunkPos::new(*x, *z))
            .collect();

        // The record is updated here rather than by the caller, and that is a
        // deliberate coupling: a caller that could take the change and not
        // apply it would be a caller that can leave this set describing a
        // client it does not describe.
        //
        // **What the client holds is what it kept plus what it is being sent**,
        // which is only the same as `wanted` when nothing was limited. Writing
        // `wanted` here would mark a truncated pass as complete and the columns
        // it did not send would never be sent at all.
        self.loaded.retain(|column| wanted.contains(column));
        self.loaded.extend(send.iter().map(|pos| (pos.x, pos.z)));

        ViewChange {
            send,
            forget,
            recentre,
        }
    }

    /// The column the view is centred on, or `None` before the first move.
    pub fn centre(&self) -> Option<ChunkPos> {
        self.centre
    }

    /// How many columns the client is holding.
    pub fn loaded(&self) -> usize {
        self.loaded.len()
    }

    /// Whether the client holds this column.
    pub fn holds(&self, pos: ChunkPos) -> bool {
        self.loaded.contains(&(pos.x, pos.z))
    }
}

fn distance_squared(pos: ChunkPos, centre: ChunkPos) -> i64 {
    let dx = i64::from(pos.x - centre.x);
    let dz = i64::from(pos.z - centre.z);
    dx * dx + dz * dz
}

/// Which column a block position falls in.
///
/// Arithmetic shift rather than division: a column is sixteen blocks and
/// `-1 / 16` is zero while `-1 >> 4` is minus one. Every coordinate west or
/// north of the origin takes the wrong column under division, and the mistake
/// is invisible until somebody walks across x = 0.
pub fn column_of(x: f64, z: f64) -> ChunkPos {
    ChunkPos::new(
        (x.floor() as i64 >> 4) as i32,
        (z.floor() as i64 >> 4) as i32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_move_sends_the_whole_square_and_forgets_nothing() {
        let mut view = View::with_radius(2);
        let change = view.move_to(ChunkPos::new(0, 0));
        assert_eq!(change.send.len(), 25, "(2*2+1)^2");
        assert!(change.forget.is_empty());
        assert!(change.recentre);
        assert_eq!(view.loaded(), 25);
    }

    #[test]
    fn the_nearest_column_is_sent_first() {
        let mut view = View::with_radius(2);
        let change = view.move_to(ChunkPos::new(0, 0));
        assert_eq!(change.send[0], ChunkPos::new(0, 0), "the one underfoot");
        let last = change.send.last().expect("not empty");
        assert_eq!(
            distance_squared(*last, ChunkPos::new(0, 0)),
            8,
            "and a corner last"
        );
    }

    #[test]
    fn staying_still_costs_nothing() {
        let mut view = View::with_radius(2);
        view.move_to(ChunkPos::new(0, 0));
        let change = view.move_to(ChunkPos::new(0, 0));
        assert_eq!(change, ViewChange::default());
        assert!(!change.recentre);
    }

    #[test]
    fn one_step_sends_and_forgets_one_edge_each() {
        let mut view = View::with_radius(2);
        view.move_to(ChunkPos::new(0, 0));
        let change = view.move_to(ChunkPos::new(1, 0));
        // A five-by-five square stepping one east gains a column of five and
        // loses a column of five. Anything else means the set and the square
        // disagree.
        assert_eq!(change.send.len(), 5);
        assert_eq!(change.forget.len(), 5);
        assert!(change.send.iter().all(|pos| pos.x == 3));
        assert!(change.forget.iter().all(|pos| pos.x == -2));
        assert_eq!(view.loaded(), 25, "the square is still a square");
    }

    #[test]
    fn a_jump_beyond_the_view_replaces_everything() {
        let mut view = View::with_radius(2);
        view.move_to(ChunkPos::new(0, 0));
        let change = view.move_to(ChunkPos::new(100, 100));
        assert_eq!(change.send.len(), 25);
        assert_eq!(change.forget.len(), 25, "nothing overlaps");
    }

    #[test]
    fn no_column_is_ever_sent_twice_across_a_walk() {
        // Walk a long way in a straight line and require that every column the
        // client is told about is one it does not already hold. Sending a
        // column twice is invisible on the wire and costs a chunk packet each
        // time; over a thousand blocks it is a hundred of them.
        let mut view = View::with_radius(2);
        let mut held: BTreeSet<(i32, i32)> = BTreeSet::new();
        for x in 0..64 {
            let change = view.move_to(ChunkPos::new(x, 0));
            for pos in &change.forget {
                assert!(
                    held.remove(&(pos.x, pos.z)),
                    "forgot {pos:?}, which the client was never sent"
                );
            }
            for pos in &change.send {
                assert!(
                    held.insert((pos.x, pos.z)),
                    "sent {pos:?} twice without forgetting it in between"
                );
            }
            assert_eq!(held.len(), view.loaded());
        }
    }

    #[test]
    fn a_column_is_found_by_shifting_rather_than_dividing() {
        assert_eq!(column_of(0.5, 0.5), ChunkPos::new(0, 0));
        assert_eq!(column_of(15.9, 15.9), ChunkPos::new(0, 0));
        assert_eq!(column_of(16.0, 16.0), ChunkPos::new(1, 1));
        // The cases division gets wrong: everything in the block west of the
        // origin belongs to column -1, and `-1 / 16` is 0.
        assert_eq!(column_of(-0.5, -0.5), ChunkPos::new(-1, -1));
        assert_eq!(column_of(-16.0, -16.0), ChunkPos::new(-1, -1));
        assert_eq!(column_of(-16.1, -16.1), ChunkPos::new(-2, -2));
    }
}

#[cfg(test)]
mod limited_tests {
    use super::*;

    /// A truncated pass leaves the rest wanted, and the next one sends it.
    ///
    /// This is the property that lets a join end the loading screen after the
    /// near square and stream the rest afterwards. Without it — if a limited
    /// pass marked the whole square loaded — the columns it skipped would
    /// never be sent, and a player would stand in a hole ringed by nothing.
    #[test]
    fn a_limited_move_leaves_the_rest_to_the_next_one() {
        let mut view = View::with_radius(2);
        let centre = ChunkPos::new(0, 0);

        let near = view.move_to_limited(centre, Some(9));
        assert_eq!(near.send.len(), 9, "the nine nearest");
        assert!(near.recentre);

        let rest = view.move_to(centre);
        assert_eq!(rest.send.len(), 16, "the other sixteen of the five by five");
        assert!(!rest.recentre, "the centre did not move");

        // Together, the whole square and no repeats.
        let mut all: Vec<ChunkPos> = near.send;
        all.extend(rest.send);
        all.sort_by_key(|pos| (pos.x, pos.z));
        all.dedup();
        assert_eq!(all.len(), 25);

        // And a third pass has nothing left to say.
        assert!(view.move_to(centre).send.is_empty());
    }

    /// The nearest are sent first, so a limited pass sends what can be seen.
    #[test]
    fn a_limited_move_takes_the_nearest() {
        let mut view = View::with_radius(4);
        let change = view.move_to_limited(ChunkPos::new(0, 0), Some(5));
        assert_eq!(change.send.len(), 5);
        for pos in &change.send {
            assert!(
                pos.x.abs() <= 1 && pos.z.abs() <= 1,
                "{pos:?} is not one of the five nearest columns"
            );
        }
    }

    /// Forgetting is never limited, even when sending is.
    ///
    /// A column is a coordinate pair to forget and a megabyte to send, and a
    /// client left holding a column the server has stopped tracking is exactly
    /// the disagreement this type exists to prevent.
    #[test]
    fn a_limited_move_still_forgets_everything_it_should() {
        let mut view = View::with_radius(2);
        view.move_to(ChunkPos::new(0, 0));
        let change = view.move_to_limited(ChunkPos::new(20, 20), Some(1));
        assert_eq!(change.send.len(), 1, "sending is limited");
        assert_eq!(
            change.forget.len(),
            25,
            "forgetting is not: every column of the old square is gone"
        );
    }
}
