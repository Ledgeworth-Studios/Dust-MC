//! The blocks players have changed, and how everybody hears about it.
//!
//! # Why the edits are a layer over the world rather than inside it
//!
//! `FlatWorld` builds one column and every position shares it — which is
//! correct for a world where every position *is* the same, and stops being
//! correct the moment somebody breaks a block. The obvious repair is to give
//! the world a chunk per position and mutate those. That is also what a real
//! generator will want, and it is not what this should be yet: it would mean
//! keeping a megabyte per column for a world whose columns are still identical
//! except in the handful of cells a player has touched.
//!
//! So an edit is a `(position, state)` and the world is generated-plus-edits.
//! A chunk goes out as the template with its edits applied; a block query is
//! the edit if there is one and the template otherwise. That representation is
//! honest about what is actually different, and it is what a chunk cache would
//! be built *from* rather than something a cache would replace.
//!
//! # What is deliberately not here
//!
//! No physics, no block updates, no drops, no tool checks, and no reach
//! validation — a player may break bedrock from across the map. Every one of
//! those is a rule about *the game* rather than about the world's storage, and
//! the place they go is between this and the session, not inside either. The
//! gap is worth stating because "you can place blocks" invites the assumption
//! that placing them follows any rules at all.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use dust_protocol::types::Position;
use dust_world::chunk::Chunk;
use dust_world::coords::ChunkPos;
use tokio::sync::broadcast;

use super::world::FlatWorld;

/// A column, as the edit map keys it. Not [`ChunkPos`], because this is a hash
/// key and giving a domain type a `Hash` it does not otherwise need would put
/// the map's requirements into the world's vocabulary.
type ColumnKey = (i32, i32);

/// One column's changed cells, by `(x, y, z)` with x and z local to the column.
type ColumnEdits = HashMap<(i32, i32, i32), u32>;

/// One block changing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edit {
    pub position: Position,
    pub state: u32,
}

/// How many block changes a slow session may fall behind before it is told it
/// missed some.
///
/// A `broadcast` channel drops the oldest for a receiver that lags, and says
/// so rather than silently skipping — which matters, because a session that
/// missed an edit is showing a world that is wrong and the honest repair is to
/// resend the column rather than to carry on. Sixty-four is a couple of
/// seconds of one player mining at speed.
const EDIT_BACKLOG: usize = 64;

/// The world as it currently stands: what was generated, plus what was changed.
#[derive(Debug)]
pub struct EditedWorld {
    generated: FlatWorld,
    /// Keyed by column so applying edits to a chunk is one lookup rather than
    /// a scan of every edit in the world.
    edits: RwLock<HashMap<ColumnKey, ColumnEdits>>,
    announce: broadcast::Sender<Edit>,
}

impl EditedWorld {
    pub fn new(generated: FlatWorld) -> Self {
        let (announce, _) = broadcast::channel(EDIT_BACKLOG);
        Self {
            generated,
            edits: RwLock::new(HashMap::new()),
            announce,
        }
    }

    /// Listen for every edit made from now on.
    ///
    /// Subscribing before the first chunk is sent is what keeps a session from
    /// missing an edit in the window between generating a column and starting
    /// to listen. A duplicate — an edit both applied to the chunk and
    /// announced — is harmless, because setting a block to the state it
    /// already holds is not observable; a missed one is a wrong world.
    pub fn subscribe(&self) -> broadcast::Receiver<Edit> {
        self.announce.subscribe()
    }

    /// The column at `pos`, with every edit in it applied.
    pub fn chunk(&self, pos: ChunkPos) -> Chunk {
        let mut chunk = self.generated.column().clone();
        let edits = self.edits.read().expect("the edit map is never poisoned");
        if let Some(column) = edits.get(&(pos.x, pos.z)) {
            for ((x, y, z), state) in column {
                chunk.set_block(*x as u32, *y, *z as u32, *state);
            }
            // The heightmaps travel in the chunk packet and the client uses
            // them for lighting and for where rain lands, so a column with a
            // block missing from its surface has to say so. Recomputed rather
            // than adjusted because `set_block` above does not maintain them
            // and an adjustment that disagreed would be invisible.
            let air = self.generated.palette().air;
            chunk.recompute_heightmaps(|_, state| state != air);
        }
        chunk
    }

    /// Whether this column has been edited at all.
    ///
    /// The common case by far is that it has not, and an untouched column can
    /// be sent from the template without a clone.
    pub fn is_edited(&self, pos: ChunkPos) -> bool {
        self.edits
            .read()
            .expect("the edit map is never poisoned")
            .contains_key(&(pos.x, pos.z))
    }

    /// The column as generated, for the untouched case.
    pub fn template(&self) -> &Chunk {
        self.generated.column()
    }

    /// The state at a block position.
    pub fn block_at(&self, position: Position) -> u32 {
        let column = column_of(position);
        let local = local_of(position);
        let edits = self.edits.read().expect("the edit map is never poisoned");
        if let Some(state) = edits.get(&column).and_then(|c| c.get(&local)) {
            return *state;
        }
        self.generated
            .column()
            .get_block(local.0 as u32, local.1, local.2 as u32)
    }

    /// Change a block, and tell everyone listening.
    ///
    /// Returns `false` for a position outside the world's height, which is the
    /// one refusal here — a client is entitled to ask about y = 1000 and this
    /// is not the place to be surprised by it.
    pub fn set_block(&self, position: Position, state: u32) -> bool {
        let world = self.generated.height();
        if position.y < world.min_y() || position.y >= world.min_y() + world.height() as i32 {
            return false;
        }
        {
            let mut edits = self.edits.write().expect("the edit map is never poisoned");
            edits
                .entry(column_of(position))
                .or_default()
                .insert(local_of(position), state);
        }
        // Errors when nobody is listening, which is the ordinary state of a
        // server with no players and not a failure.
        let _ = self.announce.send(Edit { position, state });
        true
    }

    /// Apply edits read from a save, without announcing any of them.
    ///
    /// Silent because nobody is connected yet: an announcement at boot would
    /// go to a channel with no receivers, and a receiver that appeared later
    /// would be told about a change already present in the first chunk it was
    /// sent.
    pub fn restore(&self, blocks: impl IntoIterator<Item = (Position, u32)>) -> usize {
        let mut edits = self.edits.write().expect("the edit map is never poisoned");
        let mut applied = 0;
        for (position, state) in blocks {
            let world = self.generated.height();
            if position.y < world.min_y() || position.y >= world.min_y() + world.height() as i32 {
                continue;
            }
            edits
                .entry(column_of(position))
                .or_default()
                .insert(local_of(position), state);
            applied += 1;
        }
        applied
    }

    /// Every changed block, for writing down.
    ///
    /// Ordered, so two saves of the same world produce the same file and a
    /// diff between them is the changes rather than a reshuffle of a hash map.
    pub fn snapshot(&self) -> Vec<(Position, u32)> {
        let edits = self.edits.read().expect("the edit map is never poisoned");
        let mut out: Vec<(Position, u32)> = edits
            .iter()
            .flat_map(|((cx, cz), column)| {
                column.iter().map(move |((x, y, z), state)| {
                    (
                        Position {
                            x: (cx << 4) + x,
                            y: *y,
                            z: (cz << 4) + z,
                        },
                        *state,
                    )
                })
            })
            .collect();
        out.sort_by_key(|(p, _)| (p.x, p.y, p.z));
        out
    }

    /// How many blocks have been changed. For tests and for a status line.
    pub fn edit_count(&self) -> usize {
        self.edits
            .read()
            .expect("the edit map is never poisoned")
            .values()
            .map(HashMap::len)
            .sum()
    }
}

/// The column a block position falls in.
///
/// Shifting rather than dividing, for the reason spelled out in
/// [`super::view::column_of`]: `-1 / 16` is zero and the block west of the
/// origin is in column -1.
fn column_of(position: Position) -> (i32, i32) {
    (position.x >> 4, position.z >> 4)
}

/// Where in its column a block sits. The x and z are masked rather than
/// remaindered, because `-1 % 16` is `-1` and a chunk has no cell -1.
fn local_of(position: Position) -> (i32, i32, i32) {
    (position.x & 15, position.y, position.z & 15)
}

/// A handle sessions share.
pub type SharedWorld = Arc<EditedWorld>;

#[cfg(test)]
mod tests {
    use super::*;

    fn world() -> EditedWorld {
        fresh_world()
    }

    /// A second name for the same builder, so a test that already has a
    /// `world` binding can still make another one.
    fn fresh_world() -> EditedWorld {
        let palette = super::super::world::Palette::resolve().expect("the block table");
        EditedWorld::new(FlatWorld::new(palette, 0, 64))
    }

    #[test]
    fn a_snapshot_round_trips_through_restore_and_is_ordered() {
        let world = world();
        for (x, z) in [(5, 5), (-1, -1), (0, 0), (20, 3)] {
            world.set_block(
                Position {
                    x,
                    y: super::super::world::SURFACE_Y,
                    z,
                },
                0,
            );
        }
        let snapshot = world.snapshot();
        assert_eq!(snapshot.len(), 4);
        // Ordered, so two saves of one world are the same file and a diff
        // between them is the changes rather than a reshuffled hash map.
        let mut sorted = snapshot.clone();
        sorted.sort_by_key(|(p, _)| (p.x, p.y, p.z));
        assert_eq!(snapshot, sorted);

        // And the positions come back as world coordinates, not local ones —
        // the case that catches a column key folded in the wrong direction.
        assert!(snapshot.iter().any(|(p, _)| p.x == -1 && p.z == -1));
        assert!(snapshot.iter().any(|(p, _)| p.x == 20 && p.z == 3));

        let fresh = fresh_world();
        assert_eq!(fresh.restore(snapshot.clone()), 4);
        assert_eq!(fresh.snapshot(), snapshot);
    }

    #[test]
    fn an_unedited_column_reads_from_the_template() {
        let world = world();
        assert!(!world.is_edited(ChunkPos::new(0, 0)));
        assert_eq!(world.edit_count(), 0);
        let surface = Position {
            x: 0,
            y: super::super::world::SURFACE_Y,
            z: 0,
        };
        assert_ne!(world.block_at(surface), 0, "the surface is not air");
    }

    #[test]
    fn a_broken_block_reads_back_as_air_and_only_in_its_own_column() {
        let world = world();
        let here = Position {
            x: 3,
            y: super::super::world::SURFACE_Y,
            z: 5,
        };
        let elsewhere = Position { x: 4, ..here };
        let before = world.block_at(elsewhere);

        assert!(world.set_block(here, 0));
        assert_eq!(world.block_at(here), 0);
        assert_eq!(world.block_at(elsewhere), before, "one block, not two");
        assert!(world.is_edited(ChunkPos::new(0, 0)));
        assert!(!world.is_edited(ChunkPos::new(1, 0)));
        assert_eq!(world.edit_count(), 1);
    }

    #[test]
    fn negative_coordinates_land_in_the_column_and_cell_they_belong_to() {
        // The case masking and shifting exist for. Block x = -1 is cell 15 of
        // column -1; under division and remainder it is cell -1 of column 0,
        // which is not a cell at all.
        let world = world();
        let west = Position {
            x: -1,
            y: super::super::world::SURFACE_Y,
            z: -1,
        };
        assert!(world.set_block(west, 0));
        assert!(world.is_edited(ChunkPos::new(-1, -1)));
        assert_eq!(world.block_at(west), 0);

        // And the cell with the same local coordinates in the column to the
        // east is untouched, which is what says the column key is right.
        let east = Position {
            x: 15,
            y: super::super::world::SURFACE_Y,
            z: 15,
        };
        assert_ne!(world.block_at(east), 0);
    }

    #[test]
    fn a_block_outside_the_world_is_refused_rather_than_stored() {
        let world = world();
        assert!(!world.set_block(
            Position {
                x: 0,
                y: 5000,
                z: 0
            },
            0
        ));
        assert!(!world.set_block(
            Position {
                x: 0,
                y: -5000,
                z: 0
            },
            0
        ));
        assert_eq!(world.edit_count(), 0);
    }

    #[test]
    fn an_edit_reaches_a_listener_that_subscribed_first() {
        let world = world();
        let mut listener = world.subscribe();
        let here = Position {
            x: 1,
            y: super::super::world::SURFACE_Y,
            z: 1,
        };
        world.set_block(here, 0);
        let edit = listener.try_recv().expect("the edit was announced");
        assert_eq!(
            edit,
            Edit {
                position: here,
                state: 0
            }
        );
    }

    #[test]
    fn a_chunk_carries_the_edits_made_in_it() {
        let world = world();
        let here = Position {
            x: 2,
            y: super::super::world::SURFACE_Y,
            z: 2,
        };
        world.set_block(here, 0);
        let chunk = world.chunk(ChunkPos::new(0, 0));
        assert_eq!(chunk.get_block(2, super::super::world::SURFACE_Y, 2), 0);
        // And the heightmap followed the block down, which is what the client
        // uses to decide where the sky starts. `first_available` is one above
        // the highest solid block, so removing the block at SURFACE_Y takes it
        // from SURFACE_Y + 1 to SURFACE_Y — asserted exactly, because "lower
        // than before" would also pass if it had collapsed to the bedrock.
        let motion = dust_world::heightmap::HeightmapKind::MotionBlocking;
        assert_eq!(
            chunk.heightmaps().get(motion).first_available(2, 2),
            super::super::world::SURFACE_Y
        );
        assert_eq!(
            world
                .chunk(ChunkPos::new(0, 0))
                .heightmaps()
                .get(motion)
                .first_available(3, 3),
            super::super::world::SURFACE_Y + 1,
            "the column next to it is untouched"
        );
    }
}
