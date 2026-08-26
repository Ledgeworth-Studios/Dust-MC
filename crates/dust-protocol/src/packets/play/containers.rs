//! Field types for containers, inventories and the recipes that fill them.
//!
//! # The Slot seam runs through everything here
//!
//! Every item-carrying field in this module bottoms out in [`crate::types::Slot`],
//! which decodes an envelope and refuses component-bearing stacks by name. That
//! decision is inherited, not relitigated: a stack whose added components are
//! present cannot be stepped over without knowing every component's layout,
//! and guessing loses the position of every byte after it. What these types add
//! on top is the *structure around* the stacks — which slot, how many, what
//! terminator ends the list — so that when the wall falls, only `Slot` itself
//! changes.

use crate::nbt::Nbt;
use crate::text::Component;
use crate::types::{Decode, Encode, Identifier, ProtocolString, Slot, VarInt};
use crate::wire::{DecodeError, EncodeError, WireRead, WireWrite};
use crate::{var_int_enum, wire_struct, ProtocolVersion};

// ---------------------------------------------------------------------------
// Equipment: entries until the high bit says stop
// ---------------------------------------------------------------------------

var_int_enum! {
    /// Where on an entity a piece of equipment sits.
    ///
    /// Travels inside a byte whose top bit is the more-items flag, so the
    /// discriminant lives in the low seven bits and this enum's VarInt codec
    /// is deliberately unused — see [`EquipmentEntries`] for the packing.
    pub enum EquipmentSlot {
        MainHand = 0,
        OffHand = 1,
        Boots = 2,
        Leggings = 3,
        Chestplate = 4,
        Helmet = 5,
        Body = 6,
    }
}

/// One entry of a [`EquipmentEntries`] list, before the terminator logic.
#[derive(Debug, Clone, PartialEq)]
pub struct EquipmentEntry {
    pub slot: EquipmentSlot,
    pub item: Slot,
}

/// An entity's changed equipment: entries until one without the continuation
/// bit, exactly like metadata's terminator but spelled in reverse.
///
/// The list must have at least one entry — a bare "no equipment" is not a
/// message worth a packet — so encoding an empty list is refused rather than
/// written as a frame the peer reads as garbage.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EquipmentEntries(pub Vec<EquipmentEntry>);

const CONTINUES: u8 = 0x80;

impl Decode for EquipmentEntries {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let mut entries = Vec::new();
        loop {
            let raw = input.read_u8()?;
            let slot = EquipmentSlot::from_discriminant(i32::from(raw & !CONTINUES)).ok_or(
                DecodeError::UnknownVariant {
                    name: "EquipmentSlot",
                    value: i32::from(raw & !CONTINUES),
                },
            )?;
            entries.push(EquipmentEntry {
                slot,
                item: Slot::decode(input, version)?,
            });
            if raw & CONTINUES == 0 {
                return Ok(Self(entries));
            }
        }
    }
}

impl Encode for EquipmentEntries {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        let Some((last, rest)) = self.0.split_last() else {
            return Err(EncodeError::Unsupported {
                field: "equipment entries",
                why: "an equipment update carries at least one entry",
            });
        };
        for entry in rest {
            out.write_u8(entry.slot.discriminant() as u8 | CONTINUES);
            entry.item.encode(out, version)?;
        }
        out.write_u8(last.slot.discriminant() as u8);
        last.item.encode(out, version)
    }
}

// ---------------------------------------------------------------------------
// Container clicks: what the client believes changed
// ---------------------------------------------------------------------------

wire_struct! {
    /// One slot the client reports having changed, and what it now holds.
    pub struct ChangedSlot {
        number: i16,
        item: Slot,
    }
}

// ---------------------------------------------------------------------------
// Villager trades: an item cost that is almost, but not quite, a Slot
// ---------------------------------------------------------------------------

/// The price side of a villager trade: item id, count, and inline components.
///
/// Not a [`Slot`] because there is no removals list here — only additions —
/// and not decodable beyond the header for the reason every component-bearing
/// value shares. The refusal names the count that lied rather than reading
/// payloads whose length nobody knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TradeItem {
    pub item_id: VarInt,
    pub count: VarInt,
}

impl Decode for TradeItem {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        _version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let item_id = VarInt::decode(input, _version)?;
        let count = VarInt::decode(input, _version)?;
        let components = input.read_var_int()?;
        if components != 0 {
            return Err(DecodeError::Unsupported {
                field: "trade item components",
                why: "a structured component carries no length, so an unknown one cannot be \
                      skipped without losing the position of every field after it",
            });
        }
        Ok(Self { item_id, count })
    }
}

impl Encode for TradeItem {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        self.item_id.encode(out, version)?;
        self.count.encode(out, version)?;
        out.write_var_int(0);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Recipes: a typed envelope over the recipe registry's own layouts
// ---------------------------------------------------------------------------

var_int_enum! {
    /// Which tab of the crafting book a recipe appears under.
    pub enum CraftingBookCategory {
        Building = 0,
        Redstone = 1,
        Equipment = 2,
        Misc = 3,
    }
}

var_int_enum! {
    /// Which tab of a furnace-family book a recipe appears under.
    ///
    /// A different table from [`CraftingBookCategory`] because furnaces group
    /// by what they cook, not by what they build. Two enums where one sparse
    /// union would do, because the two travel in different packets' contexts
    /// and conflating them lets `Building` appear where only food belongs.
    pub enum CookingBookCategory {
        Food = 0,
        Blocks = 1,
        Misc = 2,
    }
}

wire_struct! {
    /// One ingredient slot of a recipe: any of these stacks will do.
    ///
    /// Each stack should carry a count of one; the *recipe* decides quantities.
    pub struct Ingredient {
        items: Vec<Slot>,
    }
}

wire_struct! {
    /// A shaped recipe's grid, its result and whether it toasts.
    pub struct CraftingShapedData {
        group: ProtocolString,
        category: CraftingBookCategory,
        width: VarInt,
        height: VarInt,
        /// Exactly `width * height` ingredients, indexed x + y * width.
        ingredients: Vec<Ingredient>,
        result: Slot,
        show_notification: bool,
    }
}

wire_struct! {
    /// A shapeless recipe's pile of ingredients and its result.
    pub struct CraftingShapelessData {
        group: ProtocolString,
        category: CraftingBookCategory,
        ingredients: Vec<Ingredient>,
        result: Slot,
    }
}

wire_struct! {
    /// A furnace-family recipe: one ingredient, one result, heat and time.
    pub struct CookingData {
        group: ProtocolString,
        category: CookingBookCategory,
        ingredient: Ingredient,
        result: Slot,
        experience: f32,
        cooking_time: VarInt,
    }
}

wire_struct! {
    /// A stonecutter recipe: one ingredient cut into one result.
    pub struct StonecuttingData {
        group: ProtocolString,
        ingredient: Ingredient,
        result: Slot,
    }
}

wire_struct! {
    /// Netherite upgrades: template plus base plus addition becomes result.
    pub struct SmithingTransformData {
        group: ProtocolString,
        template: Ingredient,
        base: Ingredient,
        addition: Ingredient,
        result: Slot,
    }
}

wire_struct! {
    /// Armor trims: template plus base plus addition, with the result derived.
    pub struct SmithingTrimData {
        group: ProtocolString,
        template: Ingredient,
        base: Ingredient,
        addition: Ingredient,
    }
}

/// One declared recipe: its registry id, its type, and the type's own layout.
///
/// The type id leads the data and picks which struct follows, so this is
/// another value-dependent tail — held together as one type because a recipe
/// is not decodable at all unless both halves agree about which half comes
/// next. Unknown type ids are refused rather than skipped for the usual reason:
/// their data has no length, and skipping one loses the rest of the packet.
///
/// The special "crafting_special_*" family (firework stars, map cloning, armor
/// dyeing and friends) carries only a book category — its ingredients are
/// computed client-side — which is why those ids share one arm.
#[derive(Debug, Clone, PartialEq)]
pub struct Recipe {
    pub id: Identifier,
    pub kind: RecipeKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RecipeKind {
    CraftingShaped(CraftingShapedData),
    CraftingShapeless(CraftingShapelessData),
    /// `crafting_special_*` and `crafting_decorated_pot`: the id is kept
    /// because twelve of them share this one layout.
    Special {
        type_id: i32,
        category: CraftingBookCategory,
    },
    Cooking {
        type_id: i32,
        data: CookingData,
    },
    Stonecutting(StonecuttingData),
    SmithingTransform(SmithingTransformData),
    SmithingTrim(SmithingTrimData),
}

impl Recipe {
    const SHAPED: i32 = 0;
    const SHAPELESS: i32 = 1;
    /// The special crafting ids: eleven classic ones, then the decorated pot
    /// parked at 22 after the cooking family took 15 through 21.
    const SPECIAL_IDS: [i32; 12] = [2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 22];
    const SMELTING: i32 = 15;
    const BLASTING: i32 = 16;
    const SMOKING: i32 = 17;
    const CAMPFIRE_COOKING: i32 = 18;
    const STONECUTTING: i32 = 19;
    const SMITHING_TRANSFORM: i32 = 20;
    const SMITHING_TRIM: i32 = 21;
}

impl Decode for Recipe {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        let id = Identifier::decode(input, version)?;
        let type_id = input.read_var_int()?;
        let kind = if type_id == Self::SHAPED {
            RecipeKind::CraftingShaped(CraftingShapedData::decode(input, version)?)
        } else if type_id == Self::SHAPELESS {
            RecipeKind::CraftingShapeless(CraftingShapelessData::decode(input, version)?)
        } else if Self::SPECIAL_IDS.contains(&type_id) {
            RecipeKind::Special {
                type_id,
                category: CraftingBookCategory::decode(input, version)?,
            }
        } else if matches!(
            type_id,
            Self::SMELTING | Self::BLASTING | Self::SMOKING | Self::CAMPFIRE_COOKING
        ) {
            RecipeKind::Cooking {
                type_id,
                data: CookingData::decode(input, version)?,
            }
        } else if type_id == Self::STONECUTTING {
            RecipeKind::Stonecutting(StonecuttingData::decode(input, version)?)
        } else if type_id == Self::SMITHING_TRANSFORM {
            RecipeKind::SmithingTransform(SmithingTransformData::decode(input, version)?)
        } else if type_id == Self::SMITHING_TRIM {
            RecipeKind::SmithingTrim(SmithingTrimData::decode(input, version)?)
        } else {
            return Err(DecodeError::UnknownVariant {
                name: "recipe type",
                value: type_id,
            });
        };
        Ok(Self { id, kind })
    }
}

impl Encode for Recipe {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        self.id.encode(out, version)?;
        match &self.kind {
            RecipeKind::CraftingShaped(data) => {
                out.write_var_int(Recipe::SHAPED);
                data.encode(out, version)
            }
            RecipeKind::CraftingShapeless(data) => {
                out.write_var_int(Recipe::SHAPELESS);
                data.encode(out, version)
            }
            RecipeKind::Special { type_id, category } => {
                out.write_var_int(*type_id);
                category.encode(out, version)
            }
            RecipeKind::Cooking { type_id, data } => {
                out.write_var_int(*type_id);
                data.encode(out, version)
            }
            RecipeKind::Stonecutting(data) => {
                out.write_var_int(Recipe::STONECUTTING);
                data.encode(out, version)
            }
            RecipeKind::SmithingTransform(data) => {
                out.write_var_int(Recipe::SMITHING_TRANSFORM);
                data.encode(out, version)
            }
            RecipeKind::SmithingTrim(data) => {
                out.write_var_int(Recipe::SMITHING_TRIM);
                data.encode(out, version)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Scoreboards: how a score number is dressed up
// ---------------------------------------------------------------------------

/// How the client renders a score's number, or replaces it entirely.
///
/// Absent means "use the objective's format"; this type is the per-score or
/// per-objective override itself, so there is no absent case here — presence
/// is decided by the boolean that precedes it in the packet.
#[derive(Debug, Clone, PartialEq)]
pub enum NumberFormat {
    /// Show nothing at all.
    Blank,
    /// Style the number with text-component styling fields.
    Styled(Nbt),
    /// Replace the number with fixed text.
    Fixed(Component),
}

impl Decode for NumberFormat {
    fn decode<R: WireRead + ?Sized>(
        input: &mut R,
        version: ProtocolVersion,
    ) -> Result<Self, DecodeError> {
        match input.read_var_int()? {
            0 => Ok(Self::Blank),
            1 => Nbt::decode(input, version).map(Self::Styled),
            2 => Component::decode(input, version).map(Self::Fixed),
            other => Err(DecodeError::UnknownVariant {
                name: "NumberFormat",
                value: other,
            }),
        }
    }
}

impl Encode for NumberFormat {
    fn encode<W: WireWrite + ?Sized>(
        &self,
        out: &mut W,
        version: ProtocolVersion,
    ) -> Result<(), EncodeError> {
        match self {
            Self::Blank => out.write_var_int(0),
            Self::Styled(nbt) => {
                out.write_var_int(1);
                return nbt.encode(out, version);
            }
            Self::Fixed(component) => {
                out.write_var_int(2);
                return component.encode(out, version);
            }
        }
        Ok(())
    }
}

wire_struct! {
    /// One changed statistic: which category, which counter, what it says now.
    ///
    /// Both ids are raw registry ids — resolving `minecraft:mined` id 42 to a
    /// block is the registry crate's job, and this layer would only bake a
    /// version's numbering into a type.
    pub struct StatisticEntry {
        category: VarInt,
        statistic: VarInt,
        value: VarInt,
    }
}
