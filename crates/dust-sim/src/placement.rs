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
//! **Where the shape is not enough, the blocks are named and the naming is
//! small.** Three anvils turn the direction a quarter, and six blocks read the
//! player's look including the vertical where everything else of their shape
//! reads the clicked face. Both lists were read off the measurement, both are
//! spelled out below with what they do, and everything not in them falls to the
//! rule its shape implies. A name list is what a from-scratch server's
//! behaviour looks like — the same kind of thing as `PARTICLES_DESTROY_BLOCK`
//! being 2001 — and it is not a table of Mojang's data.
//!
//! # The direction a block faces is four different rules
//!
//! Every one of these is `facing` over the same four horizontal values, and the
//! property table cannot tell them apart:
//!
//! ```text
//!   a stair, a door, a fence gate    the way the player is looking
//!   a furnace, a chest, a trapdoor   back at the player
//!   an anvil                         a quarter turn clockwise from the look
//!   a lever on a wall                the face that was clicked
//! ```
//!
//! What separates them is the *rest* of the shape — a `shape`, a `hinge`, an
//! `in_wall`, a `face` — which is why the rules below read like a list of other
//! people's properties.

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
    if let Some(state) = as_attached(block, state, click) {
        return state;
    }
    if let Some(state) = as_bell(block, state, click) {
        return state;
    }
    if let Some(state) = as_horizontal(block, state, click) {
        return state;
    }
    if let Some(state) = as_directional(block, state, click) {
        return state;
    }
    if let Some(state) = as_hopper(block, state, click) {
        return state;
    }
    state
}

/// A lever, a button or a grindstone: a `face` of floor, wall or ceiling.
///
/// The face says which surface it is stuck to, and that decides where `facing`
/// comes from — the clicked face when it is on a wall, and the player's look
/// when it is on the floor or the ceiling, because a lever on the ground has no
/// wall to take a direction from.
fn as_attached(block: Block, state: BlockState, click: Click) -> Option<BlockState> {
    let attachment = values_of(block, "face")?;
    if !same_set(attachment, &["floor", "wall", "ceiling"]) {
        return None;
    }
    if !same_set(values_of(block, "facing")?, HORIZONTAL) {
        return None;
    }
    let (surface, facing) = match click.face {
        Face::Up => ("floor", looking(click.yaw)),
        Face::Down => ("ceiling", looking(click.yaw)),
        other => ("wall", other.direction()),
    };
    state
        .with("face", surface)
        .and_then(|state| state.with("facing", facing))
}

/// Everything else with a four-valued `facing`.
///
/// Three rules and a list of three blocks, and which one applies is read off
/// the rest of the shape:
///
/// * a `hinge` is a door and an `in_wall` is a fence gate, and both face **the
///   way the player is looking**, as a stair does;
/// * the anvils turn it **a quarter clockwise**, which nothing else does and
///   nothing in the property table hints at;
/// * everything else faces **back at the player** — a furnace, a chest, a
///   trapdoor, a repeater, every glazed terracotta.
///
/// A trapdoor also takes the half the click landed in, which is the slab's own
/// rule and is why it is applied here rather than given a rule of its own.
///
/// **A door goes down as its lower half and nothing else.** Minecraft puts two
/// blocks down and this puts one, which was already true before this rule and
/// is not made worse by it: the second block is a placement that writes
/// somewhere the click did not name, and nothing here can do that yet.
fn as_horizontal(block: Block, state: BlockState, click: Click) -> Option<BlockState> {
    if !same_set(values_of(block, "facing")?, HORIZONTAL) {
        return None;
    }
    let look = looking(click.yaw);
    // A trapdoor is a lever with a different answer: hung on a wall it takes
    // the face, and set into a floor or a ceiling it takes the player. What
    // says it is one is a `half` with no `shape` — a stair has both and a
    // furnace has neither.
    let trapdoor = matches!(values_of(block, "half"), Some(values) if same_set(values, &["top", "bottom"]))
        && values_of(block, "shape").is_none();
    let facing = if values_of(block, "hinge").is_some()
        || values_of(block, "in_wall").is_some()
        || values_of(block, "part").is_some()
    {
        look
    } else if ANVILS.contains(&block.name()) {
        clockwise(look)
    } else if trapdoor && !matches!(click.face, Face::Up | Face::Down) {
        click.face.direction()
    } else {
        opposite(look)
    };
    let state = state.with("facing", facing)?;
    if trapdoor {
        return state.with("half", half(click));
    }
    Some(state)
}

/// A bell: an `attachment` rather than a `face`, and its own answer again.
///
/// On the floor or the ceiling it faces the player, like a lever. On a wall it
/// faces **away** from the wall, unlike a lever, which faces into it. One block
/// and three sentences, which is what it costs to be right about it.
///
/// `double_wall` — a bell slung between two blocks — is a neighbour rule and is
/// not reachable from a click alone, so it is never chosen here.
fn as_bell(block: Block, state: BlockState, click: Click) -> Option<BlockState> {
    let attachment = values_of(block, "attachment")?;
    if !same_set(
        attachment,
        &["floor", "ceiling", "single_wall", "double_wall"],
    ) {
        return None;
    }
    let (hung, facing) = match click.face {
        Face::Up => ("floor", looking(click.yaw)),
        Face::Down => ("ceiling", looking(click.yaw)),
        other => ("single_wall", opposite(other.direction())),
    };
    state
        .with("attachment", hung)
        .and_then(|state| state.with("facing", facing))
}

/// A block whose `facing` can point any of the six ways.
///
/// Most of them take the clicked face: a shulker box opens away from what it is
/// stuck to, an end rod points out of it, so do a lightning rod and an amethyst
/// cluster. **Six do not**, and they are named because nothing in the property
/// table separates them — they read where the player is looking, the vertical
/// included, which is how a piston ends up pointing at the ceiling when you
/// place it looking up.
fn as_directional(block: Block, state: BlockState, click: Click) -> Option<BlockState> {
    let facing = values_of(block, "facing")?;
    if !same_set(facing, ALL_SIX) {
        return None;
    }
    let name = block.name();
    let direction = if LOOKS_AWAY.contains(&name) {
        opposite(nearest_looking(click))
    } else if LOOKS_AT.contains(&name) {
        nearest_looking(click)
    } else {
        click.face.direction()
    };
    state.with("facing", direction)
}

/// A hopper: a `facing` of five, every direction but up.
///
/// It points away from the face that was clicked so that it feeds the block it
/// was put against — and clicking the *top* of a block gives `down`, because
/// there is no `up` for it to take. The five-valued facing is the shape and
/// nothing else in the game has one.
fn as_hopper(block: Block, state: BlockState, click: Click) -> Option<BlockState> {
    let facing = values_of(block, "facing")?;
    if !same_set(facing, &["down", "north", "south", "west", "east"]) {
        return None;
    }
    let direction = match click.face {
        Face::Up | Face::Down => "down",
        other => opposite(other.direction()),
    };
    state.with("facing", direction)
}

/// The three anvils, which turn the player's direction a quarter clockwise.
///
/// Named because the property table cannot say it: an anvil is `facing` and
/// nothing else, which is the same shape as a carved pumpkin, and a pumpkin
/// faces back at the player.
const ANVILS: [&str; 3] = [
    "minecraft:anvil",
    "minecraft:chipped_anvil",
    "minecraft:damaged_anvil",
];

/// Six-way blocks that point **away** from where the player is looking.
///
/// A piston pushes away from you, a barrel opens away from you, a dispenser
/// fires away from you. Named because the shape they share — a six-valued
/// `facing` — is the same one a shulker box has, and a shulker box takes the
/// clicked face.
const LOOKS_AWAY: [&str; 5] = [
    "minecraft:piston",
    "minecraft:sticky_piston",
    "minecraft:barrel",
    "minecraft:dispenser",
    "minecraft:dropper",
];

/// Six-way blocks that point **at** where the player is looking.
///
/// One, and it is the reason this is a second list rather than a flag on the
/// first: an observer placed looking down faces down where a piston placed the
/// same way faces up.
const LOOKS_AT: [&str; 1] = ["minecraft:observer"];

/// Every direction a `facing` can take.
const ALL_SIX: &[&str] = &["north", "south", "west", "east", "up", "down"];

/// The direction the player is looking, the vertical included.
///
/// Minecraft's own `getNearestLookingDirection`: the axis with the largest
/// share of the look vector wins. Written as the vector rather than as an angle
/// threshold because the threshold is where the horizontal and vertical
/// components cross, and a number written for that is a number to be wrong
/// about.
fn nearest_looking(click: Click) -> &'static str {
    let yaw = click.yaw.to_radians();
    let pitch = click.pitch.to_radians();
    let (x, y, z) = (
        -yaw.sin() * pitch.cos(),
        -pitch.sin(),
        yaw.cos() * pitch.cos(),
    );
    if y.abs() >= x.abs() && y.abs() >= z.abs() {
        if y > 0.0 {
            "up"
        } else {
            "down"
        }
    } else if x.abs() >= z.abs() {
        if x > 0.0 {
            "east"
        } else {
            "west"
        }
    } else if z > 0.0 {
        "south"
    } else {
        "north"
    }
}

/// The direction facing the other way.
fn opposite(direction: &str) -> &'static str {
    match direction {
        "north" => "south",
        "south" => "north",
        "west" => "east",
        "east" => "west",
        "up" => "down",
        _ => "up",
    }
}

/// A quarter turn clockwise, seen from above. Only the anvils want it.
fn clockwise(direction: &str) -> &'static str {
    match direction {
        "north" => "east",
        "east" => "south",
        "south" => "west",
        _ => "north",
    }
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
        // The fallback that makes a rule an improvement rather than a trade.
        // Leaves and stone have nothing here to key on and go down exactly as
        // they did before this file existed.
        for name in ["minecraft:stone", "minecraft:oak_leaves", "minecraft:glass"] {
            let block = block(name);
            assert_eq!(
                state_for(block, click(Face::North, 90.0, 0.75)),
                block.default_state(),
                "{name}"
            );
        }
    }

    #[test]
    fn a_furnace_faces_back_at_the_player_where_a_stair_faces_away() {
        // The pair the whole measurement was taken to settle. Same property,
        // same four values, opposite answers — and nothing in the property
        // table says which is which.
        let stair = state("minecraft:oak_stairs", Face::Up, 0.0, 0.25);
        let furnace = state("minecraft:furnace", Face::Up, 0.0, 0.25);
        assert_eq!(value(&stair, "facing"), "south", "the way the player looks");
        assert_eq!(value(&furnace, "facing"), "north", "back at the player");
    }

    #[test]
    fn a_trapdoor_takes_the_wall_it_is_hung_on_and_the_player_otherwise() {
        // A lever with a different answer, and the case that was wrong first
        // time: hung on a wall it faces the way the wall does, and set into a
        // floor it faces back at the player like a furnace.
        let wall = state("minecraft:oak_trapdoor", Face::North, 0.0, 0.75);
        assert_eq!(value(&wall, "facing"), "north", "the face it hangs on");
        assert_eq!(
            value(&wall, "half"),
            "top",
            "and the half the cursor was in"
        );

        let floor = state("minecraft:oak_trapdoor", Face::Up, 0.0, 0.75);
        assert_eq!(
            value(&floor, "facing"),
            "north",
            "back at a player looking south"
        );
        assert_eq!(value(&floor, "half"), "bottom", "the face, not the cursor");
    }

    #[test]
    fn a_bed_faces_the_way_the_player_looks() {
        // A four-valued facing with a `part`, which is what says it is not a
        // furnace. It goes down as its foot and no head, for the same reason a
        // door goes down as its lower half: the second block is somewhere the
        // click did not name.
        let placed = state("minecraft:black_bed", Face::Up, 0.0, 0.25);
        assert_eq!(value(&placed, "facing"), "south");
        assert_eq!(value(&placed, "part"), "foot");
    }

    #[test]
    fn a_bell_hangs_the_other_way_round_from_a_lever() {
        // Both take a surface from the clicked face, and on a wall a lever
        // faces into it while a bell faces out of it.
        let wall = state("minecraft:bell", Face::North, 0.0, 0.25);
        assert_eq!(value(&wall, "attachment"), "single_wall");
        assert_eq!(value(&wall, "facing"), "south", "away from the wall");
        assert_eq!(
            value(&state("minecraft:lever", Face::North, 0.0, 0.25), "facing"),
            "north",
            "and a lever faces into it"
        );

        let floor = state("minecraft:bell", Face::Up, 0.0, 0.25);
        assert_eq!(value(&floor, "attachment"), "floor");
        assert_eq!(
            value(&floor, "facing"),
            "south",
            "the player, as a lever does"
        );
    }

    #[test]
    fn a_door_and_a_fence_gate_face_the_way_a_stair_does() {
        // Both have a four-valued facing and neither is a stair; what says so
        // is the `hinge` on one and the `in_wall` on the other.
        assert_eq!(
            value(&state("minecraft:oak_door", Face::Up, 0.0, 0.25), "facing"),
            "south"
        );
        assert_eq!(
            value(
                &state("minecraft:oak_fence_gate", Face::Up, 0.0, 0.25),
                "facing"
            ),
            "south"
        );
    }

    #[test]
    fn an_anvil_turns_a_quarter_and_a_pumpkin_does_not() {
        // The pair that makes the anvils a named list. Both are `facing` and
        // nothing else; the anvil turns clockwise from the look and the
        // pumpkin faces back at the player.
        assert_eq!(
            value(&state("minecraft:anvil", Face::Up, 0.0, 0.25), "facing"),
            "west",
            "a quarter clockwise from south"
        );
        assert_eq!(
            value(
                &state("minecraft:carved_pumpkin", Face::Up, 0.0, 0.25),
                "facing"
            ),
            "north",
            "back at a player looking south"
        );
    }

    #[test]
    fn a_lever_takes_the_wall_it_is_on_and_the_direction_that_follows() {
        let floor = state("minecraft:lever", Face::Up, 0.0, 0.25);
        assert_eq!(value(&floor, "face"), "floor");
        assert_eq!(value(&floor, "facing"), "south", "no wall, so the look");

        let wall = state("minecraft:lever", Face::West, 0.0, 0.25);
        assert_eq!(value(&wall, "face"), "wall");
        assert_eq!(value(&wall, "facing"), "west", "the face it is stuck to");

        assert_eq!(
            value(&state("minecraft:lever", Face::Down, 0.0, 0.25), "face"),
            "ceiling"
        );
    }

    #[test]
    fn a_piston_points_away_and_an_observer_points_at() {
        // The reason there are two lists. Placed looking straight down, a
        // piston faces up and an observer faces down.
        let down = Click {
            face: Face::Up,
            cursor_y: 0.25,
            yaw: 0.0,
            pitch: 90.0,
        };
        assert_eq!(
            value(
                &state_for(block("minecraft:piston"), down).properties(),
                "facing"
            ),
            "up"
        );
        assert_eq!(
            value(
                &state_for(block("minecraft:observer"), down).properties(),
                "facing"
            ),
            "down"
        );
    }

    #[test]
    fn a_six_way_block_nobody_named_takes_the_clicked_face() {
        // The majority of that shape: a shulker box opens away from what it is
        // stuck to, and so do an end rod and a lightning rod.
        for name in [
            "minecraft:shulker_box",
            "minecraft:end_rod",
            "minecraft:lightning_rod",
        ] {
            assert_eq!(
                value(&state(name, Face::West, 0.0, 0.25), "facing"),
                "west",
                "{name}"
            );
        }
    }

    #[test]
    fn a_hopper_points_away_from_the_face_and_never_up() {
        assert_eq!(
            value(&state("minecraft:hopper", Face::North, 0.0, 0.25), "facing"),
            "south"
        );
        assert_eq!(
            value(&state("minecraft:hopper", Face::Up, 0.0, 0.25), "facing"),
            "down",
            "there is no up for it to take"
        );
        assert_eq!(
            value(&state("minecraft:hopper", Face::Down, 0.0, 0.25), "facing"),
            "down"
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
