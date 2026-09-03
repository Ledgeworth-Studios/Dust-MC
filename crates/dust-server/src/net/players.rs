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

use super::inventory::{Equipment, EquipmentChange, EQUIPMENT_SLOTS};

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
    /// Crouching. Held on the roster rather than in the session that owns the
    /// player, because a player who joins after somebody started sneaking has
    /// to be told about it, and the only place that knows is here.
    pub sneaking: bool,
    /// Running. Same reasoning as [`Player::sneaking`].
    pub sprinting: bool,
    /// What this player is wearing and holding, in the wire's own slot order.
    ///
    /// State, and on the roster for the reason [`Player::sneaking`] is: a
    /// player who logs in has to be told what everybody is already wearing,
    /// and the only thing that knows is here. A session that kept its own
    /// player's equipment and broadcast only changes would leave every
    /// joining player looking at a world of bare heads until each of its
    /// inhabitants happened to change a slot.
    pub equipment: Equipment,
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
    /// A player swung an arm.
    ///
    /// Carried as an event and not as state, because it *is* an event: there
    /// is no "currently swinging" for a joining player to be told about, only
    /// an animation that happened once.
    Swung {
        entity_id: i32,
        /// Which arm, as the protocol's animation table numbers it.
        animation: u8,
    },
    /// A player started or stopped crouching or running.
    ///
    /// Both are one packet's worth of change and both land in the same
    /// metadata update, so they travel together rather than as two events a
    /// receiver would have to combine.
    Posture {
        entity_id: i32,
        sneaking: bool,
        sprinting: bool,
    },
    /// Something to put in everybody's chat log.
    ///
    /// Carried on the roster's channel rather than a second one because it is
    /// the same fan-out to the same set, and two channels would mean two
    /// orderings — a join announcement could arrive after a message from the
    /// player who had not joined yet.
    ///
    /// The text is already rendered, because rendering it once is cheaper than
    /// once per recipient and because the sender's name has to be kept apart
    /// from the sender's words, which is `chat`'s job and not every reader's.
    Said {
        /// The entity that said it, so a session can tell its own words apart
        /// if it ever needs to. Zero for the server itself.
        entity_id: i32,
        text: dust_protocol::text::Component,
    },
    /// A player's visible gear changed: only the slots that actually differ,
    /// and never an empty list.
    ///
    /// Carries the difference rather than the whole set because
    /// `minecraft:set_equipment` charges per entry — a set of six with one
    /// helmet in it is a seventeen-byte body where the one changed slot is
    /// seven — and
    /// because everybody who can receive this has already been told the rest.
    /// One event for however many slots moved at once, so a player swapping a
    /// full set of armour costs each viewer one packet and not four.
    Equipped {
        entity_id: i32,
        slots: Vec<EquipmentChange>,
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
            sneaking: false,
            sprinting: false,
            pitch: 0.0,
            equipment: std::array::from_fn(|_| None),
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

    /// A player swung an arm. Nothing is stored; see [`RosterChange::Swung`].
    pub fn swung(&self, entity_id: i32, animation: u8) {
        let _ = self.changes.send(RosterChange::Swung {
            entity_id,
            animation,
        });
    }

    /// A player started or stopped crouching or running.
    ///
    /// Nothing is sent when neither actually changed. A client sends
    /// `player_command` for several things this does not model, and one that
    /// left the posture alone would otherwise put a metadata packet on every
    /// other player's wire for a horse nobody is riding.
    pub fn posture(&self, entity_id: i32, sneaking: Option<bool>, sprinting: Option<bool>) {
        let changed = {
            let mut players = self.players.lock().expect("the roster is never poisoned");
            let Some(player) = players.get_mut(&entity_id) else {
                return;
            };
            let before = (player.sneaking, player.sprinting);
            player.sneaking = sneaking.unwrap_or(player.sneaking);
            player.sprinting = sprinting.unwrap_or(player.sprinting);
            let after = (player.sneaking, player.sprinting);
            (before != after).then_some(after)
        };
        if let Some((sneaking, sprinting)) = changed {
            let _ = self.changes.send(RosterChange::Posture {
                entity_id,
                sneaking,
                sprinting,
            });
        }
    }

    /// Record what a player is now wearing and holding, and tell everybody
    /// what changed.
    ///
    /// Takes the whole set and works out the difference here, rather than
    /// asking each caller to. There are five places a container can change and
    /// a rule spelled at five call sites is a rule that is wrong at one of
    /// them; and the roster is the only thing that knows what was last said,
    /// so it is the only thing that can answer "did this change anything".
    /// Nothing is sent when nothing moved, which is most calls — every click
    /// in the main inventory is one.
    pub fn equipped(&self, entity_id: i32, now: Equipment) {
        let changed = {
            let mut players = self.players.lock().expect("the roster is never poisoned");
            let Some(player) = players.get_mut(&entity_id) else {
                return;
            };
            let mut changed: Vec<EquipmentChange> = Vec::new();
            for (slot, stack) in now.iter().enumerate().take(EQUIPMENT_SLOTS) {
                if &player.equipment[slot] != stack {
                    changed.push((slot as u8, stack.clone()));
                }
            }
            if changed.is_empty() {
                return;
            }
            player.equipment = now;
            changed
        };
        let _ = self.changes.send(RosterChange::Equipped {
            entity_id,
            slots: changed,
        });
    }

    /// Put a line in everybody's chat log.
    pub fn say(&self, entity_id: i32, text: dust_protocol::text::Component) {
        let _ = self.changes.send(RosterChange::Said { entity_id, text });
    }

    /// One entity id, for something that is not a player.
    ///
    /// The roster owns the allocator because the roster is what is in the
    /// world, and because an item entity and a player sharing an id is a
    /// client drawing one on top of the other. Two allocators would be two
    /// counters that have to be told about each other; one is one number.
    pub fn claim_entity_id(&self) -> i32 {
        self.next_entity_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Where everybody is, into a buffer the caller keeps.
    ///
    /// Fills rather than returns because this is called twenty times a second
    /// by the item tick, and [`Roster::snapshot`] clones a `String` per player
    /// to answer a question that is three floats.
    pub fn positions_into(&self, out: &mut Vec<(f64, f64, f64)>) {
        out.clear();
        out.extend(
            self.players
                .lock()
                .expect("the roster is never poisoned")
                .values()
                .map(|player| (player.x, player.y, player.z)),
        );
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
    fn a_message_reaches_everybody_including_the_speaker() {
        // Including the speaker, deliberately: a player has to see their own
        // words in the log, and filtering them out here would mean every
        // session had to add them back locally — two code paths for one line.
        let roster = Roster::default();
        let speaker = roster.join(uuid(1), "Speaker".to_owned(), (0.0, 0.0, 0.0));
        let mut listener = roster.join(uuid(2), "Listener".to_owned(), (0.0, 0.0, 0.0));
        roster.say(
            speaker.player.entity_id,
            dust_protocol::text::Component::text("hello"),
        );

        let mut heard = 0;
        while let Ok(change) = listener.listener.try_recv() {
            if let RosterChange::Said { entity_id, .. } = change {
                assert_eq!(entity_id, speaker.player.entity_id);
                heard += 1;
            }
        }
        assert_eq!(heard, 1);
    }

    /// A set with `count` distinct things in it, at the wire slots named.
    fn wearing(slots: &[u8]) -> Equipment {
        let item =
            dust_registry::Item::from_name("minecraft:stone").expect("stone is in every registry");
        std::array::from_fn(|index| {
            slots
                .contains(&(index as u8))
                .then(|| super::super::inventory::Stack::new(item, 1))
        })
    }

    fn equipment_changes(listener: &mut broadcast::Receiver<RosterChange>) -> Vec<Vec<u8>> {
        let mut seen = Vec::new();
        while let Ok(change) = listener.try_recv() {
            if let RosterChange::Equipped { slots, .. } = change {
                seen.push(slots.iter().map(|(slot, _)| *slot).collect());
            }
        }
        seen
    }

    #[test]
    fn four_pieces_at_once_are_one_event_and_not_four() {
        // The reason the roster takes the whole set rather than a slot: a
        // player who puts on a full suit of armour in one shift-click storm
        // should cost each viewer one packet.
        let roster = Roster::default();
        let wearer = roster.join(uuid(1), "Wearer".to_owned(), (0.0, 0.0, 0.0));
        let mut watcher = roster.join(uuid(2), "Watcher".to_owned(), (0.0, 0.0, 0.0));

        roster.equipped(wearer.player.entity_id, wearing(&[2, 3, 4, 5]));

        assert_eq!(
            equipment_changes(&mut watcher.listener),
            vec![vec![2, 3, 4, 5]]
        );
    }

    #[test]
    fn only_the_slot_that_moved_is_sent_the_second_time() {
        // Everybody who can hear this has already been told the rest, and the
        // packet charges per entry.
        let roster = Roster::default();
        let wearer = roster.join(uuid(1), "Wearer".to_owned(), (0.0, 0.0, 0.0));
        let mut watcher = roster.join(uuid(2), "Watcher".to_owned(), (0.0, 0.0, 0.0));

        roster.equipped(wearer.player.entity_id, wearing(&[2, 3, 4, 5]));
        let _ = equipment_changes(&mut watcher.listener);
        roster.equipped(wearer.player.entity_id, wearing(&[0, 2, 3, 4, 5]));

        assert_eq!(equipment_changes(&mut watcher.listener), vec![vec![0]]);
    }

    #[test]
    fn a_container_change_that_moved_nothing_visible_sends_nothing() {
        // Most clicks. Shuffling the main inventory changes no equipment slot,
        // and a packet per click to every player in the world for a stack of
        // cobblestone moving from row two to row three is the cost this
        // comparison exists to refuse.
        let roster = Roster::default();
        let wearer = roster.join(uuid(1), "Wearer".to_owned(), (0.0, 0.0, 0.0));
        let mut watcher = roster.join(uuid(2), "Watcher".to_owned(), (0.0, 0.0, 0.0));

        roster.equipped(wearer.player.entity_id, wearing(&[5]));
        let _ = equipment_changes(&mut watcher.listener);
        roster.equipped(wearer.player.entity_id, wearing(&[5]));

        assert!(equipment_changes(&mut watcher.listener).is_empty());
    }

    #[test]
    fn a_player_who_joins_later_is_told_what_everybody_is_already_wearing() {
        // The failure this whole feature is about: without the roster holding
        // the set, a joining player sees a world of bare heads until each of
        // its inhabitants happens to change a slot.
        let roster = Roster::default();
        let wearer = roster.join(uuid(1), "Wearer".to_owned(), (0.0, 0.0, 0.0));
        roster.equipped(wearer.player.entity_id, wearing(&[0, 5]));

        let later = roster.join(uuid(2), "Later".to_owned(), (0.0, 0.0, 0.0));

        let seen = later
            .existing
            .iter()
            .find(|player| player.entity_id == wearer.player.entity_id)
            .expect("the wearer is already here");
        assert!(seen.equipment[0].is_some(), "and holding something");
        assert!(seen.equipment[5].is_some(), "and wearing a helmet");
        assert!(seen.equipment[1].is_none(), "and nothing in the offhand");
    }

    #[test]
    fn equipping_a_player_who_already_left_is_ignored_rather_than_a_panic() {
        // The same race as a movement: the container write and the disconnect
        // arrive together and the disconnect can win.
        let roster = Roster::default();
        let gone = roster.join(uuid(1), "Gone".to_owned(), (0.0, 0.0, 0.0));
        roster.leave(gone.player.entity_id);
        roster.equipped(gone.player.entity_id, wearing(&[5]));
        assert_eq!(roster.count(), 0);
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
