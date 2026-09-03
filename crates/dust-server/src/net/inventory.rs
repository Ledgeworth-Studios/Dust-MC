//! What a player is carrying.
//!
//! This replaces `net::hotbar`, which was nine slots and a selection and said
//! in its own first paragraph that it was not an inventory. What that module
//! got right is kept whole — vanilla's slot numbering as a named constant, an
//! unknown item id treated as emptiness rather than as a disconnect, an
//! out-of-range selection left alone rather than wrapped — and the thirty-seven
//! slots it named as missing are here.
//!
//! # The forty-six slots, and why all of them
//!
//! A player's own container is `0..=45` in vanilla's numbering and every packet
//! that touches it uses those numbers:
//!
//! ```text
//!  0        crafting output
//!  1..=4    crafting grid
//!  5..=8    armour: head, chest, legs, feet
//!  9..=35   main inventory
//! 36..=44   hotbar
//! 45        offhand
//! ```
//!
//! The five crafting slots are stored rather than skipped. Nothing here crafts,
//! so the output slot never fills on its own — but a player can *put* something
//! in the grid, and a container that dropped those four slots would make items
//! disappear into a hole with vanilla's own numbering on it. Storing them costs
//! twenty bytes and removes a class of bug that only shows up as lost items.
//!
//! # Counts, and where the number comes from
//!
//! A stack is an item and a count, and the count is bounded by
//! [`Item::max_stack_size`] — 64 for dirt, 16 for an ender pearl, 1 for a
//! bucket. **Nothing here writes 64.** That number is Minecraft's, it is
//! per-item, and it already arrives from the operator's own jar: the item
//! component table `cargo xtask extract` generates carries
//! `minecraft:max_stack_size` for all 1,333 items, and the extractor refuses a
//! table where any of them is not an integer in `1..=99`. So a stack of
//! sixty-four buckets is refused here for the same reason vanilla refuses it,
//! from the same number, and a version that changed a stack size changes this
//! with no edit.
//!
//! # What is stored
//!
//! A [`Stack`] is an [`Item`], a `u8` and a component patch. The first two are
//! four bytes; the third is one `Option<Arc<[u8]>>` that is `None` for the
//! overwhelming majority of stacks. The whole container is a fixed array, so
//! reading a slot is an index and writing one is a store, and nothing on the
//! read path allocates — which matters because `held()` is read on every
//! right-click and the container is written on every click a player makes.
//!
//! The components are the whole of what makes one diamond sword different from
//! another: its name, its enchantments, how worn it is, what is inside it.
//! Dust does not model any of that and does not need to — see
//! [`dust_protocol::components`] — it walks a component to find where it ends
//! and then keeps, compares and returns the bytes exactly as they arrived.
//!
//! **Two stacks merge only if their components are equal**, and that rule
//! reaches every mode: a left click, a right click, a shift-click, a
//! double-click and a drag all ask [`Stack::stacks_with`] rather than comparing
//! items. Getting it wrong in one direction duplicates a player's property and
//! in the other destroys it. A real 1.21.1 server was asked: a stack named Bob
//! put down on a plain stack of the same block **swaps**, and it is the same
//! here.
//!
//! # What a click does
//!
//! [`Inventory::click`] is `Click Container`'s seven modes replayed over this
//! state. It is a real specification and it is followed rather than guessed at:
//! left and right click, shift-click, the number keys and F, creative clone,
//! Q and control-Q, the three drags, and double-click-to-collect.
//!
//! # What a slot will accept
//!
//! Forty-four of the forty-six slots take any item. The crafting output takes
//! none, and the four armour slots take only what is worn *in that slot* —
//! which is why [`worn_in`] exists and why it is built out of Mojang's item
//! tags rather than written down. That one rule reaches five of the seven
//! modes: a left click, a right click, a shift-click, a number key and a drag
//! all consult it, and the drag consults it when a slot *joins* the drag
//! rather than at the end, because the share each slot receives is divided by
//! how many slots joined.
//!
//! All of it was measured. `tools/bot/clicks.js` replays eighty-two clicks
//! against this server and against a real 1.21.1 server and diffs the two
//! recordings; the armour, offhand and crafting-grid clicks are the last
//! twenty-five of them, and they are what said this paragraph was wrong before
//! it was written. Decision record 0016 has the counts.
//!
//! Dropping is real and the item is *gone*: there are no item entities in the
//! world yet, so Q destroys rather than throws. Stated here because a player
//! finds that out by losing something.

use std::sync::OnceLock;

use dust_protocol::components::ComponentPatch;
use dust_protocol::types::Slot;
use dust_registry::tags::{self, TagRegistry};
use dust_registry::Item;

/// How many slots a player's own container has. Vanilla's `0..=45`.
pub const SLOTS: usize = 46;

/// The crafting output. Never filled by this server, and never writable by a
/// client — vanilla refuses a creative write here too.
pub const CRAFTING_OUTPUT: usize = 0;

/// The 2x2 crafting grid, `1..=4`.
pub const CRAFTING_START: usize = 1;
/// One past the crafting grid, so that `CRAFTING_START..CRAFTING_END` is a
/// range rather than an arithmetic exercise at each call site.
pub const CRAFTING_END: usize = 5;

/// Armour, `5..=8`: head, chest, legs, feet.
pub const ARMOUR_START: usize = 5;
/// One past the armour.
pub const ARMOUR_END: usize = 9;

/// The four armour slots by name, because `ARMOUR_START + 2` at a call site is
/// a place to be wrong by one and the mistake looks like a client bug.
pub const ARMOUR_HEAD: usize = 5;
/// The chest slot, which is also where an elytra goes.
pub const ARMOUR_CHEST: usize = 6;
/// The leggings slot.
pub const ARMOUR_LEGS: usize = 7;
/// The boots slot.
pub const ARMOUR_FEET: usize = 8;

/// The main inventory, `9..=35`.
pub const MAIN_START: usize = 9;
/// One past the main inventory, which is also where the hotbar begins.
pub const MAIN_END: usize = 36;

/// Where the hotbar sits in the player's container, which is what
/// `set_creative_mode_slot` and every click numbers its slots by.
///
/// A named range rather than a subtraction at the call site: a slot index off
/// by nine is a player holding the wrong thing, which looks exactly like a
/// client bug.
pub const HOTBAR_START: usize = 36;
/// One past the hotbar, which is also the offhand.
pub const HOTBAR_END: usize = 45;

/// The offhand, slot 45.
pub const OFFHAND: usize = 45;

/// How many hotbar slots there are. Vanilla's `Inventory.SELECTION_SIZE`.
pub const HOTBAR_SLOTS: usize = 9;

/// The slot number a click outside the window carries.
pub const OUTSIDE: i16 = -999;

/// The `button` a swap click uses to mean the offhand rather than a hotbar
/// slot. Vanilla's `Inventory.SLOT_OFFHAND`, and it is 40 rather than 45
/// because a swap's button numbers the *hotbar* and offhand is bolted onto the
/// end of that numbering.
const SWAP_OFFHAND_BUTTON: i8 = 40;

/// Where an item is *worn*, as a slot number in this container's numbering, or
/// `None` for the overwhelming majority of items that are worn nowhere.
///
/// This is the table [`Inventory::click`]'s armour rules are missing without,
/// and it is the reason the header used to say shift-click does not equip.
/// Java answers the same question with `Mob.getEquipmentSlotForItem`, which
/// walks a class hierarchy — `ArmorItem` knows its own type, `ShieldItem` is
/// hard-wired to the offhand — and a class hierarchy is not in any report. The
/// item report does not help either: on 1.21.1 every armour piece's
/// `minecraft:attribute_modifiers` is an **empty list**, so the report can say
/// how much damage a helmet absorbs nowhere and which slot it goes in nowhere.
/// The `minecraft:equippable` component that would answer this outright is
/// 1.21.2 and later.
///
/// What does answer it, on this version, is Mojang's own item tags, which
/// arrive through the same extraction as everything else:
///
/// | tag | slot |
/// |---|---|
/// | `minecraft:head_armor` | head |
/// | `minecraft:chest_armor` | chest |
/// | `minecraft:leg_armor` | legs |
/// | `minecraft:foot_armor` | feet |
/// | `minecraft:skulls` | head |
///
/// That is 32 of the 34 items a player can wear. The last two —
/// `minecraft:elytra` on the chest and `minecraft:carved_pumpkin` on the head —
/// are in no tag that names a slot, so they are named here, as is
/// `minecraft:shield`, which goes in the offhand.
///
/// **Names written down are names that can go stale, so they are guarded
/// rather than trusted.** `minecraft:enchantable/equippable` is vanilla's own
/// list of everything that is worn: the four armour tags, the skulls, the
/// elytra and the carved pumpkin, 34 items in all.
/// `every_wearable_item_has_a_slot_to_be_worn_in` walks that tag and fails on
/// any member this table places nowhere — so a version that adds a wearable
/// stops the build on the row where it happened, rather than shipping an item
/// a player cannot put on. The shield is not in that tag, because a shield is
/// held rather than worn, and is checked by name on its own.
///
/// # Cost
///
/// One byte per item, 1,333 of them, built once on the first click of the
/// server's life and read as an array index afterwards. The alternative — a
/// tag lookup per click — is five binary searches over a 514-row table on a
/// path a player hits several times a second.
fn worn_in(item: Item) -> Option<usize> {
    static WORN: OnceLock<Box<[u8]>> = OnceLock::new();
    let table = WORN.get_or_init(build_worn_table);
    // `CRAFTING_OUTPUT` is slot 0 and nothing is worn there, so zero is free to
    // mean "worn nowhere" and the table needs no `Option` per row.
    match table.get(item.protocol_id() as usize).copied() {
        None | Some(0) => None,
        Some(slot) => Some(slot as usize),
    }
}

/// The tags that name a slot, and which slot each names.
const WORN_BY_TAG: [(&str, usize); 5] = [
    ("minecraft:head_armor", ARMOUR_START),
    ("minecraft:chest_armor", ARMOUR_START + 1),
    ("minecraft:leg_armor", ARMOUR_START + 2),
    ("minecraft:foot_armor", ARMOUR_START + 3),
    ("minecraft:skulls", ARMOUR_START),
];

/// The three 1.21.1 leaves no tag for. See [`worn_in`].
const WORN_BY_NAME: [(&str, usize); 3] = [
    ("minecraft:elytra", ARMOUR_START + 1),
    ("minecraft:carved_pumpkin", ARMOUR_START),
    ("minecraft:shield", OFFHAND),
];

fn build_worn_table() -> Box<[u8]> {
    let mut table = vec![0u8; Item::registry().entry_count()];
    let mut put = |name: &str, slot: usize| {
        if let Some(item) = Item::from_name(name) {
            table[item.protocol_id() as usize] = slot as u8;
        }
    };
    for (tag, slot) in WORN_BY_TAG {
        let Some(def) = tags::from_id(TagRegistry::Item, tag) else {
            continue;
        };
        for member in def.members {
            put(member, slot);
        }
    }
    for (name, slot) in WORN_BY_NAME {
        put(name, slot);
    }
    table.into_boxed_slice()
}

/// The most of `item` one slot will hold.
///
/// Vanilla's `Slot.getMaxStackSize(ItemStack)`, which is the item's own maximum
/// everywhere in this container except the four armour slots, where `ArmorSlot`
/// returns 1. That is not a formality: `minecraft:player_head` stacks to 64 and
/// is worn on the head, so a player left-clicking a stack of sixty-four heads
/// onto the helmet slot puts **one** there and keeps sixty-three on the cursor.
/// A container that used the item's number would swallow the stack.
fn slot_limit(index: usize, item: Item) -> u8 {
    if (ARMOUR_START..ARMOUR_END).contains(&index) {
        1
    } else {
        item.max_stack_size()
    }
}

/// Whether a click may put `item` in this slot — vanilla's `Slot.mayPlace`.
///
/// Three answers, and the middle one is the whole point of [`worn_in`]: the
/// crafting output takes nothing, an armour slot takes only what is worn in
/// *that* slot, and everything else — the offhand included — takes anything.
/// The offhand really is unrestricted: a real server accepts a stack of nine
/// cobblestone into slot 45, which is measured in `tools/bot/clicks.js` and is
/// not a guess about what looks sensible.
fn may_place(index: usize, item: Item) -> bool {
    if index == CRAFTING_OUTPUT {
        return false;
    }
    if (ARMOUR_START..ARMOUR_END).contains(&index) {
        return worn_in(item) == Some(index);
    }
    true
}

/// One stack: an item, how many of it, and what makes it that one.
///
/// The count is never zero — an empty slot is `None`, not a stack of nothing —
/// and never above the item's own maximum. Both are invariants of every
/// constructor and every mutation here, which is what lets the rest of this
/// module do arithmetic without re-checking.
///
/// The third field is the stack's data components: its name, its enchantments,
/// how worn it is, what is inside it. It is one `Option<Arc<[u8]>>` — `None`
/// for the overwhelming majority of stacks, which allocate nothing — and it is
/// what [`stacks_with`] compares, because **two stacks merge only if their
/// components are equal**. Getting that comparison wrong in one direction
/// duplicates items and in the other destroys them; see
/// [`dust_protocol::components`] for why it is byte equality and which of the
/// two directions that can fail in.
///
/// [`stacks_with`]: Stack::stacks_with
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stack {
    pub item: Item,
    pub count: u8,
    pub components: ComponentPatch,
}

impl Stack {
    /// A stack of `count`, clamped to what the item allows and to at least one.
    #[must_use]
    pub fn new(item: Item, count: u8) -> Self {
        Self::with_components(item, count, ComponentPatch::EMPTY)
    }

    /// The same, carrying components.
    #[must_use]
    pub fn with_components(item: Item, count: u8, components: ComponentPatch) -> Self {
        Self {
            item,
            count: count.clamp(1, item.max_stack_size()),
            components,
        }
    }

    /// Whether these two stacks are the same thing, and may therefore pour into
    /// one another.
    ///
    /// The item **and** the components. A stack of sixteen arrows and a stack
    /// of sixteen arrows named "Bob" are two different things in Minecraft and
    /// two different things here; merging them would take Bob's name off
    /// sixteen arrows and give it to none, which the player sees as the server
    /// tidying their inventory by destroying part of it.
    #[must_use]
    pub fn stacks_with(&self, other: &Self) -> bool {
        self.item == other.item && self.components == other.components
    }

    /// A copy of this stack with a different count, components and all.
    #[must_use]
    fn of(&self, count: u8) -> Self {
        Self {
            item: self.item,
            count,
            components: self.components.clone(),
        }
    }

    /// Whether the stack is at the item's own maximum. Not the same question
    /// as whether the *slot* it is in is full — see [`slot_limit`] — and the
    /// two callers left are the double-click gather, which is about the cursor
    /// and about stacks it will not break open.
    fn is_full(&self) -> bool {
        self.count >= self.item.max_stack_size()
    }
}

/// The slots of one player's container.
pub type Slots = [Option<Stack>; SLOTS];

/// Which slots a click moved, as a bitmask.
///
/// Forty-six slots fit in a `u64` with room to spare, so "what changed" is a
/// register rather than a `Vec`. That is not a micro-optimisation for its own
/// sake: this is returned from every click, and a click that allocated to
/// report that one slot moved would allocate once per click per player.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Changed {
    slots: u64,
    cursor: bool,
}

impl Changed {
    fn mark(&mut self, slot: usize) {
        debug_assert!(slot < SLOTS);
        self.slots |= 1u64 << slot;
    }

    fn mark_cursor(&mut self) {
        self.cursor = true;
    }

    /// Whether this slot moved.
    #[must_use]
    pub fn has(self, slot: usize) -> bool {
        slot < SLOTS && self.slots & (1u64 << slot) != 0
    }

    /// Whether the cursor moved.
    #[must_use]
    pub fn cursor(self) -> bool {
        self.cursor
    }

    /// Whether nothing at all moved.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.slots == 0 && !self.cursor
    }

    /// The slots that moved, in ascending order.
    pub fn iter(self) -> impl Iterator<Item = usize> {
        (0..SLOTS).filter(move |slot| self.has(*slot))
    }
}

/// A drag in progress: the mouse is down and slots are being collected.
///
/// Vanilla calls this "quick craft" and it is a three-packet handshake — start,
/// each slot, end — which means a client that disconnects mid-drag, or one
/// sending the steps out of order, leaves state behind. Anything unexpected
/// resets it rather than being interpreted, which is vanilla's own rule and the
/// only safe one: a half-remembered drag applied to a later click is items
/// appearing where nobody put them.
#[derive(Debug, Clone, Copy, Default)]
struct Drag {
    active: bool,
    /// 0 left (split evenly), 1 right (one each), 2 middle (a full stack each,
    /// creative only).
    kind: u8,
    /// The slots collected so far, as a bitmask, for the same reason
    /// [`Changed`] is one.
    slots: u64,
    count: u8,
}

impl Drag {
    fn reset(&mut self) {
        *self = Self::default();
    }

    fn add(&mut self, slot: usize) {
        if self.slots & (1u64 << slot) == 0 {
            self.slots |= 1u64 << slot;
            self.count += 1;
        }
    }
}

/// Everything one player is carrying.
#[derive(Debug, Clone)]
pub struct Inventory {
    slots: Slots,
    /// What the player has picked up with the mouse. Not a slot: it belongs to
    /// the click protocol rather than the container, and it is sent in its own
    /// field of every packet that carries the container.
    cursor: Option<Stack>,
    /// Which hotbar slot is in hand, `0..9`.
    selected: usize,
    drag: Drag,
    /// The sequence number the client quotes back on a click. The server's
    /// alone: a click carrying a stale one was made against a window that has
    /// since moved.
    state_id: i32,
}

impl Default for Inventory {
    fn default() -> Self {
        Self {
            slots: std::array::from_fn(|_| None),
            cursor: None,
            selected: 0,
            drag: Drag::default(),
            state_id: 0,
        }
    }
}

impl Inventory {
    /// An inventory holding what was saved.
    #[must_use]
    pub fn restored(slots: Slots, selected: u8) -> Self {
        Self {
            slots,
            selected: usize::from(selected).min(HOTBAR_SLOTS - 1),
            ..Self::default()
        }
    }

    /// Every slot, in vanilla's numbering. Borrowed, never copied: this is read
    /// to build the join packet and to write the save.
    #[must_use]
    pub fn slots(&self) -> &Slots {
        &self.slots
    }

    /// What one slot holds. Borrowed: a stack carries its components and a
    /// caller that only wants to look at one should not touch their refcount.
    #[must_use]
    pub fn slot(&self, index: usize) -> Option<&Stack> {
        self.slots.get(index).and_then(Option::as_ref)
    }

    /// What is on the cursor.
    #[must_use]
    pub fn cursor(&self) -> Option<&Stack> {
        self.cursor.as_ref()
    }

    /// Which hotbar slot is in hand, `0..9`.
    #[must_use]
    pub fn selected(&self) -> u8 {
        self.selected as u8
    }

    /// The item in the selected hotbar slot, if there is one.
    #[must_use]
    pub fn held(&self) -> Option<Item> {
        self.slots[HOTBAR_START + self.selected]
            .as_ref()
            .map(|stack| stack.item)
    }

    /// The sequence number to stamp on the next sync.
    #[must_use]
    pub fn state_id(&self) -> i32 {
        self.state_id
    }

    /// Advance and return the sequence number for a sync about to be sent.
    pub fn next_state_id(&mut self) -> i32 {
        self.state_id = self.state_id.wrapping_add(1);
        self.state_id
    }

    /// Switch to a hotbar slot.
    ///
    /// Returns whether the index named one. An out-of-range slot leaves the
    /// selection alone rather than wrapping: a client that sent 9 has said
    /// something this server does not understand, and picking slot 0 for it
    /// would be inventing an answer.
    pub fn select(&mut self, slot: i16) -> bool {
        let Ok(index) = usize::try_from(slot) else {
            return false;
        };
        if index >= HOTBAR_SLOTS {
            return false;
        }
        self.selected = index;
        true
    }

    /// A creative client writing a slot directly.
    ///
    /// Returns `Ok(slot)` for a write this server took, `Err(slot)` for one it
    /// refused and the client must be told about, and `Ok(None)` for a write
    /// that names no slot at all.
    ///
    /// Vanilla's own three rules, in vanilla's order:
    ///
    /// - **-1 drops the stack.** It is how a creative client throws something
    ///   out of the menu. There are no item entities, so it is destroyed.
    /// - **1..=45 is a write.** Slot 0 is the crafting output and vanilla
    ///   refuses a write to it, because a client that could write the output
    ///   of a recipe could conjure the result of any recipe.
    /// - **A count above the item's maximum is refused.** This is where
    ///   [`Item::max_stack_size`] earns its place: sixty-four buckets in one
    ///   slot is not something a client should be able to ask for, and the
    ///   number that says so is per-item and Minecraft's.
    pub fn set_creative(&mut self, slot: i16, item: &Slot) -> Result<Option<usize>, usize> {
        if slot == -1 {
            // Thrown out of the creative menu. Nothing to report: the client
            // has already forgotten it.
            return Ok(None);
        }
        let Ok(index) = usize::try_from(slot) else {
            return Ok(None);
        };
        if !(CRAFTING_START..SLOTS).contains(&index) {
            return Ok(None);
        }
        match decode(item) {
            Decoded::Empty => {
                self.slots[index] = None;
                Ok(Some(index))
            }
            Decoded::Stack(stack) => {
                self.slots[index] = Some(stack);
                Ok(Some(index))
            }
            // Refused, and the slot is left as it was. The client believes it
            // put something there, so the caller has to say otherwise.
            Decoded::TooMany | Decoded::UnknownItem => Err(index),
        }
    }

    /// Replay a `Click Container` over this state.
    ///
    /// `slot` is vanilla's number, or [`OUTSIDE`] for a click on the world
    /// behind the window. Returns what moved, so the caller can send back the
    /// slots the client is now wrong about rather than the whole container.
    ///
    /// A mode or button combination this does not understand changes nothing
    /// and reports nothing changed. That is deliberate and it is the safe
    /// direction: the caller re-syncs on a click it did not understand, which
    /// costs a packet, where guessing costs the player an item.
    pub fn click(&mut self, mode: ClickMode, slot: i16, button: i8) -> Changed {
        let mut changed = Changed::default();
        // Any click that is not the next step of a drag ends the drag. Vanilla
        // does the same, and the reason is that the drag's three packets are
        // not atomic: a click arriving between them means the player did
        // something else, and finishing the drag afterwards would apply it to
        // a container they have already changed.
        if mode != ClickMode::QuickCraft && self.drag.active {
            self.drag.reset();
        }
        match mode {
            ClickMode::Pickup => self.pickup(slot, button, &mut changed),
            ClickMode::QuickMove => self.quick_move(slot, &mut changed),
            ClickMode::Swap => self.swap(slot, button, &mut changed),
            ClickMode::Clone => self.clone_slot(slot, &mut changed),
            ClickMode::Throw => self.throw(slot, button, &mut changed),
            ClickMode::QuickCraft => self.quick_craft(slot, button, &mut changed),
            ClickMode::PickupAll => self.pickup_all(button, &mut changed),
        }
        changed
    }

    /// What a player's window close does to what they were holding.
    ///
    /// Vanilla throws the cursor and the crafting grid on the floor. There is
    /// no floor to throw onto here, so both are put back into the inventory
    /// where they fit — which is better for the player than deleting them and
    /// is the only difference from vanilla in this file. What does not fit is
    /// lost, and there is nowhere else for it to go.
    pub fn closed(&mut self) -> Changed {
        let mut changed = Changed::default();
        if let Some(stack) = self.cursor.take() {
            changed.mark_cursor();
            self.give(stack, &mut changed);
        }
        for index in CRAFTING_START..CRAFTING_END {
            if let Some(stack) = self.slots[index].take() {
                changed.mark(index);
                self.give(stack, &mut changed);
            }
        }
        changed
    }

    /// Take a stack up off the ground.
    ///
    /// The same placement [`Inventory::closed`] uses — partial stacks first,
    /// then the main inventory, then the hotbar — because it is the placement
    /// vanilla uses and a player has forty hours of habit about where a picked
    /// up stack lands.
    ///
    /// Returns what did **not** fit. A full inventory is a real state and the
    /// item stays on the ground for it; deleting the overflow would be a
    /// player watching their pickaxe vanish because their pockets were full.
    pub fn collect(&mut self, stack: Stack) -> (Changed, Option<Stack>) {
        let mut changed = Changed::default();
        let mut left = stack;
        self.move_to(MAIN_START..HOTBAR_END, &mut left, &mut changed);
        (changed, (left.count > 0).then_some(left))
    }

    /// Put a stack into the main inventory and hotbar, merging into partial
    /// stacks first. Whatever does not fit is dropped, which is the only
    /// caller-visible loss and only happens with a full inventory.
    fn give(&mut self, stack: Stack, changed: &mut Changed) {
        let mut left = stack;
        // The hotbar is filled after the main inventory, matching vanilla's
        // `moveItemStackTo(stack, 9, 45, false)`: a player who closes a window
        // does not want their hand's contents replaced.
        self.move_to(MAIN_START..HOTBAR_END, &mut left, changed);
    }

    // -- the seven modes ---------------------------------------------------

    fn pickup(&mut self, slot: i16, button: i8, changed: &mut Changed) {
        if slot == OUTSIDE {
            // Clicked the world behind the window with something on the
            // cursor. Left drops it all, right drops one.
            match (self.cursor.clone(), button) {
                (Some(_), 0) => {
                    self.cursor = None;
                    changed.mark_cursor();
                }
                (Some(mut held), 1) => {
                    held.count -= 1;
                    self.cursor = (held.count > 0).then_some(held);
                    changed.mark_cursor();
                }
                _ => {}
            }
            return;
        }
        let Some(index) = self.writable(slot) else {
            return;
        };
        match button {
            0 => self.pickup_left(index, changed),
            1 => self.pickup_right(index, changed),
            _ => {}
        }
    }

    fn pickup_left(&mut self, index: usize, changed: &mut Changed) {
        let limit = self
            .cursor
            .as_ref()
            .map_or(u8::MAX, |held| slot_limit(index, held.item));
        match (self.cursor.clone(), self.slots[index].clone()) {
            // Hand empty, slot full: take it all.
            (None, Some(stack)) => {
                self.cursor = Some(stack);
                self.slots[index] = None;
            }
            // Hand full, slot empty: put down as much as the slot will hold.
            // A slot that will not take this item at all does nothing, which is
            // what a real server does with cobblestone aimed at a helmet slot.
            (Some(mut held), None) => {
                if !may_place(index, held.item) {
                    return;
                }
                let moved = held.count.min(limit);
                held.count -= moved;
                self.slots[index] = Some(held.of(moved));
                self.cursor = (held.count > 0).then_some(held);
            }
            // Both full, same item: pour the hand into the slot up to what the
            // slot will hold and keep the rest.
            (Some(mut held), Some(mut there))
                if held.stacks_with(&there)
                    && there.count < limit
                    && may_place(index, held.item) =>
            {
                let moved = held.count.min(limit - there.count);
                there.count += moved;
                held.count -= moved;
                self.slots[index] = Some(there);
                self.cursor = (held.count > 0).then_some(held);
            }
            // Both full, different items — or the same item with no room.
            // Swap, if the slot will take what is on the cursor and the whole
            // of it fits.
            (Some(held), Some(there)) => {
                if !may_place(index, held.item) || held.count > limit {
                    return;
                }
                self.slots[index] = Some(held);
                self.cursor = Some(there);
            }
            (None, None) => return,
        }
        changed.mark(index);
        changed.mark_cursor();
    }

    fn pickup_right(&mut self, index: usize, changed: &mut Changed) {
        match (self.cursor.clone(), self.slots[index].clone()) {
            // Hand empty: take half, rounded up. Vanilla rounds the *taken*
            // half up, so a right-click on three leaves one behind.
            (None, Some(mut there)) => {
                let taken = there.count.div_ceil(2);
                self.cursor = Some(there.of(taken));
                there.count -= taken;
                self.slots[index] = (there.count > 0).then_some(there);
            }
            // Hand full, slot empty or the same item with room: put one down.
            (Some(mut held), None) => {
                if !may_place(index, held.item) {
                    return;
                }
                held.count -= 1;
                self.slots[index] = Some(held.of(1));
                self.cursor = (held.count > 0).then_some(held);
            }
            (Some(mut held), Some(mut there))
                if held.stacks_with(&there)
                    && there.count < slot_limit(index, held.item)
                    && may_place(index, held.item) =>
            {
                held.count -= 1;
                there.count += 1;
                self.slots[index] = Some(there);
                self.cursor = (held.count > 0).then_some(held);
            }
            (Some(held), Some(there)) => {
                if !may_place(index, held.item) || held.count > slot_limit(index, held.item) {
                    return;
                }
                self.slots[index] = Some(held);
                self.cursor = Some(there);
            }
            (None, None) => return,
        }
        changed.mark(index);
        changed.mark_cursor();
    }

    /// Shift-click: send the stack where a real client sends it.
    ///
    /// Vanilla's `AbstractContainerMenu.clicked` does not call
    /// `quickMoveStack` once. It calls it **in a loop**, until a call moves
    /// nothing or the slot no longer holds the same item, and that loop is not
    /// a detail — it is the only reason shift-clicking a stack of nine player
    /// heads works. The first pass sees an empty head slot and moves one head
    /// there, because an armour slot holds one. The second pass sees the head
    /// slot occupied, takes a different arm entirely, and sends the other eight
    /// to the hotbar. A single pass leaves eight heads sitting in the slot the
    /// player shift-clicked, which is what this did until a real server was
    /// asked.
    fn quick_move(&mut self, slot: i16, changed: &mut Changed) {
        let Ok(index) = usize::try_from(slot) else {
            return;
        };
        if index >= SLOTS {
            return;
        }
        loop {
            let Some(mut stack) = self.slots[index].clone() else {
                return;
            };
            let destination = self.quick_move_destination(index, stack.item);
            self.slots[index] = None;
            let before = stack.count;
            self.move_to(destination, &mut stack, changed);
            if stack.count == before {
                // Nowhere for any of it to go. Vanilla leaves the slot alone
                // and so does this: a shift-click that moves nothing must not
                // report a change, or the client redraws a slot that did not
                // move.
                self.slots[index] = Some(stack);
                return;
            }
            changed.mark(index);
            if stack.count == 0 {
                return;
            }
            self.slots[index] = Some(stack);
        }
    }

    /// Where one pass of a shift-click sends what is in this slot.
    ///
    /// Vanilla's `InventoryMenu.quickMoveStack`, arm for arm and in its order,
    /// because the order *is* the rule: the equipment arms sit between the
    /// container's two halves, so a helmet in the main inventory goes to the
    /// head — but a helmet already **in** an armour slot comes off, and a
    /// helmet with a helmet already on the head goes to the hotbar like any
    /// other item.
    ///
    /// - the crafting output, the crafting grid and the armour empty into the
    ///   inventory as a whole,
    /// - an item that is worn, whose slot is empty, is put on,
    /// - the main inventory goes to the hotbar,
    /// - the hotbar goes to the main inventory,
    /// - anything else — the offhand — goes to the inventory as a whole.
    fn quick_move_destination(&self, index: usize, item: Item) -> std::ops::Range<usize> {
        if index < ARMOUR_END {
            return MAIN_START..HOTBAR_END;
        }
        if let Some(to) = worn_in(item).filter(|&to| self.slots[to].is_none()) {
            return to..to + 1;
        }
        if (MAIN_START..MAIN_END).contains(&index) {
            HOTBAR_START..HOTBAR_END
        } else if (HOTBAR_START..HOTBAR_END).contains(&index) {
            MAIN_START..MAIN_END
        } else {
            MAIN_START..HOTBAR_END
        }
    }

    /// A number key or F: swap this slot with a hotbar slot or the offhand.
    ///
    /// The named slot has an opinion and the hotbar slot does not, so only one
    /// direction is checked: pressing 1 over the helmet slot with cobblestone
    /// in hotbar slot 0 does nothing at all, and pressing it with a helmet
    /// there swaps. A real server does exactly that, and a server that swapped
    /// anyway is a player wearing a block.
    fn swap(&mut self, slot: i16, button: i8, changed: &mut Changed) {
        let Some(index) = self.writable(slot) else {
            return;
        };
        let other = if button == SWAP_OFFHAND_BUTTON {
            OFFHAND
        } else if (0..HOTBAR_SLOTS as i8).contains(&button) {
            HOTBAR_START + button as usize
        } else {
            return;
        };
        if other == index {
            return;
        }
        let Some(mut coming) = self.slots[other].clone() else {
            // Nothing coming in: this is a take, and every slot here may be
            // taken from.
            if self.slots[index].is_some() {
                self.slots.swap(index, other);
                changed.mark(index);
                changed.mark(other);
            }
            return;
        };
        if !may_place(index, coming.item) {
            return;
        }
        let limit = slot_limit(index, coming.item);
        if coming.count <= limit {
            self.slots.swap(index, other);
            changed.mark(index);
            changed.mark(other);
            return;
        }
        // More than the slot holds — a stack of skulls aimed at the head. The
        // slot takes what it holds, the rest stays in the hotbar, and whatever
        // was in the slot goes back into the inventory rather than being
        // deleted to make room.
        let going = self.slots[index].take();
        coming.count -= limit;
        self.slots[index] = Some(coming.of(limit));
        self.slots[other] = Some(coming);
        changed.mark(index);
        changed.mark(other);
        if let Some(going) = going {
            self.give(going, changed);
        }
    }

    /// Creative middle-click: a full stack of whatever is there.
    ///
    /// Every player on this server is in creative, which is the condition
    /// vanilla gates this on. The count is the item's maximum and not 64 —
    /// middle-clicking a bucket gives one bucket.
    fn clone_slot(&mut self, slot: i16, changed: &mut Changed) {
        let Some(index) = self.writable(slot) else {
            return;
        };
        if self.cursor.is_some() {
            return;
        }
        let Some(there) = self.slots[index].as_ref() else {
            return;
        };
        // Vanilla's middle-click is `ItemStack.copy()`, which copies the
        // components too: middle-clicking a named pickaxe gives a named
        // pickaxe, not a plain one.
        self.cursor = Some(there.of(there.item.max_stack_size()));
        changed.mark_cursor();
    }

    /// Q and control-Q. The item is destroyed: see this module's header.
    fn throw(&mut self, slot: i16, button: i8, changed: &mut Changed) {
        if self.cursor.is_some() {
            // Vanilla ignores a throw while something is on the cursor — that
            // gesture is the outside-click drop instead.
            return;
        }
        let Some(index) = self.writable(slot) else {
            return;
        };
        let Some(mut there) = self.slots[index].clone() else {
            return;
        };
        match button {
            0 => {
                there.count -= 1;
                self.slots[index] = (there.count > 0).then_some(there);
            }
            1 => self.slots[index] = None,
            _ => return,
        }
        changed.mark(index);
    }

    /// The three-packet drag.
    ///
    /// `button` encodes both which drag and which step: `kind * 4 + step`,
    /// where step 0 starts, 1 adds a slot and 2 ends. Anything out of order
    /// resets, which is vanilla's rule and the one that keeps a dropped packet
    /// from turning into items nobody placed.
    fn quick_craft(&mut self, slot: i16, button: i8, changed: &mut Changed) {
        let (kind, step) = (button / 4, button % 4);
        if !(0..=2).contains(&kind) || !(0..=2).contains(&step) {
            self.drag.reset();
            return;
        }
        let kind = kind as u8;
        match step {
            0 => {
                self.drag.reset();
                self.drag.active = true;
                self.drag.kind = kind;
            }
            1 => {
                if !self.drag.active || self.drag.kind != kind {
                    self.drag.reset();
                    return;
                }
                let Some(index) = self.writable(slot) else {
                    return;
                };
                // A slot only joins the drag if the cursor's item could go
                // there: the slot will take that item, and it is empty or the
                // same item with room. Vanilla checks both at *this* step and
                // not at the end, which is load-bearing — the share each slot
                // gets is `count / slots.len()`, so a slot that is filtered on
                // the way in makes the others' share larger. Dragging twenty-one
                // cobblestone across the chest slot and one ordinary slot puts
                // all twenty-one in the ordinary slot on a real server, not ten.
                let Some(held) = self.cursor.as_ref() else {
                    self.drag.reset();
                    return;
                };
                let fits = may_place(index, held.item)
                    && match self.slots[index].as_ref() {
                        None => true,
                        Some(there) => {
                            there.stacks_with(held) && there.count < slot_limit(index, held.item)
                        }
                    };
                if fits {
                    self.drag.add(index);
                }
            }
            2 => {
                if !self.drag.active || self.drag.kind != kind {
                    self.drag.reset();
                    return;
                }
                self.finish_drag(kind, changed);
                self.drag.reset();
            }
            _ => unreachable!("step is 0..=2"),
        }
    }

    fn finish_drag(&mut self, kind: u8, changed: &mut Changed) {
        let Some(held) = self.cursor.clone() else {
            return;
        };
        if self.drag.count == 0 {
            return;
        }
        // Left drag splits what is on the cursor evenly and keeps the
        // remainder; right drag puts one in each; middle drag is creative and
        // fills each slot without spending anything.
        let share = match kind {
            0 => held.count / self.drag.count,
            1 => 1,
            _ => held.item.max_stack_size(),
        };
        if share == 0 {
            return;
        }
        let mut left = held.count;
        for index in 0..SLOTS {
            if self.drag.slots & (1u64 << index) == 0 {
                continue;
            }
            let existing = self.slots[index].as_ref().map_or(0, |s| s.count);
            let want = share.min(slot_limit(index, held.item).saturating_sub(existing));
            if want == 0 {
                continue;
            }
            // A creative middle drag spends nothing, so the cursor's count
            // never limits it.
            let take = if kind == 2 { want } else { want.min(left) };
            if take == 0 {
                break;
            }
            // Every slot in the drag joined it holding either nothing or a
            // stack this one merges with, so writing the cursor's components
            // over the slot's writes the same bytes it already had.
            self.slots[index] = Some(held.of(existing + take));
            changed.mark(index);
            if kind != 2 {
                left -= take;
            }
        }
        if kind != 2 && left != held.count {
            self.cursor = (left > 0).then_some(held.of(left));
            changed.mark_cursor();
        }
    }

    /// Double-click: gather every loose one of this item onto the cursor.
    ///
    /// Two passes, because vanilla makes two: partial stacks first, so that
    /// double-clicking with a half stack tidies the loose ones up instead of
    /// breaking a full stack somewhere else in the inventory.
    fn pickup_all(&mut self, button: i8, changed: &mut Changed) {
        let Some(mut held) = self.cursor.clone() else {
            return;
        };
        if held.is_full() {
            return;
        }
        let max = held.item.max_stack_size();
        for pass in 0..2 {
            for step in 0..SLOTS {
                // Button 1 is the same gesture from the other end of the
                // container, which is what vanilla's `reverse` flag means.
                let index = if button == 1 { SLOTS - 1 - step } else { step };
                if index == CRAFTING_OUTPUT {
                    continue;
                }
                let Some(mut there) = self.slots[index].clone() else {
                    continue;
                };
                if !there.stacks_with(&held) {
                    continue;
                }
                if pass == 0 && there.is_full() {
                    continue;
                }
                let moved = there.count.min(max - held.count);
                if moved == 0 {
                    continue;
                }
                held.count += moved;
                there.count -= moved;
                self.slots[index] = (there.count > 0).then_some(there);
                changed.mark(index);
                if held.count >= max {
                    self.cursor = Some(held);
                    changed.mark_cursor();
                    return;
                }
            }
        }
        if !changed.is_empty() {
            self.cursor = Some(held);
            changed.mark_cursor();
        }
    }

    // -- shared moves ------------------------------------------------------

    /// The slot a click may write, if the number names one.
    ///
    /// The crafting output is not one: a click there in vanilla takes the
    /// result of a recipe, and there is no recipe here to have produced it.
    fn writable(&self, slot: i16) -> Option<usize> {
        let index = usize::try_from(slot).ok()?;
        (index != CRAFTING_OUTPUT && index < SLOTS).then_some(index)
    }

    /// Vanilla's `AbstractContainerMenu.moveItemStackTo`, which is what every
    /// shift-click and every put-it-back is made of.
    ///
    /// Two passes and they are not the same pass. The first pours into partial
    /// stacks of the same item, so a shift-clicked stack tops up what is
    /// already there rather than opening a new slot beside it. The second puts
    /// what is left into **one** empty slot and stops — vanilla breaks out of
    /// that loop, and since no slot here holds more than a stack there is never
    /// anything left over to want a second one.
    ///
    /// Both passes ask [`slot_limit`] rather than the item's own maximum, and
    /// the second asks [`may_place`]. Neither matters for a range inside the
    /// inventory; both matter for the one-slot range an armour move uses.
    fn move_to(&mut self, range: std::ops::Range<usize>, stack: &mut Stack, changed: &mut Changed) {
        if stack.item.max_stack_size() > 1 {
            for index in range.clone() {
                if stack.count == 0 {
                    return;
                }
                let Some(mut there) = self.slots[index].clone() else {
                    continue;
                };
                let limit = slot_limit(index, stack.item);
                if !there.stacks_with(stack) || there.count >= limit {
                    continue;
                }
                let moved = stack.count.min(limit - there.count);
                there.count += moved;
                stack.count -= moved;
                self.slots[index] = Some(there);
                changed.mark(index);
            }
        }
        if stack.count == 0 {
            return;
        }
        for index in range {
            if self.slots[index].is_some() || !may_place(index, stack.item) {
                continue;
            }
            let moved = stack.count.min(slot_limit(index, stack.item));
            self.slots[index] = Some(stack.of(moved));
            stack.count -= moved;
            changed.mark(index);
            return;
        }
    }
}

/// The seven things a click can be.
///
/// A copy of [`dust_protocol::packets::play::containers::ClickType`] rather
/// than a re-export, so that this module can be tested and reasoned about
/// without a packet, and so a mode this server does not implement is a
/// conversion that fails rather than a match arm nobody notices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickMode {
    Pickup,
    QuickMove,
    Swap,
    Clone,
    Throw,
    QuickCraft,
    PickupAll,
}

impl From<dust_protocol::packets::play::containers::ClickType> for ClickMode {
    fn from(kind: dust_protocol::packets::play::containers::ClickType) -> Self {
        use dust_protocol::packets::play::containers::ClickType as T;
        match kind {
            T::Pickup => Self::Pickup,
            T::QuickMove => Self::QuickMove,
            T::Swap => Self::Swap,
            T::Clone => Self::Clone,
            T::Throw => Self::Throw,
            T::QuickCraft => Self::QuickCraft,
            T::PickupAll => Self::PickupAll,
        }
    }
}

/// What a wire [`Slot`] turned out to be.
enum Decoded {
    Empty,
    Stack(Stack),
    /// A count above the item's own maximum. Refused rather than clamped: a
    /// client that asked for sixty-four buckets has to be told it did not get
    /// them, and clamping would leave it believing it did.
    TooMany,
    /// An id this build has no item for. It arrives from a client that may be
    /// modded or may be a version ahead, and dropping the connection over an
    /// item nobody can place would be a disconnect for a right-click.
    UnknownItem,
}

fn decode(slot: &Slot) -> Decoded {
    match slot {
        Slot::Empty => Decoded::Empty,
        Slot::Present { count, item_id, .. } => {
            let Some(item) = u32::try_from(*item_id)
                .ok()
                .and_then(Item::from_protocol_id)
            else {
                return Decoded::UnknownItem;
            };
            let Ok(count) = u8::try_from(*count) else {
                return Decoded::TooMany;
            };
            if count == 0 {
                return Decoded::Empty;
            }
            if count > item.max_stack_size() {
                return Decoded::TooMany;
            }
            let components = match components_of(slot) {
                Some(components) => components,
                None => return Decoded::Empty,
            };
            Decoded::Stack(Stack {
                item,
                count,
                components,
            })
        }
    }
}

/// This stack's components, as the wire gave them.
fn components_of(slot: &Slot) -> Option<ComponentPatch> {
    match slot {
        Slot::Empty => Some(ComponentPatch::EMPTY),
        Slot::Present { components, .. } => Some(components.clone()),
    }
}

/// Tell `dust-protocol` how to name a data-component type id.
///
/// The layouts of the fifty-seven component types are protocol knowledge and
/// live in `dust-protocol`; their *numbers* are Minecraft's, they are a
/// position in `minecraft:data_component_type`, and that registry is extracted
/// from the operator's own jar. Writing them down a second time in
/// `dust-protocol` would be a second answer to a question the registry already
/// answers — decision record 0016 declined the same trade for equipment slots —
/// so the lookup is installed here instead, where both halves are visible.
///
/// Idempotent, and cheap enough to call on every server construction: the
/// registry handle is resolved once and the lookup is a function pointer.
pub fn install_component_types() {
    fn name_of(id: i32) -> Option<&'static str> {
        static REGISTRY: OnceLock<Option<dust_registry::Registry>> = OnceLock::new();
        let registry = (*REGISTRY
            .get_or_init(|| dust_registry::Registry::from_name("minecraft:data_component_type")))?;
        registry.entry_name(u32::try_from(id).ok()?)
    }
    dust_protocol::components::install_type_names(name_of);
}

/// A stack as the wire wants it.
///
/// The components are the bytes that arrived, in their canonical order. A
/// `memcpy` and nothing else: they are not re-encoded per send, which matters
/// because this is called once per slot the client is wrong about, on every
/// click, for every player.
#[must_use]
pub fn to_wire(stack: Option<&Stack>) -> Slot {
    match stack {
        None => Slot::Empty,
        Some(stack) => Slot::Present {
            count: i32::from(stack.count),
            item_id: stack.item.protocol_id() as i32,
            components: stack.components.clone(),
        },
    }
}

/// A stack as a wire [`Slot`], for reading a client's opinion of one back.
#[must_use]
pub fn from_wire(slot: &Slot) -> Option<Stack> {
    match decode(slot) {
        Decoded::Stack(stack) => Some(stack),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A patch that sets one component, with the id the operator's own
    /// registry gave it. Nothing here writes a component number down.
    fn patch(component: &str, payload: &[u8]) -> ComponentPatch {
        install_component_types();
        let id = dust_registry::Registry::from_name("minecraft:data_component_type")
            .and_then(|registry| registry.entry_id(component))
            .expect("the extracted registry has that component type") as i32;
        let mut bytes = Vec::new();
        dust_protocol::varint::write_var_int(1, &mut bytes);
        dust_protocol::varint::write_var_int(0, &mut bytes);
        dust_protocol::varint::write_var_int(id, &mut bytes);
        bytes.extend_from_slice(payload);
        ComponentPatch::from_wire_bytes(&bytes).expect("a patch this build can walk")
    }

    /// `minecraft:damage`, which is one VarInt and is what a used tool carries.
    fn worn(amount: i32) -> ComponentPatch {
        let mut payload = Vec::new();
        dust_protocol::varint::write_var_int(amount, &mut payload);
        patch("minecraft:damage", &payload)
    }

    /// `minecraft:custom_name`, which is one network-NBT value. An empty
    /// compound stands in for the text: this is about identity, not rendering.
    fn named() -> ComponentPatch {
        patch("minecraft:custom_name", &[10, 0])
    }

    fn item(name: &str) -> Item {
        Item::from_name(name).expect("this build has that item")
    }

    fn stone() -> Item {
        item("minecraft:stone")
    }

    fn dirt() -> Item {
        item("minecraft:dirt")
    }

    /// Stack size 1, which is what makes it the interesting case everywhere a
    /// count is arithmetic.
    ///
    /// A *water* bucket and not an empty one: an empty bucket stacks to 16 on
    /// 1.21.1, which is exactly the sort of thing a hand-written table gets
    /// wrong and the generated one does not.
    fn bucket() -> Item {
        item("minecraft:water_bucket")
    }

    fn helmet() -> Item {
        item("minecraft:iron_helmet")
    }

    fn boots() -> Item {
        item("minecraft:iron_boots")
    }

    /// Worn on the head and stacks to 64, which is the only combination in the
    /// game where an armour slot's own limit of one is visible.
    fn head() -> Item {
        item("minecraft:player_head")
    }

    fn wire(item: Item, count: i32) -> Slot {
        Slot::Present {
            count,
            item_id: item.protocol_id() as i32,
            components: ComponentPatch::EMPTY,
        }
    }

    fn with(pairs: &[(usize, Item, u8)]) -> Inventory {
        let mut inventory = Inventory::default();
        for &(index, item, count) in pairs {
            inventory.slots[index] = Some(Stack::new(item, count));
        }
        inventory
    }

    #[test]
    fn the_stack_sizes_are_minecrafts_and_they_differ() {
        // The whole reason nothing here writes 64. If these were equal this
        // module could hardcode one number and every test below would still
        // pass, which is exactly the trap.
        assert_eq!(stone().max_stack_size(), 64);
        assert_eq!(item("minecraft:ender_pearl").max_stack_size(), 16);
        assert_eq!(item("minecraft:bucket").max_stack_size(), 16);
        assert_eq!(bucket().max_stack_size(), 1);
    }

    #[test]
    fn a_fresh_inventory_holds_nothing() {
        let inventory = Inventory::default();
        assert_eq!(inventory.held(), None);
        assert_eq!(inventory.cursor().cloned(), None);
        assert!(inventory.slots().iter().all(Option::is_none));
    }

    #[test]
    fn a_creative_write_lands_in_the_slot_it_names() {
        // 36 is hotbar slot 0 and 44 is slot 8; 5 is the helmet and 45 the
        // offhand. All four are slots the old hotbar dropped on the floor.
        let mut inventory = Inventory::default();
        assert_eq!(inventory.set_creative(36, &wire(stone(), 1)), Ok(Some(36)));
        assert_eq!(inventory.held(), Some(stone()), "slot 0 is selected");
        assert_eq!(inventory.set_creative(44, &wire(dirt(), 5)), Ok(Some(44)));
        assert_eq!(inventory.set_creative(9, &wire(dirt(), 64)), Ok(Some(9)));
        assert_eq!(inventory.set_creative(45, &wire(bucket(), 1)), Ok(Some(45)));
        assert_eq!(inventory.slot(9).map(|s| s.count), Some(64));
        assert_eq!(inventory.slot(45).map(|s| s.item), Some(bucket()));
        assert!(inventory.select(8));
        assert_eq!(inventory.held(), Some(dirt()));
    }

    #[test]
    fn a_count_above_the_items_own_maximum_is_refused_and_the_slot_is_untouched() {
        // The check that would pass with a hardcoded 64 and does not: a bucket
        // stacks to one, so two is already too many.
        let mut inventory = Inventory::default();
        assert_eq!(inventory.set_creative(36, &wire(bucket(), 2)), Err(36));
        assert_eq!(inventory.slot(36).cloned(), None);
        assert_eq!(inventory.set_creative(37, &wire(stone(), 65)), Err(37));
        assert_eq!(inventory.slot(37).cloned(), None);
        // And the ones that are fine stay fine.
        assert_eq!(inventory.set_creative(38, &wire(stone(), 64)), Ok(Some(38)));
        assert_eq!(
            inventory.set_creative(39, &wire(item("minecraft:ender_pearl"), 16)),
            Ok(Some(39))
        );
        assert_eq!(
            inventory.set_creative(40, &wire(item("minecraft:ender_pearl"), 17)),
            Err(40)
        );
    }

    #[test]
    fn the_crafting_output_is_not_writable_and_neither_is_a_slot_off_the_end() {
        let mut inventory = Inventory::default();
        assert_eq!(inventory.set_creative(0, &wire(stone(), 1)), Ok(None));
        assert_eq!(inventory.set_creative(46, &wire(stone(), 1)), Ok(None));
        assert_eq!(inventory.set_creative(-2, &wire(stone(), 1)), Ok(None));
        // -1 is the creative menu's "throw this away", which is a real
        // instruction and not a refusal.
        assert_eq!(inventory.set_creative(-1, &wire(stone(), 1)), Ok(None));
        assert!(inventory.slots().iter().all(Option::is_none));
    }

    #[test]
    fn an_item_this_build_has_never_heard_of_is_refused_rather_than_dropping_the_player() {
        let mut inventory = Inventory::default();
        assert_eq!(
            inventory.set_creative(
                36,
                &Slot::Present {
                    count: 1,
                    item_id: 999_999,
                    components: ComponentPatch::EMPTY,
                }
            ),
            Err(36)
        );
        assert_eq!(inventory.held(), None);
    }

    #[test]
    fn a_selection_outside_the_hotbar_leaves_the_one_in_hand_alone() {
        let mut inventory = Inventory::default();
        assert_eq!(inventory.set_creative(36, &wire(stone(), 1)), Ok(Some(36)));
        assert!(!inventory.select(9));
        assert!(!inventory.select(-1));
        assert_eq!(inventory.held(), Some(stone()));
    }

    #[test]
    fn left_click_takes_a_stack_puts_it_down_merges_and_swaps() {
        let mut inventory = with(&[(9, stone(), 30), (10, stone(), 50), (11, dirt(), 1)]);

        // Take.
        let changed = inventory.click(ClickMode::Pickup, 9, 0);
        assert_eq!(inventory.cursor().map(|s| s.count), Some(30));
        assert_eq!(inventory.slot(9).cloned(), None);
        assert!(changed.has(9) && changed.cursor());

        // Merge: 30 into a stack of 50 leaves 16 in hand and 64 in the slot.
        inventory.click(ClickMode::Pickup, 10, 0);
        assert_eq!(inventory.slot(10).map(|s| s.count), Some(64));
        assert_eq!(inventory.cursor().map(|s| s.count), Some(16));

        // Swap: a different item.
        inventory.click(ClickMode::Pickup, 11, 0);
        assert_eq!(inventory.slot(11).cloned(), Some(Stack::new(stone(), 16)));
        assert_eq!(inventory.cursor().cloned(), Some(Stack::new(dirt(), 1)));

        // Put down.
        inventory.click(ClickMode::Pickup, 12, 0);
        assert_eq!(inventory.slot(12).cloned(), Some(Stack::new(dirt(), 1)));
        assert_eq!(inventory.cursor().cloned(), None);
    }

    #[test]
    fn right_click_takes_half_rounded_up_and_puts_one_down() {
        let mut inventory = with(&[(9, stone(), 3)]);
        inventory.click(ClickMode::Pickup, 9, 1);
        assert_eq!(
            inventory.cursor().map(|s| s.count),
            Some(2),
            "half of 3, up"
        );
        assert_eq!(inventory.slot(9).map(|s| s.count), Some(1));

        inventory.click(ClickMode::Pickup, 10, 1);
        assert_eq!(inventory.slot(10).map(|s| s.count), Some(1));
        assert_eq!(inventory.cursor().map(|s| s.count), Some(1));

        inventory.click(ClickMode::Pickup, 10, 1);
        assert_eq!(inventory.slot(10).map(|s| s.count), Some(2));
        assert_eq!(inventory.cursor().cloned(), None);
    }

    #[test]
    fn right_click_on_a_single_leaves_the_slot_empty_rather_than_a_stack_of_nothing() {
        let mut inventory = with(&[(9, bucket(), 1)]);
        inventory.click(ClickMode::Pickup, 9, 1);
        assert_eq!(inventory.slot(9).cloned(), None);
        assert_eq!(inventory.cursor().cloned(), Some(Stack::new(bucket(), 1)));
    }

    #[test]
    fn shift_click_sends_the_hotbar_to_the_inventory_and_back() {
        let mut inventory = with(&[(36, stone(), 20)]);
        inventory.click(ClickMode::QuickMove, 36, 0);
        assert_eq!(inventory.slot(36).cloned(), None);
        assert_eq!(inventory.slot(9).cloned(), Some(Stack::new(stone(), 20)));

        inventory.click(ClickMode::QuickMove, 9, 0);
        assert_eq!(inventory.slot(9).cloned(), None);
        assert_eq!(inventory.slot(36).cloned(), Some(Stack::new(stone(), 20)));
    }

    #[test]
    fn shift_click_merges_before_it_takes_an_empty_slot() {
        // 40 in the hotbar and 34 already sitting in slot 9. The merge fills
        // slot 9 to 64 and the ten that do not fit take the next empty slot —
        // vanilla's `moveItemStackTo`, which is a merge pass followed by a
        // fill pass and not one or the other. A server that only filled would
        // put 40 in slot 10 and leave 34 loose.
        let mut inventory = with(&[(36, stone(), 40), (9, stone(), 34)]);
        inventory.click(ClickMode::QuickMove, 36, 0);
        assert_eq!(inventory.slot(9).map(|s| s.count), Some(64));
        assert_eq!(inventory.slot(10).map(|s| s.count), Some(10));
        assert_eq!(inventory.slot(36).cloned(), None);
    }

    #[test]
    fn shift_click_with_nowhere_to_go_changes_nothing() {
        // Every main slot full of a different item, so a hotbar stack has no
        // home. A server that reported a change here would make the client
        // redraw a slot that did not move.
        let mut inventory = Inventory::default();
        for index in MAIN_START..MAIN_END {
            inventory.slots[index] = Some(Stack::new(dirt(), 64));
        }
        inventory.slots[36] = Some(Stack::new(stone(), 5));
        let changed = inventory.click(ClickMode::QuickMove, 36, 0);
        assert!(changed.is_empty());
        assert_eq!(inventory.slot(36).cloned(), Some(Stack::new(stone(), 5)));
    }

    #[test]
    fn a_number_key_swaps_with_that_hotbar_slot_and_f_swaps_with_the_offhand() {
        let mut inventory = with(&[(9, stone(), 4), (38, dirt(), 2)]);
        inventory.click(ClickMode::Swap, 9, 2);
        assert_eq!(inventory.slot(9).cloned(), Some(Stack::new(dirt(), 2)));
        assert_eq!(inventory.slot(38).cloned(), Some(Stack::new(stone(), 4)));

        inventory.click(ClickMode::Swap, 9, SWAP_OFFHAND_BUTTON);
        assert_eq!(inventory.slot(9).cloned(), None);
        assert_eq!(
            inventory.slot(OFFHAND).cloned(),
            Some(Stack::new(dirt(), 2))
        );
    }

    #[test]
    fn middle_click_clones_a_full_stack_of_that_items_own_maximum() {
        let mut inventory = with(&[(9, stone(), 1), (10, bucket(), 1)]);
        inventory.click(ClickMode::Clone, 9, 2);
        assert_eq!(inventory.cursor().cloned(), Some(Stack::new(stone(), 64)));
        assert_eq!(
            inventory.slot(9).cloned(),
            Some(Stack::new(stone(), 1)),
            "unchanged"
        );

        // And the number is the item's, not 64.
        let mut inventory = with(&[(10, bucket(), 1)]);
        inventory.click(ClickMode::Clone, 10, 2);
        assert_eq!(inventory.cursor().cloned(), Some(Stack::new(bucket(), 1)));
    }

    #[test]
    fn q_drops_one_and_control_q_drops_the_stack() {
        let mut inventory = with(&[(36, stone(), 3)]);
        inventory.click(ClickMode::Throw, 36, 0);
        assert_eq!(inventory.slot(36).map(|s| s.count), Some(2));
        inventory.click(ClickMode::Throw, 36, 1);
        assert_eq!(inventory.slot(36).cloned(), None);
    }

    #[test]
    fn clicking_outside_the_window_drops_what_is_on_the_cursor() {
        let mut inventory = with(&[(9, stone(), 4)]);
        inventory.click(ClickMode::Pickup, 9, 0);
        inventory.click(ClickMode::Pickup, OUTSIDE, 1);
        assert_eq!(inventory.cursor().map(|s| s.count), Some(3));
        inventory.click(ClickMode::Pickup, OUTSIDE, 0);
        assert_eq!(inventory.cursor().cloned(), None);
    }

    #[test]
    fn a_left_drag_splits_evenly_and_keeps_the_remainder() {
        let mut inventory = with(&[(9, stone(), 10)]);
        inventory.click(ClickMode::Pickup, 9, 0);
        assert_eq!(inventory.cursor().map(|s| s.count), Some(10));

        inventory.click(ClickMode::QuickCraft, -999, 0);
        inventory.click(ClickMode::QuickCraft, 10, 1);
        inventory.click(ClickMode::QuickCraft, 11, 1);
        inventory.click(ClickMode::QuickCraft, 12, 1);
        inventory.click(ClickMode::QuickCraft, -999, 2);

        // Three each, one left over.
        assert_eq!(inventory.slot(10).map(|s| s.count), Some(3));
        assert_eq!(inventory.slot(11).map(|s| s.count), Some(3));
        assert_eq!(inventory.slot(12).map(|s| s.count), Some(3));
        assert_eq!(inventory.cursor().map(|s| s.count), Some(1));
    }

    #[test]
    fn a_right_drag_puts_one_in_each() {
        let mut inventory = with(&[(9, stone(), 10)]);
        inventory.click(ClickMode::Pickup, 9, 0);
        inventory.click(ClickMode::QuickCraft, -999, 4);
        inventory.click(ClickMode::QuickCraft, 10, 5);
        inventory.click(ClickMode::QuickCraft, 11, 5);
        inventory.click(ClickMode::QuickCraft, -999, 6);
        assert_eq!(inventory.slot(10).map(|s| s.count), Some(1));
        assert_eq!(inventory.slot(11).map(|s| s.count), Some(1));
        assert_eq!(inventory.cursor().map(|s| s.count), Some(8));
    }

    #[test]
    fn a_drag_interrupted_by_another_click_applies_nothing() {
        let mut inventory = with(&[(9, stone(), 10)]);
        inventory.click(ClickMode::Pickup, 9, 0);
        inventory.click(ClickMode::QuickCraft, -999, 0);
        inventory.click(ClickMode::QuickCraft, 10, 1);
        // A pickup arrives mid-drag. The drag is abandoned, and the end that
        // arrives after it does nothing.
        inventory.click(ClickMode::Pickup, 20, 0);
        inventory.click(ClickMode::QuickCraft, -999, 2);
        assert_eq!(inventory.slot(10).cloned(), None, "the drag never landed");
        assert_eq!(inventory.slot(20).map(|s| s.count), Some(10));
    }

    #[test]
    fn double_click_gathers_the_partial_stacks_before_the_full_ones() {
        // A full stack in slot 9, loose ones in 10 and 11. Picking up 5 and
        // double-clicking should empty the loose slots and leave the full
        // stack alone until they run out.
        let mut inventory = with(&[
            (9, stone(), 64),
            (10, stone(), 7),
            (11, stone(), 3),
            (12, stone(), 5),
        ]);
        inventory.click(ClickMode::Pickup, 12, 0);
        assert_eq!(inventory.cursor().map(|s| s.count), Some(5));
        inventory.click(ClickMode::PickupAll, 12, 0);
        // 5 + 7 + 3 = 15, then 49 taken off the full stack to reach 64.
        assert_eq!(inventory.cursor().map(|s| s.count), Some(64));
        assert_eq!(inventory.slot(10).cloned(), None);
        assert_eq!(inventory.slot(11).cloned(), None);
        assert_eq!(inventory.slot(9).map(|s| s.count), Some(15));
    }

    #[test]
    fn closing_the_window_puts_the_cursor_and_the_grid_back_rather_than_deleting_them() {
        let mut inventory = with(&[(9, stone(), 4), (CRAFTING_START, dirt(), 2)]);
        inventory.click(ClickMode::Pickup, 9, 0);
        assert_eq!(inventory.cursor().map(|s| s.count), Some(4));
        inventory.closed();
        assert_eq!(inventory.cursor().cloned(), None);
        assert_eq!(inventory.slot(CRAFTING_START).cloned(), None);
        // Both landed somewhere in the inventory.
        let total: u32 = inventory
            .slots()
            .iter()
            .flatten()
            .map(|s| u32::from(s.count))
            .sum();
        assert_eq!(total, 6);
    }

    #[test]
    fn a_click_naming_a_slot_that_is_not_one_changes_nothing() {
        let mut inventory = with(&[(9, stone(), 4)]);
        for slot in [-1i16, 46, 1000] {
            assert!(inventory.click(ClickMode::Pickup, slot, 0).is_empty());
        }
        // And the crafting output, which is a real slot number and still not
        // one a click may take from.
        assert!(inventory.click(ClickMode::Pickup, 0, 0).is_empty());
        assert_eq!(inventory.slot(9).cloned(), Some(Stack::new(stone(), 4)));
    }

    #[test]
    fn the_changed_mask_names_exactly_the_slots_that_moved() {
        let mut inventory = with(&[(9, stone(), 4)]);
        let changed = inventory.click(ClickMode::Swap, 9, 3);
        let moved: Vec<usize> = changed.iter().collect();
        assert_eq!(moved, vec![9, 39]);
        assert!(!changed.cursor());
    }

    #[test]
    fn a_stack_survives_the_wire_and_back() {
        for (item, count) in [(stone(), 64u8), (bucket(), 1), (dirt(), 17)] {
            let stack = Stack::new(item, count);
            assert_eq!(from_wire(&to_wire(Some(&stack))), Some(stack));
        }
        assert_eq!(to_wire(None), Slot::Empty);
        assert_eq!(from_wire(&Slot::Empty), None);
    }

    #[test]
    fn every_wearable_item_has_a_slot_to_be_worn_in() {
        // `minecraft:enchantable/equippable` is vanilla's own list of the 34
        // things a player wears, and this table places 32 of them from four
        // slot-naming tags and the other two by name. A version that adds a
        // wearable — or renames one of those two — fails here rather than
        // shipping an item that cannot be put on.
        let wearable = dust_registry::tags::wire(TagRegistry::Item)
            .expect("the item tags resolve")
            .into_iter()
            .find(|tag| tag.id == "minecraft:enchantable/equippable")
            .expect("1.21.1 has that tag");
        let mut placed_nowhere = Vec::new();
        for id in &wearable.entries {
            let item = Item::from_protocol_id(*id).expect("a tag member is an item");
            if worn_in(item).is_none() {
                placed_nowhere.push(item.name());
            }
        }
        assert!(
            placed_nowhere.is_empty(),
            "these are worn somewhere and this table says nowhere: {placed_nowhere:?}"
        );
        assert_eq!(wearable.entries.len(), 34, "the wearables on 1.21.1");
        // The shield is not worn and so is not in that tag; it is the one
        // offhand answer and nothing else guards it.
        assert_eq!(worn_in(item("minecraft:shield")), Some(OFFHAND));
    }

    #[test]
    fn the_four_armour_slots_take_only_what_is_worn_in_them() {
        assert_eq!(worn_in(helmet()), Some(ARMOUR_HEAD));
        assert_eq!(worn_in(boots()), Some(ARMOUR_FEET));
        assert_eq!(worn_in(item("minecraft:elytra")), Some(ARMOUR_CHEST));
        assert_eq!(worn_in(item("minecraft:carved_pumpkin")), Some(ARMOUR_HEAD));
        assert_eq!(worn_in(stone()), None);

        assert!(may_place(ARMOUR_HEAD, helmet()));
        assert!(!may_place(ARMOUR_HEAD, boots()));
        assert!(!may_place(ARMOUR_FEET, helmet()));
        assert!(!may_place(ARMOUR_HEAD, stone()));
        // The offhand and the inventory take anything, the output nothing.
        assert!(may_place(OFFHAND, stone()));
        assert!(may_place(MAIN_START, helmet()));
        assert!(!may_place(CRAFTING_OUTPUT, stone()));
    }

    #[test]
    fn a_left_click_cannot_put_a_block_on_a_players_head() {
        let mut inventory = with(&[(MAIN_START, stone(), 9)]);
        inventory.click(ClickMode::Pickup, MAIN_START as i16, 0);
        let changed = inventory.click(ClickMode::Pickup, ARMOUR_HEAD as i16, 0);
        assert!(changed.is_empty(), "a refused click changes nothing");
        assert_eq!(inventory.slot(ARMOUR_HEAD).cloned(), None);
        assert_eq!(inventory.cursor().cloned(), Some(Stack::new(stone(), 9)));

        // Boots into the helmet slot are refused for the same reason, and the
        // helmet slot takes the helmet.
        inventory.click(ClickMode::Pickup, MAIN_START as i16, 0);
        let mut inventory = with(&[(MAIN_START, boots(), 1), (MAIN_START + 1, helmet(), 1)]);
        inventory.click(ClickMode::Pickup, MAIN_START as i16, 0);
        assert!(inventory
            .click(ClickMode::Pickup, ARMOUR_HEAD as i16, 0)
            .is_empty());
        inventory.click(ClickMode::Pickup, MAIN_START as i16, 0);
        inventory.click(ClickMode::Pickup, (MAIN_START + 1) as i16, 0);
        inventory.click(ClickMode::Pickup, ARMOUR_HEAD as i16, 0);
        assert_eq!(
            inventory.slot(ARMOUR_HEAD).cloned(),
            Some(Stack::new(helmet(), 1))
        );
    }

    #[test]
    fn shift_click_puts_armour_on_and_takes_it_off_again() {
        let mut inventory = with(&[(MAIN_START, helmet(), 1)]);
        inventory.click(ClickMode::QuickMove, MAIN_START as i16, 0);
        assert_eq!(
            inventory.slot(ARMOUR_HEAD).cloned(),
            Some(Stack::new(helmet(), 1))
        );
        assert_eq!(inventory.slot(MAIN_START).cloned(), None);

        // Off again, and into the inventory rather than back onto the head.
        inventory.click(ClickMode::QuickMove, ARMOUR_HEAD as i16, 0);
        assert_eq!(inventory.slot(ARMOUR_HEAD).cloned(), None);
        assert_eq!(
            inventory.slot(MAIN_START).cloned(),
            Some(Stack::new(helmet(), 1))
        );
    }

    #[test]
    fn a_second_helmet_goes_to_the_hotbar_because_the_head_is_taken() {
        let mut inventory = with(&[
            (ARMOUR_HEAD, helmet(), 1),
            (MAIN_START, item("minecraft:golden_helmet"), 1),
        ]);
        inventory.click(ClickMode::QuickMove, MAIN_START as i16, 0);
        assert_eq!(
            inventory.slot(ARMOUR_HEAD).cloned(),
            Some(Stack::new(helmet(), 1))
        );
        assert_eq!(
            inventory.slot(HOTBAR_START).cloned(),
            Some(Stack::new(item("minecraft:golden_helmet"), 1))
        );
    }

    #[test]
    fn a_shield_shift_clicks_into_the_offhand_unless_it_is_taken() {
        let shield = item("minecraft:shield");
        let mut inventory = with(&[(MAIN_START, shield, 1)]);
        inventory.click(ClickMode::QuickMove, MAIN_START as i16, 0);
        assert_eq!(
            inventory.slot(OFFHAND).cloned(),
            Some(Stack::new(shield, 1))
        );

        let mut inventory = with(&[(MAIN_START, shield, 1), (OFFHAND, stone(), 1)]);
        inventory.click(ClickMode::QuickMove, MAIN_START as i16, 0);
        assert_eq!(
            inventory.slot(OFFHAND).cloned(),
            Some(Stack::new(stone(), 1))
        );
        assert_eq!(
            inventory.slot(HOTBAR_START).cloned(),
            Some(Stack::new(shield, 1))
        );
    }

    #[test]
    fn the_offhand_and_the_crafting_grid_empty_into_the_inventory() {
        let mut inventory = with(&[(OFFHAND, stone(), 9), (CRAFTING_START, stone(), 4)]);
        inventory.click(ClickMode::QuickMove, OFFHAND as i16, 0);
        assert_eq!(
            inventory.slot(MAIN_START).cloned(),
            Some(Stack::new(stone(), 9))
        );
        inventory.click(ClickMode::QuickMove, CRAFTING_START as i16, 0);
        assert_eq!(
            inventory.slot(MAIN_START).cloned(),
            Some(Stack::new(stone(), 13))
        );
        assert_eq!(inventory.slot(CRAFTING_START).cloned(), None);
    }

    #[test]
    fn a_number_key_over_an_armour_slot_obeys_the_slot_and_not_the_key() {
        let mut inventory = with(&[
            (ARMOUR_HEAD, helmet(), 1),
            (HOTBAR_START, stone(), 6),
            (HOTBAR_START + 1, item("minecraft:golden_helmet"), 1),
        ]);
        let changed = inventory.click(ClickMode::Swap, ARMOUR_HEAD as i16, 0);
        assert!(changed.is_empty(), "a block cannot be swapped onto a head");
        assert_eq!(
            inventory.slot(ARMOUR_HEAD).cloned(),
            Some(Stack::new(helmet(), 1))
        );

        inventory.click(ClickMode::Swap, ARMOUR_HEAD as i16, 1);
        assert_eq!(
            inventory.slot(ARMOUR_HEAD).cloned(),
            Some(Stack::new(item("minecraft:golden_helmet"), 1))
        );
        assert_eq!(
            inventory.slot(HOTBAR_START + 1).cloned(),
            Some(Stack::new(helmet(), 1))
        );
    }

    #[test]
    fn an_armour_slot_holds_one_of_something_that_stacks_to_sixty_four() {
        // A player head is worn and stacks to 64. `ArmorSlot.getMaxStackSize`
        // is 1, so one goes on and the rest stays on the cursor.
        assert_eq!(head().max_stack_size(), 64);
        let mut inventory = with(&[(MAIN_START, head(), 9)]);
        inventory.click(ClickMode::Pickup, MAIN_START as i16, 0);
        inventory.click(ClickMode::Pickup, ARMOUR_HEAD as i16, 0);
        assert_eq!(
            inventory.slot(ARMOUR_HEAD).cloned(),
            Some(Stack::new(head(), 1))
        );
        assert_eq!(inventory.cursor().cloned(), Some(Stack::new(head(), 8)));

        // And a shift-click puts one on the head and then keeps going: the
        // second pass finds the head slot occupied, takes the ordinary arm and
        // sends the other eight to the hotbar. Measured against a real server;
        // a single pass leaves them in the slot that was clicked.
        let mut inventory = with(&[(MAIN_START, head(), 9)]);
        inventory.click(ClickMode::QuickMove, MAIN_START as i16, 0);
        assert_eq!(
            inventory.slot(ARMOUR_HEAD).cloned(),
            Some(Stack::new(head(), 1))
        );
        assert_eq!(inventory.slot(MAIN_START).cloned(), None);
        assert_eq!(
            inventory.slot(HOTBAR_START).cloned(),
            Some(Stack::new(head(), 8))
        );
    }

    #[test]
    fn a_drag_skips_the_slot_that_will_not_take_it_and_the_rest_share_more() {
        // Twenty-one cobblestone dragged across the chest slot and one
        // ordinary slot. The chest slot never joins the drag, so the share is
        // 21/1 and not 21/2 — measured against a real server.
        let mut inventory = with(&[(MAIN_START, stone(), 21)]);
        inventory.click(ClickMode::Pickup, MAIN_START as i16, 0);
        inventory.click(ClickMode::QuickCraft, OUTSIDE, 0);
        inventory.click(ClickMode::QuickCraft, ARMOUR_CHEST as i16, 1);
        inventory.click(ClickMode::QuickCraft, (MAIN_START + 8) as i16, 1);
        inventory.click(ClickMode::QuickCraft, OUTSIDE, 2);
        assert_eq!(inventory.slot(ARMOUR_CHEST).cloned(), None);
        assert_eq!(
            inventory.slot(MAIN_START + 8).cloned(),
            Some(Stack::new(stone(), 21))
        );
        assert_eq!(inventory.cursor().cloned(), None);
    }

    #[test]
    fn a_block_goes_in_the_offhand_because_the_offhand_takes_anything() {
        let mut inventory = with(&[(MAIN_START, stone(), 9), (OFFHAND, dirt(), 2)]);
        inventory.click(ClickMode::Pickup, MAIN_START as i16, 0);
        inventory.click(ClickMode::Pickup, OFFHAND as i16, 0);
        assert_eq!(
            inventory.slot(OFFHAND).cloned(),
            Some(Stack::new(stone(), 9))
        );
        assert_eq!(inventory.cursor().cloned(), Some(Stack::new(dirt(), 2)));
    }
    fn stone_with(components: ComponentPatch, count: u8) -> Stack {
        Stack::with_components(item("minecraft:stone"), count, components)
    }

    #[test]
    fn a_left_click_pours_one_stack_into_another_only_when_the_components_match() {
        // The whole point of this module's change, from the player's side: a
        // named stack poured onto a plain one would take the name off both.
        let mut inventory = Inventory::default();
        inventory.slots[MAIN_START] = Some(stone_with(named(), 16));
        inventory.cursor = Some(stone_with(named(), 16));
        inventory.click(ClickMode::Pickup, MAIN_START as i16, 0);
        assert_eq!(
            inventory.slot(MAIN_START).map(|s| s.count),
            Some(32),
            "two stacks with the same components must merge"
        );
        assert_eq!(inventory.cursor(), None);

        let mut inventory = Inventory::default();
        inventory.slots[MAIN_START] = Some(stone_with(named(), 16));
        inventory.cursor = Some(stone_with(ComponentPatch::EMPTY, 16));
        inventory.click(ClickMode::Pickup, MAIN_START as i16, 0);
        // A swap, which is what vanilla does with two stacks that are not the
        // same thing. Not a merge, and not a refusal.
        assert_eq!(inventory.slot(MAIN_START).map(|s| s.count), Some(16));
        assert_eq!(
            inventory.slot(MAIN_START).map(|s| s.components.clone()),
            Some(ComponentPatch::EMPTY)
        );
        assert_eq!(
            inventory.cursor().map(|s| s.components.clone()),
            Some(named())
        );
    }

    #[test]
    fn a_shift_click_finds_the_stack_that_matches_and_not_the_one_that_does_not() {
        let mut inventory = Inventory::default();
        // Two partial stacks of stone in the hotbar: one worn, one plain. A
        // shift-clicked worn stack must top up the worn one and leave the
        // plain one alone, even though the plain one comes first.
        inventory.slots[HOTBAR_START] = Some(stone_with(ComponentPatch::EMPTY, 60));
        inventory.slots[HOTBAR_START + 1] = Some(stone_with(worn(3), 60));
        inventory.slots[MAIN_START] = Some(stone_with(worn(3), 8));
        inventory.click(ClickMode::QuickMove, MAIN_START as i16, 0);
        assert_eq!(inventory.slot(HOTBAR_START).map(|s| s.count), Some(60));
        assert_eq!(inventory.slot(HOTBAR_START + 1).map(|s| s.count), Some(64));
        // The four that did not fit took an empty slot rather than the plain
        // stack sitting in front of it.
        assert_eq!(inventory.slot(HOTBAR_START + 2).map(|s| s.count), Some(4));
        assert_eq!(inventory.slot(MAIN_START), None);
    }

    #[test]
    fn a_double_click_gathers_the_ones_that_are_the_same_thing() {
        let mut inventory = Inventory {
            cursor: Some(stone_with(worn(3), 1)),
            ..Inventory::default()
        };
        inventory.slots[MAIN_START] = Some(stone_with(worn(3), 10));
        inventory.slots[MAIN_START + 1] = Some(stone_with(ComponentPatch::EMPTY, 10));
        inventory.slots[MAIN_START + 2] = Some(stone_with(worn(9), 10));
        inventory.click(ClickMode::PickupAll, MAIN_START as i16, 0);
        assert_eq!(inventory.cursor().map(|s| s.count), Some(11));
        assert_eq!(inventory.slot(MAIN_START), None);
        assert_eq!(inventory.slot(MAIN_START + 1).map(|s| s.count), Some(10));
        assert_eq!(inventory.slot(MAIN_START + 2).map(|s| s.count), Some(10));
    }

    #[test]
    fn a_drag_skips_a_slot_holding_the_same_item_with_different_components() {
        // The drag decides at the moment a slot joins, and the share is divided
        // by how many joined — so a slot filtered here makes the others' share
        // larger rather than losing its own.
        let mut inventory = Inventory {
            cursor: Some(stone_with(named(), 20)),
            ..Inventory::default()
        };
        inventory.slots[MAIN_START] = Some(stone_with(ComponentPatch::EMPTY, 1));
        inventory.click(ClickMode::QuickCraft, OUTSIDE, 0);
        inventory.click(ClickMode::QuickCraft, MAIN_START as i16, 1);
        inventory.click(ClickMode::QuickCraft, (MAIN_START + 1) as i16, 1);
        inventory.click(ClickMode::QuickCraft, OUTSIDE, 2);
        assert_eq!(inventory.slot(MAIN_START).map(|s| s.count), Some(1));
        assert_eq!(inventory.slot(MAIN_START + 1).map(|s| s.count), Some(20));
    }

    #[test]
    fn a_middle_click_copies_the_components_the_way_vanilla_copies_the_stack() {
        let mut inventory = Inventory::default();
        inventory.slots[MAIN_START] = Some(stone_with(named(), 1));
        inventory.click(ClickMode::Clone, MAIN_START as i16, 0);
        assert_eq!(inventory.cursor().map(|s| s.count), Some(64));
        assert_eq!(
            inventory.cursor().map(|s| s.components.clone()),
            Some(named())
        );
    }

    #[test]
    fn a_named_stack_survives_the_wire_and_comes_back_the_same_stack() {
        let stack = stone_with(named(), 3);
        let wire = to_wire(Some(&stack));
        assert_eq!(from_wire(&wire), Some(stack));
    }

    #[test]
    fn a_creative_write_keeps_the_components_the_client_sent() {
        install_component_types();
        let mut inventory = Inventory::default();
        let sent = Slot::Present {
            count: 1,
            item_id: item("minecraft:stone").protocol_id() as i32,
            components: named(),
        };
        assert_eq!(
            inventory.set_creative(MAIN_START as i16, &sent),
            Ok(Some(MAIN_START))
        );
        assert_eq!(
            inventory.slot(MAIN_START).map(|s| s.components.clone()),
            Some(named())
        );
    }

    #[test]
    fn the_boot_path_installs_the_component_registry() {
        // Without this the whole feature is inert: `dust-protocol` refuses
        // every component by number, and it would do it quietly, one packet at
        // a time, on a server that looked like it was working.
        install_component_types();
        assert!(dust_protocol::components::type_names_installed());
        assert_eq!(
            dust_protocol::components::type_name(
                dust_registry::Registry::from_name("minecraft:data_component_type")
                    .and_then(|r| r.entry_id("minecraft:custom_name"))
                    .expect("in the registry") as i32
            ),
            Some("minecraft:custom_name")
        );
    }
}
