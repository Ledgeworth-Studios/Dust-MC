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
    /// Move the view to `centre` at `radius`, and say what that costs.
    ///
    /// Idempotent: calling it twice with the same centre returns an empty
    /// change the second time. That is what makes it safe to call on every
    /// movement packet, which arrive twenty times a second and almost never
    /// cross a chunk boundary.
    pub fn move_to(&mut self, centre: ChunkPos, radius: i32) -> ViewChange {
        let recentre = self.centre != Some(centre);
        self.centre = Some(centre);

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

        let forget: Vec<ChunkPos> = self
            .loaded
            .difference(&wanted)
            .map(|(x, z)| ChunkPos::new(*x, *z))
            .collect();

        // The record is updated here rather than by the caller, and that is a
        // deliberate coupling: a caller that could take the change and not
        // apply it would be a caller that can leave this set describing a
        // client it does not describe.
        self.loaded = wanted;

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
        let mut view = View::default();
        let change = view.move_to(ChunkPos::new(0, 0), 2);
        assert_eq!(change.send.len(), 25, "(2*2+1)^2");
        assert!(change.forget.is_empty());
        assert!(change.recentre);
        assert_eq!(view.loaded(), 25);
    }

    #[test]
    fn the_nearest_column_is_sent_first() {
        let mut view = View::default();
        let change = view.move_to(ChunkPos::new(0, 0), 2);
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
        let mut view = View::default();
        view.move_to(ChunkPos::new(0, 0), 2);
        let change = view.move_to(ChunkPos::new(0, 0), 2);
        assert_eq!(change, ViewChange::default());
        assert!(!change.recentre);
    }

    #[test]
    fn one_step_sends_and_forgets_one_edge_each() {
        let mut view = View::default();
        view.move_to(ChunkPos::new(0, 0), 2);
        let change = view.move_to(ChunkPos::new(1, 0), 2);
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
        let mut view = View::default();
        view.move_to(ChunkPos::new(0, 0), 2);
        let change = view.move_to(ChunkPos::new(100, 100), 2);
        assert_eq!(change.send.len(), 25);
        assert_eq!(change.forget.len(), 25, "nothing overlaps");
    }

    #[test]
    fn no_column_is_ever_sent_twice_across_a_walk() {
        // Walk a long way in a straight line and require that every column the
        // client is told about is one it does not already hold. Sending a
        // column twice is invisible on the wire and costs a chunk packet each
        // time; over a thousand blocks it is a hundred of them.
        let mut view = View::default();
        let mut held: BTreeSet<(i32, i32)> = BTreeSet::new();
        for x in 0..64 {
            let change = view.move_to(ChunkPos::new(x, 0), 3);
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
