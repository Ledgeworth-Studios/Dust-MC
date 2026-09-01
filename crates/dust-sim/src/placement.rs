//! Which state a block takes when a player puts it down.
//!
//! # What this is, and what it is measured against
//!
//! Decision record 0011 is the account. A block used to go down in its
//! **default** state, which is the wrong state for 481 of the 925 blocks a
//! player can place: a stair faced north whichever way you stood, a log always
//! lay on its end, a slab was always the bottom half.
//!
//! Minecraft computes the state in `Block.getStateForPlacement`, in Java, and
//! that method needs a `Level` — so unlike the light constants and the sound
//! groups it cannot be asked of the jar by reflection. What it *can* be asked
//! of is a running server, one placement at a time, and
//! `cargo xtask harness placement` scores this file against the answers.
//!
//! **Every rule here was read off that measurement rather than written from
//! memory**, and the difference is not academic. A stair faces *where the
//! player looks*; a furnace faces *back at them*. Both are `facing` with the
//! same four values on blocks that look alike from the property table, and
//! either one written from memory has even odds.
//!
//! # Why the rules are keyed on the property shape
//!
//! A block that has an `axis` of `x`, `y`, `z` and nothing else takes its axis
//! from the face that was clicked — every log, every pillar, the chains. That
//! is a shape, not a list of names, so it needs no table and covers blocks this
//! version has never heard of.
//!
//! **Where the shape is not enough, nothing is guessed.** `minecraft:ladder`
//! and `minecraft:furnace` both have a four-valued `facing` and one other
//! property, and a ladder takes its facing from the clicked face while a
//! furnace takes it from the player. There is no way to tell them apart from
//! the table, so neither is handled here yet and both keep the default state
//! they had — which the score reports rather than hides. A rule that guessed
//! would be right about ninety blocks and wrong about a dozen, which is the
//! trap decision record 0008's item table already argues about at length.

use dust_registry::{Block, BlockState};

/// Which face of a block was clicked.
///
/// The protocol's own numbering, because that is what arrives and a second
/// numbering is a second thing to get wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Face {
    Down = 0,
    Up = 1,
    North = 2,
    South = 3,
    West = 4,
    East = 5,
}

impl Face {
    /// The face a protocol number names, or `None` for one this build does not
    /// know — which vanilla refuses outright rather than reading as a guess.
    #[must_use]
    pub fn from_protocol(face: u8) -> Option<Self> {
        Some(match face {
            0 => Self::Down,
            1 => Self::Up,
            2 => Self::North,
            3 => Self::South,
            4 => Self::West,
            5 => Self::East,
            _ => return None,
        })
    }

    /// The axis this face lies along, as a block's `axis` property spells it.
    #[must_use]
    pub fn axis(self) -> &'static str {
        match self {
            Self::Down | Self::Up => "y",
            Self::North | Self::South => "z",
            Self::West | Self::East => "x",
        }
    }

    /// The direction this face points, as a `facing` property spells it.
    #[must_use]
    pub fn direction(self) -> &'static str {
        match self {
            Self::Down => "down",
            Self::Up => "up",
            Self::North => "north",
            Self::South => "south",
            Self::West => "west",
            Self::East => "east",
        }
    }
}

/// Everything about a right-click that a placement can read.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Click {
    /// The face of the clicked block.
    pub face: Face,
    /// How far up that face the cursor was, `0.0..=1.0`.
    pub cursor_y: f32,
    /// The player's yaw in the protocol's degrees: 0 is south and it turns
    /// clockwise seen from above.
    pub yaw: f32,
    /// The player's pitch in the protocol's degrees: -90 is straight up.
    pub pitch: f32,
}

/// The state `block` takes for this click.
///
/// Falls back to the block's default state for every shape no rule here
/// recognises, which is what the server did for all of them before this
/// existed. That fallback is deliberate and is what makes each rule a strict
/// improvement: a shape nobody has written a rule for behaves exactly as it did
/// yesterday, and the score says how many are left.
#[must_use]
pub fn state_for(block: Block, click: Click) -> BlockState {
    let state = block.default_state();
    if let Some(state) = as_pillar(block, state, click) {
        return state;
    }
    if let Some(state) = as_slab(block, state, click) {
        return state;
    }
    if let Some(state) = as_stairs(block, state, click) {
        return state;
    }
    state
}

/// A log, a pillar or a chain: `axis` and nothing else that orients it.
///
/// The axis of the face that was clicked, so a log placed against a wall lies
/// down. Nothing else in the game has a bare `axis`, which is what makes this
/// safe to key on a shape.
fn as_pillar(block: Block, state: BlockState, click: Click) -> Option<BlockState> {
    if values_of(block, "facing").is_some() {
        return None;
    }
    let axis = values_of(block, "axis")?;
    if !same_set(axis, &["x", "y", "z"]) {
        return None;
    }
    state.with("axis", click.face.axis())
}

/// A slab: `type` of `top`, `bottom` or `double`.
///
/// Clicked from below it is the top half, from above the bottom half, and on a
/// side face it is whichever half the cursor was in.
///
/// **`double` is not reachable from here and that is correct.** A slab placed
/// into a cell that already holds its own other half makes a double slab, and
/// that is a rule about what is *already there* rather than about the click —
/// a different question, in a different place, and one this crate does not yet
/// get to ask.
fn as_slab(block: Block, state: BlockState, click: Click) -> Option<BlockState> {
    let kind = values_of(block, "type")?;
    if !same_set(kind, &["top", "bottom", "double"]) {
        return None;
    }
    state.with("type", half(click))
}

/// Stairs: a four-valued `facing`, a `half`, and a `shape`.
///
/// **`facing` is where the player is looking and not the opposite**, which is
/// the one thing about this rule that cannot be reasoned out: a furnace with
/// the same four values faces back at the player. Measured, not remembered.
///
/// `shape` is left at `straight`. It is computed from the stairs already beside
/// this one, which is a neighbour rule and not a placement rule; the arena the
/// measurement was taken in has no neighbours, so this file has nothing to say
/// about it and says nothing.
fn as_stairs(block: Block, state: BlockState, click: Click) -> Option<BlockState> {
    let facing = values_of(block, "facing")?;
    if !same_set(facing, HORIZONTAL) {
        return None;
    }
    values_of(block, "shape")?;
    let half_values = values_of(block, "half")?;
    if !same_set(half_values, &["top", "bottom"]) {
        return None;
    }
    state
        .with("facing", looking(click.yaw))
        .and_then(|state| state.with("half", half(click)))
}

/// The four horizontal directions, in no particular order: `same_set` compares
/// them as a set because the generated table's order is the block report's and
/// means nothing here.
const HORIZONTAL: &[&str] = &["north", "south", "west", "east"];

/// Which half of a block a click lands in.
///
/// The face decides it outright when the face is horizontal — clicking the
/// underside means the top half whichever way the cursor was — and the cursor
/// decides it on a side face.
fn half(click: Click) -> &'static str {
    match click.face {
        Face::Down => "top",
        Face::Up => "bottom",
        _ if click.cursor_y >= 0.5 => "top",
        _ => "bottom",
    }
}

/// The horizontal direction a player at `yaw` is looking.
///
/// Minecraft's own quarter-turn rounding: yaw 0 is south and it turns
/// clockwise, so the four quadrants are south, west, north, east. Written with
/// `rem_euclid` rather than a mask because a yaw is a float and may be negative
/// — a client that has turned left three times sends -270, and `-3 & 3` is 1.
fn looking(yaw: f32) -> &'static str {
    const QUADRANTS: [&str; 4] = ["south", "west", "north", "east"];
    let quadrant = (yaw / 90.0 + 0.5).floor().rem_euclid(4.0);
    QUADRANTS[quadrant as usize]
}

/// The values a block's property takes, if it has one by that name.
fn values_of(block: Block, property: &str) -> Option<&'static [&'static str]> {
    block
        .properties()
        .iter()
        .find(|def| def.name == property)
        .map(|def| def.values)
}

/// Whether two lists hold the same values, whatever order they are in.
fn same_set(a: &[&str], b: &[&str]) -> bool {
    a.len() == b.len() && b.iter().all(|value| a.contains(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(name: &str) -> Block {
        Block::from_name(name).expect("this build has that block")
    }

    fn click(face: Face, yaw: f32, cursor_y: f32) -> Click {
        Click {
            face,
            cursor_y,
            yaw,
            pitch: 0.0,
        }
    }

    fn state(name: &str, face: Face, yaw: f32, cursor_y: f32) -> Vec<(&'static str, &'static str)> {
        state_for(block(name), click(face, yaw, cursor_y)).properties()
    }

    fn value(properties: &[(&'static str, &'static str)], name: &str) -> &'static str {
        properties
            .iter()
            .find(|(property, _)| *property == name)
            .map(|(_, value)| *value)
            .expect("the block has that property")
    }

    // ---------------------------------------------------------------------
    // Every expectation below was read off `harness placement`'s answers,
    // which is why each one names the situation it came from rather than
    // asserting a shape.
    // ---------------------------------------------------------------------

    #[test]
    fn a_log_lies_along_the_face_it_was_clicked_on() {
        for (face, axis) in [
            (Face::Down, "y"),
            (Face::Up, "y"),
            (Face::North, "z"),
            (Face::South, "z"),
            (Face::West, "x"),
            (Face::East, "x"),
        ] {
            let placed = state("minecraft:oak_log", face, 0.0, 0.25);
            assert_eq!(value(&placed, "axis"), axis, "{face:?}");
        }
    }

    #[test]
    fn a_chain_is_a_pillar_too_and_keeps_its_other_properties() {
        // Same shape, one more property. The rule is keyed on the shape and
        // not on a list of names, so a block nobody thought about is covered.
        let placed = state("minecraft:chain", Face::West, 0.0, 0.25);
        assert_eq!(value(&placed, "axis"), "x");
        assert_eq!(value(&placed, "waterlogged"), "false");
    }

    #[test]
    fn a_slab_takes_the_half_the_click_landed_in() {
        assert_eq!(
            value(&state("minecraft:oak_slab", Face::Down, 0.0, 0.25), "type"),
            "top",
            "clicked from below"
        );
        assert_eq!(
            value(&state("minecraft:oak_slab", Face::Up, 0.0, 0.75), "type"),
            "bottom",
            "clicked from above, and the cursor does not overrule the face"
        );
        assert_eq!(
            value(&state("minecraft:oak_slab", Face::North, 0.0, 0.25), "type"),
            "bottom",
            "low on a side face"
        );
        assert_eq!(
            value(&state("minecraft:oak_slab", Face::North, 0.0, 0.75), "type"),
            "top",
            "high on a side face"
        );
    }

    #[test]
    fn a_stair_faces_where_the_player_is_looking() {
        // The measurement's whole point. A furnace with the same four values
        // faces back at the player, so this direction is not a thing that can
        // be reasoned out from the property table.
        for (yaw, facing) in [
            (0.0, "south"),
            (90.0, "west"),
            (180.0, "north"),
            (-90.0, "east"),
        ] {
            let placed = state("minecraft:oak_stairs", Face::Up, yaw, 0.25);
            assert_eq!(value(&placed, "facing"), facing, "yaw {yaw}");
        }
    }

    #[test]
    fn a_yaw_that_has_gone_round_more_than_once_still_points_somewhere() {
        // A client that has turned left three times sends -270, and the mask
        // this used to be written with reads that as the wrong quadrant.
        for (yaw, facing) in [
            (-270.0, "west"),
            (450.0, "west"),
            (359.0, "south"),
            (-1.0, "south"),
        ] {
            let placed = state("minecraft:oak_stairs", Face::Up, yaw, 0.25);
            assert_eq!(value(&placed, "facing"), facing, "yaw {yaw}");
        }
    }

    #[test]
    fn a_stair_takes_its_half_the_way_a_slab_does() {
        assert_eq!(
            value(
                &state("minecraft:oak_stairs", Face::Down, 0.0, 0.25),
                "half"
            ),
            "top"
        );
        assert_eq!(
            value(
                &state("minecraft:oak_stairs", Face::North, 0.0, 0.75),
                "half"
            ),
            "top"
        );
        assert_eq!(
            value(
                &state("minecraft:oak_stairs", Face::North, 0.0, 0.25),
                "half"
            ),
            "bottom"
        );
    }

    #[test]
    fn a_shape_no_rule_recognises_keeps_the_default_state() {
        // The fallback that makes every rule a strict improvement. A furnace
        // and a ladder are the two blocks this file deliberately does not
        // handle, because their shapes are the same and their rules are not.
        for name in ["minecraft:furnace", "minecraft:ladder", "minecraft:stone"] {
            let block = block(name);
            assert_eq!(
                state_for(block, click(Face::North, 90.0, 0.75)),
                block.default_state(),
                "{name}"
            );
        }
    }

    #[test]
    fn a_trapdoor_is_not_mistaken_for_a_stair() {
        // It has `facing` and `half` and no `shape`, and its rules are not the
        // stair's. Left alone rather than half-handled.
        let block = block("minecraft:oak_trapdoor");
        assert_eq!(
            state_for(block, click(Face::North, 90.0, 0.75)),
            block.default_state()
        );
    }

    #[test]
    fn a_face_the_protocol_does_not_have_is_refused_rather_than_guessed() {
        assert_eq!(Face::from_protocol(6), None);
        assert_eq!(Face::from_protocol(255), None);
        assert_eq!(Face::from_protocol(0), Some(Face::Down));
        assert_eq!(Face::from_protocol(5), Some(Face::East));
    }
}
