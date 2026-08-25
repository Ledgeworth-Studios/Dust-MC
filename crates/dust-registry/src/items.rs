//! Every item's default data components.
//!
//! An item on 1.21.1 is a name, a number, and a map of components — and the
//! components are where everything interesting lives. `minecraft:diamond_sword`
//! is not "a sword" to the protocol; it is an item with an attack damage of 6,
//! an attack speed of -2.4, a max damage of 1,561 and five mining rules. D3
//! chose this version because a server can hand an unmodified client an item
//! whose components it made up, and this table is what that starts from.
//!
//! # The representation, and why it is a value tree
//!
//! Component values are heterogeneous: a number, a string, a nested object, a
//! list of objects with different key sets. Three shapes were available.
//!
//! **A typed struct per component type.** The most pleasant to use and the one
//! this crate does not do. 57 component types exist in the
//! `minecraft:data_component_type` registry and only 30 of them have a default
//! on any item, so 27 structs would be written against the wiki rather than
//! against data — and nothing in this repository could check them, because the
//! report says nothing about a component no item carries. Worse, several of the
//! 30 that *do* appear have `{}` as their default: `minecraft:potion_contents`,
//! `minecraft:writable_book_content`, `minecraft:bucket_entity_data`. An empty
//! object teaches nothing about the shape of a full one. Typing from this
//! report would be typing from a guess for more than half of it, and a guess
//! with a struct around it reads exactly like knowledge.
//!
//! **A generic value tree.** What [`ComponentValue`] is. Every value in the
//! report is representable, a future component needs no code change, and the
//! table is honest about what it is: the report, restructured, and not a model
//! of item behaviour.
//!
//! **A hybrid**, which is what this ended up being. The tree is the only
//! storage; on top of it sit typed accessors for the handful of components that
//! are on every item and whose shape the extractor checks across all 1,333:
//! [`Item::max_stack_size`], [`Item::rarity`], [`Item::repair_cost`],
//! [`Item::max_damage`], [`Item::damage`], [`Item::is_fire_resistant`]. Those
//! cannot be lies, because `cargo xtask extract` refuses to emit a table where
//! any item's `max_stack_size` is not an integer in `1..=99` or whose rarity is
//! not one of the four [`Rarity`] variants. A fifth rarity in 1.21.2 stops the
//! extraction rather than being rounded into the enum.
//!
//! **What this makes hard later, stated plainly.** The custom item builder D3
//! is for will need to *construct* components and serialise them to the client,
//! and this table will not tell it how — a tree that can hold anything cannot
//! say what is well-formed. Those types will have to be written against the
//! protocol specification, and this table's role then is to be the fixture they
//! are checked against, not the source they are generated from. Reading a
//! nested value also costs a string lookup per level rather than a field
//! access, so anything on a hot path should read it once and keep what it
//! found. Neither is a surprise waiting to happen; both are the price of not
//! writing 27 structs from memory.
//!
//! # Floats
//!
//! Component numbers are `f64` because that is the width the report's own text
//! implies, and re-spelling one at `f32` width changes it: the report writes
//! `1.2` for a value whose `f32` widens back to `1.2000000476837158`, and
//! `-2.4000000953674316` for one that is an `f32` exactly. [`ComponentValue::as_f32`]
//! is there for the fields the protocol sends as a float, and it is a narrowing
//! the caller asks for rather than one this table did on its way in.

use crate::generated::items::{COMPONENT_MAPS, ITEM_COMPONENTS};
use crate::Item;

/// One component value.
///
/// `Int` and `Float` are separate because the report distinguishes them and the
/// protocol does too — `6` and `6.0` are not the same thing to a codec.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ComponentValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(&'static str),
    List(&'static [ComponentValue]),
    /// Fields sorted by name. The report's key order is not preserved because
    /// it is not semantic: a component reaches the client in its codec's field
    /// order, not in the order a report printed it.
    Map(&'static [(&'static str, ComponentValue)]),
}

impl ComponentValue {
    pub fn as_bool(self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(b),
            _ => None,
        }
    }

    pub fn as_i64(self) -> Option<i64> {
        match self {
            Self::Int(i) => Some(i),
            _ => None,
        }
    }

    /// The value as the report spells it.
    ///
    /// An `Int` is *not* widened into one. A component that is an integer in
    /// the report is an integer, and a caller that would accept either should
    /// say so by asking for both.
    pub fn as_f64(self) -> Option<f64> {
        match self {
            Self::Float(f) => Some(f),
            _ => None,
        }
    }

    /// The value narrowed to the width the protocol sends some of these fields
    /// at.
    ///
    /// Explicitly the caller's decision. Several of these numbers are Java
    /// `float`s that reached the report widened, and narrowing them again is
    /// exact; the rest are not, and narrowing those loses something. Which is
    /// which depends on the component's field, which this table does not model
    /// — so it is offered rather than applied.
    pub fn as_f32(self) -> Option<f32> {
        self.as_f64().map(|f| f as f32)
    }

    pub fn as_str(self) -> Option<&'static str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_list(self) -> Option<&'static [ComponentValue]> {
        match self {
            Self::List(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_map(self) -> Option<&'static [(&'static str, ComponentValue)]> {
        match self {
            Self::Map(fields) => Some(fields),
            _ => None,
        }
    }

    /// The field of this value with that name, if this is a map and it has one.
    pub fn get(self, field: &str) -> Option<Self> {
        let fields = self.as_map()?;
        let index = fields
            .binary_search_by(|(name, _)| (*name).cmp(field))
            .ok()?;
        Some(fields[index].1)
    }
}

/// One item's default components.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Components(&'static [(&'static str, ComponentValue)]);

impl Components {
    /// The component with this namespaced id, e.g. `minecraft:max_damage`.
    ///
    /// A bare name is not accepted, for the same reason nothing else in this
    /// crate accepts one.
    pub fn get(self, component: &str) -> Option<ComponentValue> {
        let index = self
            .0
            .binary_search_by(|(name, _)| (*name).cmp(component))
            .ok()?;
        Some(self.0[index].1)
    }

    /// Whether the item carries this component at all.
    ///
    /// Worth its own method because several components are *unit* components —
    /// `minecraft:fire_resistant` is an empty object, and its presence is the
    /// whole of its meaning.
    pub fn contains(self, component: &str) -> bool {
        self.get(component).is_some()
    }

    pub fn len(self) -> usize {
        self.0.len()
    }

    pub fn is_empty(self) -> bool {
        self.0.is_empty()
    }

    /// Every component, in name order.
    pub fn iter(self) -> impl Iterator<Item = (&'static str, ComponentValue)> {
        self.0.iter().copied()
    }
}

/// How prominently a client colours an item's name.
///
/// A closed set of four, which the extractor checks against all 1,333 items
/// rather than this asserting it. A version with a fifth stops the extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Rarity {
    Common,
    Uncommon,
    Rare,
    Epic,
}

impl Rarity {
    /// The name the report uses, which is also the name on the wire.
    pub fn name(self) -> &'static str {
        match self {
            Self::Common => "common",
            Self::Uncommon => "uncommon",
            Self::Rare => "rare",
            Self::Epic => "epic",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "common" => Self::Common,
            "uncommon" => Self::Uncommon,
            "rare" => Self::Rare,
            "epic" => Self::Epic,
            _ => return None,
        })
    }
}

impl Item {
    /// This item's default components.
    pub fn components(self) -> Components {
        let map = ITEM_COMPONENTS[self.protocol_id() as usize];
        Components(COMPONENT_MAPS[map as usize])
    }

    /// How many of this item fit in one stack — 1, 16 or 64 on 1.21.1.
    ///
    /// Every item has one, and the extractor refuses to emit a table where any
    /// item's is not an integer in `1..=99`, so this cannot be absent and
    /// cannot need a fallback.
    pub fn max_stack_size(self) -> u8 {
        self.expect_int("minecraft:max_stack_size") as u8
    }

    pub fn rarity(self) -> Rarity {
        let name = self
            .components()
            .get("minecraft:rarity")
            .and_then(ComponentValue::as_str)
            .expect("the extractor checks every item has one");
        Rarity::from_name(name).expect("the extractor checks it is one of the four")
    }

    /// How much the anvil charges to repair this item again. Zero for every
    /// item on 1.21.1 — it is what *use* raises, not what an item starts with.
    pub fn repair_cost(self) -> u32 {
        self.expect_int("minecraft:repair_cost") as u32
    }

    /// The durability of this item, or `None` for the 1,265 that have none.
    pub fn max_damage(self) -> Option<u32> {
        self.optional_int("minecraft:max_damage")
    }

    /// How damaged this item starts, which is zero wherever it is present.
    pub fn damage(self) -> Option<u32> {
        self.optional_int("minecraft:damage")
    }

    /// Whether this item survives lava.
    ///
    /// A unit component: its value is an empty object and its presence is the
    /// whole of its meaning, which the extractor insists on rather than this
    /// quietly ignoring a value if one ever appeared.
    pub fn is_fire_resistant(self) -> bool {
        self.components().contains("minecraft:fire_resistant")
    }

    fn expect_int(self, component: &str) -> i64 {
        self.components()
            .get(component)
            .and_then(ComponentValue::as_i64)
            .expect("the extractor checks every item has one and that it is an integer")
    }

    fn optional_int(self, component: &str) -> Option<u32> {
        let value = self.components().get(component)?.as_i64()?;
        Some(value as u32)
    }
}
