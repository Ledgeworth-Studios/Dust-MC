//! What a player is holding.
//!
//! # Not an inventory, and the distance between the two is the point
//!
//! A real inventory is forty-one slots, a cursor, shift-click semantics, stack
//! merging, a container protocol with a click replay and a disagreement
//! pushback, and item components that survive being moved. None of that is
//! here and none of it is pretended at.
//!
//! What is here is the nine hotbar slots and which one is selected, because a
//! **creative** client is allowed to write a slot directly — `set_creative_mode
//! _slot` is the one inventory write that needs no container open — and Dust
//! puts every player in creative. So the whole path from "the player picked
//! cobblestone out of the creative menu" to "the server knows they are holding
//! cobblestone" is two packets, and neither of them is a container click.
//!
//! That is what makes this worth having on its own: it is the smallest thing
//! that turns "there is one placeable block" into "you place what you are
//! holding", and it does not require the forty other slots to exist first.
//!
//! # What is deliberately missing
//!
//! **Counts.** A creative player's stack does not shrink, so nothing here
//! subtracts one; a survival server will need a count and this will be the
//! wrong type by then.
//!
//! **Components.** [`Slot`](dust_protocol::types::Slot) carries a list of
//! component *removals* and Dust cannot yet decode the additions at all. A
//! renamed block, a shulker box with things in it, a spawn egg with an entity
//! on it — all of those arrive as the plain item and are placed as the plain
//! block. Stated rather than dressed up.
//!
//! **The other thirty-two slots.** Nothing writes them, so nothing stores them.
//! A player's armour and their main inventory are not modelled and a client
//! that expects the server to remember them across a relog will find it does
//! not.

use dust_protocol::types::Slot;
use dust_registry::Item;

/// How many slots a hotbar has. Vanilla's `Inventory.SELECTION_SIZE`.
pub const SLOTS: usize = 9;

/// Where the hotbar sits in the player's container, which is what
/// `set_creative_mode_slot` numbers its slots by.
///
/// The player inventory container runs 0..=45 with the crafting grid and armour
/// first; the hotbar is 36..=44. Vanilla's own numbering, and the reason this
/// is a named range rather than a subtraction at the call site: a slot index
/// off by nine is a player holding the wrong thing, which looks exactly like a
/// client bug.
const HOTBAR_START: i16 = 36;

/// The nine slots a player can hold something in, and which one is in hand.
#[derive(Debug, Clone, Default)]
pub struct Hotbar {
    slots: [Option<Item>; SLOTS],
    selected: usize,
}

impl Hotbar {
    /// The item in the selected slot, if there is one.
    pub fn held(&self) -> Option<Item> {
        self.slots[self.selected]
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
        if index >= SLOTS {
            return false;
        }
        self.selected = index;
        true
    }

    /// Put an item in a container slot, if that slot is part of the hotbar.
    ///
    /// Returns whether the slot was one this stores. **Everything outside the
    /// hotbar is dropped on the floor, and that is a real limitation rather
    /// than a filter**: a creative player who puts a block in their main
    /// inventory and then selects it from there is holding something this
    /// server does not know about, and will place the world's surface block.
    pub fn set(&mut self, slot: i16, item: &Slot) -> bool {
        let Some(index) = usize::try_from(slot - HOTBAR_START)
            .ok()
            .filter(|index| *index < SLOTS)
        else {
            return false;
        };
        self.slots[index] = match item {
            Slot::Empty => None,
            Slot::Present { item_id, .. } => {
                // An id this build has no item for is emptiness and not a
                // refusal. It arrives from a client that may be modded or may
                // be a version ahead, and dropping the connection over an item
                // nobody can place would be a disconnect for a right-click.
                u32::try_from(*item_id)
                    .ok()
                    .and_then(Item::from_protocol_id)
            }
        };
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str) -> Item {
        Item::from_name(name).expect("this build has that item")
    }

    fn stack(item: Item) -> Slot {
        Slot::Present {
            count: 1,
            item_id: item.protocol_id() as i32,
            removed_components: Vec::new(),
        }
    }

    #[test]
    fn a_fresh_hotbar_holds_nothing() {
        assert_eq!(Hotbar::default().held(), None);
    }

    #[test]
    fn a_creative_write_lands_in_the_slot_it_names() {
        // 36 is hotbar slot 0 and 44 is slot 8. The offset is vanilla's and
        // getting it wrong by nine puts the block in a slot the player is not
        // holding, which reads as the server ignoring them.
        let mut hotbar = Hotbar::default();
        let stone = item("minecraft:stone");
        assert!(hotbar.set(36, &stack(stone)));
        assert_eq!(
            hotbar.held(),
            Some(stone),
            "slot 0 is selected to begin with"
        );

        let dirt = item("minecraft:dirt");
        assert!(hotbar.set(44, &stack(dirt)));
        assert_eq!(hotbar.held(), Some(stone), "still holding slot 0");
        assert!(hotbar.select(8));
        assert_eq!(hotbar.held(), Some(dirt));
    }

    #[test]
    fn a_slot_outside_the_hotbar_is_refused_rather_than_folded_into_it() {
        // 35 is the last main-inventory slot and 45 is the offhand. Both are
        // one step outside, which is where a subtraction with the wrong sign
        // or a missing bound would land.
        let mut hotbar = Hotbar::default();
        let stone = stack(item("minecraft:stone"));
        assert!(!hotbar.set(35, &stone));
        assert!(!hotbar.set(45, &stone));
        assert!(!hotbar.set(0, &stone));
        assert!(!hotbar.set(-1, &stone));
        assert_eq!(hotbar.held(), None, "none of them reached a hotbar slot");
    }

    #[test]
    fn clearing_a_slot_is_holding_nothing_rather_than_holding_air() {
        let mut hotbar = Hotbar::default();
        assert!(hotbar.set(36, &stack(item("minecraft:stone"))));
        assert!(hotbar.set(36, &Slot::Empty));
        assert_eq!(hotbar.held(), None);
    }

    #[test]
    fn an_item_this_build_has_never_heard_of_is_held_as_nothing() {
        // A modded client, or one a version ahead. Emptiness and not a
        // disconnect: nobody should lose their session over a right-click.
        let mut hotbar = Hotbar::default();
        assert!(hotbar.set(
            36,
            &Slot::Present {
                count: 1,
                item_id: 999_999,
                removed_components: Vec::new(),
            }
        ));
        assert_eq!(hotbar.held(), None);
    }

    #[test]
    fn a_selection_outside_the_hotbar_leaves_the_one_in_hand_alone() {
        let mut hotbar = Hotbar::default();
        let stone = item("minecraft:stone");
        assert!(hotbar.set(36, &stack(stone)));
        assert!(!hotbar.select(9));
        assert!(!hotbar.select(-1));
        assert_eq!(hotbar.held(), Some(stone), "still slot 0");
    }
}
