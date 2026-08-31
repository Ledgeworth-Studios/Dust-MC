//! Generated vanilla registries: blocks, items, entities, fluids.
//!
//! The tables in [`generated`] are produced by `cargo xtask extract` from
//! Minecraft's own data generators; everything here is the hand-written API
//! over them. Two tables, because Minecraft's registries come in two shapes:
//!
//! - **Blocks** have a state space — [`Block`], [`BlockState`], and the
//!   properties below.
//! - **The flat registries** are a name and a number and nothing else: items,
//!   entity types, fluids, particles, sound events, seventy-odd of them.
//!   [`Item`], [`EntityType`] and [`Fluid`] have first-class types because
//!   passing one where another belongs is a mistake worth making impossible;
//!   the rest are reached through [`Registry`] by registry id.
//!
//! # One table here is not generated
//!
//! [`constants::BlockConstants`] is the exception to the first sentence above, and it
//! is an exception on purpose. How much light a block state costs to enter and
//! how much it gives off are Java code in Minecraft — in no report, no data
//! pack and nothing `xtask extract`'s generators can reach — so decision
//! record 0008 says they arrive at run time from the operator's own jar, the
//! same rule D6 and D7 set for ore density and registry contents. What lives
//! here is the reader and the shape; the numbers live on the operator's disk.
//! It is in this crate because a table keyed by block-state id is meaningless
//! without [`STATE_COUNT`], and that is what refuses a table extracted from a
//! different version of the game.
//!
//! `minecraft:block` is in the flat report too and is deliberately not in
//! [`Registry`]: a block's protocol id is its position in the block table, so a
//! second list of block names would be a second answer to the same question.
//! The extractor checks the two reports agree on that order rather than
//! assuming it.
//!
//! # Block states
//!
//! A block state id encodes a block and one value for each of that block's
//! properties. The ids of a block's states are contiguous from a base, and the
//! encoding is mixed-radix with the first property varying slowest. That is a
//! property of Minecraft's data rather than a choice made here, and the
//! extractor verifies it against every state rather than assuming it — see
//! `xtask/src/extract/blocks.rs`, where four blocks on 1.21.1 turn out to
//! disagree with the obvious reading of the report.

pub mod commands;
pub mod constants;
pub mod flat;
pub mod fluids;
pub mod generated;
pub mod items;
pub mod loot;
pub mod recipes;
pub mod registry;
pub mod synced;
pub mod tags;

pub use commands::{ArgumentProperties, CommandDef, CommandGraph, NodeKind};
pub use constants::{BlockConstants, ConstantsError};
pub use flat::{EntityType, Fluid, Item};
pub use generated::blocks::{DATA_VERSION, STATE_COUNT, STATE_SAMPLES};
pub use generated::items::COMPONENT_SAMPLES;
pub use generated::registries::{ENTRY_COUNT, ENTRY_SAMPLES};
pub use items::{ComponentValue, Components, Rarity};
pub use registry::{Registry, RegistryDef};

/// One property of a block, and the values it may take, in id order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PropertyDef {
    pub name: &'static str,
    pub values: &'static [&'static str],
}

/// One block, as the generated table holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockDef {
    /// Namespaced id, e.g. `minecraft:oak_log`.
    pub name: &'static str,
    pub base_state_id: u32,
    pub state_count: u32,
    pub default_state_id: u32,
    /// Properties in state-id order: the first varies slowest.
    pub properties: &'static [PropertyDef],
}

/// A block kind — `minecraft:oak_log`, without which way up it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Block(u16);

impl Block {
    /// Look a block up by its namespaced id.
    ///
    /// A bare name is not accepted. `oak_log` and `minecraft:oak_log` are the
    /// same block to a player and different strings to a lookup, and accepting
    /// both here would mean every caller downstream is unsure which it holds.
    pub fn from_name(name: &str) -> Option<Self> {
        generated::blocks::BLOCKS_BY_NAME
            .binary_search_by(|&i| Self::def_at(i).name.cmp(name))
            .ok()
            .map(|position| Self(generated::blocks::BLOCKS_BY_NAME[position]))
    }

    pub fn name(self) -> &'static str {
        self.def().name
    }

    /// The state a block takes when it is placed with nothing said about it.
    pub fn default_state(self) -> BlockState {
        BlockState(self.def().default_state_id)
    }

    pub fn properties(self) -> &'static [PropertyDef] {
        self.def().properties
    }

    /// Every state of this block, in id order.
    pub fn states(self) -> impl Iterator<Item = BlockState> {
        let def = self.def();
        (def.base_state_id..def.base_state_id + def.state_count).map(BlockState)
    }

    /// Every block, in state-id order.
    pub fn all() -> impl Iterator<Item = Self> {
        (0..generated::blocks::BLOCKS.len() as u16).map(Self)
    }

    /// This block's id in `minecraft:block`, which is what a block tag on the
    /// wire carries.
    ///
    /// It is the row's position in the generated table and not a stored
    /// number, because the two would be a second answer to one question. The
    /// extraction checks that the block report's ids and the state order agree
    /// before leaving `minecraft:block` out of the registry table entirely —
    /// see `check_block_ids_match_state_order` — so the position *is* the id.
    pub fn protocol_id(self) -> u32 {
        u32::from(self.0)
    }

    fn def(self) -> &'static BlockDef {
        Self::def_at(self.0)
    }

    fn def_at(index: u16) -> &'static BlockDef {
        &generated::blocks::BLOCKS[index as usize]
    }
}

/// A block with every property settled — what a chunk actually stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockState(u32);

impl BlockState {
    /// The state with this id, or `None` if nothing has that id.
    pub fn from_id(id: u32) -> Option<Self> {
        (id < STATE_COUNT).then_some(Self(id))
    }

    pub fn id(self) -> u32 {
        self.0
    }

    pub fn block(self) -> Block {
        // The blocks are in base-id order and their states tile 0..STATE_COUNT
        // with no gap — both checked at extraction — so the owning block is the
        // last one whose base is at or below this id.
        let index = generated::blocks::BLOCKS
            .partition_point(|def| def.base_state_id <= self.0)
            .saturating_sub(1);
        Block(index as u16)
    }

    /// The value this state has for `property`, or `None` if the block has no
    /// such property.
    pub fn property(self, property: &str) -> Option<&'static str> {
        let def = self.block().def();
        let mut remainder = self.0 - def.base_state_id;
        // Digits are read least-significant first, which is the last property.
        for candidate in def.properties.iter().rev() {
            let radix = candidate.values.len() as u32;
            let digit = remainder % radix;
            remainder /= radix;
            if candidate.name == property {
                return Some(candidate.values[digit as usize]);
            }
        }
        None
    }

    /// Every property of this state, in id order, as name and value.
    pub fn properties(self) -> Vec<(&'static str, &'static str)> {
        let def = self.block().def();
        let mut remainder = self.0 - def.base_state_id;
        let mut pairs = Vec::with_capacity(def.properties.len());
        for candidate in def.properties.iter().rev() {
            let radix = candidate.values.len() as u32;
            pairs.push((
                candidate.name,
                candidate.values[(remainder % radix) as usize],
            ));
            remainder /= radix;
        }
        pairs.reverse();
        pairs
    }

    /// The same block with one property changed.
    ///
    /// `None` when the block has no such property, or the value is not one it
    /// takes — never a state belonging to a different block, which is what an
    /// arithmetic-only implementation returns when handed an out-of-range
    /// value index.
    pub fn with(self, property: &str, value: &str) -> Option<Self> {
        let def = self.block().def();
        let mut index = 0u32;
        let mut changed = false;
        for (name, current) in self.properties() {
            let candidate = def
                .properties
                .iter()
                .find(|p| p.name == name)
                .expect("came from this block");
            let chosen = if name == property {
                changed = true;
                value
            } else {
                current
            };
            let position = candidate.values.iter().position(|v| *v == chosen)?;
            index = index * candidate.values.len() as u32 + position as u32;
        }
        changed.then_some(Self(def.base_state_id + index))
    }
}
