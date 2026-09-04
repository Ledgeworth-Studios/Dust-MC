//! What the world does about a block that changed: the queue, and the tick
//! that drains it.
//!
//! # The mechanism, and why it is a queue rather than the edit channel
//!
//! A change at one cell is news for the cells around it. Vanilla delivers that
//! news synchronously and recursively inside the write, bounded by
//! `max-chained-neighbor-updates`, which ships at a million: one block break
//! may legitimately touch a million positions before the server does anything
//! else. Dust puts the positions in a queue instead and drains at most
//! [`PER_TICK`] of them per tick, which turns an unbounded stall into a bounded
//! rate. The visible cost is that a torch falls off its wall on the tick after
//! the wall went rather than in the same instant, which is fifty milliseconds
//! and which nobody can see.
//!
//! The obvious alternative was to have this listen on the edit channel the
//! sessions already listen on, which would have cost no new field and no new
//! lock in the write path. It is wrong, and the reason is the failure mode
//! this whole task has to avoid: a `broadcast` channel drops the oldest for a
//! receiver that lags, and the receiver that lags is exactly the one draining
//! a cascade. A dropped edit is a torch that stays in the air for ever — a
//! world that updates *almost* right, which reads as broken rather than as
//! absent. The queue is explicit, is deduplicated, and says out loud when it
//! overflows.
//!
//! # What one change costs
//!
//! Seven positions are pushed per write: the cell and its six neighbours. Each
//! is one hash insert into a set that already holds it or does not, and a
//! push. Draining one costs a block read and, for the 20,110 states of 26,684
//! that survive alone and do not fall, **two bit tests and nothing else** —
//! which is what almost every neighbour of almost every edit is. Only a cell
//! that answers yes to [`Rules::reacts`](dust_sim::updates::Rules::reacts)
//! pays for the six reads that build its neighbourhood.
//!
//! # Leaves, which are the reason the queue has to cascade at all
//!
//! Everything else here is a rule about one cell and its six neighbours. A
//! leaf is not: it holds a `distance` counted up from the nearest log, and
//! felling a trunk changes that number for a canopy of a hundred cells, each
//! of which learns it from the one beside it. That is why the queue exists in
//! the shape it does — a relabelled leaf is written like any other block, and
//! the write queues its own six neighbours, so the front spreads outward and
//! stops on its own when a distance stops changing.
//!
//! # Scheduled ticks, which the save format now has to carry
//!
//! Sand does not fall the instant its support goes: vanilla schedules a tick
//! two ticks out and the block becomes an entity when that tick comes due.
//! That two-tick pause is the difference between a column of sand that
//! collapses like sand and one that snaps out of existence. Dust therefore has
//! a scheduled-tick queue for the first time, which decision record 0012's
//! note said the Anvil round trip copies rather than models. It still copies
//! them; what has changed is that Dust now makes some of its own, and record
//! 0040 says what that means for a save.

use std::collections::{HashSet, VecDeque};

use dust_protocol::types::Position;
use dust_sim::placement::Face;
use dust_sim::updates::{Reaction, Rules};

use super::edits::EditedWorld;
use super::falling::{FallingWorld, Landing};
use super::items::ItemWorld;
use super::players::Roster;

/// How many queued positions one tick may look at.
///
/// Four thousand and ninety-six. The number is a **policy about how often an
/// O(n) step may run**, and it lives here because it is the caller's to set:
/// at the measured 41 ns a position for a cell that reacts to nothing, a full
/// tick of this is 168 microseconds of a fifty-millisecond budget, or a third
/// of one per cent. A cascade longer than that is spread over the ticks after
/// it, which is what stops one break from being a stall.
pub const PER_TICK: usize = 4_096;

/// How many positions may be waiting at once.
///
/// Sixteen thousand three hundred and eighty-four, which is four ticks of
/// draining. Past it new positions are refused and counted rather than
/// queued: a queue with no ceiling is the shape of every server that has ever
/// been killed by one player and a bucket.
pub const MAX_PENDING: usize = 16_384;

/// How many ticks after its support goes before a block starts to fall.
/// Vanilla's `FallingBlock` tick delay.
pub const FALL_DELAY: u64 = 2;

/// The chance, per tick, that Minecraft looks at any one decaying leaf.
///
/// Vanilla decides a leaf's fate on a **random tick**: three positions are
/// drawn out of each sixteen-cubed section every tick, so a given cell is
/// looked at with probability three in 4,096 and a leaf that has lost its
/// tree waits a mean of 1,365 ticks — about a minute — before it goes. That
/// wait is the whole look of a felled tree, and it is why this is not done
/// the instant the log goes: a canopy that vanishes with the trunk reads as
/// the block having been part of the trunk, and one that pops out over a
/// minute reads as a tree dying.
///
/// Dust has no random ticking and this does not add one. A random tick over
/// every loaded section is an O(loaded world) step run twenty times a second
/// whose only caller here would be a few hundred leaves; drawing each leaf's
/// wait from the same geometric distribution the moment it becomes decayable
/// gives a player the identical thing to look at and costs one draw per leaf.
/// That is the second decision-rule priority deciding between two options the
/// first cannot tell apart.
pub const DECAY_CHANCE: (u64, u64) = (3, 4_096);

/// The longest a leaf may be made to wait, in ticks. Five minutes.
///
/// A geometric draw has no upper bound, and a leaf scheduled an hour out is an
/// entry held for an hour. The tail past five minutes is 1.2% of leaves, and
/// by then the tree has been gone for four and a half minutes and nobody is
/// watching the last of it.
pub const DECAY_HORIZON: u64 = 6_000;

/// Cells whose surroundings changed and that nobody has looked at yet.
///
/// A queue for the order and a set for the membership. Both, because the order
/// is what makes a collapse look like a collapse — the cell nearest the break
/// goes first — and the set is what stops a cascade from queueing the same
/// position once per neighbour that touched it, which for a wall of sand is
/// six times each.
#[derive(Debug, Default)]
pub struct Pending {
    queue: VecDeque<Position>,
    /// Keyed by the three coordinates rather than by `Position`, for the
    /// reason the edit map is: a hash on a domain type is the map's
    /// requirement leaking into the world's vocabulary.
    waiting: HashSet<(i32, i32, i32)>,
    /// How many were refused because the ceiling was reached.
    refused: u64,
}

impl Pending {
    /// Note that this cell, and the six around it, may have to react.
    ///
    /// Returns how many were newly queued, which is what a test asserts on and
    /// what the server counts.
    pub fn touch(&mut self, position: Position) -> usize {
        let mut queued = usize::from(self.push(position));
        for side in Face::ALL {
            let (dx, dy, dz) = side.offset();
            queued += usize::from(self.push(Position {
                x: position.x + dx,
                y: position.y + dy,
                z: position.z + dz,
            }));
        }
        queued
    }

    fn push(&mut self, position: Position) -> bool {
        if self.queue.len() >= MAX_PENDING {
            self.refused += 1;
            return false;
        }
        if !self.waiting.insert((position.x, position.y, position.z)) {
            return false;
        }
        self.queue.push_back(position);
        true
    }

    /// Take up to `limit` positions off the front.
    pub fn take(&mut self, limit: usize, out: &mut Vec<Position>) {
        out.clear();
        while out.len() < limit {
            let Some(position) = self.queue.pop_front() else {
                break;
            };
            self.waiting.remove(&(position.x, position.y, position.z));
            out.push(position);
        }
    }

    /// How many are waiting.
    #[must_use]
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// How many updates were dropped on the floor because the queue was full.
    #[must_use]
    pub fn refused(&self) -> u64 {
        self.refused
    }
}

/// Work with a tick to wait for: which cell, and when.
///
/// A sorted `Vec` and not a heap. The queue holds the handful of cells that
/// are mid-collapse — sand two ticks from falling — and every push is at or
/// near the end because a delay is a constant, so the insertion is O(1) in
/// practice and the drain is a walk off the front. A `BinaryHeap` would be
/// asymptotically better at a size this never reaches and would not keep ties
/// in the order they were scheduled, which is what makes a column of sand
/// collapse from the bottom.
#[derive(Debug, Default)]
pub struct Schedule {
    due: VecDeque<(u64, Position)>,
    waiting: HashSet<(i32, i32, i32)>,
}

impl Schedule {
    /// Ask for this cell to be looked at on `at`, unless it already is.
    pub fn at(&mut self, at: u64, position: Position) {
        if !self.waiting.insert((position.x, position.y, position.z)) {
            return;
        }
        // Almost always the end, because every caller uses the same delay.
        let mut insert = self.due.len();
        while insert > 0 && self.due[insert - 1].0 > at {
            insert -= 1;
        }
        self.due.insert(insert, (at, position));
    }

    /// Everything due at or before `now`.
    pub fn take_due(&mut self, now: u64, out: &mut Vec<Position>) {
        out.clear();
        while let Some((at, position)) = self.due.front().copied() {
            if at > now {
                break;
            }
            self.due.pop_front();
            self.waiting.remove(&(position.x, position.y, position.z));
            out.push(position);
        }
    }

    /// How many cells are waiting for a tick.
    #[must_use]
    pub fn len(&self) -> usize {
        self.due.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.due.is_empty()
    }
}

/// What a broken block yields, and where it lands.
///
/// One implementation of the loot roll rather than two: a break a player asked
/// for and a break the world decided on drop the same things, and a second
/// copy of this would be a second place for a fortune branch to be wrong. The
/// session builds one of these out of its context; the tick loop builds one
/// out of its own handles.
#[derive(Debug)]
pub struct Spill<'a> {
    pub drops: &'a dust_sim::drops::Tables,
    pub items: &'a ItemWorld,
    pub roster: &'a Roster,
    pub constants: Option<&'a dust_registry::BlockConstants>,
    /// The `requires_tool` column, resolved once at boot.
    pub requires_tool: Option<dust_registry::constants::Flag>,
}

impl Spill<'_> {
    /// Roll what `previous` yields and pop it out of `at`.
    ///
    /// `neighbours` are the `(offset, state)` pairs a table may read — the
    /// cells above and below, which is how a double-tall plant decides which
    /// half of itself drops. `held` is the stack that broke it, and is `None`
    /// when nothing did: a block that fell off a wall was broken by the world,
    /// and the world holds no pickaxe and no enchantment.
    pub fn at(
        &self,
        at: Position,
        previous: u32,
        neighbours: &[(i8, u32)],
        held: Option<&super::inventory::Stack>,
        seed: u64,
    ) {
        let Some(state) = dust_registry::BlockState::from_id(previous) else {
            return;
        };
        let Some(table) = self.drops.table(state.block()) else {
            // No table for this block, which is not "drops nothing" — see
            // `dust_sim::drops::Tables::table`. Nothing is dropped either way,
            // and the difference is why this branch is written down rather
            // than being the same `return` as an empty roll.
            return;
        };
        let around: Vec<(i8, dust_registry::BlockState)> = neighbours
            .iter()
            .filter_map(|(offset, state)| {
                dust_registry::BlockState::from_id(*state).map(|state| (*offset, state))
            })
            .collect();
        let enchantments = held
            .and_then(|stack| stack.components.component("minecraft:enchantments"))
            .map(dust_registry::enchantments::parse)
            .unwrap_or_default();
        let context = dust_sim::drops::Break {
            state,
            requires_tool: match (self.constants, self.requires_tool) {
                (Some(constants), Some(flag)) => constants.is_set(flag, previous),
                _ => false,
            },
            tool: dust_sim::drops::Tool {
                item: held.map(|stack| stack.item),
                enchantments: &enchantments,
            },
            // Whether anybody broke it. `held` is the stack that did, and is
            // `None` exactly when the world decided: a torch that fell off a
            // wall, a leaf that ran out of tree, a falling block that could
            // not land. Saying `true` there would let a loot condition that
            // asks whether an entity did this answer yes for a break nobody
            // made.
            broken_by_entity: held.is_some(),
            neighbours: &around,
        };
        let mut rolled = Vec::new();
        let mut rng = dust_sim::drops::Rng::from_seed(seed);
        table.roll(&context, &mut rng, &mut rolled);
        for drop in rolled {
            let limit = u32::from(drop.item.max_stack_size().max(1));
            let mut left = drop.count;
            while left > 0 {
                let taken = left.min(limit);
                left -= taken;
                self.items.pop(
                    self.roster,
                    at,
                    drop.item,
                    u8::try_from(taken).unwrap_or(u8::MAX),
                    seed ^ u64::from(left),
                );
            }
        }
    }
}

/// The tick loop's block updates: what reacts, what falls, and what lands.
pub struct WorldTicker {
    world: std::sync::Arc<EditedWorld>,
    items: std::sync::Arc<ItemWorld>,
    falling: std::sync::Arc<FallingWorld>,
    roster: std::sync::Arc<Roster>,
    drops: std::sync::Arc<dust_sim::drops::Tables>,
    constants: Option<std::sync::Arc<dust_registry::BlockConstants>>,
    requires_tool: Option<dust_registry::constants::Flag>,
    air: u32,
    schedule: Schedule,
    /// Reused between ticks, for the reason every other participant's buffers
    /// are: a `Vec` allocated twenty times a second to hold a handful of
    /// positions is an allocation twenty times a second.
    batch: Vec<Position>,
    landed: Vec<Landing>,
    footprint: Vec<dust_world::coords::ChunkPos>,
    claim: super::residency::ColumnClaim,
    /// The stream the world's own breaks roll their loot out of.
    seed: u64,
    /// How many cells this server has broken, dropped and landed, for the log
    /// and for the bench.
    counts: Counts,
}

/// What the block-update tick has done since the server started.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// Positions taken off the queue and looked at.
    pub examined: u64,
    /// Cells that could not stay and were dropped.
    pub broken: u64,
    /// Cells that became a falling entity.
    pub fell: u64,
    /// Falling entities that became a block again.
    pub landed: u64,
    /// Falling entities that could not, and spilled as an item instead.
    pub spilled: u64,
    /// Leaves given a new `distance` because the nearest log moved.
    pub relabelled: u64,
    /// Leaves that ran out of tree and came down.
    pub decayed: u64,
}

impl std::fmt::Debug for WorldTicker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorldTicker")
            .field("counts", &self.counts)
            .field("scheduled", &self.schedule.len())
            .finish_non_exhaustive()
    }
}

impl WorldTicker {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        world: std::sync::Arc<EditedWorld>,
        items: std::sync::Arc<ItemWorld>,
        falling: std::sync::Arc<FallingWorld>,
        roster: std::sync::Arc<Roster>,
        drops: std::sync::Arc<dust_sim::drops::Tables>,
        constants: Option<std::sync::Arc<dust_registry::BlockConstants>>,
        air: u32,
    ) -> Self {
        let requires_tool = constants
            .as_deref()
            .and_then(|table| table.flag("requires_tool"));
        Self {
            claim: super::residency::ColumnClaim::new(world.residency(), world.warming()),
            world,
            items,
            falling,
            roster,
            drops,
            constants,
            requires_tool,
            air,
            schedule: Schedule::default(),
            batch: Vec::new(),
            landed: Vec::new(),
            footprint: Vec::new(),
            seed: 0x9e37_79b9_7f4a_7c15,
            counts: Counts::default(),
        }
    }

    /// What this participant has done. Public for the bench and the log.
    #[must_use]
    pub fn counts(&self) -> Counts {
        self.counts
    }

    /// How long this leaf waits before it comes down.
    ///
    /// A geometric draw with [`DECAY_CHANCE`], which is the distribution a
    /// random tick produces, so a canopy goes the way a canopy goes rather
    /// than all at once. The float is here and not in a tick loop: this runs
    /// once for each leaf that loses its tree and never per tick.
    fn decay_delay(&mut self) -> u64 {
        #[allow(clippy::cast_precision_loss)]
        let uniform =
            ((self.next_seed() >> 11) as f64 / (1u64 << 53) as f64).max(f64::MIN_POSITIVE);
        #[allow(clippy::cast_precision_loss)]
        let chance = DECAY_CHANCE.0 as f64 / DECAY_CHANCE.1 as f64;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let ticks = (uniform.ln() / (1.0 - chance).ln()).ceil() as u64;
        ticks.clamp(1, DECAY_HORIZON)
    }

    fn next_seed(&mut self) -> u64 {
        self.seed = self
            .seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.seed
    }

    fn spill(&self) -> Spill<'_> {
        Spill {
            drops: &self.drops,
            items: &self.items,
            roster: &self.roster,
            constants: self.constants.as_deref(),
            requires_tool: self.requires_tool,
        }
    }

    /// Break a cell because the world says it cannot be there, and drop it.
    ///
    /// `by` is `-1` and never a player: nobody did this, so nobody is left out
    /// of the effect. A player standing beside a torch that falls off a wall
    /// hears it break, which is the whole difference between a block that
    /// disappeared and a block that broke.
    fn topple(&mut self, at: Position, state: u32) {
        let below = self.world.block_at(Position { y: at.y - 1, ..at });
        let above = self.world.block_at(Position { y: at.y + 1, ..at });
        if !self.world.break_block(at, self.air, -1) {
            return;
        }
        let seed = self.next_seed();
        self.spill()
            .at(at, state, &[(-1i8, below), (1i8, above)], None, seed);
        self.counts.broken += 1;
    }

    /// Write a leaf's new distance from the nearest log.
    ///
    /// The write queues this cell and its six neighbours like any other, which
    /// is the whole of the cascade: a felled trunk relabels the ring of leaves
    /// touching it, each of those relabels the ring beyond, and the front
    /// stops when a distance stops changing. Nothing here needs to know how
    /// big the canopy is.
    fn relabel(&mut self, at: Position, next: dust_registry::BlockState) {
        self.world.set_block(at, next.id());
        self.counts.relabelled += 1;
    }

    /// Turn a cell into a falling entity, or leave it alone if it cannot be.
    fn launch(&mut self, at: Position, state: u32) {
        let id = self.roster.claim_entity_id();
        // Air first and the entity second, so the cell is never both a block
        // and an entity. The other order is a block a client draws twice.
        if self.falling.spawn(id, state, at.x, at.y, at.z).is_none() {
            // The ceiling. The block stays where it is, which is the right
            // failure: a server under a sand flood that refuses to animate is
            // one a player can dig out of, and one that deletes the sand is
            // not.
            return;
        }
        self.world.set_block(at, self.air);
        self.counts.fell += 1;
    }
}

impl crate::participant::TickParticipant for WorldTicker {
    fn name(&self) -> &str {
        "updates"
    }

    /// Ahead of the item physics, because a block that breaks this tick should
    /// have its drops in the world before the drops are moved.
    fn priority(&self) -> i32 {
        -1
    }

    fn tick(&mut self, ctx: &crate::participant::TickContext) {
        let Some(constants) = self.constants.clone() else {
            // No table, no rules. The server every operator had before this
            // landed, and a great deal better than one that guesses which
            // blocks hold each other up.
            return;
        };
        let Some(rules) = Rules::from_constants(&constants) else {
            return;
        };

        // The columns everything about to be touched is in, claimed before the
        // tick that reads them rather than after.
        if !self.falling.is_empty() {
            self.falling.footprint_into(&mut self.footprint);
            let mut wanted = std::mem::take(&mut self.footprint);
            self.claim.set(&mut wanted);
            self.footprint = wanted;
        }

        // 1. Blocks in the air.
        if !self.falling.is_empty() {
            let world = &self.world;
            let height = world.height();
            self.landed.clear();
            let mut landed = std::mem::take(&mut self.landed);
            self.falling.tick(
                height.min_y(),
                |x, y, z| {
                    let state = world.block_at(Position { x, y, z });
                    dust_registry::BlockState::from_id(state).is_none_or(|state| rules.free(state))
                },
                &mut landed,
            );
            for landing in landed.drain(..) {
                match landing {
                    Landing::Placed { state, x, y, z } => {
                        self.world.set_block(Position { x, y, z }, state);
                        self.counts.landed += 1;
                    }
                    Landing::Spilled { state, x, y, z } => {
                        let at = Position { x, y, z };
                        let seed = self.next_seed();
                        self.spill().at(at, state, &[], None, seed);
                        self.counts.spilled += 1;
                    }
                }
            }
            self.landed = landed;
        }

        // 2. Cells whose scheduled tick has come due.
        if !self.schedule.is_empty() {
            let mut batch = std::mem::take(&mut self.batch);
            self.schedule.take_due(ctx.tick_index, &mut batch);
            for at in batch.drain(..) {
                let id = self.world.block_at(at);
                let Some(state) = dust_registry::BlockState::from_id(id) else {
                    continue;
                };
                let around = self.world.around(at);
                // The whole question again rather than only the one that was
                // asked when this was scheduled. A cell whose support came
                // back in the two ticks it waited should stay, and a leaf
                // whose tree was replanted in the minute it waited should
                // stay: re-asking is what makes the delay a pause rather than
                // a decision already taken.
                match rules.reaction(state, around) {
                    Reaction::Stay => {}
                    Reaction::Break => self.topple(at, id),
                    Reaction::Fall => self.launch(at, id),
                    Reaction::Relabel(next) => self.relabel(at, next),
                    Reaction::Decay => {
                        self.topple(at, id);
                        self.counts.decayed += 1;
                    }
                }
            }
            self.batch = batch;
        }

        // 3. Cells somebody changed the neighbourhood of.
        let mut batch = std::mem::take(&mut self.batch);
        self.world.take_updates(PER_TICK, &mut batch);
        for at in batch.drain(..) {
            self.counts.examined += 1;
            let id = self.world.block_at(at);
            let Some(state) = dust_registry::BlockState::from_id(id) else {
                continue;
            };
            // The cheap half: two bit tests, and this is where all but 6,606
            // of 26,684 states leave.
            if !rules.reacts(state) {
                continue;
            }
            let around = self.world.around(at);
            match rules.reaction(state, around) {
                Reaction::Stay => {}
                Reaction::Break => self.topple(at, id),
                // Not now: vanilla waits two ticks, and that pause is what
                // makes a column of sand collapse like sand.
                Reaction::Fall => self.schedule.at(ctx.tick_index + FALL_DELAY, at),
                Reaction::Relabel(next) => self.relabel(at, next),
                // Not now either, and for longer: see `decay_delay`.
                Reaction::Decay => {
                    let delay = self.decay_delay();
                    self.schedule.at(ctx.tick_index + delay, at);
                }
            }
        }
        self.batch = batch;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: i32, y: i32, z: i32) -> Position {
        Position { x, y, z }
    }

    #[test]
    fn one_change_queues_the_cell_and_its_six_neighbours() {
        let mut pending = Pending::default();
        assert_eq!(pending.touch(at(0, 0, 0)), 7);
        assert_eq!(pending.len(), 7);
    }

    #[test]
    fn a_cell_touched_twice_is_queued_once() {
        // The clause that keeps a cascade from being quadratic: a wall of sand
        // touches every one of its cells from six sides.
        let mut pending = Pending::default();
        pending.touch(at(0, 0, 0));
        assert_eq!(pending.touch(at(0, 0, 0)), 0);
        assert_eq!(pending.len(), 7);
    }

    #[test]
    fn the_queue_refuses_rather_than_growing_without_a_bound() {
        let mut pending = Pending::default();
        let mut y = 0;
        while pending.len() < MAX_PENDING {
            pending.touch(at(y * 3, 0, 0));
            y += 1;
        }
        let before = pending.len();
        pending.touch(at(1_000_000, 0, 0));
        assert_eq!(pending.len(), before);
        assert!(pending.refused() > 0);
    }

    #[test]
    fn positions_come_back_in_the_order_they_arrived() {
        let mut pending = Pending::default();
        pending.touch(at(0, 0, 0));
        let mut out = Vec::new();
        pending.take(3, &mut out);
        assert_eq!(out, vec![at(0, 0, 0), at(0, -1, 0), at(0, 1, 0)]);
        assert_eq!(pending.len(), 4);
    }

    #[test]
    fn a_taken_position_can_be_queued_again() {
        // The set is membership of the *queue* and not a memory of everything
        // ever seen. A cell that reacted this tick has to be able to react
        // again next tick, or a column of sand falls one block and stops.
        let mut pending = Pending::default();
        pending.touch(at(0, 0, 0));
        let mut out = Vec::new();
        pending.take(64, &mut out);
        assert!(pending.is_empty());
        assert_eq!(pending.touch(at(0, 0, 0)), 7);
    }

    #[test]
    fn a_scheduled_tick_comes_due_and_only_once() {
        let mut schedule = Schedule::default();
        schedule.at(10, at(1, 2, 3));
        schedule.at(10, at(1, 2, 3));
        let mut out = Vec::new();
        schedule.take_due(9, &mut out);
        assert!(out.is_empty());
        schedule.take_due(10, &mut out);
        assert_eq!(out, vec![at(1, 2, 3)]);
        schedule.take_due(11, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn ties_keep_the_order_they_were_scheduled_in() {
        // Which is what makes a column of sand collapse from the bottom rather
        // than in whatever order a heap happened to hold.
        let mut schedule = Schedule::default();
        for y in 0..4 {
            schedule.at(5, at(0, y, 0));
        }
        schedule.at(4, at(9, 9, 9));
        let mut out = Vec::new();
        schedule.take_due(5, &mut out);
        assert_eq!(
            out,
            vec![
                at(9, 9, 9),
                at(0, 0, 0),
                at(0, 1, 0),
                at(0, 2, 0),
                at(0, 3, 0)
            ]
        );
    }
}
