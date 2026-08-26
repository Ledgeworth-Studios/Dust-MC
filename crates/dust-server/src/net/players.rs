//! Who else is here.
//!
//! # What a player has to be told about another player
//!
//! Two things, and they are separate on the wire because they are separate
//! ideas. A **tab list entry** says a name exists on this server; an **entity**
//! says a body exists at coordinates. A client shown the entity without the
//! entry renders a player with no name plate and no skin; one shown the entry
//! without the entity sees a name in the list and empty air where it should be
//! standing.
//!
//! # Why this is a registry with a broadcast, like the edits next door
//!
//! The same shape solves the same problem: something happens in one session
//! that every other session has to hear about, and the sessions do not know
//! each other. A joining player also has to learn about everyone already here,
//! which the broadcast cannot tell it — a channel carries what happens next,
//! not what already happened. So the registry holds the current roster and the
//! channel carries the changes, and a session reads the first *while holding
//! the lock that stops the second changing*, so nothing falls between them.
//!
//! That ordering is the whole subtlety and it is worth stating: subscribe,
//! then snapshot, and only then release. Snapshot-then-subscribe loses anyone
//! who joined in between; subscribe-then-snapshot without the lock can show
//! somebody twice, which is harmless, and can also miss the removal of
//! somebody the snapshot included, which is not — that player stays on screen
//! forever.
//!
//! # Entity ids
//!
//! Allocated here rather than being a constant per session, because two
//! players sharing an entity id means each one's movement moves the other. The
//! counter never reuses an id: reuse is correct only if every client has
//! already been told the old entity is gone, and "every client" is exactly the
//! thing this type cannot wait for.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;

/// How many roster changes a slow session may fall behind before it is told.
///
/// Smaller than the block-edit backlog because joins and leaves are rare and a
/// session that missed one is showing a player who is not there — which is
/// repaired by rebuilding the roster, and the smaller the number the sooner
/// that happens.
const ROSTER_BACKLOG: usize = 32;

/// One player, as everybody else needs to see them.
#[derive(Debug, Clone, PartialEq)]
pub struct Player {
    pub entity_id: i32,
    pub uuid: [u8; 16],
    pub name: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f32,
    pub pitch: f32,
}

/// Something that happened to the roster.
#[derive(Debug, Clone, PartialEq)]
pub enum RosterChange {
    Joined(Player),
    Left {
        entity_id: i32,
        uuid: [u8; 16],
    },
    Moved {
        entity_id: i32,
        x: f64,
        y: f64,
        z: f64,
        yaw: f32,
        pitch: f32,
    },
}

/// Everyone currently connected.
#[derive(Debug)]
pub struct Roster {
    players: Mutex<HashMap<i32, Player>>,
    changes: broadcast::Sender<RosterChange>,
    next_entity_id: AtomicI32,
}

impl Default for Roster {
    fn default() -> Self {
        let (changes, _) = broadcast::channel(ROSTER_BACKLOG);
        Self {
            players: Mutex::new(HashMap::new()),
            changes,
            // Entity id 1 is not taken by anything; starting above it leaves
            // room for whatever the world eventually wants low ids for.
            next_entity_id: AtomicI32::new(100),
        }
    }
}

impl Roster {
    /// Add a player, and return both the roster as it was *before* them and a
    /// subscription that starts *before* they were added.
    ///
    /// The three happen under one lock on purpose. A joining session needs to
    /// be told about everybody already here and then about everybody who
    /// arrives afterwards, with nobody falling into the gap and nobody's
    /// departure arriving before the arrival it refers to — and the only way
    /// to have both halves agree is to take them together.
    pub fn join(&self, uuid: [u8; 16], name: String, at: (f64, f64, f64)) -> Joined {
        let entity_id = self.next_entity_id.fetch_add(1, Ordering::Relaxed);
        let player = Player {
            entity_id,
            uuid,
            name,
            x: at.0,
            y: at.1,
            z: at.2,
            yaw: 0.0,
            pitch: 0.0,
        };

        let mut players = self.players.lock().expect("the roster is never poisoned");
        let listener = self.changes.subscribe();
        let existing: Vec<Player> = players.values().cloned().collect();
        players.insert(entity_id, player.clone());
        drop(players);

        // Announced after the lock is released, so a receiver waking on this
        // cannot block the next join behind its own handling.
        let _ = self.changes.send(RosterChange::Joined(player.clone()));

        Joined {
            player,
            existing,
            listener,
        }
    }

    /// Remove a player and tell everybody.
    pub fn leave(&self, entity_id: i32) {
        let removed = self
            .players
            .lock()
            .expect("the roster is never poisoned")
            .remove(&entity_id);
        if let Some(player) = removed {
            let _ = self.changes.send(RosterChange::Left {
                entity_id,
                uuid: player.uuid,
            });
        }
    }

    /// Record a player's new position and tell everybody.
    ///
    /// Every movement packet, which is twenty a second per player. The lock is
    /// held for a map lookup and six field writes; anything longer here would
    /// be a lock every player contends for on every step.
    pub fn moved(&self, entity_id: i32, x: f64, y: f64, z: f64, yaw: f32, pitch: f32) {
        {
            let mut players = self.players.lock().expect("the roster is never poisoned");
            let Some(player) = players.get_mut(&entity_id) else {
                // Left between the packet arriving and this call. Not an
                // error: the session that owns them is already tearing down.
                return;
            };
            player.x = x;
            player.y = y;
            player.z = z;
            player.yaw = yaw;
            player.pitch = pitch;
        }
        let _ = self.changes.send(RosterChange::Moved {
            entity_id,
            x,
            y,
            z,
            yaw,
            pitch,
        });
    }

    /// Everyone here, for rebuilding a session that fell behind.
    pub fn snapshot(&self) -> Vec<Player> {
        self.players
            .lock()
            .expect("the roster is never poisoned")
            .values()
            .cloned()
            .collect()
    }

    pub fn count(&self) -> usize {
        self.players
            .lock()
            .expect("the roster is never poisoned")
            .len()
    }
}

/// What a session gets when its player joins.
#[derive(Debug)]
pub struct Joined {
    /// The player itself, with the entity id it was given.
    pub player: Player,
    /// Everybody who was already here.
    pub existing: Vec<Player>,
    /// Changes from the moment before this player was added, so this session
    /// also hears about its own join — which it ignores, and which is cheaper
    /// than a channel that filters.
    pub listener: broadcast::Receiver<RosterChange>,
}

/// The handle sessions share.
pub type SharedRoster = Arc<Roster>;

#[cfg(test)]
mod tests {
    use super::*;

    fn uuid(n: u8) -> [u8; 16] {
        [n; 16]
    }

    #[test]
    fn the_first_player_sees_nobody_and_the_second_sees_the_first() {
        let roster = Roster::default();
        let first = roster.join(uuid(1), "First".to_owned(), (0.0, 0.0, 0.0));
        assert!(first.existing.is_empty());

        let second = roster.join(uuid(2), "Second".to_owned(), (1.0, 2.0, 3.0));
        assert_eq!(second.existing.len(), 1);
        assert_eq!(second.existing[0].name, "First");
        assert_eq!(roster.count(), 2);
    }

    #[test]
    fn two_players_never_share_an_entity_id() {
        // Sharing one means each player's movement moves the other, which is
        // the sort of thing that looks like a physics bug for a week.
        let roster = Roster::default();
        let a = roster.join(uuid(1), "A".to_owned(), (0.0, 0.0, 0.0));
        let b = roster.join(uuid(2), "B".to_owned(), (0.0, 0.0, 0.0));
        assert_ne!(a.player.entity_id, b.player.entity_id);

        // And an id is not reused after a leave. Reuse is only safe once every
        // client has been told the old entity is gone, and that is exactly
        // what this type cannot wait for.
        roster.leave(a.player.entity_id);
        let c = roster.join(uuid(3), "C".to_owned(), (0.0, 0.0, 0.0));
        assert_ne!(c.player.entity_id, a.player.entity_id);
        assert_ne!(c.player.entity_id, b.player.entity_id);
    }

    #[test]
    fn a_session_hears_about_a_join_that_lands_after_its_snapshot() {
        // The gap this type exists to close. The listener is taken under the
        // same lock as the snapshot, so a player who arrives between the two
        // is in exactly one of them — never neither.
        let roster = Roster::default();
        let mut watcher = roster.join(uuid(1), "Watcher".to_owned(), (0.0, 0.0, 0.0));
        assert!(watcher.existing.is_empty());

        roster.join(uuid(2), "Later".to_owned(), (5.0, 6.0, 7.0));

        // The watcher's own join comes first — the subscription starts before
        // it — and then the one it needs.
        let mut names = Vec::new();
        while let Ok(change) = watcher.listener.try_recv() {
            if let RosterChange::Joined(player) = change {
                names.push(player.name);
            }
        }
        assert_eq!(names, vec!["Watcher", "Later"]);
    }

    #[test]
    fn leaving_is_announced_with_the_ids_a_client_needs_to_forget() {
        let roster = Roster::default();
        let gone = roster.join(uuid(1), "Gone".to_owned(), (0.0, 0.0, 0.0));
        let mut watcher = roster.join(uuid(2), "Watcher".to_owned(), (0.0, 0.0, 0.0));
        roster.leave(gone.player.entity_id);

        let mut left = None;
        while let Ok(change) = watcher.listener.try_recv() {
            if let RosterChange::Left { entity_id, uuid } = change {
                left = Some((entity_id, uuid));
            }
        }
        // Both ids: the entity id removes the body, the uuid removes the tab
        // list row, and they are different namespaces.
        assert_eq!(left, Some((gone.player.entity_id, uuid(1))));
        assert_eq!(roster.count(), 1);
    }

    #[test]
    fn a_move_updates_the_roster_so_a_later_joiner_sees_the_new_place() {
        let roster = Roster::default();
        let walker = roster.join(uuid(1), "Walker".to_owned(), (0.0, 0.0, 0.0));
        roster.moved(walker.player.entity_id, 100.0, 64.0, -50.0, 90.0, 0.0);

        let arriving = roster.join(uuid(2), "Arriving".to_owned(), (0.0, 0.0, 0.0));
        let seen = &arriving.existing[0];
        assert_eq!((seen.x, seen.y, seen.z), (100.0, 64.0, -50.0));
        assert_eq!(seen.yaw, 90.0, "and facing the way they turned");
    }

    #[test]
    fn moving_a_player_who_already_left_is_ignored_rather_than_a_panic() {
        // The packet and the disconnect race, and the disconnect can win.
        let roster = Roster::default();
        let gone = roster.join(uuid(1), "Gone".to_owned(), (0.0, 0.0, 0.0));
        roster.leave(gone.player.entity_id);
        roster.moved(gone.player.entity_id, 1.0, 2.0, 3.0, 0.0, 0.0);
        assert_eq!(roster.count(), 0);
    }
}
