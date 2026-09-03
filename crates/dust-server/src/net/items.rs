//! Items lying on the ground: the first entity Dust has.
//!
//! # The shape every entity will get
//!
//! Decision record 0023 is the account. In short: **one flat vector under one
//! lock, ticked by one participant of the tick loop, announced on one
//! broadcast channel.** Not a task per entity, not an ECS, not a quadtree.
//! A thousand item entities is the number this has to survive, and at a
//! thousand a `Vec<ItemEntity>` is 88 kilobytes that a tick walks in one pass
//! with no pointer chasing, while the alternatives are all a thousand
//! allocations, a thousand wakeups, or a spatial index rebuilt twenty times a
//! second to answer a question a linear scan answers in microseconds.
//!
//! # Three ways this stays bounded, and each is load-bearing
//!
//! 1. **Nothing far from a player is ticked.** Physics, merging and the
//!    despawn clock all run only for entities within [`TICK_RADIUS`] of
//!    somebody. The rest are a `Vec` entry and nothing else. A tunnel full of
//!    dropped cobblestone that nobody is standing in costs one bounds check
//!    each per tick.
//! 2. **Two of the same item lying together become one.** Merging is what
//!    keeps a mined-out vein from being sixty entities, and it is also what a
//!    player expects to see.
//! 3. **Everything despawns.** [`LIFETIME_TICKS`] is five minutes of game
//!    time, so a mining session leaves no permanent carpet, and
//!    [`MAX_ENTITIES`] is a hard ceiling above which the oldest goes first —
//!    which vanilla does not have, and is here because a server that dies
//!    under a dropped-item flood is worse for everyone in it than one that
//!    forgets the oldest cobblestone.
//!
//! # Why there are no movement packets
//!
//! An item is spawned with a velocity and the client simulates the arc itself:
//! the same gravity, the same drag, the same numbers. So this sends
//! **`AddEntity` once, `TeleportEntity` once when the item comes to rest, and
//! nothing in between** — two packets for the whole life of a drop, against
//! the twenty a second a server that streamed positions would send. Where the
//! two simulations disagree is the moment the item settles, and that is
//! exactly where the one correction is sent.
//!
//! # What a player feels, which is what decided all of it
//!
//! The item comes out of the *centre of the block that broke*, not the
//! player's feet, with a little upward velocity, so it pops. It cannot be
//! picked up for [`PICKUP_DELAY_TICKS`] — a fifth of a second — so a block
//! broken while walking backwards does not vanish into the hand before it is
//! seen. Walking over it collects it with no key pressed. Two of the same item
//! lying together become one stack. And it does not lie there for ever.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use dust_guard::Solidity;
use dust_protocol::packets::play;
use dust_protocol::types::{Angle, Slot, Uuid, VarInt};
use dust_registry::Item;
use tokio::sync::broadcast;

use dust_world::coords::ChunkPos;

use super::edits::EditedWorld;
use super::players::Roster;

/// How far from a player an item is still simulated, in blocks.
///
/// Four chunks. Inside it a player can see an item arc and land; outside it
/// they cannot see the item at all, because the entity was never sent to a
/// client that far away either.
pub const TICK_RADIUS: f64 = 64.0;

/// How long an item lives, in ticks. Five minutes, which is vanilla's.
pub const LIFETIME_TICKS: u32 = 6_000;

/// How long after it appears before anybody may pick it up, in ticks.
///
/// Vanilla's ten. It is what stops a block broken while walking backwards from
/// being collected in the same instant it appears, which reads as the drop
/// never having happened.
pub const PICKUP_DELAY_TICKS: u16 = 10;

/// How many item entities exist at once before the oldest is forgotten.
///
/// Vanilla has no such number and pays for it. See the module documentation.
pub const MAX_ENTITIES: usize = 4_096;

/// How near a player has to be for an item to fly to them, in blocks, measured
/// horizontally from the player's feet.
const PICKUP_REACH: f64 = 1.4;

/// How far above and below a player's feet a pickup reaches, in blocks.
const PICKUP_BELOW: f64 = 0.7;
const PICKUP_ABOVE: f64 = 2.0;

/// How near two items have to be to become one: horizontally, and vertically.
///
/// **A block's worth of drops has to merge, and the spread is what decides
/// how far apart they can start.** An item pops out anywhere within a quarter
/// of a block of the centre, so two of them can land half a block apart on
/// each axis — three quarters of a block by the diagonal. A merge reach under
/// that is a reach that works most of the time, which for a mined-out vein is
/// a pile that is sometimes one stack and sometimes six. Vanilla asks the same
/// question as a box inflated by half a block around an item a quarter wide,
/// which comes to the same number.
const MERGE_REACH: f64 = 1.0;
const MERGE_RISE: f64 = 0.5;

/// Blocks per tick per tick. Vanilla's item gravity.
const GRAVITY: f64 = 0.04;

/// What a tick multiplies horizontal motion by in the air, and on the ground.
const DRAG_AIR: f64 = 0.98;
const DRAG_GROUND: f64 = 0.6 * 0.98;

/// Below this, in blocks per tick, an item is at rest and stops being moved.
const AT_REST: f64 = 0.003;

/// How many item changes a slow session may fall behind before it is told it
/// missed some. Larger than the edit backlog because one break makes one edit
/// and can make several drops.
const ITEM_BACKLOG: usize = 128;

/// One item lying in, or falling through, the world.
///
/// Plain fields and no methods on purpose: the tick walks a slice of these and
/// every accessor would be a call it cannot see through.
#[derive(Debug, Clone)]
pub struct ItemEntity {
    pub id: i32,
    pub uuid: u128,
    pub item: Item,
    /// How many. A count is a `u8` because a stack is, and a loot table that
    /// said more than a stack was split before it reached here.
    pub count: u8,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub vx: f64,
    pub vy: f64,
    pub vz: f64,
    /// Ticks lived. Only advanced while somebody is near enough to tick it.
    pub age: u32,
    pub pickup_delay: u16,
    /// Whether it has come to rest and its resting place has been sent.
    pub settled: bool,
}

/// Something that happened to an item entity, for the sessions to relay.
#[derive(Debug, Clone)]
pub enum ItemChange {
    /// It exists now, here, moving this way. The client takes it from there.
    Spawned {
        id: i32,
        uuid: u128,
        item: Item,
        count: u8,
        x: f64,
        y: f64,
        z: f64,
        vx: f64,
        vy: f64,
        vz: f64,
    },
    /// It has stopped. The one correction the client gets.
    Settled { id: i32, x: f64, y: f64, z: f64 },
    /// Somebody picked it up: the animation of it flying to them, then gone.
    Collected {
        id: i32,
        by: i32,
        count: u8,
        x: f64,
        z: f64,
    },
    /// It went away without anybody taking it — merged, aged out, or over the
    /// ceiling.
    Removed { id: i32, x: f64, z: f64 },
}

impl ItemChange {
    /// Where this happened, for a session deciding whether its player holds
    /// the column. A change outside the view is a packet for nobody.
    pub fn at(&self) -> (f64, f64) {
        match self {
            Self::Spawned { x, z, .. }
            | Self::Settled { x, z, .. }
            | Self::Collected { x, z, .. }
            | Self::Removed { x, z, .. } => (*x, *z),
        }
    }
}

/// Every item on the ground.
#[derive(Debug)]
pub struct ItemWorld {
    entities: Mutex<Vec<ItemEntity>>,
    announce: broadcast::Sender<ItemChange>,
    /// How many exist, readable without taking the lock so a break can decide
    /// whether it is worth trying.
    live: AtomicUsize,
    /// How many were dropped because the ceiling was reached, for the log.
    over_ceiling: AtomicUsize,
}

impl Default for ItemWorld {
    fn default() -> Self {
        let (announce, _) = broadcast::channel(ITEM_BACKLOG);
        Self {
            entities: Mutex::new(Vec::new()),
            announce,
            live: AtomicUsize::new(0),
            over_ceiling: AtomicUsize::new(0),
        }
    }
}

impl ItemWorld {
    /// Listen for every item change from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<ItemChange> {
        self.announce.subscribe()
    }

    /// How many items are lying in the world.
    pub fn len(&self) -> usize {
        self.live.load(Ordering::Relaxed)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// How many drops were refused because the ceiling was already reached.
    pub fn over_ceiling(&self) -> usize {
        self.over_ceiling.load(Ordering::Relaxed)
    }

    /// How many items have come to rest.
    ///
    /// Public for the bench, and the reason is a defect class rather than a
    /// convenience: an item that has settled takes an early return out of
    /// `step`, so a bench that popped a thousand items and then ran a thousand
    /// ticks would spend the first fifteen measuring physics and the other
    /// nine hundred and eighty-five measuring a branch. **A bench whose
    /// subject stops moving reports the cost of it having stopped.** This is
    /// what lets the bench say which of the two it measured.
    pub fn at_rest(&self) -> usize {
        self.entities
            .lock()
            .expect("the item world is never poisoned")
            .iter()
            .filter(|entity| entity.settled)
            .count()
    }

    /// Pop an item out of the centre of a block that just broke.
    ///
    /// **The centre of the block, and not the player.** A drop that appeared
    /// at the breaker's feet would be right for the one player who is standing
    /// still and wrong for everybody who is looking at the block, including
    /// the breaker the moment they move — and it would put the item on the
    /// wrong side of a wall for a block broken through one.
    pub fn pop(
        &self,
        roster: &Roster,
        block: dust_protocol::types::Position,
        item: Item,
        count: u8,
        seed: u64,
    ) -> i32 {
        // A spread small enough that a block's worth of drops stays in the
        // cell it came out of, and large enough that four of them are four
        // things and not one. Vanilla's ±0.25, from `Block.popResource`.
        let mut rng = seed;
        let mut next = || {
            rng ^= rng >> 12;
            rng ^= rng << 25;
            rng ^= rng >> 27;
            let bits = rng.wrapping_mul(0x2545_f491_4f6c_dd1d);
            ((bits >> 40) as f64) / f64::from(1u32 << 24)
        };
        let x = f64::from(block.x) + 0.5 + (next() - 0.5) * 0.5;
        let y = f64::from(block.y) + 0.5 - 0.125;
        let z = f64::from(block.z) + 0.5 + (next() - 0.5) * 0.5;
        // Vanilla's `ItemEntity` constructor: a tenth of a block sideways and
        // a fifth upwards. The upward part is the pop a player watches for.
        let vx = (next() - 0.5) * 0.2;
        let vz = (next() - 0.5) * 0.2;

        let id = roster.claim_entity_id();
        // The uuid is derived from the id rather than random: a client keys
        // entities by the id, uses the uuid only for equality, and a server
        // that had to be able to generate randomness to drop an item would
        // have one more thing that can fail on a break.
        let uuid = 0x1_0000_0000_0000_0000u128 | u128::from(id as u32);
        let entity = ItemEntity {
            id,
            uuid,
            item,
            count,
            x,
            y,
            z,
            vx,
            vy: 0.2,
            vz,
            age: 0,
            pickup_delay: PICKUP_DELAY_TICKS,
            settled: false,
        };

        let mut entities = self
            .entities
            .lock()
            .expect("the item world is never poisoned");
        if entities.len() >= MAX_ENTITIES {
            // The oldest goes, and the newest arrives: a player who is mining
            // right now keeps what they are mining. Announced so every client
            // stops drawing it rather than being left with a ghost.
            let gone = entities.remove(0);
            self.over_ceiling.fetch_add(1, Ordering::Relaxed);
            let _ = self.announce.send(ItemChange::Removed {
                id: gone.id,
                x: gone.x,
                z: gone.z,
            });
        }
        let _ = self.announce.send(ItemChange::Spawned {
            id,
            uuid,
            item,
            count,
            x,
            y,
            z,
            vx,
            vy: 0.2,
            vz,
        });
        entities.push(entity);
        self.live.store(entities.len(), Ordering::Relaxed);
        id
    }

    /// Everything a session's player is standing close enough to take.
    ///
    /// Claimed under the lock and removed in the same breath, so two players
    /// walking over the same stack cannot both be given it. The caller is
    /// handed what it now owns; if putting it in the inventory fails, it is
    /// gone — which is the same trade `EditedWorld::break_block` makes about
    /// the double announcement, and for the same reason: the alternative is a
    /// lock held across a socket write.
    pub fn claim_near(&self, by: i32, at: (f64, f64, f64), out: &mut Vec<(Item, u8)>) {
        let mut entities = self
            .entities
            .lock()
            .expect("the item world is never poisoned");
        let mut index = 0;
        while index < entities.len() {
            let entity = &entities[index];
            if entity.pickup_delay > 0 || !reachable(entity, at) {
                index += 1;
                continue;
            }
            let taken = entities.remove(index);
            let _ = self.announce.send(ItemChange::Collected {
                id: taken.id,
                by,
                count: taken.count,
                x: taken.x,
                z: taken.z,
            });
            out.push((taken.item, taken.count));
        }
        self.live.store(entities.len(), Ordering::Relaxed);
    }

    /// One tick of every item near a player: gravity, landing, merging, age.
    ///
    /// `players` and `near` are the caller's buffers, reused between ticks:
    /// two `Vec`s allocated twenty times a second to hold a handful of numbers
    /// are two allocations twenty times a second to hold a handful of numbers.
    pub fn tick(
        &self,
        world: &EditedWorld,
        constants: Option<&dust_registry::BlockConstants>,
        players: &[(f64, f64, f64)],
        near: &mut Vec<usize>,
    ) {
        let mut entities = self
            .entities
            .lock()
            .expect("the item world is never poisoned");
        if entities.is_empty() {
            return;
        }
        let mut ground = super::collide::Ground::of(world, constants);

        near.clear();
        let mut index = 0;
        while index < entities.len() {
            if !near_any(&entities[index], players) {
                index += 1;
                continue;
            }
            let entity = &mut entities[index];
            entity.age = entity.age.saturating_add(1);
            entity.pickup_delay = entity.pickup_delay.saturating_sub(1);
            if entity.age >= LIFETIME_TICKS {
                let gone = entities.remove(index);
                let _ = self.announce.send(ItemChange::Removed {
                    id: gone.id,
                    x: gone.x,
                    z: gone.z,
                });
                continue;
            }
            if step(&mut entities[index], ground.as_mut()) {
                let entity = &entities[index];
                let _ = self.announce.send(ItemChange::Settled {
                    id: entity.id,
                    x: entity.x,
                    y: entity.y,
                    z: entity.z,
                });
            }
            near.push(index);
            index += 1;
        }

        // Merging is a second pass and not part of the first, because a merge
        // is a fact about a *pair* and the first pass has one entity in hand.
        //
        // Quadratic over the items **near a player** and not over the world's,
        // which is what the near list is for: fifty items in one room is 1,225
        // squared-distance comparisons, and four thousand items spread over a
        // world nobody is standing in is zero. A far item that a new drop
        // lands beside merges the moment somebody walks back to it.
        let mut left = 0;
        while left < near.len() {
            let mut right = left + 1;
            while right < near.len() {
                let (a, b) = (near[left], near[right]);
                if let Some(total) = merged(&entities, a, b) {
                    entities[a].count = total;
                    // The younger of the two goes, so a stack a player has
                    // been waiting on does not have its clock reset.
                    let gone = entities.remove(b);
                    let _ = self.announce.send(ItemChange::Removed {
                        id: gone.id,
                        x: gone.x,
                        z: gone.z,
                    });
                    // Every index past the removed one has moved down.
                    near.remove(right);
                    for entry in near.iter_mut() {
                        if *entry > b {
                            *entry -= 1;
                        }
                    }
                    continue;
                }
                right += 1;
            }
            left += 1;
        }
        self.live.store(entities.len(), Ordering::Relaxed);
    }
}

/// Whether a player standing at `at` is close enough to take this item.
fn reachable(entity: &ItemEntity, at: (f64, f64, f64)) -> bool {
    let dx = entity.x - at.0;
    let dz = entity.z - at.2;
    if dx * dx + dz * dz > PICKUP_REACH * PICKUP_REACH {
        return false;
    }
    let dy = entity.y - at.1;
    (-PICKUP_BELOW..=PICKUP_ABOVE).contains(&dy)
}

fn near_any(entity: &ItemEntity, players: &[(f64, f64, f64)]) -> bool {
    players.iter().any(|(x, _, z)| {
        let dx = entity.x - x;
        let dz = entity.z - z;
        dx * dx + dz * dz <= TICK_RADIUS * TICK_RADIUS
    })
}

/// Which columns the items that will tick this tick are standing in.
///
/// The set `net::residency::ColumnClaim` keeps for the item world, and the
/// reason it is a set of columns rather than a ring around anybody: an item is
/// simulated from up to [`TICK_RADIUS`] away — four chunks — so a heap of
/// cobblestone can be ticking in a column no player's own ring covers, and it
/// would rebuild that column out of the region file twenty times a second
/// forever. Bounded twice over: nothing outside `TICK_RADIUS` of a player is in
/// it at all, and the whole thing goes when the items despawn.
///
/// Appended to a buffer the caller reuses, and not deduplicated here — the
/// claim sorts and dedups, and a thousand items in one column would otherwise
/// be a thousand `contains` scans on this side of it.
pub fn footprint_into(items: &ItemWorld, players: &[(f64, f64, f64)], out: &mut Vec<ChunkPos>) {
    out.clear();
    let entities = items
        .entities
        .lock()
        .expect("the item world is never poisoned");
    let mut last: Option<ChunkPos> = None;
    for entity in entities.iter() {
        if !near_any(entity, players) {
            continue;
        }
        // Items are stored in the order they were dropped and a heap of them
        // shares a column, so remembering the last one answers almost every
        // entity without touching the vector.
        let pos = ChunkPos::new(
            (entity.x.floor() as i32) >> 4,
            (entity.z.floor() as i32) >> 4,
        );
        if last != Some(pos) {
            out.push(pos);
            last = Some(pos);
        }
    }
}

/// Whether two entities may become one, and what the survivor's count is.
fn merged(entities: &[ItemEntity], left: usize, right: usize) -> Option<u8> {
    let (a, b) = (&entities[left], &entities[right]);
    if a.item != b.item {
        return None;
    }
    let total = u32::from(a.count) + u32::from(b.count);
    if total > u32::from(a.item.max_stack_size()) {
        return None;
    }
    let dx = a.x - b.x;
    let dz = a.z - b.z;
    if dx * dx + dz * dz > MERGE_REACH * MERGE_REACH || (a.y - b.y).abs() > MERGE_RISE {
        return None;
    }
    Some(total as u8)
}

/// One tick of one item's motion. Returns whether it has just come to rest.
///
/// Vanilla's numbers, in vanilla's order: gravity, then the move, then drag.
/// Getting the order wrong is a drop that hangs for a frame or sinks a
/// fraction of a block into the floor, which is small and which a player who
/// has played Minecraft sees immediately.
fn step(entity: &mut ItemEntity, ground: Option<&mut super::collide::Ground<'_>>) -> bool {
    if entity.settled {
        return false;
    }
    entity.vy -= GRAVITY;
    let x = entity.x + entity.vx;
    let mut y = entity.y + entity.vy;
    let z = entity.z + entity.vz;

    // Landing, and only landing. An item is a quarter of a block wide and a
    // quarter high, so it fits through anything a player fits through and the
    // only surface it meets in ordinary play is the one under it. Sideways
    // collision is left to the day entities have boxes: without a table saying
    // what is solid there is no landing at all, which is a server whose drops
    // fall to the bottom of the world, so a world with no constants keeps its
    // items where the block was.
    let mut on_ground = false;
    match ground {
        None => {
            if entity.vy < 0.0 {
                y = entity.y;
                entity.vy = 0.0;
                on_ground = true;
            }
        }
        Some(ground) => {
            let cell = (
                x.floor() as i32,
                (y - 0.02).floor() as i32,
                z.floor() as i32,
            );
            if entity.vy <= 0.0 && ground.first_solid(cell, cell).is_some() {
                y = f64::from(cell.1 + 1);
                // **Zero, not a bounce.** Vanilla's item does multiply its
                // vertical speed by minus a half on landing, but only after
                // the collision that stopped it has already set that speed to
                // zero, so the multiply is on nothing and an item does not
                // bounce. Written as a bounce here it hopped for three seconds
                // before it came to rest, which is three seconds of a player
                // watching cobblestone behave like a rubber ball — and three
                // seconds in which two drops from one block are never within
                // merging distance of each other at the same instant.
                entity.vy = 0.0;
                on_ground = true;
            }
        }
    }

    entity.x = x;
    entity.y = y;
    entity.z = z;
    let drag = if on_ground { DRAG_GROUND } else { DRAG_AIR };
    entity.vx *= drag;
    entity.vz *= drag;
    entity.vy *= DRAG_AIR;

    if on_ground
        && entity.vx.abs() < AT_REST
        && entity.vy.abs() < AT_REST
        && entity.vz.abs() < AT_REST
    {
        entity.vx = 0.0;
        entity.vy = 0.0;
        entity.vz = 0.0;
        entity.settled = true;
        return true;
    }
    false
}

/// The packet that puts an item in a client's world.
pub fn spawn(change: &ItemChange, item_entity_type: i32) -> Option<play::clientbound::AddEntity> {
    let ItemChange::Spawned {
        id,
        uuid,
        x,
        y,
        z,
        vx,
        vy,
        vz,
        ..
    } = change
    else {
        return None;
    };
    Some(play::clientbound::AddEntity {
        entity_id: VarInt(*id),
        uuid: Uuid(*uuid),
        kind: VarInt(item_entity_type),
        x: *x,
        y: *y,
        z: *z,
        pitch: Angle::from_degrees(0.0),
        yaw: Angle::from_degrees(0.0),
        head_yaw: Angle::from_degrees(0.0),
        data: VarInt(0),
        velocity: velocity(*vx, *vy, *vz),
    })
}

/// What the item entity is holding.
///
/// Index 8 on an item entity, and the reason `MetadataValue::Slot` exists: an
/// item entity with no metadata renders as nothing at all, so a drop without
/// this packet is a drop the player cannot see.
pub fn contents(id: i32, item: Item, count: u8) -> play::clientbound::SetEntityData {
    play::clientbound::SetEntityData {
        entity_id: VarInt(id),
        entries: play::metadata::MetadataEntries(vec![play::metadata::MetadataEntry {
            index: 8,
            value: play::metadata::MetadataValue::Slot(Slot::Present {
                count: i32::from(count),
                item_id: item.protocol_id() as i32,
                // A block drop comes out of a loot table, which names an
                // item and never a component patch. When Q starts throwing a
                // player's stack instead of destroying it, that stack's own
                // components come through here.
                components: dust_protocol::components::ComponentPatch::EMPTY,
            }),
        }]),
    }
}

/// Velocity in the protocol's own 1/8000 of a block per tick, clamped rather
/// than wrapped: a wrapped velocity sends an item the other way.
fn velocity(x: f64, y: f64, z: f64) -> play::EntityVelocity {
    let unit = |v: f64| (v * 8000.0).clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16;
    play::EntityVelocity {
        x: unit(x),
        y: unit(y),
        z: unit(z),
    }
}

/// The tick loop's item physics.
///
/// **This is the first thing the tick loop actually owns.** Until now it ran
/// three placeholders and the world lived entirely on the network side, which
/// `net/mod.rs` says is a seam left uninvented on purpose. It is invented
/// here, and the shape is the smallest one that works: the participant holds
/// the same `Arc`s the sessions hold, does its whole pass under one lock, and
/// announces on the channel the sessions are already listening to. Nothing
/// about a socket reaches it and nothing about it reaches a socket.
pub struct ItemTicker {
    items: std::sync::Arc<ItemWorld>,
    world: std::sync::Arc<EditedWorld>,
    roster: std::sync::Arc<Roster>,
    constants: Option<std::sync::Arc<dust_registry::BlockConstants>>,
    /// Reused between ticks. See [`Roster::positions_into`].
    players: Vec<(f64, f64, f64)>,
    /// Which entities were near enough to tick, reused for the same reason.
    near: Vec<usize>,
    /// The columns those entities are in, reused for the same reason again.
    footprint: Vec<ChunkPos>,
    /// The server's claim on those columns, so that an item lying four chunks
    /// from anybody is not read out of a region file on the tick thread twenty
    /// times a second. Given up when this participant is dropped.
    claim: super::residency::ColumnClaim,
}

impl ItemTicker {
    pub fn new(
        items: std::sync::Arc<ItemWorld>,
        world: std::sync::Arc<EditedWorld>,
        roster: std::sync::Arc<Roster>,
        constants: Option<std::sync::Arc<dust_registry::BlockConstants>>,
    ) -> Self {
        Self {
            items,
            claim: super::residency::ColumnClaim::new(std::sync::Arc::clone(&world)),
            world,
            roster,
            constants,
            players: Vec::new(),
            near: Vec::new(),
            footprint: Vec::new(),
        }
    }
}

impl std::fmt::Debug for ItemTicker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ItemTicker")
            .field("items", &self.items.len())
            .finish_non_exhaustive()
    }
}

impl crate::participant::TickParticipant for ItemTicker {
    fn name(&self) -> &str {
        "items"
    }

    /// Ahead of the placeholders and behind the status probe. An item that
    /// moved this tick should have moved before anything reports on the tick.
    fn priority(&self) -> i32 {
        0
    }

    fn tick(&mut self, _ctx: &crate::participant::TickContext) {
        // An empty world is the common case on a server nobody is mining in,
        // and it costs one atomic read rather than a lock and a roster walk.
        if self.items.is_empty() {
            return;
        }
        self.roster.positions_into(&mut self.players);
        if self.players.is_empty() {
            return;
        }
        // Claimed before the tick that reads them, not after, so the warming
        // thread has the whole tick's worth of wall clock to get ahead of the
        // next one. The first tick after a heap of items appears still builds
        // its own columns; every tick after it reads the server's copy.
        footprint_into(&self.items, &self.players, &mut self.footprint);
        let mut wanted = std::mem::take(&mut self.footprint);
        self.claim.set(&mut wanted);
        self.footprint = wanted;
        self.items.tick(
            &self.world,
            self.constants.as_deref(),
            &self.players,
            &mut self.near,
        );
    }
}

impl ItemWorld {
    /// Every item within `reach` blocks of a point, for a session that has
    /// just joined.
    ///
    /// A join is the one moment a client has to be told about items it did not
    /// watch appear. Everything after it arrives on the channel, which is why
    /// this is the only place the whole list is walked for one player.
    pub fn visible_from(&self, at: (f64, f64, f64), reach: f64, out: &mut Vec<ItemChange>) {
        let entities = self
            .entities
            .lock()
            .expect("the item world is never poisoned");
        for entity in entities.iter() {
            let dx = entity.x - at.0;
            let dz = entity.z - at.2;
            if dx * dx + dz * dz > reach * reach {
                continue;
            }
            out.push(ItemChange::Spawned {
                id: entity.id,
                uuid: entity.uuid,
                item: entity.item,
                count: entity.count,
                x: entity.x,
                y: entity.y,
                z: entity.z,
                // A settled item is sent at rest, so a client that has just
                // arrived does not watch a week-old drop fall again.
                vx: entity.vx,
                vy: entity.vy,
                vz: entity.vz,
            });
        }
    }
}
