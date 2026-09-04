//! Sand in mid-air: the second kind of entity Dust has.
//!
//! # Why this is an entity and not a block that moves down a cell a tick
//!
//! A block stepped downward once per tick travels twenty blocks a second from
//! a standing start. Vanilla's sand accelerates from nothing at 0.04 blocks per
//! tick per tick and takes eleven ticks to cover its first four blocks. The
//! cheap version is right for the one-block fall a player sees most often and
//! visibly wrong for the gravel above a shaft, and it would arrive as twenty
//! block updates a second to everybody in view rather than as two packets for
//! the whole fall. Decision record 0023 is the account of the shape; this is
//! the second thing built to it.
//!
//! # What is different from an item, and why
//!
//! * **Everything is ticked, near a player or not.** An item that nobody is
//!   standing near is a `Vec` entry and nothing else, because an item that is
//!   never simulated is still an item lying where it fell. A falling block that
//!   is never simulated is a **hole in the world**: the cell it left is air, the
//!   block it will become has not landed, and a player who walks back finds
//!   neither. So this pays for the whole list every tick, and what makes that
//!   affordable is that a fall is over in a second or two and
//!   [`MAX_ENTITIES`] is small.
//! * **No merging and no pickup.** Two falling sands are two blocks.
//! * **It lands as a block**, and that is the whole point: the entity exists
//!   for the seconds between one cell and another.
//!
//! # What a player feels
//!
//! Vanilla's numbers throughout — the two-tick pause before a column starts to
//! go, the same gravity and the same drag, the landing in the cell above
//! whatever stopped it. The client runs a falling block entity's physics
//! itself, exactly as it runs an item's, so this sends `AddEntity` once and a
//! removal once and nothing in between.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use dust_protocol::packets::play;
use dust_protocol::types::{Angle, Uuid, VarInt};
use tokio::sync::broadcast;

use dust_world::coords::ChunkPos;

/// Blocks per tick per tick. Vanilla's falling-block gravity, which is also an
/// item's.
const GRAVITY: f64 = 0.04;

/// What a tick multiplies vertical motion by. Vanilla's 0.98.
const DRAG: f64 = 0.98;

/// How long a falling block lives before it gives up and drops as an item, in
/// ticks. Vanilla's 600 — thirty seconds, which is what a block falling down a
/// shaft into the void gets.
pub const LIFETIME_TICKS: u32 = 600;

/// How many falling blocks exist at once before the oldest is landed early.
///
/// Smaller than the item ceiling by a factor of eight, and deliberately: a
/// falling block is ticked whether anybody is near it or not, so this number is
/// a per-tick cost and the item one is not. Five hundred and twelve is eight
/// full columns of sand falling at once, which is more than any one player
/// makes.
pub const MAX_ENTITIES: usize = 512;

/// How many falling changes a slow session may fall behind before it is told.
const FALLING_BACKLOG: usize = 128;

/// One block on its way down.
#[derive(Debug, Clone)]
pub struct FallingBlock {
    pub id: i32,
    pub uuid: u128,
    /// The block state that will be put down where this lands.
    pub state: u32,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub vy: f64,
    /// Ticks lived, against [`LIFETIME_TICKS`].
    pub age: u32,
}

/// Something that happened to a falling block, for the sessions to relay.
#[derive(Debug, Clone)]
pub enum FallingChange {
    /// It exists now, here, falling. The client takes it from there.
    Spawned {
        id: i32,
        uuid: u128,
        state: u32,
        x: f64,
        y: f64,
        z: f64,
        vy: f64,
    },
    /// It is not an entity any more. The block it became travels on the edit
    /// channel, which is what every other block change travels on.
    Gone { id: i32, x: f64, z: f64 },
}

impl FallingChange {
    /// Where this happened, for a session deciding whether its player holds
    /// the column.
    #[must_use]
    pub fn at(&self) -> (f64, f64) {
        match self {
            Self::Spawned { x, z, .. } | Self::Gone { x, z, .. } => (*x, *z),
        }
    }
}

/// Where a falling block ended up, handed back to whoever ticked it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Landing {
    /// It became a block again, in this cell.
    Placed { state: u32, x: i32, y: i32, z: i32 },
    /// It could not: the cell it stopped in was taken, or it ran out of time.
    /// The caller drops the block's own item there instead, exactly as vanilla
    /// does.
    Spilled { state: u32, x: i32, y: i32, z: i32 },
}

/// Every block currently in the air.
#[derive(Debug)]
pub struct FallingWorld {
    entities: Mutex<Vec<FallingBlock>>,
    announce: broadcast::Sender<FallingChange>,
    /// Readable without the lock, so a tick with nothing falling costs one
    /// atomic read.
    live: AtomicUsize,
}

impl Default for FallingWorld {
    fn default() -> Self {
        let (announce, _) = broadcast::channel(FALLING_BACKLOG);
        Self {
            entities: Mutex::new(Vec::new()),
            announce,
            live: AtomicUsize::new(0),
        }
    }
}

impl FallingWorld {
    /// Listen for every falling block from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<FallingChange> {
        self.announce.subscribe()
    }

    /// How many blocks are in the air.
    pub fn len(&self) -> usize {
        self.live.load(Ordering::Relaxed)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Turn the block at a cell into a falling one.
    ///
    /// The caller has already made the cell air; this is only the entity. Two
    /// steps and not one because the block change belongs on the edit channel
    /// with every other block change, and an entity spawn does not.
    ///
    /// Returns `None` when the ceiling is reached, which the caller reads as
    /// "leave the block where it is" — a sand block that stays put is a great
    /// deal better than one that disappears.
    pub fn spawn(&self, id: i32, state: u32, x: i32, y: i32, z: i32) -> Option<i32> {
        let mut entities = self
            .entities
            .lock()
            .expect("the falling world is never poisoned");
        if entities.len() >= MAX_ENTITIES {
            return None;
        }
        // Derived from the id for the reason an item's is: a client keys
        // entities by id and uses the uuid only for equality.
        let uuid = 0x2_0000_0000_0000_0000u128 | u128::from(id as u32);
        let entity = FallingBlock {
            id,
            uuid,
            state,
            x: f64::from(x) + 0.5,
            y: f64::from(y),
            z: f64::from(z) + 0.5,
            vy: 0.0,
            age: 0,
        };
        let _ = self.announce.send(FallingChange::Spawned {
            id,
            uuid,
            state,
            x: entity.x,
            y: entity.y,
            z: entity.z,
            vy: 0.0,
        });
        entities.push(entity);
        self.live.store(entities.len(), Ordering::Relaxed);
        Some(id)
    }

    /// One tick of every falling block, appending what landed to `landed`.
    ///
    /// `free` answers whether a cell can be fallen through, which is the
    /// caller's business because it needs the world and the constants table.
    /// `floor` is the lowest y in the world; a block that reaches it has left
    /// the world and is spilled rather than placed at a coordinate the chunk
    /// cannot hold.
    pub fn tick(
        &self,
        floor: i32,
        free: impl Fn(i32, i32, i32) -> bool,
        landed: &mut Vec<Landing>,
    ) {
        let mut entities = self
            .entities
            .lock()
            .expect("the falling world is never poisoned");
        if entities.is_empty() {
            return;
        }
        let mut index = 0;
        while index < entities.len() {
            let entity = &mut entities[index];
            entity.age = entity.age.saturating_add(1);
            entity.vy -= GRAVITY;
            let next = entity.y + entity.vy;
            let cell = (
                entity.x.floor() as i32,
                next.floor() as i32,
                entity.z.floor() as i32,
            );
            let expired = entity.age >= LIFETIME_TICKS;
            let stopped = cell.1 < floor || !free(cell.0, cell.1, cell.2);
            if !stopped && !expired {
                entity.y = next;
                entity.vy *= DRAG;
                index += 1;
                continue;
            }
            // The cell it comes to rest in is the one **above** whatever
            // stopped it, because the entity's own origin is its feet. A block
            // placed in the stopping cell would replace the floor it landed
            // on, which is a sand column that eats the stone under it.
            let (x, y, z) = if expired && !stopped {
                (cell.0, cell.1.max(floor), cell.2)
            } else {
                (cell.0, (cell.1 + 1).max(floor), cell.2)
            };
            let gone = entities.remove(index);
            let landing = if !expired && free(x, y, z) {
                Landing::Placed {
                    state: gone.state,
                    x,
                    y,
                    z,
                }
            } else {
                Landing::Spilled {
                    state: gone.state,
                    x,
                    y,
                    z,
                }
            };
            landed.push(landing);
            let _ = self.announce.send(FallingChange::Gone {
                id: gone.id,
                x: gone.x,
                z: gone.z,
            });
        }
        self.live.store(entities.len(), Ordering::Relaxed);
    }

    /// Which columns the falling blocks are in, for the server's claim on
    /// them.
    ///
    /// **Where it will be and not where it is**, for the reason
    /// `items::footprint_into` gives: the tick moves the block and then asks
    /// the world about the cell it moved into, and a claim made on the cell it
    /// left is a claim on the wrong column at the moment it crosses a border.
    pub fn footprint_into(&self, out: &mut Vec<ChunkPos>) {
        out.clear();
        let entities = self
            .entities
            .lock()
            .expect("the falling world is never poisoned");
        for entity in entities.iter() {
            out.push(ChunkPos::new(
                (entity.x.floor() as i32) >> 4,
                (entity.z.floor() as i32) >> 4,
            ));
        }
    }

    /// Everything falling within `reach` of a point, for a session that has
    /// just joined.
    pub fn visible_from(&self, at: (f64, f64, f64), reach: f64, out: &mut Vec<FallingChange>) {
        let entities = self
            .entities
            .lock()
            .expect("the falling world is never poisoned");
        for entity in entities.iter() {
            let dx = entity.x - at.0;
            let dz = entity.z - at.2;
            if dx * dx + dz * dz > reach * reach {
                continue;
            }
            out.push(FallingChange::Spawned {
                id: entity.id,
                uuid: entity.uuid,
                state: entity.state,
                x: entity.x,
                y: entity.y,
                z: entity.z,
                vy: entity.vy,
            });
        }
    }
}

/// The packet that puts a falling block in a client's world.
///
/// `data` carries the block state id, which is what a falling block entity's
/// spawn packet means by it and is the only way the client knows whether it is
/// drawing sand or an anvil.
#[must_use]
pub fn spawn_packet(
    change: &FallingChange,
    falling_entity_type: i32,
) -> Option<play::clientbound::AddEntity> {
    let FallingChange::Spawned {
        id,
        uuid,
        state,
        x,
        y,
        z,
        vy,
    } = change
    else {
        return None;
    };
    Some(play::clientbound::AddEntity {
        entity_id: VarInt(*id),
        uuid: Uuid(*uuid),
        kind: VarInt(falling_entity_type),
        x: *x,
        y: *y,
        z: *z,
        pitch: Angle::from_degrees(0.0),
        yaw: Angle::from_degrees(0.0),
        head_yaw: Angle::from_degrees(0.0),
        data: VarInt(*state as i32),
        velocity: play::EntityVelocity {
            x: 0,
            y: (vy * 8000.0).clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16,
            z: 0,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether a cell can be fallen **through**, which is the sense the tick
    /// takes and the opposite of "is there a block here". Everything from y = 1
    /// up is open and everything at y = 0 and below is floor.
    ///
    /// Written the right way round after being written the wrong way round:
    /// the first version of this helper answered "is it solid" under the name
    /// the tick reads as "is it free", and three cases passed a block straight
    /// through the ground.
    fn open(_x: i32, y: i32, _z: i32) -> bool {
        y >= 1
    }

    #[test]
    fn a_block_falls_and_lands_on_the_cell_above_what_stopped_it() {
        let world = FallingWorld::default();
        world.spawn(1, 42, 3, 8, 5).expect("there is room");
        let mut landed = Vec::new();
        for _ in 0..200 {
            world.tick(-64, open, &mut landed);
            if !landed.is_empty() {
                break;
            }
        }
        assert_eq!(
            landed,
            vec![Landing::Placed {
                state: 42,
                x: 3,
                y: 1,
                z: 5
            }],
            "the block lands on top of the floor and not inside it"
        );
        assert!(world.is_empty());
    }

    #[test]
    fn a_one_block_fall_takes_seven_ticks() {
        // Vanilla's arithmetic and the reason it is asserted: 0.04 a tick
        // squared covers the first block in seven ticks and the tenth in two,
        // which is what makes sand read as sand. A block that took a second to
        // drop one cell reads as lag, and one that teleported reads as a
        // missing animation.
        let world = FallingWorld::default();
        world.spawn(1, 42, 0, 2, 0).expect("there is room");
        let mut landed = Vec::new();
        let mut ticks = 0;
        while landed.is_empty() && ticks < 100 {
            world.tick(-64, open, &mut landed);
            ticks += 1;
        }
        assert_eq!(ticks, 7);
        assert!(matches!(landed.as_slice(), [Landing::Placed { y: 1, .. }]));
    }

    #[test]
    fn the_ceiling_refuses_rather_than_forgetting_the_oldest() {
        // The opposite of the item world's rule, and on purpose: an item over
        // the ceiling is a dropped cobblestone nobody misses, and a falling
        // block over the ceiling is a hole in somebody's build. Refusing means
        // the caller leaves the block where it is.
        let world = FallingWorld::default();
        for id in 0..MAX_ENTITIES {
            assert!(world.spawn(id as i32, 42, 0, 100, 0).is_some());
        }
        assert!(world.spawn(9_999, 42, 0, 100, 0).is_none());
    }

    #[test]
    fn a_block_that_runs_out_of_time_spills_rather_than_vanishing() {
        let world = FallingWorld::default();
        world.spawn(1, 42, 0, 100, 0).expect("there is room");
        let mut landed = Vec::new();
        for _ in 0..LIFETIME_TICKS + 2 {
            // Nothing is ever solid and the floor is out of reach, so it never
            // lands and the clock is the only thing that can end it.
            world.tick(-100_000, |_, _, _| true, &mut landed);
        }
        assert!(
            matches!(landed.as_slice(), [Landing::Spilled { .. }]),
            "a block that never lands drops as an item rather than being forgotten"
        );
    }

    #[test]
    fn the_floor_of_the_world_stops_it() {
        let world = FallingWorld::default();
        world.spawn(1, 42, 0, -60, 0).expect("there is room");
        let mut landed = Vec::new();
        for _ in 0..200 {
            world.tick(-64, |_, _, _| true, &mut landed);
            if !landed.is_empty() {
                break;
            }
        }
        assert!(
            matches!(landed.as_slice(), [Landing::Placed { y: -64, .. }]),
            "the block stops at the bottom of the world rather than at a \
             coordinate no chunk can hold, and it was {landed:?}"
        );
    }
}
