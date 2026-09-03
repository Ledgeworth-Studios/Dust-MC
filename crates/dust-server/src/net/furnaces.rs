//! Furnaces: the first block in Dust that has a life of its own.
//!
//! # Why this is not a container
//!
//! Every container this server has had so far is inert between clicks. A
//! crafting table's grid changes only because somebody moved something in it,
//! and when the screen shuts there is nothing left to do. A furnace is the
//! opposite: it burns fuel, advances a timer and produces an item **while
//! nobody is looking at it**, which is most of the time. So its state cannot
//! live on a session — a session is a socket, and the socket closes.
//!
//! It lives here, in the world, beside `EditedWorld`'s block edits and the
//! two shared maps that hold a player's position and pockets. Decision record
//! 0036 is the account.
//!
//! # The tick set, which is the whole resource argument
//!
//! A world can hold thousands of furnaces and almost none of them are doing
//! anything. **A furnace that is not burning and has nothing to start on is
//! not in the tick set at all** — it is a row in a map and costs nothing per
//! tick, not a bounds check, not a branch. Membership changes only when
//! somebody clicks in one or when a burn ends, which is the rule the whole of
//! this project keeps: never per-tick work that can be per-change.
//!
//! What is in the set ticks **wherever it is, whether or not anybody is near**.
//! That is deliberate and it is the one place this costs more than it could.
//! `ItemTicker` skips entities nobody is close to, and it is right to, because
//! an item lying in an empty tunnel is doing nothing a player will ask about.
//! A furnace is the reverse: *walking away is the normal way to use one*. A
//! player lights eight furnaces, goes mining and comes back for the iron, and
//! a server that paused them would be a server where the smelting only happens
//! while you stand and watch it. See [`Furnaces::tick`] for what it costs.
//!
//! # Restart
//!
//! Written into the world's save beside the block edits and read back with
//! them, tick counts and all — see `net::save`. A furnace does **not** advance
//! while the server is down, which is vanilla's behaviour and the one a player
//! can reason about: they come back to the furnace they left.
//!
//! # What a session sees
//!
//! A session with a furnace open holds a *mirror* of the three slots in its
//! own [`Inventory`](super::inventory::Inventory) — see `FURNACE_START` — and
//! that mirror is refreshed from here at the top of every click and written
//! back at the bottom of it, under one lock. It is therefore never stale at
//! the moment it matters, which is what stops a click racing a tick that has
//! just produced an ingot.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use dust_protocol::types::Position;
use dust_registry::placement::ItemBlocks;
use dust_registry::Item;
use dust_sim::cooking::{Cooking, Fire};
use tokio::sync::broadcast;

use super::inventory::Stack;

/// How many slots a furnace has: input, fuel, output.
pub const SLOTS: usize = 3;
/// The slot the fire cooks.
pub const INPUT: usize = 0;
/// The slot the fire burns.
pub const FUEL: usize = 1;
/// The slot the fire fills.
pub const OUTPUT: usize = 2;

/// How fast a lit-then-unlit furnace loses its progress, in ticks per tick.
///
/// Vanilla's `AbstractFurnaceBlockEntity.BURN_COOL_SPEED`. Two, so an
/// unattended furnace that runs out of fuel gives back its half-cooked
/// progress in half the time it took — which a player watches happen on the
/// arrow, and is why it is not simply held or dropped.
const COOL_SPEED: u16 = 2;

/// How many changes a session may fall behind before it is told to resync.
const CHANGE_BACKLOG: usize = 256;

/// One furnace's whole state.
///
/// Everything is a `u16` of ticks. The longest fuel in the game is a lava
/// bucket at 20,000 and the longest cook a campfire at 600, so nothing here
/// needs more, and integers rather than durations means the save and the wire
/// carry the same numbers the tick counts.
#[derive(Debug, Clone, PartialEq)]
pub struct Furnace {
    /// Which fire this is, which decides the recipe table it reads.
    pub fire: Fire,
    /// Input, fuel, output.
    pub slots: [Option<Stack>; SLOTS],
    /// Ticks of fuel left. Zero is not burning.
    pub lit: u16,
    /// What the fuel now burning was worth, so the flame can be drawn at the
    /// right height. Never zero while `lit` is non-zero.
    pub lit_total: u16,
    /// Ticks the current item has cooked for.
    pub cooking: u16,
    /// Ticks the current item takes. Zero when there is nothing to cook.
    pub total: u16,
    /// Experience banked by completed smelts, paid out when a player takes
    /// from the output.
    ///
    /// A float because the recipes are — an iron ingot is worth 0.7 — and
    /// rounding each one would turn every ingot in the game into nothing. It
    /// is rounded once, at the moment somebody takes it.
    pub experience: f32,
}

impl Furnace {
    /// An empty furnace of this fire.
    #[must_use]
    pub fn new(fire: Fire) -> Self {
        Self {
            fire,
            slots: [None, None, None],
            lit: 0,
            lit_total: 0,
            cooking: 0,
            total: 0,
            experience: 0.0,
        }
    }

    /// Whether the fire is alight.
    #[must_use]
    pub fn is_lit(&self) -> bool {
        self.lit > 0
    }

    /// Whether this furnace holds anything at all — items, fire or progress.
    ///
    /// A furnace that holds nothing is forgotten rather than saved: an empty
    /// one is indistinguishable from a furnace that has never been used, and
    /// a world where every furnace anybody ever opened is a row on disk grows
    /// for ever for no reason.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.iter().all(Option::is_none)
            && self.lit == 0
            && self.cooking == 0
            && self.experience == 0.0
    }

    /// Whether this furnace has anything to do — the tick set's membership
    /// test, and the whole of the resource argument in this module.
    ///
    /// Burning, or cooling down from having burned, or holding both a fuel and
    /// something the fire could cook. Anything else is a furnace that will
    /// still be exactly as it is a thousand ticks from now.
    #[must_use]
    pub fn is_active(&self, cooking: Option<&Cooking>, fuel: Option<&ItemBlocks>) -> bool {
        if self.lit > 0 || self.cooking > 0 {
            return true;
        }
        let Some(recipe) = self.recipe(cooking) else {
            return false;
        };
        let _ = recipe;
        self.slots[FUEL]
            .as_ref()
            .and_then(|stack| fuel?.burn(stack.item))
            .is_some()
    }

    /// What the fire would make of what is in the input right now.
    fn recipe<'a>(&self, cooking: Option<&'a Cooking>) -> Option<&'a dust_sim::cooking::Cooked> {
        let input = self.slots[INPUT].as_ref()?;
        cooking?.find(self.fire, input.item)
    }

    /// Whether the output can take what the input would become.
    fn can_burn(&self, cooking: Option<&Cooking>) -> bool {
        let Some(recipe) = self.recipe(cooking) else {
            return false;
        };
        let (item, count) = recipe.result();
        match self.slots[OUTPUT].as_ref() {
            None => true,
            Some(there) => {
                there.stacks_with(&Stack::new(item, count))
                    && u16::from(there.count) + u16::from(count) <= u16::from(item.max_stack_size())
            }
        }
    }

    /// One tick of this furnace, in vanilla's own order.
    ///
    /// The order is not incidental. `litTime` comes down **first**, so a
    /// furnace whose fuel ends this tick spends the next one lighting the next
    /// piece; then the fuel is taken if the fire is out and there is something
    /// worth lighting it for; then, and only if the fire is now alight, the
    /// cook advances. Doing the fuel before the countdown would burn one item
    /// per fuel fewer than the game does, which over a stack of coal is eight
    /// ingots a player does not get.
    ///
    /// Returns whether anything a watcher can see moved.
    fn tick(&mut self, cooking: Option<&Cooking>, fuel: Option<&ItemBlocks>) -> bool {
        let was_lit = self.is_lit();
        if self.lit > 0 {
            self.lit -= 1;
        }
        let mut changed = false;
        let can_burn = self.can_burn(cooking);
        if self.is_lit() || (self.slots[FUEL].is_some() && self.slots[INPUT].is_some()) {
            if !self.is_lit() && can_burn {
                if let Some(worth) = self.light(fuel) {
                    self.lit = worth;
                    self.lit_total = worth;
                    changed = true;
                }
            }
            if self.is_lit() && can_burn {
                // Set here rather than only on an input change, because a
                // furnace restored from a save may have a cook in flight and
                // an input whose recipe the operator's data pack has since
                // changed. Reading the number every tick costs one array load
                // and cannot be stale.
                if let Some(recipe) = self.recipe(cooking) {
                    self.total = recipe.ticks();
                }
                self.cooking += 1;
                if self.cooking >= self.total {
                    self.cooking = 0;
                    self.smelt(cooking);
                    changed = true;
                }
            } else if self.cooking != 0 {
                self.cooking = 0;
                changed = true;
            }
        } else if self.cooking > 0 {
            // Not lit and nothing to light: the arrow retreats rather than
            // sticking where it was, which is what a player sees on a furnace
            // that ran out of coal.
            let cooled = self.cooking.saturating_sub(COOL_SPEED).min(self.total);
            changed |= cooled != self.cooking;
            self.cooking = cooled;
        }
        if was_lit != self.is_lit() {
            changed = true;
        }
        changed
    }

    /// Spend one fuel item, returning what it was worth.
    ///
    /// The empty bucket a lava bucket leaves behind goes back into the fuel
    /// slot, which is where the game puts it and where a player looks for it.
    fn light(&mut self, fuel: Option<&ItemBlocks>) -> Option<u16> {
        let table = fuel?;
        let mut stack = self.slots[FUEL].clone()?;
        let worth = table.burn(stack.item)?;
        let item = stack.item;
        stack.count -= 1;
        self.slots[FUEL] = if stack.count > 0 {
            Some(stack)
        } else {
            dust_sim::crafting::remainder(item).map(|left| Stack::new(left, 1))
        };
        Some(worth)
    }

    /// Turn one input into one output and bank what it was worth.
    fn smelt(&mut self, cooking: Option<&Cooking>) {
        let Some(recipe) = self.recipe(cooking).copied() else {
            return;
        };
        let (item, count) = recipe.result();
        match self.slots[OUTPUT].clone() {
            None => self.slots[OUTPUT] = Some(Stack::new(item, count)),
            Some(mut there) => {
                there.count += count;
                self.slots[OUTPUT] = Some(there);
            }
        }
        if let Some(mut input) = self.slots[INPUT].clone() {
            input.count -= 1;
            self.slots[INPUT] = (input.count > 0).then_some(input);
        }
        self.experience += recipe.experience();
    }

    /// Take the banked experience, leaving none.
    ///
    /// Rounded down, with the fraction paid as a chance — vanilla's
    /// `popExperience`. Eight iron ingots are worth 5.6 points, and a server
    /// that floored every ingot on its own would pay nothing at all for any of
    /// them, while one that rounded up would pay eight.
    pub fn take_experience(&mut self, roll: f32) -> u32 {
        let banked = self.experience;
        self.experience = 0.0;
        if !banked.is_finite() || banked <= 0.0 {
            return 0;
        }
        let whole = banked.floor();
        let fraction = banked - whole;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let mut points = whole as u32;
        if fraction > 0.0 && roll < fraction {
            points += 1;
        }
        points
    }
}

/// What moved in one furnace, for a session that has it open.
#[derive(Debug, Clone)]
pub struct FurnaceChange {
    /// Which furnace.
    pub at: Position,
    /// Its three slots, as they now are.
    pub slots: [Option<Stack>; SLOTS],
    /// The four numbers the furnace screen draws its two bars from, in the
    /// order `container_set_data` numbers them.
    pub properties: [i16; PROPERTIES],
    /// Whether the block's own `lit` state changed with this, so the caller
    /// can put the glowing texture down without asking.
    pub lit: bool,
}

/// How many properties a furnace screen reads: lit, lit total, cook progress,
/// cook total. `AbstractFurnaceMenu`'s `DATA_COUNT`.
pub const PROPERTIES: usize = 4;

/// Every furnace in the world.
#[derive(Debug)]
pub struct Furnaces {
    inner: Mutex<Inner>,
    /// How many are in the tick set, readable without the lock so an idle
    /// server's tick is one atomic load and a return.
    active: AtomicUsize,
    announce: broadcast::Sender<FurnaceChange>,
}

#[derive(Debug)]
struct Inner {
    /// One entry per furnace that has ever held anything, `None` where one was
    /// broken. Tombstones rather than compaction, because [`Inner::active`]
    /// holds indices into this and a compaction would move them.
    cells: Vec<Option<Cell>>,
    at: HashMap<Position, u32>,
    free: Vec<u32>,
    /// The indices that must tick. Every entry's cell is `Some` and its
    /// `active` flag is set; the two are kept in step by [`Inner::refresh`].
    active: Vec<u32>,
}

#[derive(Debug)]
struct Cell {
    at: Position,
    furnace: Furnace,
    active: bool,
}

impl Default for Furnaces {
    fn default() -> Self {
        Self::new()
    }
}

impl Furnaces {
    /// A world with no furnaces in it.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                cells: Vec::new(),
                at: HashMap::new(),
                free: Vec::new(),
                active: Vec::new(),
            }),
            active: AtomicUsize::new(0),
            announce: broadcast::channel(CHANGE_BACKLOG).0,
        }
    }

    /// Watch every furnace that moves.
    ///
    /// One channel for the whole world rather than one per furnace: a session
    /// filters by the position it has open, which is one comparison per change
    /// on a server where changes are rare and sessions are few. A channel per
    /// furnace would be a channel per block.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<FurnaceChange> {
        self.announce.subscribe()
    }

    /// How many furnaces hold anything.
    #[must_use]
    pub fn len(&self) -> usize {
        let inner = self
            .inner
            .lock()
            .expect("the furnace world is never poisoned");
        inner.at.len()
    }

    /// Whether none do.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// How many are in the tick set. One atomic load; no lock.
    #[must_use]
    pub fn active(&self) -> usize {
        self.active.load(Ordering::Relaxed)
    }

    /// Read one furnace, or `None` if that block has never held anything.
    #[must_use]
    pub fn get(&self, at: Position) -> Option<Furnace> {
        let inner = self
            .inner
            .lock()
            .expect("the furnace world is never poisoned");
        let index = *inner.at.get(&at)?;
        inner.cells[index as usize]
            .as_ref()
            .map(|cell| cell.furnace.clone())
    }

    /// Do something to one furnace, under the lock, creating it if it is the
    /// first time anybody has opened this block.
    ///
    /// **The only way in.** A caller that read, thought and wrote would have a
    /// window between the read and the write, and the tick runs in that window
    /// — which is exactly the race that would let a click overwrite an ingot
    /// the fire had just made. Everything a session does to a furnace is one
    /// call to this.
    pub fn with<T>(
        &self,
        at: Position,
        fire: Fire,
        cooking: Option<&Cooking>,
        fuel: Option<&ItemBlocks>,
        act: impl FnOnce(&mut Furnace) -> T,
    ) -> T {
        let mut inner = self
            .inner
            .lock()
            .expect("the furnace world is never poisoned");
        let index = inner.ensure(at, fire);
        let cell = inner.cells[index as usize]
            .as_mut()
            .expect("ensure just put one here");
        let out = act(&mut cell.furnace);
        let change = inner.refresh(index, cooking, fuel, false);
        self.active.store(inner.active.len(), Ordering::Relaxed);
        drop(inner);
        if let Some(change) = change {
            let _ = self.announce.send(change);
        }
        out
    }

    /// Forget one furnace and hand back what it held, because the block is
    /// gone.
    ///
    /// Returns the items so the caller can drop them on the floor. A furnace
    /// that was broken with iron in it and simply forgotten would be items
    /// deleted by a pickaxe.
    pub fn remove(&self, at: Position) -> Option<Furnace> {
        let mut inner = self
            .inner
            .lock()
            .expect("the furnace world is never poisoned");
        let index = inner.at.remove(&at)?;
        let cell = inner.cells[index as usize].take()?;
        inner.active.retain(|held| *held != index);
        inner.free.push(index);
        self.active.store(inner.active.len(), Ordering::Relaxed);
        Some(cell.furnace)
    }

    /// Every furnace that holds something, sorted, for the save.
    #[must_use]
    pub fn snapshot(&self) -> Vec<(Position, Furnace)> {
        let inner = self
            .inner
            .lock()
            .expect("the furnace world is never poisoned");
        let mut out: Vec<(Position, Furnace)> = inner
            .cells
            .iter()
            .flatten()
            .filter(|cell| !cell.furnace.is_empty())
            .map(|cell| (cell.at, cell.furnace.clone()))
            .collect();
        out.sort_by_key(|(at, _)| (at.x, at.y, at.z));
        out
    }

    /// Put back what a save held. Silent: nothing is announced, because
    /// nobody is connected yet.
    pub fn restore(
        &self,
        furnaces: impl IntoIterator<Item = (Position, Furnace)>,
        cooking: Option<&Cooking>,
        fuel: Option<&ItemBlocks>,
    ) -> usize {
        let mut inner = self
            .inner
            .lock()
            .expect("the furnace world is never poisoned");
        let mut restored = 0;
        for (at, furnace) in furnaces {
            let fire = furnace.fire;
            let index = inner.ensure(at, fire);
            if let Some(cell) = inner.cells[index as usize].as_mut() {
                cell.furnace = furnace;
            }
            inner.refresh(index, cooking, fuel, true);
            restored += 1;
        }
        self.active.store(inner.active.len(), Ordering::Relaxed);
        restored
    }

    /// One tick of every furnace that has something to do.
    ///
    /// Returns the block-state changes that have to reach the world — a
    /// furnace that lit or went out — because this holds its own lock and the
    /// world holds another, and taking both in one place is how a deadlock
    /// gets written.
    pub fn tick(
        &self,
        cooking: Option<&Cooking>,
        fuel: Option<&ItemBlocks>,
        lit: &mut Vec<(Position, bool)>,
    ) {
        lit.clear();
        // The common case on a server nobody is smelting on: one atomic load
        // and a return, with no lock taken and no map walked.
        if self.active.load(Ordering::Relaxed) == 0 {
            return;
        }
        let mut changes = Vec::new();
        {
            let mut inner = self
                .inner
                .lock()
                .expect("the furnace world is never poisoned");
            // Walked by index and compacted in place: a furnace that stops
            // being active leaves the set during the same pass that noticed,
            // with no second walk and no allocation.
            let mut write = 0;
            for read in 0..inner.active.len() {
                let index = inner.active[read];
                let Some(cell) = inner.cells[index as usize].as_mut() else {
                    continue;
                };
                let was_lit = cell.furnace.is_lit();
                let moved = cell.furnace.tick(cooking, fuel);
                let now_lit = cell.furnace.is_lit();
                if was_lit != now_lit {
                    lit.push((cell.at, now_lit));
                }
                if moved {
                    changes.push(change_of(cell));
                }
                let still = cell.furnace.is_active(cooking, fuel);
                cell.active = still;
                if still {
                    inner.active[write] = index;
                    write += 1;
                }
            }
            inner.active.truncate(write);
            self.active.store(inner.active.len(), Ordering::Relaxed);
        }
        for change in changes {
            let _ = self.announce.send(change);
        }
    }
}

impl Inner {
    /// The index of the furnace at this block, making an empty one if there is
    /// none.
    fn ensure(&mut self, at: Position, fire: Fire) -> u32 {
        if let Some(index) = self.at.get(&at) {
            return *index;
        }
        let cell = Cell {
            at,
            furnace: Furnace::new(fire),
            active: false,
        };
        let index = match self.free.pop() {
            Some(index) => {
                self.cells[index as usize] = Some(cell);
                index
            }
            None => {
                self.cells.push(Some(cell));
                (self.cells.len() - 1) as u32
            }
        };
        self.at.insert(at, index);
        index
    }

    /// Put this furnace into or out of the tick set, drop it if it now holds
    /// nothing, and say what changed.
    fn refresh(
        &mut self,
        index: u32,
        cooking: Option<&Cooking>,
        fuel: Option<&ItemBlocks>,
        quiet: bool,
    ) -> Option<FurnaceChange> {
        let cell = self.cells[index as usize].as_mut()?;
        let active = cell.furnace.is_active(cooking, fuel);
        let change = (!quiet).then(|| change_of(cell));
        if active && !cell.active {
            cell.active = true;
            self.active.push(index);
        } else if !active && cell.active {
            cell.active = false;
            self.active.retain(|held| *held != index);
        }
        // An empty furnace nobody is using is forgotten rather than kept, so
        // that opening every furnace in a village does not add a row to the
        // save for each.
        if !active
            && self.cells[index as usize]
                .as_ref()
                .is_some_and(|cell| cell.furnace.is_empty())
        {
            if let Some(cell) = self.cells[index as usize].take() {
                self.at.remove(&cell.at);
                self.free.push(index);
            }
        }
        change
    }
}

fn change_of(cell: &Cell) -> FurnaceChange {
    FurnaceChange {
        at: cell.at,
        slots: cell.furnace.slots.clone(),
        properties: properties_of(&cell.furnace),
        lit: cell.furnace.is_lit(),
    }
}

/// The four numbers a furnace screen draws, in the order the protocol numbers
/// them: `AbstractFurnaceMenu`'s `DATA_LIT_TIME`, `DATA_LIT_DURATION`,
/// `DATA_COOKING_PROGRESS`, `DATA_COOKING_TOTAL_TIME`.
///
/// Signed 16-bit on the wire, and every one of them fits: the longest fuel is
/// 20,000 ticks and the longest cook 600.
#[must_use]
pub fn properties_of(furnace: &Furnace) -> [i16; PROPERTIES] {
    let clamp = |value: u16| i16::try_from(value).unwrap_or(i16::MAX);
    [
        clamp(furnace.lit),
        clamp(furnace.lit_total),
        clamp(furnace.cooking),
        clamp(furnace.total),
    ]
}

/// Which item id `minecraft:bucket` is, for the fuel slot's one exception.
#[must_use]
pub fn bucket() -> Option<Item> {
    Item::from_name("minecraft:bucket")
}

/// The tick loop's furnaces.
///
/// The second participant the tick loop owns, after `ItemTicker`, and the
/// smaller of the two: it takes no roster, claims no columns and reads no
/// chunks. A furnace's whole world is its own three slots and two timers.
pub struct FurnaceTicker {
    furnaces: std::sync::Arc<Furnaces>,
    world: std::sync::Arc<super::edits::EditedWorld>,
    cooking: Option<std::sync::Arc<Cooking>>,
    fuel: Option<std::sync::Arc<ItemBlocks>>,
    /// Reused between ticks: the furnaces whose block state has to change
    /// because the fire lit or went out.
    lit: Vec<(Position, bool)>,
}

impl FurnaceTicker {
    /// A ticker over these furnaces, writing `lit` back into this world.
    #[must_use]
    pub fn new(
        furnaces: std::sync::Arc<Furnaces>,
        world: std::sync::Arc<super::edits::EditedWorld>,
        cooking: Option<std::sync::Arc<Cooking>>,
        fuel: Option<std::sync::Arc<ItemBlocks>>,
    ) -> Self {
        Self {
            furnaces,
            world,
            cooking,
            fuel,
            lit: Vec::new(),
        }
    }
}

impl std::fmt::Debug for FurnaceTicker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FurnaceTicker")
            .field("active", &self.furnaces.active())
            .finish_non_exhaustive()
    }
}

impl crate::participant::TickParticipant for FurnaceTicker {
    fn name(&self) -> &str {
        "furnaces"
    }

    /// Beside the items, and after them by registration order. A furnace that
    /// produced an ingot this tick has produced it before anything reports on
    /// the tick.
    fn priority(&self) -> i32 {
        0
    }

    fn tick(&mut self, _ctx: &crate::participant::TickContext) {
        self.furnaces
            .tick(self.cooking.as_deref(), self.fuel.as_deref(), &mut self.lit);
        // The block state, out here rather than under the furnace lock. A
        // furnace that lights turns into its `lit=true` state, which is the
        // texture change every player in sight sees and the thing that says
        // "this one is working" from across a room.
        for (at, lit) in self.lit.drain(..) {
            let there = dust_registry::BlockState::from_id(self.world.block_at(at));
            let Some(there) = there else { continue };
            let Some(wanted) = there.with("lit", if lit { "true" } else { "false" }) else {
                continue;
            };
            if wanted != there {
                self.world.set_block(at, wanted.id());
            }
        }
    }
}

/// How many points it takes to go from `level` to the next one.
///
/// Minecraft's `Player.getXpNeededForNextLevel`: three straight lines with
/// knees at 15 and 30. Written as the game writes it rather than fitted,
/// because a curve that is close is a bar that is visibly in the wrong place
/// for every player above level 16.
#[must_use]
pub fn points_for_level(level: u32) -> u32 {
    if level >= 30 {
        112 + (level - 30) * 9
    } else if level >= 15 {
        37 + (level - 15) * 5
    } else {
        7 + level * 2
    }
}

/// A total number of points as the bar the client draws: how full it is, and
/// which level it is under.
///
/// Walked rather than solved. The closed form is three quadratics and their
/// inverses, and inverting a quadratic in `f32` puts a player at level 29 with
/// a bar at 100% — which is a level they can see they should have had. The
/// walk is at most a few hundred subtractions of `u32`, and it runs when a
/// player's experience changes and never otherwise.
#[must_use]
pub fn bar_of(total: u32) -> (f32, u32) {
    let mut left = total;
    let mut level = 0;
    loop {
        let needed = points_for_level(level);
        if left < needed {
            #[allow(clippy::cast_precision_loss)]
            return (left as f32 / needed as f32, level);
        }
        left -= needed;
        level += 1;
    }
}
