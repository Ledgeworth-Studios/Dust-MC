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
//! # What is stored, and what is lost
//!
//! A [`Stack`] is an [`Item`] and a `u8`: four bytes, `Copy`, no allocation.
//! The whole container is a fixed array, so reading a slot is an index and
//! writing one is a store. Nothing on this path allocates, which matters
//! because `held()` is read on every right-click and the container is written
//! on every click a player makes.
//!
//! What that costs is **components**. [`Slot`] carries a list of component
//! *removals* and Dust cannot decode component *additions* at all — see
//! [`dust_protocol::types::Slot`] for why partial decoding is not on offer —
//! so a stack's removals are dropped on the way in. A renamed block, a shulker
//! box with things in it, a tool with an enchantment: all of them are stored
//! and given back as the plain item. That is the same limitation the hotbar
//! had; what has changed is that it is now a limitation about a stack that
//! *survives a relog* rather than one that vanished anyway.
//!
//! # What a click does
//!
//! [`Inventory::click`] is `Click Container`'s seven modes replayed over this
//! state. It is a real specification and it is followed rather than guessed at:
//! left and right click, shift-click, the number keys and F, creative clone,
//! Q and control-Q, the three drags, and double-click-to-collect. The one thing
//! it does not do is **auto-equip armour on shift-click**, because which slot a
//! helmet goes in is `Item.getEquipmentSlot()` in Java and is in no report — it
//! is the next column `dust-items.tsv` needs. Every other way of filling an
//! armour slot works, including dragging one there.
//!
//! Dropping is real and the item is *gone*: there are no item entities in the
//! world yet, so Q destroys rather than throws. Stated here because a player
//! finds that out by losing something.

use dust_protocol::types::Slot;
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

/// One stack: an item and how many of it.
///
/// The count is never zero — an empty slot is `None`, not a stack of nothing —
/// and never above the item's own maximum. Both are invariants of every
/// constructor and every mutation here, which is what lets the rest of this
/// module do arithmetic without re-checking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stack {
    pub item: Item,
    pub count: u8,
}

impl Stack {
    /// A stack of `count`, clamped to what the item allows and to at least one.
    #[must_use]
    pub fn new(item: Item, count: u8) -> Self {
        Self {
            item,
            count: count.clamp(1, item.max_stack_size()),
        }
    }

    /// How many more of this item the stack could hold.
    fn room(self) -> u8 {
        self.item.max_stack_size().saturating_sub(self.count)
    }

    fn is_full(self) -> bool {
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
            slots: [None; SLOTS],
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

    /// What one slot holds.
    #[must_use]
    pub fn slot(&self, index: usize) -> Option<Stack> {
        self.slots.get(index).copied().flatten()
    }

    /// What is on the cursor.
    #[must_use]
    pub fn cursor(&self) -> Option<Stack> {
        self.cursor
    }

    /// Which hotbar slot is in hand, `0..9`.
    #[must_use]
    pub fn selected(&self) -> u8 {
        self.selected as u8
    }

    /// The item in the selected hotbar slot, if there is one.
    #[must_use]
    pub fn held(&self) -> Option<Item> {
        self.slots[HOTBAR_START + self.selected].map(|stack| stack.item)
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

    /// Put a stack into the main inventory and hotbar, merging into partial
    /// stacks first. Whatever does not fit is dropped, which is the only
    /// caller-visible loss and only happens with a full inventory.
    fn give(&mut self, stack: Stack, changed: &mut Changed) {
        let mut left = stack;
        // The hotbar is filled after the main inventory, matching vanilla's
        // `moveItemStackTo(stack, 9, 45, false)`: a player who closes a window
        // does not want their hand's contents replaced.
        if self.merge_into(MAIN_START..HOTBAR_END, &mut left, changed) {
            return;
        }
        self.fill_empty(MAIN_START..HOTBAR_END, &mut left, changed);
    }

    // -- the seven modes ---------------------------------------------------

    fn pickup(&mut self, slot: i16, button: i8, changed: &mut Changed) {
        if slot == OUTSIDE {
            // Clicked the world behind the window with something on the
            // cursor. Left drops it all, right drops one.
            match (self.cursor, button) {
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
        match (self.cursor, self.slots[index]) {
            // Hand empty, slot full: take it all.
            (None, Some(stack)) => {
                self.cursor = Some(stack);
                self.slots[index] = None;
            }
            // Hand full, slot empty: put it all down.
            (Some(stack), None) => {
                self.slots[index] = Some(stack);
                self.cursor = None;
            }
            // Both full, same item: pour the hand into the slot up to the
            // item's own maximum and keep the rest.
            (Some(mut held), Some(mut there)) if held.item == there.item && !there.is_full() => {
                let moved = held.count.min(there.room());
                there.count += moved;
                held.count -= moved;
                self.slots[index] = Some(there);
                self.cursor = (held.count > 0).then_some(held);
            }
            // Both full, different items — or the same item with no room.
            // Swap.
            (Some(held), Some(there)) => {
                self.slots[index] = Some(held);
                self.cursor = Some(there);
            }
            (None, None) => return,
        }
        changed.mark(index);
        changed.mark_cursor();
    }

    fn pickup_right(&mut self, index: usize, changed: &mut Changed) {
        match (self.cursor, self.slots[index]) {
            // Hand empty: take half, rounded up. Vanilla rounds the *taken*
            // half up, so a right-click on three leaves one behind.
            (None, Some(mut there)) => {
                let taken = there.count.div_ceil(2);
                self.cursor = Some(Stack {
                    item: there.item,
                    count: taken,
                });
                there.count -= taken;
                self.slots[index] = (there.count > 0).then_some(there);
            }
            // Hand full, slot empty or the same item with room: put one down.
            (Some(mut held), None) => {
                held.count -= 1;
                self.slots[index] = Some(Stack {
                    item: held.item,
                    count: 1,
                });
                self.cursor = (held.count > 0).then_some(held);
            }
            (Some(mut held), Some(mut there)) if held.item == there.item && !there.is_full() => {
                held.count -= 1;
                there.count += 1;
                self.slots[index] = Some(there);
                self.cursor = (held.count > 0).then_some(held);
            }
            (Some(held), Some(there)) => {
                self.slots[index] = Some(held);
                self.cursor = Some(there);
            }
            (None, None) => return,
        }
        changed.mark(index);
        changed.mark_cursor();
    }

    /// Shift-click: send the stack to the other half of the container.
    ///
    /// Vanilla's `InventoryMenu.quickMoveStack`, minus the armour rules — see
    /// this module's header for why those need a table nobody has extracted
    /// yet. The destinations that remain are exactly vanilla's:
    ///
    /// - the crafting slots and the armour slots empty into the inventory,
    /// - the main inventory goes to the hotbar,
    /// - the hotbar goes to the main inventory.
    fn quick_move(&mut self, slot: i16, changed: &mut Changed) {
        let Ok(index) = usize::try_from(slot) else {
            return;
        };
        if index >= SLOTS {
            return;
        }
        let Some(mut stack) = self.slots[index] else {
            return;
        };
        let destination = if (MAIN_START..MAIN_END).contains(&index) {
            HOTBAR_START..HOTBAR_END
        } else if (HOTBAR_START..HOTBAR_END).contains(&index) {
            MAIN_START..MAIN_END
        } else {
            // The crafting slots, the armour slots and the offhand all empty
            // into the inventory as a whole.
            MAIN_START..HOTBAR_END
        };
        self.slots[index] = None;
        let before = stack.count;
        if !self.merge_into(destination.clone(), &mut stack, changed) {
            self.fill_empty(destination, &mut stack, changed);
        }
        if stack.count == before {
            // Nowhere for any of it to go. Vanilla leaves the slot alone and
            // so does this: a shift-click that moves nothing must not report a
            // change, or the client redraws a slot that did not move.
            self.slots[index] = Some(stack);
            return;
        }
        self.slots[index] = (stack.count > 0).then_some(stack);
        changed.mark(index);
    }

    /// A number key or F: swap this slot with a hotbar slot or the offhand.
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
        self.slots.swap(index, other);
        changed.mark(index);
        changed.mark(other);
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
        let Some(there) = self.slots[index] else {
            return;
        };
        self.cursor = Some(Stack {
            item: there.item,
            count: there.item.max_stack_size(),
        });
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
        let Some(mut there) = self.slots[index] else {
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
                // there: empty, or the same item with room. Vanilla checks the
                // same thing, and it matters because the share each slot gets
                // is computed from how many slots there are.
                let Some(held) = self.cursor else {
                    self.drag.reset();
                    return;
                };
                let fits = match self.slots[index] {
                    None => true,
                    Some(there) => there.item == held.item && !there.is_full(),
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
        let Some(held) = self.cursor else {
            return;
        };
        if self.drag.count == 0 {
            return;
        }
        let max = held.item.max_stack_size();
        // Left drag splits what is on the cursor evenly and keeps the
        // remainder; right drag puts one in each; middle drag is creative and
        // fills each slot without spending anything.
        let share = match kind {
            0 => held.count / self.drag.count,
            1 => 1,
            _ => max,
        };
        if share == 0 {
            return;
        }
        let mut left = held.count;
        for index in 0..SLOTS {
            if self.drag.slots & (1u64 << index) == 0 {
                continue;
            }
            let existing = self.slots[index].map_or(0, |s| s.count);
            let want = share.min(max.saturating_sub(existing));
            if want == 0 {
                continue;
            }
            // A creative middle drag spends nothing, so the cursor's count
            // never limits it.
            let take = if kind == 2 { want } else { want.min(left) };
            if take == 0 {
                break;
            }
            self.slots[index] = Some(Stack {
                item: held.item,
                count: existing + take,
            });
            changed.mark(index);
            if kind != 2 {
                left -= take;
            }
        }
        if kind != 2 && left != held.count {
            self.cursor = (left > 0).then_some(Stack {
                item: held.item,
                count: left,
            });
            changed.mark_cursor();
        }
    }

    /// Double-click: gather every loose one of this item onto the cursor.
    ///
    /// Two passes, because vanilla makes two: partial stacks first, so that
    /// double-clicking with a half stack tidies the loose ones up instead of
    /// breaking a full stack somewhere else in the inventory.
    fn pickup_all(&mut self, button: i8, changed: &mut Changed) {
        let Some(mut held) = self.cursor else {
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
                let Some(mut there) = self.slots[index] else {
                    continue;
                };
                if there.item != held.item {
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

    /// Pour `stack` into partial stacks of the same item in `range`. Returns
    /// whether it all fitted.
    fn merge_into(
        &mut self,
        range: std::ops::Range<usize>,
        stack: &mut Stack,
        changed: &mut Changed,
    ) -> bool {
        for index in range {
            let Some(mut there) = self.slots[index] else {
                continue;
            };
            if there.item != stack.item || there.is_full() {
                continue;
            }
            let moved = stack.count.min(there.room());
            there.count += moved;
            stack.count -= moved;
            self.slots[index] = Some(there);
            changed.mark(index);
            if stack.count == 0 {
                return true;
            }
        }
        stack.count == 0
    }

    /// Put whatever is left of `stack` into the first empty slots in `range`.
    fn fill_empty(
        &mut self,
        range: std::ops::Range<usize>,
        stack: &mut Stack,
        changed: &mut Changed,
    ) {
        for index in range {
            if stack.count == 0 {
                return;
            }
            if self.slots[index].is_some() {
                continue;
            }
            let moved = stack.count.min(stack.item.max_stack_size());
            self.slots[index] = Some(Stack {
                item: stack.item,
                count: moved,
            });
            stack.count -= moved;
            changed.mark(index);
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
            Decoded::Stack(Stack { item, count })
        }
    }
}

/// A stack as the wire wants it.
///
/// The removals list is empty because nothing here stores one; see this
/// module's header.
#[must_use]
pub fn to_wire(stack: Option<Stack>) -> Slot {
    match stack {
        None => Slot::Empty,
        Some(stack) => Slot::Present {
            count: i32::from(stack.count),
            item_id: stack.item.protocol_id() as i32,
            removed_components: Vec::new(),
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

    fn wire(item: Item, count: i32) -> Slot {
        Slot::Present {
            count,
            item_id: item.protocol_id() as i32,
            removed_components: Vec::new(),
        }
    }

    fn with(pairs: &[(usize, Item, u8)]) -> Inventory {
        let mut inventory = Inventory::default();
        for &(index, item, count) in pairs {
            inventory.slots[index] = Some(Stack { item, count });
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
        assert_eq!(inventory.cursor(), None);
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
        assert_eq!(inventory.slot(36), None);
        assert_eq!(inventory.set_creative(37, &wire(stone(), 65)), Err(37));
        assert_eq!(inventory.slot(37), None);
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
                    removed_components: Vec::new(),
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
        assert_eq!(inventory.slot(9), None);
        assert!(changed.has(9) && changed.cursor());

        // Merge: 30 into a stack of 50 leaves 16 in hand and 64 in the slot.
        inventory.click(ClickMode::Pickup, 10, 0);
        assert_eq!(inventory.slot(10).map(|s| s.count), Some(64));
        assert_eq!(inventory.cursor().map(|s| s.count), Some(16));

        // Swap: a different item.
        inventory.click(ClickMode::Pickup, 11, 0);
        assert_eq!(inventory.slot(11), Some(Stack::new(stone(), 16)));
        assert_eq!(inventory.cursor(), Some(Stack::new(dirt(), 1)));

        // Put down.
        inventory.click(ClickMode::Pickup, 12, 0);
        assert_eq!(inventory.slot(12), Some(Stack::new(dirt(), 1)));
        assert_eq!(inventory.cursor(), None);
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
        assert_eq!(inventory.cursor(), None);
    }

    #[test]
    fn right_click_on_a_single_leaves_the_slot_empty_rather_than_a_stack_of_nothing() {
        let mut inventory = with(&[(9, bucket(), 1)]);
        inventory.click(ClickMode::Pickup, 9, 1);
        assert_eq!(inventory.slot(9), None);
        assert_eq!(inventory.cursor(), Some(Stack::new(bucket(), 1)));
    }

    #[test]
    fn shift_click_sends_the_hotbar_to_the_inventory_and_back() {
        let mut inventory = with(&[(36, stone(), 20)]);
        inventory.click(ClickMode::QuickMove, 36, 0);
        assert_eq!(inventory.slot(36), None);
        assert_eq!(inventory.slot(9), Some(Stack::new(stone(), 20)));

        inventory.click(ClickMode::QuickMove, 9, 0);
        assert_eq!(inventory.slot(9), None);
        assert_eq!(inventory.slot(36), Some(Stack::new(stone(), 20)));
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
        assert_eq!(inventory.slot(36), None);
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
        assert_eq!(inventory.slot(36), Some(Stack::new(stone(), 5)));
    }

    #[test]
    fn a_number_key_swaps_with_that_hotbar_slot_and_f_swaps_with_the_offhand() {
        let mut inventory = with(&[(9, stone(), 4), (38, dirt(), 2)]);
        inventory.click(ClickMode::Swap, 9, 2);
        assert_eq!(inventory.slot(9), Some(Stack::new(dirt(), 2)));
        assert_eq!(inventory.slot(38), Some(Stack::new(stone(), 4)));

        inventory.click(ClickMode::Swap, 9, SWAP_OFFHAND_BUTTON);
        assert_eq!(inventory.slot(9), None);
        assert_eq!(inventory.slot(OFFHAND), Some(Stack::new(dirt(), 2)));
    }

    #[test]
    fn middle_click_clones_a_full_stack_of_that_items_own_maximum() {
        let mut inventory = with(&[(9, stone(), 1), (10, bucket(), 1)]);
        inventory.click(ClickMode::Clone, 9, 2);
        assert_eq!(inventory.cursor(), Some(Stack::new(stone(), 64)));
        assert_eq!(inventory.slot(9), Some(Stack::new(stone(), 1)), "unchanged");

        // And the number is the item's, not 64.
        let mut inventory = with(&[(10, bucket(), 1)]);
        inventory.click(ClickMode::Clone, 10, 2);
        assert_eq!(inventory.cursor(), Some(Stack::new(bucket(), 1)));
    }

    #[test]
    fn q_drops_one_and_control_q_drops_the_stack() {
        let mut inventory = with(&[(36, stone(), 3)]);
        inventory.click(ClickMode::Throw, 36, 0);
        assert_eq!(inventory.slot(36).map(|s| s.count), Some(2));
        inventory.click(ClickMode::Throw, 36, 1);
        assert_eq!(inventory.slot(36), None);
    }

    #[test]
    fn clicking_outside_the_window_drops_what_is_on_the_cursor() {
        let mut inventory = with(&[(9, stone(), 4)]);
        inventory.click(ClickMode::Pickup, 9, 0);
        inventory.click(ClickMode::Pickup, OUTSIDE, 1);
        assert_eq!(inventory.cursor().map(|s| s.count), Some(3));
        inventory.click(ClickMode::Pickup, OUTSIDE, 0);
        assert_eq!(inventory.cursor(), None);
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
        assert_eq!(inventory.slot(10), None, "the drag never landed");
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
        assert_eq!(inventory.slot(10), None);
        assert_eq!(inventory.slot(11), None);
        assert_eq!(inventory.slot(9).map(|s| s.count), Some(15));
    }

    #[test]
    fn closing_the_window_puts_the_cursor_and_the_grid_back_rather_than_deleting_them() {
        let mut inventory = with(&[(9, stone(), 4), (CRAFTING_START, dirt(), 2)]);
        inventory.click(ClickMode::Pickup, 9, 0);
        assert_eq!(inventory.cursor().map(|s| s.count), Some(4));
        inventory.closed();
        assert_eq!(inventory.cursor(), None);
        assert_eq!(inventory.slot(CRAFTING_START), None);
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
        assert_eq!(inventory.slot(9), Some(Stack::new(stone(), 4)));
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
            let stack = Stack { item, count };
            assert_eq!(from_wire(&to_wire(Some(stack))), Some(stack));
        }
        assert_eq!(to_wire(None), Slot::Empty);
        assert_eq!(from_wire(&Slot::Empty), None);
    }
}
