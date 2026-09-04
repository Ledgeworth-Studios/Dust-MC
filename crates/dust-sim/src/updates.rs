//! What a block does when the world around it changes.
//!
//! # The half of a neighbour rule that runs afterwards
//!
//! [`placement`](crate::placement) answers what a block *becomes* when
//! something beside it changes — a fence grows an arm, a wall raises its post.
//! This answers the other question, which has no answer in a state at all:
//! whether the block can still be there. A torch whose wall is mined, a rail
//! whose ground is dug out, a flower on dirt that is replaced by a hole — every
//! one of those is Minecraft's `canSurvive`, and the block is destroyed and
//! dropped rather than reshaped.
//!
//! Beside it sits the other reaction a cell can have, which is to stop being a
//! block at all: sand, gravel, the concrete powders, the anvils and the dragon
//! egg **fall** when nothing holds them up.
//!
//! # Where the answers come from
//!
//! Both are Java in the game, in no report and no data pack, so both arrive the
//! way opacity and hardness do — off the operator's own jar, per block state,
//! in `dust-constants.tsv`, under decision record 0008's rule. The oracle asks
//! `canSurvive` seven ways per state and writes seven columns:
//!
//! * [`SURVIVES_ALONE`] — true where every neighbour is air. True for 20,110
//!   of 1.21.1's 26,684 states, and it is the column read first, because it
//!   costs one bit to say "this block does not care what happens around it".
//! * [`SUPPORT`] — six columns, one per side, each meaning **that neighbour on
//!   its own is enough**. Read as an `or`: a state naming two sides stands on
//!   either.
//!
//! 6,574 states need something; 6,195 of them name a side and 379 do not — a
//! multiface lichen, a crop that also wants light, a piston head. Those never
//! break, which is the safe direction and is stated in decision record 0040.
//!
//! # What counts as holding a block up, and why it is not the sturdy column
//!
//! The obvious rule is that the supporting face has to be *sturdy*, which the
//! same table already answers per state. It is wrong, and wrong in the
//! direction that deletes a player's build: the top half of a door stands on
//! the bottom half, a stalk of sugar cane stands on more sugar cane, a cactus
//! on a cactus — and none of those is a sturdy face. A rule keyed on sturdiness
//! destroys every door in the world the first time anything near one changes.
//!
//! So the test is that the supporting cell **is not replaceable**, which is
//! Minecraft's own `canBeReplaced()` and is exactly the set air, fire and the
//! fluids belong to. That is right for the thing that actually happens in play,
//! which is that a support gets mined; it is generous where a support is
//! swapped for something that could not have held the block in the first place,
//! and generous is the direction to be wrong in. A world that keeps a torch it
//! should have dropped is a bug in one block. A world that drops blocks it
//! should have kept is a server that eats builds.

use dust_registry::constants::Flag;
use dust_registry::{BlockConstants, BlockState};

use crate::placement::{Around, Face};

/// The constants column that says a state needs nothing beside it.
pub const SURVIVES_ALONE: &str = "SURVIVES_ALONE";

/// The constants columns naming which single neighbour is enough, in [`Face`]
/// order — down, up, north, south, west, east.
pub const SUPPORT: [&str; 6] = [
    "SUPPORT_DOWN",
    "SUPPORT_UP",
    "SUPPORT_NORTH",
    "SUPPORT_SOUTH",
    "SUPPORT_WEST",
    "SUPPORT_EAST",
];

/// The constants column that says a state falls when nothing is under it.
pub const FALLS: &str = "falls";

/// What the world should do about a cell whose surroundings just changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reaction {
    /// Nothing. The overwhelmingly common answer, and the one that has to be
    /// cheap.
    Stay,
    /// It cannot be here any more: break it and drop what it yields.
    Break,
    /// Nothing is holding it up: it becomes a falling entity.
    Fall,
}

/// The rules, bound to one operator's constants table.
///
/// **Constructed only from a table that has the columns**, which is the
/// `has_x` question rather than the "what does it answer when it does not
/// know" one. A server holding no `Rules` runs no support rule at all and
/// nothing falls — which is the server every operator had until this landed,
/// and is a great deal better than one that guesses and deletes.
#[derive(Debug, Clone, Copy)]
pub struct Rules<'a> {
    constants: &'a BlockConstants,
    alone: Flag,
    /// In [`Face`] order, so the lookup is an index and not a match.
    support: [Flag; 6],
    /// `None` for a table written before the column: nothing falls, rather
    /// than everything falling or a guess about which.
    falls: Option<Flag>,
}

impl<'a> Rules<'a> {
    /// The rules, if this table can answer them.
    ///
    /// The support columns are required and `falls` is not, because the two
    /// answer different questions and a table with one and not the other is a
    /// table that can still stop a torch hanging in the air.
    #[must_use]
    pub fn from_constants(constants: &'a BlockConstants) -> Option<Self> {
        if !constants.has_replaceable() {
            return None;
        }
        let alone = constants.flag(SURVIVES_ALONE)?;
        let mut support = [alone; 6];
        for (at, column) in SUPPORT.iter().enumerate() {
            support[at] = constants.flag(column)?;
        }
        Some(Self {
            constants,
            alone,
            support,
            falls: constants.flag(FALLS),
        })
    }

    /// Whether a change beside this state could destroy or drop it.
    ///
    /// The cheap half of the pair, and the one the world calls for six
    /// neighbours of every edit: two bit tests for the stone, dirt and wood
    /// that almost every neighbour of almost every edit actually is.
    #[must_use]
    pub fn reacts(&self, state: BlockState) -> bool {
        !self.survives_alone(state) || self.falls(state)
    }

    /// Whether this state needs nothing beside it.
    #[must_use]
    pub fn survives_alone(&self, state: BlockState) -> bool {
        self.constants.is_set(self.alone, state.id())
    }

    /// Whether this state falls when the cell below is free.
    #[must_use]
    pub fn falls(&self, state: BlockState) -> bool {
        self.falls
            .is_some_and(|flag| self.constants.is_set(flag, state.id()))
    }

    /// Whether a cell holding this state can be fallen into.
    ///
    /// Minecraft's `FallingBlock.isFree`, which is `canBeReplaced()` and
    /// nothing else: air, fire and the fluids. A falling block passes through
    /// all three and lands on everything else.
    #[must_use]
    pub fn free(&self, state: BlockState) -> bool {
        self.constants.replaceable(state.id())
    }

    /// Whether this state can still be where it is, given its six neighbours.
    #[must_use]
    pub fn survives(&self, state: BlockState, around: Around) -> bool {
        if self.survives_alone(state) {
            return true;
        }
        let mut named = false;
        for side in Face::ALL {
            if !self
                .constants
                .is_set(self.support[side as usize], state.id())
            {
                continue;
            }
            named = true;
            if !self.free(around.at(side)) {
                return true;
            }
        }
        // A state that needs something and names no side is one the probe
        // could not resolve. Kept, always. See the module note.
        !named
    }

    /// What the world should do about this cell.
    ///
    /// Support is asked before falling because they cannot both be true — no
    /// state in 1.21.1 both needs holding up and falls — and because the order
    /// would matter the day one did: a block that cannot be there should be
    /// dropped rather than launched.
    #[must_use]
    pub fn reaction(&self, state: BlockState, around: Around) -> Reaction {
        if !self.survives(state, around) {
            return Reaction::Break;
        }
        if self.falls(state) && self.free(around.at(Face::Down)) {
            return Reaction::Fall;
        }
        Reaction::Stay
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dust_registry::Block;

    /// A table with the seven columns, every state surviving alone and
    /// nothing falling, which every case below then edits.
    fn table(rows: &[(&str, &[&str])]) -> BlockConstants {
        let states = dust_registry::STATE_COUNT as usize;
        let mut header = String::from("# state_id\topacity\temission\tocclude\treplaceable");
        header.push_str("\tSURVIVES_ALONE");
        for column in SUPPORT {
            header.push('\t');
            header.push_str(column);
        }
        header.push_str("\tfalls\n");
        let mut set: Vec<[bool; 8]> =
            vec![[true, false, false, false, false, false, false, false]; states];
        let mut replaceable = vec![false; states];
        replaceable[Block::from_name("minecraft:air")
            .unwrap()
            .default_state()
            .id() as usize] = true;
        for (name, columns) in rows {
            let block = Block::from_name(name).expect("the test names a real block");
            for state in block.states() {
                for column in *columns {
                    let at = match *column {
                        "SURVIVES_ALONE" => 0,
                        "falls" => 7,
                        other => {
                            1 + SUPPORT
                                .iter()
                                .position(|c| *c == other)
                                .expect("the test names a real column")
                        }
                    };
                    // `SURVIVES_ALONE` in the list means it does *not*.
                    set[state.id() as usize][at] = at != 0;
                    if at == 0 {
                        set[state.id() as usize][0] = false;
                    }
                }
            }
        }
        let mut out = header;
        for (id, flags) in set.iter().enumerate() {
            out.push_str(&format!(
                "{id}\t0\t0\t0\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                u8::from(replaceable[id]),
                u8::from(flags[0]),
                u8::from(flags[1]),
                u8::from(flags[2]),
                u8::from(flags[3]),
                u8::from(flags[4]),
                u8::from(flags[5]),
                u8::from(flags[6]),
                u8::from(flags[7]),
            ));
        }
        BlockConstants::parse(&out).expect("the table this test wrote parses")
    }

    fn state(name: &str) -> BlockState {
        Block::from_name(name)
            .expect("the test names a real block")
            .default_state()
    }

    fn air() -> BlockState {
        state("minecraft:air")
    }

    fn around_with(side: Face, there: BlockState) -> Around {
        Around::empty().with(side, there)
    }

    #[test]
    fn a_block_that_needs_nothing_is_never_touched() {
        let table = table(&[]);
        let rules = Rules::from_constants(&table).expect("the columns are there");
        assert!(!rules.reacts(state("minecraft:stone")));
        assert_eq!(
            rules.reaction(state("minecraft:stone"), Around::empty()),
            Reaction::Stay
        );
    }

    #[test]
    fn a_torch_stands_on_a_block_and_falls_off_air() {
        let table = table(&[("minecraft:torch", &["SURVIVES_ALONE", "SUPPORT_DOWN"])]);
        let rules = Rules::from_constants(&table).expect("the columns are there");
        let torch = state("minecraft:torch");
        assert!(rules.reacts(torch));
        assert_eq!(
            rules.reaction(torch, around_with(Face::Down, state("minecraft:stone"))),
            Reaction::Stay
        );
        assert_eq!(rules.reaction(torch, Around::empty()), Reaction::Break);
        // The side it does not name is not a support, however solid it is.
        assert_eq!(
            rules.reaction(torch, around_with(Face::North, state("minecraft:stone"))),
            Reaction::Break
        );
    }

    #[test]
    fn two_named_sides_are_an_or_and_not_an_and() {
        let table = table(&[(
            "minecraft:vine",
            &["SURVIVES_ALONE", "SUPPORT_UP", "SUPPORT_NORTH"],
        )]);
        let rules = Rules::from_constants(&table).expect("the columns are there");
        let vine = state("minecraft:vine");
        assert_eq!(
            rules.reaction(vine, around_with(Face::Up, state("minecraft:stone"))),
            Reaction::Stay
        );
        assert_eq!(
            rules.reaction(vine, around_with(Face::North, state("minecraft:stone"))),
            Reaction::Stay
        );
        assert_eq!(rules.reaction(vine, Around::empty()), Reaction::Break);
    }

    #[test]
    fn a_state_that_names_no_side_is_kept_rather_than_deleted() {
        // What 379 of 1.21.1's states look like: they need something, and the
        // probe could not say what. Deleting them is the failure this asserts
        // against.
        let table = table(&[("minecraft:glow_lichen", &["SURVIVES_ALONE"])]);
        let rules = Rules::from_constants(&table).expect("the columns are there");
        let lichen = state("minecraft:glow_lichen");
        assert!(!rules.survives_alone(lichen));
        assert_eq!(rules.reaction(lichen, Around::empty()), Reaction::Stay);
    }

    #[test]
    fn sand_falls_into_air_and_rests_on_anything_else() {
        let table = table(&[("minecraft:sand", &["falls"])]);
        let rules = Rules::from_constants(&table).expect("the columns are there");
        let sand = state("minecraft:sand");
        assert!(rules.reacts(sand));
        assert_eq!(rules.reaction(sand, Around::empty()), Reaction::Fall);
        assert_eq!(
            rules.reaction(sand, around_with(Face::Down, state("minecraft:stone"))),
            Reaction::Stay
        );
    }

    #[test]
    fn a_table_with_no_falls_column_still_holds_torches_up() {
        // The `has_x` question asked of the halves separately: an operator
        // whose table predates the falling column gets support rules and no
        // falling, rather than neither.
        let mut header =
            String::from("# state_id\topacity\temission\tocclude\treplaceable\tSURVIVES_ALONE");
        for column in SUPPORT {
            header.push('\t');
            header.push_str(column);
        }
        header.push('\n');
        let mut out = header;
        for id in 0..dust_registry::STATE_COUNT as usize {
            out.push_str(&format!("{id}\t0\t0\t0\t0\t1\t0\t0\t0\t0\t0\t0\n"));
        }
        let table = BlockConstants::parse(&out).expect("the table this test wrote parses");
        let rules = Rules::from_constants(&table).expect("the support columns are there");
        assert!(!rules.falls(state("minecraft:sand")));
        assert_eq!(
            rules.reaction(state("minecraft:sand"), Around::empty()),
            Reaction::Stay
        );
    }

    #[test]
    fn air_under_a_falling_block_is_what_free_means() {
        let table = table(&[("minecraft:sand", &["falls"])]);
        let rules = Rules::from_constants(&table).expect("the columns are there");
        assert!(rules.free(air()));
        assert!(!rules.free(state("minecraft:stone")));
    }
}
