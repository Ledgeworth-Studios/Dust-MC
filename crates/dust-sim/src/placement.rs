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

use dust_registry::constants::Flag;
use dust_registry::tags::TagRegistry;
use dust_registry::{Block, BlockConstants, BlockState, WallForm};

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

    /// The face a `facing` property's value names.
    ///
    /// The other side of [`Face::direction`], and here because a neighbour's
    /// direction arrives as the string a block state carries: which way a
    /// stair beside this one faces, which way a fence gate is turned.
    #[must_use]
    pub fn from_direction(direction: &str) -> Option<Self> {
        Some(match direction {
            "down" => Self::Down,
            "up" => Self::Up,
            "north" => Self::North,
            "south" => Self::South,
            "west" => Self::West,
            "east" => Self::East,
            _ => return None,
        })
    }

    /// The face on the other side.
    #[must_use]
    pub fn opposite(self) -> Self {
        match self {
            Self::Down => Self::Up,
            Self::Up => Self::Down,
            Self::North => Self::South,
            Self::South => Self::North,
            Self::West => Self::East,
            Self::East => Self::West,
        }
    }

    /// The four sides a connection rule looks along.
    pub const HORIZONTAL: [Self; 4] = [Self::North, Self::South, Self::West, Self::East];

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
    /// What is already in the cell the block is going into.
    ///
    /// The one part of a right-click that is not a number off the wire, and
    /// three rules read it: a block goes down **waterlogged** in water, a
    /// second layer of snow **stacks** on the first, and a slab put into its
    /// own other half becomes a **double** slab. All three are the same
    /// question — *what is there already* — and Minecraft asks it of
    /// `BlockPlaceContext.getClickedPos()`, which is this cell and not the one
    /// the click named.
    ///
    /// `minecraft:air` for a caller with nothing to look up with, which is
    /// what every cell a placement lands in used to be assumed to hold.
    pub into: BlockState,
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
    // Two rules answer with the state that is *already there* rather than with
    // a fresh one, so they come first and they return outright: a second snow
    // layer is the first one with a bigger number, and a slab put into its own
    // other half is that slab turned double. Neither has an orientation to
    // compute, and running the click rules over them would compute one.
    if let Some(state) = as_stacked(block, click) {
        return state;
    }
    if let Some(state) = as_doubled(block, click) {
        return state;
    }
    // Water is the last word and not the first: it applies to whatever the
    // click rules settled on, and every one of them would otherwise carry a
    // `waterlogged` clause of its own.
    watered(oriented(block, click), click)
}

/// The state an item with **two** blocks puts down.
///
/// A sign, a torch, a banner and a head each have a standing form and a wall
/// form, and which one goes down is decided by the face that was clicked. The
/// item carries both blocks — `StandingAndWallBlockItem` in Java, two more
/// columns of `dust-items.tsv` here — because nothing else relates them: a
/// torch and a wall torch share no property and no name a rule could derive.
///
/// **The attachment direction is data and not an assumption.** A sign stands
/// on the ground and attaches `down`, so clicking the *top* of a block puts a
/// standing sign above it. A **hanging** sign attaches `up`, so the same click
/// puts nothing at all and clicking the *underside* is what hangs one.
///
/// Three of the six faces are settled here and the other three are not, which
/// is what the survey could say:
///
/// * the face **opposite** the attachment puts down the standing form;
/// * the four **horizontal** faces put down the wall form, facing out of the
///   wall — measured, and it is the clicked face rather than the player's look;
/// * the attachment's own face puts down neither and vanilla refuses. This
///   keeps the standing form there, which is what it did before and is right
///   for a skull, whose standing form needs nothing to hold it up.
///
/// **A hanging sign keeps its old answer on a wall**, deliberately. Its wall
/// form faces *across* the wall rather than out of it — a north face gives
/// `west` and an east face gives `south`, which is not a function of the
/// clicked face — and the grid was taken at one yaw, so it cannot say which of
/// the two perpendicular directions wins. A wall block facing the wrong way is
/// wrong in a way a player sees; the standing form is wrong in the way it
/// already was.
#[must_use]
pub fn state_for_item(standing: Block, wall: Option<WallForm>, click: Click) -> BlockState {
    let Some(form) = wall else {
        return state_for(standing, click);
    };
    let Some(attaches) = Face::from_direction(form.attaches) else {
        return state_for(standing, click);
    };
    if click.face == attaches.opposite() || form.attaches != "down" {
        return state_for(standing, click);
    }
    let Some(facing) = on_wall(form.block, click) else {
        return state_for(standing, click);
    };
    watered(facing, click)
}

/// The wall form's own state: it faces out of the wall it is on.
///
/// `None` for a block with no horizontal `facing`, which no wall form in this
/// version is — asked rather than assumed, because the answer decides whether
/// the wall form is used at all.
fn on_wall(block: Block, click: Click) -> Option<BlockState> {
    let values = values_of(block, "facing")?;
    if !same_set(values, HORIZONTAL) {
        return None;
    }
    block.default_state().with("facing", click.face.direction())
}

/// The state the click's four numbers alone imply.
fn oriented(block: Block, click: Click) -> BlockState {
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
    if let Some(state) = as_rail(block, state, click) {
        return state;
    }
    if let Some(state) = as_leaves(block, state) {
        return state;
    }
    if let Some(state) = as_turned(block, state, click) {
        return state;
    }
    if let Some(state) = as_hung(block, state, click) {
        return state;
    }
    if let Some(state) = as_multiface(block, state, click) {
        return state;
    }
    state
}

/// A block put down in water goes down **wet**.
///
/// Minecraft asks the cell's own `getFluidState` and sets `waterlogged` from
/// it, so this runs on every block that has the property and answers `false`
/// for the ones that are not in water — which is not the no-op it sounds like.
/// A conduit, a sea pickle and all twenty coral fans have `waterlogged=true`
/// in their **default** state, so a server that never touched the property put
/// every one of them down flooded on dry land: 122 of the 496 rows the grid
/// survey called wrong were exactly that, and none of them involved water at
/// all.
///
/// The clause left out is the one Minecraft spells `isSourceOfType`: pointed
/// dripstone waterlogs on a source and not on flowing water, where everything
/// else waterlogs on either. Flowing water is not a cell a placement usually
/// lands in — it is one block wide and gone the next tick — and the survey
/// this rule was read off could not sustain one to measure it.
fn watered(state: BlockState, click: Click) -> BlockState {
    if state.property("waterlogged").is_none() {
        return state;
    }
    let wet = if holds_water(click.into) {
        "true"
    } else {
        "false"
    };
    state.with("waterlogged", wet).unwrap_or(state)
}

/// Whether a cell holding this state holds **water**.
///
/// Minecraft's `BlockState.getFluidState`, which is not the same question as
/// "is this block water": kelp, seagrass and a bubble column are blocks that
/// stand *in* water and report it, so a fence placed into seagrass comes out
/// waterlogged. Measured against every block this build knows rather than
/// guessed — decision record 0015 has the run and the count — which is why
/// this is a list and not `state.block().name() == "minecraft:water"`.
fn holds_water(state: BlockState) -> bool {
    matches!(
        state.block().name(),
        "minecraft:water"
            | "minecraft:bubble_column"
            | "minecraft:kelp"
            | "minecraft:kelp_plant"
            | "minecraft:seagrass"
            | "minecraft:tall_seagrass"
    )
}

/// Snow on snow: one more layer, up to eight.
///
/// The one rule in this file whose answer is the state that was *already
/// there*. Minecraft returns the existing block with `layers` raised by one
/// rather than a fresh block, which matters for nothing today and would matter
/// the moment snow carried a second property.
///
/// Keyed on a `layers` property running one to eight, which is snow and
/// nothing else. Whether the placement is allowed to land there at all is
/// [`replaces`]'s question, not this one — eight layers refuse a ninth.
fn as_stacked(block: Block, click: Click) -> Option<BlockState> {
    if click.into.block() != block {
        return None;
    }
    let values = values_of(block, "layers")?;
    if !same_set(values, LAYERS) {
        return None;
    }
    let deep: u8 = click.into.property("layers")?.parse().ok()?;
    click.into.with("layers", LAYERS[deep.min(7) as usize])
}

/// The eight depths of snow, in order, so an index is a depth.
const LAYERS: &[&str] = &["1", "2", "3", "4", "5", "6", "7", "8"];

/// A slab into its own other half: a double slab.
///
/// `type=double` and `waterlogged=false` together, because Minecraft says both
/// in the same line: a double slab fills the cell, so there is nowhere left
/// for the water to be, and a slab doubled in the sea leaves no water behind.
fn as_doubled(block: Block, click: Click) -> Option<BlockState> {
    if click.into.block() != block {
        return None;
    }
    let values = values_of(block, "type")?;
    if !same_set(values, &["top", "bottom", "double"]) {
        return None;
    }
    if click.into.property("type") == Some("double") {
        return None;
    }
    let doubled = click.into.with("type", "double")?;
    Some(match doubled.with("waterlogged", "false") {
        Some(dry) => dry,
        None => doubled,
    })
}

/// A lantern: it hangs when the ceiling was the face that was clicked.
///
/// A `hanging` bool and nothing else that orients it, which is the two
/// lanterns and nothing else in the game.
fn as_hung(block: Block, state: BlockState, click: Click) -> Option<BlockState> {
    let values = values_of(block, "hanging")?;
    if !same_set(values, BOOL) {
        return None;
    }
    state.with(
        "hanging",
        if click.face == Face::Down {
            "true"
        } else {
            "false"
        },
    )
}

/// Glow lichen, sculk vein and vine: they stick to the face they were put on.
///
/// Six bools and a `waterlogged` is a multiface block; five bools with no
/// `down` and no `waterlogged` is a vine. **The `waterlogged` is what keeps
/// the mushroom blocks out**, which also have six bools and whose every side
/// is `true` by default — a rule that set one side and cleared the rest would
/// turn a mushroom block inside out, and the grid says it is right today.
///
/// The side that is set is the one *opposite* the clicked face: clicking the
/// top of a block puts the lichen above it, hanging from its own `down`.
fn as_multiface(block: Block, state: BlockState, click: Click) -> Option<BlockState> {
    let mut sides = 0;
    let mut wet = false;
    let mut has_down = false;
    for property in block.properties() {
        match property.name {
            "north" | "south" | "west" | "east" | "up" if same_set(property.values, BOOL) => {
                sides += 1;
            }
            "down" if same_set(property.values, BOOL) => {
                sides += 1;
                has_down = true;
            }
            "waterlogged" => wet = true,
            _ => return None,
        }
    }
    let multiface = sides == 6 && wet;
    let vine = sides == 5 && !has_down && !wet;
    if !multiface && !vine {
        return None;
    }
    state.with(click.face.opposite().direction(), "true")
}

/// A sign, a banner or a head standing on the ground: sixteen ways round.
///
/// One property and **two** rules, which is the thing worth writing down. A
/// sign and a banner face *the player*, so their segment is taken of the yaw
/// turned half round; a head faces *the same way as the player*, so its
/// segment is taken of the yaw itself. Both were read off the grid: at yaw 180
/// a sign is `rotation=0` and a skull is `rotation=8`, and no single rule gives
/// both.
///
/// Which of the two a block gets is `powered` — a head has it and a sign does
/// not — and that is a shape rather than a list of seven skulls.
///
/// The arithmetic is Minecraft's `RotationSegment.convertToSegment`: sixteen
/// segments of 22.5 degrees, rounded to the nearest and wrapped. Written with
/// `rem_euclid` for the same reason `looking` is: a yaw off the wire may be
/// negative, and a mask on a negative number is not the segment.
fn as_turned(block: Block, state: BlockState, click: Click) -> Option<BlockState> {
    let values = values_of(block, "rotation")?;
    if values.len() != 16 {
        return None;
    }
    let facing_away = values_of(block, "powered").is_some();
    let degrees = if facing_away {
        click.yaw
    } else {
        click.yaw + 180.0
    };
    let segment = (f64::from(degrees) * 16.0 / 360.0 + 0.5)
        .floor()
        .rem_euclid(16.0);
    state.with("rotation", values[segment as usize])
}

/// Leaves a **player** put down do not decay.
///
/// The only rule here that reads neither the click nor the cell: a leaf block
/// is `persistent` because a person placed it, and that is the whole of it.
/// Minecraft then recomputes `distance` from the logs nearby, which in an
/// arena with no logs is the 7 the default already carries — so the distance
/// clause is not written here and the survey cannot say whether it should be.
///
/// Keyed on `distance` and `persistent` together, which is leaves and nothing
/// else: scaffolding has a `distance` and no `persistent`.
///
/// Seventy-nine of the 496 rows the grid survey called wrong were the ten leaf
/// blocks, and every one of them was this.
fn as_leaves(block: Block, state: BlockState) -> Option<BlockState> {
    values_of(block, "distance")?;
    let values = values_of(block, "persistent")?;
    if !same_set(values, BOOL) {
        return None;
    }
    state.with("persistent", "true")
}

/// Whether a right-click **on** this cell puts the block *into* it.
///
/// `plain` is the constants table's own `canBeReplaced()`, which is a property
/// of the state and knows nothing about what the player is carrying. Minecraft
/// asks a version that does, and two blocks answer differently for it:
///
/// * **snow** stacks under its own item, so long as the top face was the one
///   clicked and there are fewer than eight layers. Otherwise a single layer
///   is replaceable by anything and a deeper drift is replaceable by nothing —
///   which is why a ninth layer is refused rather than flattening the first
///   eight.
/// * a **slab** takes its own other half. Clicked from above a bottom slab
///   doubles and a top slab does not, and on a side face it is the cursor that
///   decides — the same half rule the click already uses, asked of the block
///   that is there rather than of the one going down.
///
/// Everything else falls to `plain`, so a table without the column and a
/// server without these two blocks behave exactly as they did.
#[must_use]
pub fn replaces_clicked(block: Block, there: BlockState, plain: bool, click: Click) -> bool {
    if let Some(deep) = snow_depth(block, there) {
        if deep < 8 {
            return click.face == Face::Up;
        }
        return false;
    }
    if let Some(kind) = slab_half(block, there) {
        return match kind {
            "bottom" => click.face == Face::Up || (high(click) && click.face.axis() != "y"),
            "top" => click.face == Face::Down || (!high(click) && click.face.axis() != "y"),
            _ => false,
        };
    }
    plain
}

/// Whether a placement that has already gone past the clicked cell may land
/// here.
///
/// The same two blocks and the easier half of the question: nothing about the
/// click decides it, because the player did not aim at this cell — they aimed
/// at the one in front of it. Snow stacks and a slab doubles whichever way the
/// click came in.
#[must_use]
pub fn replaces_beside(block: Block, there: BlockState, plain: bool) -> bool {
    if let Some(deep) = snow_depth(block, there) {
        return deep < 8;
    }
    if let Some(kind) = slab_half(block, there) {
        return kind != "double";
    }
    plain
}

/// How deep the snow in this cell is, when it is the same snow the player is
/// holding.
fn snow_depth(block: Block, there: BlockState) -> Option<u8> {
    if there.block() != block {
        return None;
    }
    let values = values_of(block, "layers")?;
    if !same_set(values, LAYERS) {
        return None;
    }
    there.property("layers")?.parse().ok()
}

/// Which half of a slab is in this cell, when it is the same slab the player
/// is holding.
fn slab_half(block: Block, there: BlockState) -> Option<&'static str> {
    if there.block() != block {
        return None;
    }
    let values = values_of(block, "type")?;
    if !same_set(values, &["top", "bottom", "double"]) {
        return None;
    }
    there.property("type")
}

/// Whether the cursor was in the upper half of the face it landed on.
fn high(click: Click) -> bool {
    click.cursor_y > 0.5
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
    } else if AS_LOOKED.contains(&block.name()) {
        look
    } else if ON_THE_WALL.contains(&block.name()) {
        click.face.direction()
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

/// Four-way blocks that face **the way the player is looking**, like a stair.
///
/// Named because nothing separates them from a furnace. A campfire has `lit`
/// and so does a furnace; a decorated pot has `facing` and `waterlogged` and
/// so does a chest. The property table cannot say which of the two answers a
/// block gives, and the grid can: at yaw 180 all four of these come out facing
/// north and a furnace comes out facing south.
const AS_LOOKED: [&str; 4] = [
    "minecraft:campfire",
    "minecraft:soul_campfire",
    "minecraft:decorated_pot",
    "minecraft:calibrated_sculk_sensor",
];

/// Four-way blocks that face **out of the wall they were put on**.
///
/// A ladder and a tripwire hook hang on the block behind them, so their facing
/// is the clicked face and not the player at all — which is a lever's rule
/// without a lever's `face` property to say so. Two names, because a ladder's
/// shape is `facing` and `waterlogged`, which is a decorated pot's shape and a
/// chest's.
///
/// **Getting this wrong turns a ladder inside out**: it hangs off the wrong
/// side of the cell, against nothing, and a player cannot climb it.
const ON_THE_WALL: [&str; 2] = ["minecraft:ladder", "minecraft:tripwire_hook"];

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
    if !is_stairs(block) {
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

/// A rail, which lies along the axis the player is walking.
///
/// The click rule and not the neighbour one: a rail goes down `east_west` when
/// the player is looking east or west and `north_south` otherwise, and only
/// *then* does it bend towards the rails beside it. The survey caught this
/// because it is in the grid at all — one situation of the eight, the one at
/// yaw 90 — and it had been counted among the neighbour rules on the strength
/// of the property's name.
///
/// Keyed on the shape's values rather than on four block names, so it covers
/// the plain rail with its ten shapes and the three powered kinds with their
/// six. Nothing else in the game has a property naming an ascending direction.
///
/// **What this does not do is bend.** A rail beside another rail turns towards
/// it, rises to one a block higher, and rewrites that rail in turn — a rule
/// that reaches further than one ring, and the one thing decision record 0014
/// measured and left. A rail here lies along the player's axis and stays there.
fn as_rail(block: Block, state: BlockState, click: Click) -> Option<BlockState> {
    let shapes = values_of(block, "shape")?;
    if !shapes.contains(&"ascending_east") || !shapes.contains(&"north_south") {
        return None;
    }
    let along = looking(click.yaw);
    state.with(
        "shape",
        if along == "east" || along == "west" {
            "east_west"
        } else {
            "north_south"
        },
    )
}

/// Which half of a block a click lands in.
///
/// The face decides it outright when the face is horizontal — clicking the
/// underside means the top half whichever way the cursor was — and the cursor
/// decides it on a side face.
fn half(click: Click) -> &'static str {
    match click.face {
        Face::Down => "top",
        Face::Up => "bottom",
        _ if high(click) => "top",
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

// ---------------------------------------------------------------------------
// What is beside a block, and what that makes of it.
//
// Everything above this line reads the click. Nothing above it can answer why a
// fence has an arm on one side and not the other, because that is not in the
// click at all — it is in the cell next door. Decision record 0014 is the
// account of what was measured and what was decided; the short version is that
// this is **one rule applied twice**, at placement and again whenever a
// neighbour changes, because a fence that connected only in the direction it
// was built is worse to look at than one that never connects.
// ---------------------------------------------------------------------------

/// What is in the six cells around one.
///
/// Indexed by [`Face`], which is the same six directions the click already
/// names and so is one vocabulary rather than two. A fixed array and not a map:
/// this is built for every neighbour of every block a player places or breaks,
/// and six words on the stack is the whole of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Around([BlockState; 6]);

impl Around {
    /// The six states, in [`Face`] order: down, up, north, south, west, east.
    #[must_use]
    pub fn new(states: [BlockState; 6]) -> Self {
        Self(states)
    }

    /// Nothing at all around the cell.
    ///
    /// What a caller with no world to look at says, and what every rule below
    /// reads as "no connections" rather than as "unknown".
    #[must_use]
    pub fn empty() -> Self {
        let air = AIR.default_state();
        Self([air; 6])
    }

    /// The state on one side.
    #[must_use]
    pub fn at(self, side: Face) -> BlockState {
        self.0[side as usize]
    }

    /// The same neighbourhood with one side replaced.
    #[must_use]
    pub fn with(mut self, side: Face, state: BlockState) -> Self {
        self.0[side as usize] = state;
        self
    }
}

/// Whether a block state presents a full square face, side by side.
///
/// This is the one thing every connection rule needs and the block table cannot
/// say. Minecraft answers it from the block's collision shape, which is Java
/// and reaches this project the way every other such value does — off the
/// operator's own jar, in the constants table, under decision record 0008's
/// rule. What is here is the reader and the question.
///
/// **Six answers and not one, because a block can have a full face on one side
/// and not another.** The commonest such block is a stair: the back of a bottom
/// stair is a full square and the front is not, so a fence joins a stair from
/// behind and refuses it from in front. A single "is this a full cube" would
/// say no to both.
///
/// **Constructed only from a table that has the columns**, which is the `has_x`
/// question rather than the "what does it answer when it does not know" one. A
/// caller holding no `Solid` runs no connection rule at all, and that is
/// deliberate: a fence that joins fences and not the ground is half-connected,
/// and half-connected looks worse than never connected.
#[derive(Debug, Clone, Copy)]
pub struct Solid<'a> {
    constants: &'a BlockConstants,
    /// In [`Face`] order, so the lookup is an index and not a match.
    faces: [Flag; 6],
}

impl<'a> Solid<'a> {
    /// The predicate, if this table can answer it.
    #[must_use]
    pub fn from_constants(constants: &'a BlockConstants) -> Option<Self> {
        let mut faces = [constants.flag(STURDY[0])?; 6];
        for (at, column) in STURDY.iter().enumerate() {
            faces[at] = constants.flag(column)?;
        }
        Some(Self { constants, faces })
    }

    /// Whether `state` has a full square face on its `side`.
    #[must_use]
    pub fn sturdy(self, state: BlockState, side: Face) -> bool {
        self.constants.is_set(self.faces[side as usize], state.id())
    }
}

/// The constants columns that say which of a state's faces are full, in
/// [`Face`] order.
///
/// Named here rather than at the call site because the reader and the oracle
/// that writes them have to agree on the strings, and one place to change is
/// one place to be wrong.
pub const STURDY: [&str; 6] = [
    "STURDY_DOWN",
    "STURDY_UP",
    "STURDY_NORTH",
    "STURDY_SOUTH",
    "STURDY_WEST",
    "STURDY_EAST",
];

/// The state a block settles into given what is around it.
///
/// Applied to a block going down *and* to every neighbour of a block that just
/// changed, which is why it takes a whole state rather than a `Block` and a
/// [`Click`]: at the second call site there was no click. It is idempotent —
/// running it on its own answer changes nothing — which is what lets the world
/// call it on any write without tracking whether it already has.
///
/// A block with no shape rule comes back unchanged, so this is safe to call on
/// everything and costs one property-shape test for the ones it does not know.
#[must_use]
pub fn shaped(state: BlockState, around: Around, solid: Solid) -> BlockState {
    if let Some(state) = as_cross(state, around, solid) {
        return state;
    }
    if let Some(state) = as_wall(state, around, solid) {
        return state;
    }
    if let Some(state) = as_stairs_shape(state, around) {
        return state;
    }
    state
}

/// Whether a change beside this state could change it.
///
/// The world asks this of six neighbours on every placement and every break, so
/// it is the cheap half of the pair: a scan of at most a handful of property
/// names, and `false` for the stone, dirt and wood that almost every neighbour
/// of almost every edit actually is. [`shaped`] would answer the same question
/// by doing the work; this answers it without.
#[must_use]
pub fn reads_neighbours(state: BlockState) -> bool {
    let block = state.block();
    is_cross(block) || is_wall(block) || is_stairs(block)
}

/// A fence, a glass pane or iron bars: four bool connections and nothing else
/// that orients it.
///
/// The two families share a shape and differ in what they attach to, and which
/// one a block is comes from `#minecraft:fences` — a tag, which is data
/// Minecraft publishes, rather than a list written here.
///
/// * a **fence** joins its own kind, a fence gate turned across it, and any
///   full-faced block that is not one of the exceptions;
/// * a **pane** joins any pane or iron bars, any wall, and the same full-faced
///   blocks — but *not* a fence gate, which is the one clause that separates
///   the two rules and is not guessable from either shape.
fn as_cross(state: BlockState, around: Around, solid: Solid) -> Option<BlockState> {
    let block = state.block();
    if !is_cross(block) {
        return None;
    }
    let fence = in_tag(block, "minecraft:fences");
    let wooden = in_tag(block, "minecraft:wooden_fences");
    let mut next = state;
    for side in Face::HORIZONTAL {
        let other = around.at(side);
        let joins = if fence {
            same_fence(other, wooden)
                || gate_across(other, side)
                || (!exception(other) && solid.sturdy(other, side.opposite()))
        } else {
            is_cross(other.block()) && !in_tag(other.block(), "minecraft:fences")
                || in_tag(other.block(), "minecraft:walls")
                || (!exception(other) && solid.sturdy(other, side.opposite()))
        };
        next = next.with(side.direction(), if joins { "true" } else { "false" })?;
    }
    Some(next)
}

/// A wall: four connections of `none`, `low` or `tall`, and a post.
///
/// A wall joins more than a fence does — it joins panes and iron bars as well
/// as its own kind — and it has two answers a fence does not have.
///
/// **How high a connection is, is not about the neighbour.** It is about what
/// is *above the wall itself*: a full block overhead makes every connection
/// `tall`, and nothing overhead makes them all `low`. That was measured and it
/// is worth saying because the property sits on the connection and reads like
/// it belongs to it.
///
/// **The post goes up unless the wall runs through**, and "runs through" is a
/// line in *either* axis and not an exclusive one. A wall alone has a post; a
/// wall in the middle of a north-south line does not, and a wall connected on
/// all four sides does not either — which is the clause the phrase "a straight
/// run" gets wrong, and it was measured rather than reasoned. A long wall
/// having no post is what makes it look like a wall and not a row of pillars,
/// and a crossroads is the place a spare post is most visible.
///
/// A block on top of a line through does **not** raise the post back up: the
/// connections it makes `tall` reach the top of the wall already, so there is
/// nothing for a post to add. What does raise it is a wall above with its own
/// post, and the odd list in `#minecraft:wall_post_override` — a torch, a
/// button, a sign, a lantern: small things a player puts on top of a wall that
/// need a post under them to stand on.
fn as_wall(state: BlockState, around: Around, solid: Solid) -> Option<BlockState> {
    if !is_wall(state.block()) {
        return None;
    }
    let above = around.at(Face::Up);
    let tall = solid.sturdy(above, Face::Down);
    let mut next = state;
    let mut joined = [false; 4];
    for (at, side) in Face::HORIZONTAL.into_iter().enumerate() {
        let other = around.at(side);
        let joins = in_tag(other.block(), "minecraft:walls")
            || is_cross(other.block()) && !in_tag(other.block(), "minecraft:fences")
            || gate_across(other, side)
            || (!exception(other) && solid.sturdy(other, side.opposite()));
        joined[at] = joins;
        let height = if !joins {
            "none"
        } else if tall {
            "tall"
        } else {
            "low"
        };
        next = next.with(side.direction(), height)?;
    }
    // `Face::HORIZONTAL` is north, south, west, east, so a line through is one
    // of the two pairs. Written out rather than indexed by direction because
    // the whole question is which pair, and there are only two of them.
    let [north, south, west, east] = joined;
    let through = (north && south) || (west && east);
    let post = in_tag(above.block(), "minecraft:walls") && above.property("up") == Some("true")
        || !through
        || (!tall && in_tag(above.block(), "minecraft:wall_post_override"));
    next.with("up", if post { "true" } else { "false" })
}

/// A stair's `shape`: whether it is straight, or turns a corner.
///
/// The whole rule reads two cells and neither of them is the one that was
/// clicked, which is why nothing above this line could compute it.
///
/// A stair takes an **outer** corner from the stair it faces and an **inner**
/// corner from the stair behind it, in both cases only when that stair runs
/// across it rather than along it and is in the same half. Left and right are
/// from the stair's own facing: a stair facing north whose neighbour faces west
/// turns left, and west is what Minecraft calls north's counter-clockwise.
///
/// **The half has to match**, and that is the clause a rule written from the
/// facing alone would miss: a bottom stair beside a top stair facing across it
/// stays straight, and the survey asks that question on purpose.
fn as_stairs_shape(state: BlockState, around: Around) -> Option<BlockState> {
    let block = state.block();
    if !is_stairs(block) {
        return None;
    }
    let facing = Face::from_direction(state.property("facing")?)?;
    let half = state.property("half")?;
    // In front first, and the order matters: a stair with a corner available
    // both ways takes the outer one. Measured, from a scene that offers both.
    if let Some(ahead) = stair_across(around.at(facing), facing, half) {
        if free_of_stairs(around.at(ahead.opposite()), facing, half) {
            return state.with(
                "shape",
                if ahead == counter_clockwise(facing) {
                    "outer_left"
                } else {
                    "outer_right"
                },
            );
        }
    }
    if let Some(behind) = stair_across(around.at(facing.opposite()), facing, half) {
        if free_of_stairs(around.at(behind), facing, half) {
            return state.with(
                "shape",
                if behind == counter_clockwise(facing) {
                    "inner_left"
                } else {
                    "inner_right"
                },
            );
        }
    }
    state.with("shape", "straight")
}

/// The way a neighbouring stair faces, if it is a stair in the same half and
/// running across `facing` rather than along it.
fn stair_across(other: BlockState, facing: Face, half: &str) -> Option<Face> {
    if !is_stairs(other.block()) || other.property("half") != Some(half) {
        return None;
    }
    let theirs = Face::from_direction(other.property("facing")?)?;
    (theirs.axis() != facing.axis()).then_some(theirs)
}

/// Whether the cell on the far side has nothing in it that would rather this
/// stair stayed straight.
///
/// Minecraft's `canTakeShape`: a stair does not turn a corner away from a stair
/// that is already lined up with it, because the two would leave a step-shaped
/// hole between them.
fn free_of_stairs(other: BlockState, facing: Face, half: &str) -> bool {
    !is_stairs(other.block())
        || other.property("facing") != Some(facing.direction())
        || other.property("half") != Some(half)
}

/// Whether a neighbour is a fence of this fence's own kind.
///
/// Minecraft's own rule and it is a strange one worth spelling out: a wooden
/// fence joins wooden fences and a nether brick fence joins nether brick
/// fences, and the two never join each other — so the test is not "are they
/// both fences" but "do they answer the wooden question the same way".
fn same_fence(other: BlockState, wooden: bool) -> bool {
    let block = other.block();
    in_tag(block, "minecraft:fences") && in_tag(block, "minecraft:wooden_fences") == wooden
}

/// Whether a neighbour is a fence gate turned across the way we are looking at
/// it — which is the only way a gate has a post to join.
///
/// A gate facing north opens along the north-south line, so its posts stand to
/// the east and the west of it and a fence to either side joins them. A fence
/// standing where the gate opens joins nothing, because there is nothing there
/// to join.
fn gate_across(other: BlockState, side: Face) -> bool {
    in_tag(other.block(), "minecraft:fence_gates")
        && other
            .property("facing")
            .and_then(Face::from_direction)
            .is_some_and(|facing| facing.axis() != side.axis())
}

/// The blocks a fence, a wall or a pane will not join even though they have a
/// full face.
///
/// Minecraft's `isExceptionForConnection`, and every one of them is a full cube
/// that would otherwise connect: leaves, a barrier, the two pumpkins that have
/// been carved, a melon and a whole pumpkin, and the shulker boxes. Two of the
/// seven are tags and the rest are named, which is exactly how Minecraft spells
/// it — a name list here is not a table of Mojang's data, it is the behaviour of
/// a block, the same kind of thing as an anvil turning a quarter.
fn exception(other: BlockState) -> bool {
    let block = other.block();
    matches!(
        block.name(),
        "minecraft:barrier"
            | "minecraft:carved_pumpkin"
            | "minecraft:jack_o_lantern"
            | "minecraft:melon"
            | "minecraft:pumpkin"
    ) || in_tag(block, "minecraft:leaves")
        || in_tag(block, "minecraft:shulker_boxes")
}

/// Whether a block is in a tag, following the tags a tag names.
///
/// Tags are the one part of this that is published data rather than Java, so it
/// is what the rules key on wherever Minecraft's own code does. The walk is
/// bounded: `minecraft:fences` names `minecraft:wooden_fences` and stops there,
/// and a depth limit turns a table that referred to itself into a `false`
/// rather than a stack overflow.
fn in_tag(block: Block, tag: &str) -> bool {
    fn walk(tag: &str, name: &str, depth: u8) -> bool {
        let Some(def) = dust_registry::tags::from_id(TagRegistry::Block, tag) else {
            return false;
        };
        if def.contains(name) {
            return true;
        }
        depth > 0
            && def
                .references()
                .any(|inner| walk(&inner[1..], name, depth - 1))
    }
    walk(tag, block.name(), 4)
}

/// A fence, a pane or iron bars, by shape: four bool connections, an optional
/// `waterlogged`, and nothing else.
///
/// Nothing else in the game has that shape. Vines, tripwire, fire and redstone
/// wire all have four connections and each has a fifth property this refuses;
/// chorus plants and the mushroom blocks have six connections rather than four.
fn is_cross(block: Block) -> bool {
    let mut connections = 0;
    for property in block.properties() {
        match property.name {
            "north" | "south" | "west" | "east" if same_set(property.values, BOOL) => {
                connections += 1;
            }
            "waterlogged" => {}
            _ => return false,
        }
    }
    connections == 4
}

/// A wall, by shape: four three-valued connections, a post and nothing else.
fn is_wall(block: Block) -> bool {
    let mut connections = 0;
    let mut post = false;
    for property in block.properties() {
        match property.name {
            "north" | "south" | "west" | "east" if same_set(property.values, WALL_SIDE) => {
                connections += 1;
            }
            "up" if same_set(property.values, BOOL) => post = true,
            "waterlogged" => {}
            _ => return false,
        }
    }
    connections == 4 && post
}

/// Stairs, by shape: a horizontal `facing`, a `half` and a `shape`.
///
/// The same test [`as_stairs`] makes about the click, and it is one function
/// because a second spelling of "is this a stair" is a second thing to drift.
fn is_stairs(block: Block) -> bool {
    values_of(block, "facing").is_some_and(|values| same_set(values, HORIZONTAL))
        && values_of(block, "half").is_some_and(|values| same_set(values, &["top", "bottom"]))
        && values_of(block, "shape").is_some()
}

/// A quarter turn anticlockwise, seen from above. Which way a stair's corner
/// leans is written in terms of it.
fn counter_clockwise(direction: Face) -> Face {
    match direction {
        Face::North => Face::West,
        Face::West => Face::South,
        Face::South => Face::East,
        _ => Face::North,
    }
}

/// `minecraft:air`, resolved once.
///
/// A `LazyLock` rather than a `const` because the lookup is a binary search
/// through the generated table, and [`Around::empty`] is built per neighbour of
/// per edit — often enough that doing it six times each is worth not doing.
static AIR: std::sync::LazyLock<Block> = std::sync::LazyLock::new(|| {
    Block::from_name("minecraft:air").expect("every version of the game has air")
});

/// The two values of a bool property, in no particular order.
const BOOL: &[&str] = &["true", "false"];

/// How high one side of a wall stands.
const WALL_SIDE: &[&str] = &["none", "low", "tall"];

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
            into: air(),
        }
    }

    /// A placement into an empty cell, which is what every situation the grid
    /// survey measured was.
    fn air() -> BlockState {
        AIR.default_state()
    }

    /// A placement into a cell that already holds something.
    fn into(face: Face, cursor_y: f32, there: BlockState) -> Click {
        Click {
            face,
            cursor_y,
            yaw: 0.0,
            pitch: 0.0,
            into: there,
        }
    }

    /// A state from `minecraft:name[a=b]`, for saying what is already there.
    fn there(name: &str, properties: &[(&str, &str)]) -> BlockState {
        let mut state = block(name).default_state();
        for (property, value) in properties {
            state = state
                .with(property, value)
                .expect("this build's block has that property");
        }
        state
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
    fn a_rail_lies_along_the_axis_the_player_is_walking() {
        // Four items, and it is a *click* rule rather than the neighbour rule
        // the property's name suggests. Both axes are asserted because a rule
        // that answered `north_south` to everything would pass one of them and
        // `north_south` is the default state.
        for (yaw, shape) in [
            (0.0, "north_south"),
            (90.0, "east_west"),
            (180.0, "north_south"),
            (-90.0, "east_west"),
        ] {
            for name in [
                "minecraft:rail",
                "minecraft:powered_rail",
                "minecraft:detector_rail",
                "minecraft:activator_rail",
            ] {
                let placed = state(name, Face::Up, yaw, 0.25);
                assert_eq!(value(&placed, "shape"), shape, "{name} at yaw {yaw}");
            }
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
    fn a_block_put_down_in_water_comes_out_wet() {
        // The one a player hits constantly: every dock, every fence post in a
        // river. Both directions, because the wrong one is the one that was
        // already happening.
        let water = there("minecraft:water", &[]);
        assert_eq!(
            value(
                &state_for(block("minecraft:oak_fence"), into(Face::Up, 0.25, water)).properties(),
                "waterlogged"
            ),
            "true"
        );
        assert_eq!(
            value(
                &state_for(block("minecraft:oak_fence"), click(Face::Up, 0.0, 0.25)).properties(),
                "waterlogged"
            ),
            "false"
        );
    }

    #[test]
    fn a_conduit_on_dry_land_is_not_waterlogged() {
        // The 122 rows of the grid survey that were wrong with no water in
        // sight. A conduit, a sea pickle and every coral fan carry
        // `waterlogged=true` in their *default* state, so a server that never
        // touched the property flooded them on land.
        for name in [
            "minecraft:conduit",
            "minecraft:sea_pickle",
            "minecraft:tube_coral_fan",
        ] {
            assert_eq!(
                value(
                    &state_for(block(name), click(Face::Up, 0.0, 0.25)).properties(),
                    "waterlogged"
                ),
                "false",
                "{name}"
            );
        }
    }

    #[test]
    fn seagrass_is_water_and_a_lily_pad_is_not() {
        // `getFluidState`, not "is this block water". Seagrass stands in it
        // and reports it; a lily pad floats on it and does not, because the
        // cell a lily pad is in holds air.
        assert!(holds_water(there("minecraft:seagrass", &[])));
        assert!(holds_water(there("minecraft:kelp_plant", &[])));
        assert!(holds_water(there("minecraft:bubble_column", &[])));
        assert!(!holds_water(there("minecraft:lily_pad", &[])));
        assert!(!holds_water(there("minecraft:lava", &[])));
    }

    #[test]
    fn snow_stacks_to_eight_and_the_ninth_is_refused() {
        let snow = block("minecraft:snow");
        for (deep, then) in [("1", "2"), ("4", "5"), ("7", "8")] {
            let click = into(Face::Up, 0.25, there("minecraft:snow", &[("layers", deep)]));
            assert_eq!(
                value(&state_for(snow, click).properties(), "layers"),
                then,
                "{deep}"
            );
            assert!(replaces_beside(
                snow,
                there("minecraft:snow", &[("layers", deep)]),
                false
            ));
        }
        // Eight is where it stops, and it stops by refusing rather than by
        // flattening the drift back to one.
        assert!(!replaces_beside(
            snow,
            there("minecraft:snow", &[("layers", "8")]),
            false
        ));
        assert!(!replaces_clicked(
            snow,
            there("minecraft:snow", &[("layers", "8")]),
            false,
            click(Face::Up, 0.0, 0.25)
        ));
        // And a drift is only stacked from *above*. Clicking its side puts the
        // snow beside it.
        assert!(!replaces_clicked(
            snow,
            there("minecraft:snow", &[("layers", "3")]),
            false,
            click(Face::North, 0.0, 0.25)
        ));
    }

    #[test]
    fn a_slab_into_its_own_other_half_is_a_double_slab() {
        let slab = block("minecraft:oak_slab");
        let bottom = there("minecraft:oak_slab", &[("type", "bottom")]);
        let state = state_for(slab, into(Face::Up, 0.25, bottom));
        assert_eq!(value(&state.properties(), "type"), "double");
        // A double slab fills the cell, so there is nowhere left for water to
        // be — Minecraft says both in the same line and so does this.
        assert_eq!(value(&state.properties(), "waterlogged"), "false");
        // A slab into a slab of the *other* kind is not a double anything.
        let spruce = there("minecraft:spruce_slab", &[("type", "bottom")]);
        assert_eq!(
            value(
                &state_for(slab, into(Face::Up, 0.25, spruce)).properties(),
                "type"
            ),
            "bottom"
        );
        // Clicked from above, a bottom slab doubles and a top slab does not:
        // the second one has the player aiming at the cell above it.
        assert!(replaces_clicked(
            slab,
            bottom,
            false,
            click(Face::Up, 0.0, 0.25)
        ));
        let top = there("minecraft:oak_slab", &[("type", "top")]);
        assert!(!replaces_clicked(
            slab,
            top,
            false,
            click(Face::Up, 0.0, 0.25)
        ));
        assert!(replaces_clicked(
            slab,
            top,
            false,
            click(Face::Down, 0.0, 0.25)
        ));
        // On a side face the cursor decides, which is the click rule's own
        // half asked of the block that is already there.
        assert!(replaces_clicked(
            slab,
            bottom,
            false,
            click(Face::North, 0.0, 0.75)
        ));
        assert!(!replaces_clicked(
            slab,
            bottom,
            false,
            click(Face::North, 0.0, 0.25)
        ));
    }

    #[test]
    fn a_block_that_reads_no_rule_replaces_what_the_table_says_and_nothing_more() {
        // The fallback, both ways round, so that adding two blocks with an
        // opinion did not give one to everything else.
        let stone = block("minecraft:stone");
        let grass = there("minecraft:short_grass", &[]);
        assert!(replaces_beside(stone, grass, true));
        assert!(!replaces_beside(stone, grass, false));
        assert!(replaces_clicked(
            stone,
            grass,
            true,
            click(Face::Up, 0.0, 0.25)
        ));
        assert!(!replaces_clicked(
            stone,
            grass,
            false,
            click(Face::Up, 0.0, 0.25)
        ));
    }

    #[test]
    fn leaves_a_player_put_down_do_not_decay() {
        for name in ["minecraft:oak_leaves", "minecraft:cherry_leaves"] {
            assert_eq!(
                value(
                    &state_for(block(name), click(Face::Up, 0.0, 0.25)).properties(),
                    "persistent"
                ),
                "true",
                "{name}"
            );
        }
        // Scaffolding has a `distance` too and no `persistent`, which is the
        // shape this rule is keyed to refuse.
        assert!(as_leaves(
            block("minecraft:scaffolding"),
            block("minecraft:scaffolding").default_state()
        )
        .is_none());
    }

    /// The wall form an item carries, as the item table hands it over.
    fn wall(name: &str, attaches: &'static str) -> WallForm {
        WallForm {
            block: block(name),
            attaches,
        }
    }

    #[test]
    fn a_torch_on_a_wall_is_a_wall_torch_and_on_the_ground_is_a_torch() {
        let torch = block("minecraft:torch");
        let form = Some(wall("minecraft:wall_torch", "down"));
        // The top of a block: it stands on it.
        let up = state_for_item(torch, form, click(Face::Up, 0.0, 0.25));
        assert_eq!(up.block().name(), "minecraft:torch");
        // A side: it goes on the wall, facing out of it. Measured — the
        // clicked face and not the player's look, which at yaw 0 would have
        // given south for every one of these.
        for (face, facing) in [
            (Face::North, "north"),
            (Face::South, "south"),
            (Face::East, "east"),
            (Face::West, "west"),
        ] {
            let on = state_for_item(torch, form, click(face, 0.0, 0.25));
            assert_eq!(on.block().name(), "minecraft:wall_torch", "{facing}");
            assert_eq!(value(&on.properties(), "facing"), facing);
        }
    }

    #[test]
    fn an_item_with_no_wall_form_is_untouched_by_the_rule() {
        // The 872 items that have one block, and the case a table written
        // before the columns puts *every* item in.
        let stone = block("minecraft:stone");
        for face in [Face::Up, Face::North, Face::Down] {
            assert_eq!(
                state_for_item(stone, None, click(face, 0.0, 0.25)),
                state_for(stone, click(face, 0.0, 0.25))
            );
        }
        let sign = block("minecraft:oak_sign");
        assert_eq!(
            state_for_item(sign, None, click(Face::North, 0.0, 0.25)).block(),
            sign,
            "no table, no wall sign — the answer it always gave"
        );
    }

    #[test]
    fn a_hanging_sign_attaches_upward_and_keeps_its_old_answer_on_a_wall() {
        // The attachment direction is why the columns carry it. The face that
        // stands a sign up hangs nothing, and the face that hangs one is the
        // one a sign refuses.
        let hanging = block("minecraft:oak_hanging_sign");
        let form = Some(wall("minecraft:oak_wall_hanging_sign", "up"));
        assert_eq!(
            state_for_item(hanging, form, click(Face::Down, 0.0, 0.25)).block(),
            hanging
        );
        // And on a wall it is left alone, because the wall form faces *across*
        // the wall rather than out of it and the grid was taken at one yaw.
        assert_eq!(
            state_for_item(hanging, form, click(Face::North, 0.0, 0.25)).block(),
            hanging
        );
    }

    #[test]
    fn a_sign_faces_the_player_and_a_skull_faces_the_way_they_look() {
        // One property, two rules, and no single one gives both: at yaw 180 a
        // sign is segment 0 and a skull is segment 8.
        let checks = [
            ("minecraft:oak_sign", 180.0, "0"),
            ("minecraft:oak_sign", 90.0, "12"),
            ("minecraft:black_banner", 90.0, "12"),
            ("minecraft:skeleton_skull", 180.0, "8"),
            ("minecraft:skeleton_skull", 90.0, "4"),
        ];
        for (name, yaw, segment) in checks {
            assert_eq!(
                value(
                    &state_for(block(name), click(Face::Up, yaw, 0.25)).properties(),
                    "rotation"
                ),
                segment,
                "{name} at {yaw}"
            );
        }
        // A yaw that has gone round more than once still lands in the sixteen.
        assert_eq!(
            value(
                &state_for(block("minecraft:oak_sign"), click(Face::Up, -270.0, 0.25)).properties(),
                "rotation"
            ),
            "12"
        );
    }

    #[test]
    fn a_campfire_faces_the_way_the_player_looks_and_a_furnace_faces_back() {
        // The same pair the whole measurement was taken to settle, one shape
        // further on: a campfire has `lit` and so does a furnace, and at yaw
        // 180 they come out facing opposite ways.
        for name in [
            "minecraft:campfire",
            "minecraft:soul_campfire",
            "minecraft:decorated_pot",
            "minecraft:calibrated_sculk_sensor",
        ] {
            assert_eq!(
                value(&state(name, Face::Up, 180.0, 0.25), "facing"),
                "north",
                "{name}"
            );
        }
        assert_eq!(
            value(&state("minecraft:furnace", Face::Up, 180.0, 0.25), "facing"),
            "south"
        );
    }

    #[test]
    fn a_ladder_faces_out_of_the_wall_it_hangs_on() {
        // Not the player at all. Getting this wrong hangs the ladder off the
        // wrong side of its own cell, against nothing, and it cannot be
        // climbed.
        for name in ["minecraft:ladder", "minecraft:tripwire_hook"] {
            assert_eq!(
                value(&state(name, Face::North, 180.0, 0.25), "facing"),
                "north",
                "{name}"
            );
            assert_eq!(
                value(&state(name, Face::East, 0.0, 0.25), "facing"),
                "east",
                "{name}"
            );
        }
    }

    #[test]
    fn a_lantern_hangs_from_a_ceiling_and_stands_on_a_floor() {
        assert_eq!(
            value(
                &state("minecraft:lantern", Face::Down, 0.0, 0.25),
                "hanging"
            ),
            "true"
        );
        assert_eq!(
            value(
                &state("minecraft:soul_lantern", Face::Up, 0.0, 0.25),
                "hanging"
            ),
            "false"
        );
    }

    #[test]
    fn lichen_sticks_to_the_face_it_was_put_on_and_a_mushroom_block_does_not() {
        // Six bools and a `waterlogged` is a multiface block. Six bools and no
        // `waterlogged` is a mushroom block, whose every side is `true` by
        // default — and a rule that set one and cleared the rest would turn it
        // inside out. That clause is the whole reason `waterlogged` is in the
        // shape test.
        for name in ["minecraft:glow_lichen", "minecraft:sculk_vein"] {
            assert_eq!(
                value(&state(name, Face::Up, 0.0, 0.25), "down"),
                "true",
                "{name}"
            );
            assert_eq!(
                value(&state(name, Face::North, 0.0, 0.25), "south"),
                "true",
                "{name}"
            );
        }
        let mushroom = block("minecraft:brown_mushroom_block");
        assert_eq!(
            state_for(mushroom, click(Face::Up, 0.0, 0.25)),
            mushroom.default_state(),
            "every side stays as it was"
        );
    }

    #[test]
    fn a_shape_no_rule_recognises_keeps_the_default_state() {
        // The fallback that makes a rule an improvement rather than a trade.
        // Stone and glass have nothing here to key on and go down exactly as
        // they did before this file existed.
        //
        // **Leaves used to be in this list and have earned their way out**, in
        // the direction this test exists to watch: a rule was written for them
        // and the shape it keys on is now recognised. A block leaving this
        // list is a rule arriving; a block joining it would be a rule lost.
        for name in ["minecraft:stone", "minecraft:glass", "minecraft:bedrock"] {
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
            into: air(),
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

    // ---------------------------------------------------------------------
    // What is beside a block.
    //
    // Every expectation below was read off `harness placement`'s neighbour
    // answers, the same way the click rules above were read off its grid.
    //
    // **The table these run against is written here and is not Minecraft's.**
    // It says stone, glass, planks and leaves have full faces and nothing else
    // does, which is enough to separate the rules from each other and is not
    // enough to say the rules are right — a stand-in only reaches the defects
    // its own range reaches. What says they are right is
    // `cargo xtask harness placement`, against answers asked of a real server,
    // and that is where a number belongs.
    // ---------------------------------------------------------------------

    /// A constants table where exactly the named blocks have full faces.
    fn sturdy(names: &[&str]) -> dust_registry::BlockConstants {
        let states: std::collections::HashSet<u32> = names
            .iter()
            .flat_map(|name| {
                Block::from_name(name)
                    .expect("this build has that block")
                    .states()
                    .map(BlockState::id)
            })
            .collect();
        let mut text = String::from("# state_id\topacity\temission");
        for column in STURDY {
            text.push('\t');
            text.push_str(column);
        }
        text.push('\n');
        for state in 0..dust_registry::STATE_COUNT {
            let full = u32::from(states.contains(&state));
            text.push_str(&format!("{state}\t0\t0"));
            for _ in STURDY {
                text.push_str(&format!("\t{full}"));
            }
            text.push('\n');
        }
        dust_registry::BlockConstants::parse(&text).expect("a complete table")
    }

    /// The default state of a block, for putting in a neighbouring cell.
    fn default(name: &str) -> BlockState {
        block(name).default_state()
    }

    /// A state written the way the answers file writes one.
    fn spell(state: BlockState) -> String {
        let mut properties: Vec<String> = state
            .properties()
            .into_iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        properties.sort();
        format!("{}[{}]", state.block().name(), properties.join(","))
    }

    /// What `block` becomes with those neighbours, given that table.
    fn with_neighbours(
        name: &str,
        neighbours: &[(Face, BlockState)],
        table: &dust_registry::BlockConstants,
    ) -> BlockState {
        let mut around = Around::empty();
        for (side, state) in neighbours {
            around = around.with(*side, *state);
        }
        let solid = Solid::from_constants(table).expect("the table has the columns");
        shaped(default(name), around, solid)
    }

    #[test]
    fn a_rule_that_needs_the_table_does_not_run_without_it() {
        // The `has_x` question and not the "what does it answer when it does
        // not know" one. A table with no full-face columns cannot make a
        // `Solid`, and a caller with no `Solid` runs no connection rule — which
        // is why a fence on a server with no constants table is bare rather
        // than half-connected.
        let text = "# state_id\topacity\temission\n".to_owned()
            + &(0..dust_registry::STATE_COUNT)
                .map(|state| format!("{state}\t0\t0\n"))
                .collect::<String>();
        let table = dust_registry::BlockConstants::parse(&text).expect("a complete table");
        assert!(Solid::from_constants(&table).is_none());
    }

    #[test]
    fn a_fence_reaches_for_a_full_face_and_not_for_a_slab() {
        let table = sturdy(&["minecraft:stone"]);
        let stone = with_neighbours(
            "minecraft:oak_fence",
            &[(Face::North, default("minecraft:stone"))],
            &table,
        );
        assert_eq!(stone.property("north"), Some("true"));
        assert_eq!(stone.property("south"), Some("false"));
        // A bottom slab has no full side, so a fence beside one stays bare —
        // which is the case a rule keyed on "is it a block" gets wrong.
        let slab = with_neighbours(
            "minecraft:oak_fence",
            &[(Face::North, default("minecraft:oak_slab"))],
            &table,
        );
        assert_eq!(slab.property("north"), Some("false"), "{}", spell(slab));
    }

    #[test]
    fn a_fence_joins_its_own_kind_across_an_empty_side() {
        let table = sturdy(&[]);
        let joined = with_neighbours(
            "minecraft:oak_fence",
            &[
                (Face::North, default("minecraft:spruce_fence")),
                (Face::East, default("minecraft:nether_brick_fence")),
            ],
            &table,
        );
        // Wooden joins wooden whatever the wood. Nether brick is a fence too
        // and joins nothing wooden, which is Minecraft's own rule and reads
        // like a bug until you stand next to one.
        assert_eq!(joined.property("north"), Some("true"), "{}", spell(joined));
        assert_eq!(joined.property("east"), Some("false"), "{}", spell(joined));
    }

    #[test]
    fn a_fence_joins_a_gate_turned_across_it_and_not_one_in_line() {
        let table = sturdy(&[]);
        let gate = default("minecraft:oak_fence_gate")
            .with("facing", "north")
            .expect("a gate faces");
        // A gate facing north opens along the north-south line, so its posts
        // are to its east and west. A fence to its east joins one of them; a
        // fence to its north is standing in the doorway.
        let beside = with_neighbours("minecraft:oak_fence", &[(Face::West, gate)], &table);
        assert_eq!(beside.property("west"), Some("true"), "{}", spell(beside));
        let ahead = with_neighbours("minecraft:oak_fence", &[(Face::South, gate)], &table);
        assert_eq!(ahead.property("south"), Some("false"), "{}", spell(ahead));
    }

    #[test]
    fn a_full_face_is_not_enough_when_the_block_is_an_exception() {
        // Leaves are a full cube and a fence refuses them anyway. This is the
        // one place the rule cannot be read off the shape at all, and the table
        // here says leaves are as solid as stone so that the refusal has to
        // come from the exception and cannot come from the geometry.
        //
        // **Stone is in the same call on purpose.** An assertion that a
        // connection is absent passes against a server that makes no
        // connections at all, so on its own this test is green with the whole
        // rule deleted — which is what happened when it was watched to fail.
        // The stone side is what says the rule ran.
        let table = sturdy(&["minecraft:stone", "minecraft:oak_leaves"]);
        let fence = with_neighbours(
            "minecraft:oak_fence",
            &[
                (Face::North, default("minecraft:oak_leaves")),
                (Face::South, default("minecraft:stone")),
            ],
            &table,
        );
        assert_eq!(fence.property("north"), Some("false"), "{}", spell(fence));
        assert_eq!(fence.property("south"), Some("true"), "{}", spell(fence));
    }

    #[test]
    fn a_pane_joins_panes_and_walls_where_a_fence_does_not() {
        let table = sturdy(&[]);
        let pane = with_neighbours(
            "minecraft:glass_pane",
            &[
                (Face::North, default("minecraft:iron_bars")),
                (Face::East, default("minecraft:cobblestone_wall")),
            ],
            &table,
        );
        assert_eq!(pane.property("north"), Some("true"), "{}", spell(pane));
        assert_eq!(pane.property("east"), Some("true"), "{}", spell(pane));
        // The same two beside a fence, which joins neither. One shape, two
        // rules, and nothing in the property table tells them apart.
        let fence = with_neighbours(
            "minecraft:oak_fence",
            &[
                (Face::North, default("minecraft:iron_bars")),
                (Face::East, default("minecraft:cobblestone_wall")),
            ],
            &table,
        );
        assert_eq!(fence.property("north"), Some("false"), "{}", spell(fence));
        assert_eq!(fence.property("east"), Some("false"), "{}", spell(fence));
    }

    #[test]
    fn a_wall_is_tall_under_a_block_and_low_under_the_sky() {
        let table = sturdy(&["minecraft:stone"]);
        let open = with_neighbours(
            "minecraft:cobblestone_wall",
            &[(Face::North, default("minecraft:stone"))],
            &table,
        );
        assert_eq!(open.property("north"), Some("low"), "{}", spell(open));
        let covered = with_neighbours(
            "minecraft:cobblestone_wall",
            &[
                (Face::North, default("minecraft:stone")),
                (Face::Up, default("minecraft:stone")),
            ],
            &table,
        );
        // The property is on the connection and the reason is above the wall,
        // which is the one thing about a wall that cannot be guessed.
        assert_eq!(
            covered.property("north"),
            Some("tall"),
            "{}",
            spell(covered)
        );
    }

    #[test]
    fn a_wall_keeps_its_post_until_it_runs_through() {
        // Every case here is a row of the neighbour survey, and two of them are
        // rows that were red first: a line through with a block on top of it
        // keeps no post, and a wall connected on all four sides keeps none
        // either. "A straight run" was the phrase the rule was first written
        // from and it is wrong about both.
        let table = sturdy(&["minecraft:stone"]);
        let stone = default("minecraft:stone");
        let alone = with_neighbours("minecraft:cobblestone_wall", &[], &table);
        assert_eq!(alone.property("up"), Some("true"), "{}", spell(alone));
        let corner = with_neighbours(
            "minecraft:cobblestone_wall",
            &[(Face::North, stone), (Face::East, stone)],
            &table,
        );
        assert_eq!(corner.property("up"), Some("true"), "{}", spell(corner));
        let through = with_neighbours(
            "minecraft:cobblestone_wall",
            &[(Face::North, stone), (Face::South, stone)],
            &table,
        );
        assert_eq!(through.property("up"), Some("false"), "{}", spell(through));
        // A block on top makes the connections `tall`, which already reach the
        // top of the wall — so the post is not needed and Minecraft does not
        // put one there.
        let loaded = with_neighbours(
            "minecraft:cobblestone_wall",
            &[
                (Face::North, stone),
                (Face::South, stone),
                (Face::Up, stone),
            ],
            &table,
        );
        assert_eq!(loaded.property("up"), Some("false"), "{}", spell(loaded));
        // A crossroads runs through in both axes, so it has no post either.
        let crossroads = with_neighbours(
            "minecraft:cobblestone_wall",
            &[
                (Face::North, stone),
                (Face::South, stone),
                (Face::West, stone),
                (Face::East, stone),
            ],
            &table,
        );
        assert_eq!(
            crossroads.property("up"),
            Some("false"),
            "{}",
            spell(crossroads)
        );
        // And the small things a player stands on top of a wall, which are the
        // reason the tag exists: a torch over a line through gets its post back.
        let torch = with_neighbours(
            "minecraft:cobblestone_wall",
            &[
                (Face::North, stone),
                (Face::South, stone),
                (Face::Up, default("minecraft:torch")),
            ],
            &table,
        );
        assert_eq!(torch.property("up"), Some("true"), "{}", spell(torch));
    }

    #[test]
    fn a_stair_turns_a_corner_from_the_stair_across_it() {
        let table = sturdy(&[]);
        let facing_north = default("minecraft:oak_stairs")
            .with("facing", "north")
            .expect("a stair faces");
        let across = |direction: &str| {
            default("minecraft:oak_stairs")
                .with("facing", direction)
                .expect("a stair faces")
        };
        // The stair it faces gives it an *outer* corner, and which way the
        // corner leans is the neighbour's facing against its own: west is
        // north's counter-clockwise, so a west-facing stair ahead turns left.
        let outer_left = shaped(
            facing_north,
            Around::empty().with(Face::North, across("west")),
            Solid::from_constants(&table).expect("columns"),
        );
        assert_eq!(outer_left.property("shape"), Some("outer_left"));
        let outer_right = shaped(
            facing_north,
            Around::empty().with(Face::North, across("east")),
            Solid::from_constants(&table).expect("columns"),
        );
        assert_eq!(outer_right.property("shape"), Some("outer_right"));
        // The stair *behind* it gives an inner corner instead.
        let inner_right = shaped(
            facing_north,
            Around::empty().with(Face::South, across("east")),
            Solid::from_constants(&table).expect("columns"),
        );
        assert_eq!(inner_right.property("shape"), Some("inner_right"));
    }

    #[test]
    fn a_stair_ignores_a_corner_in_the_other_half() {
        // The clause a rule written from the facing alone would miss: a bottom
        // stair beside a top stair running across it stays straight, because
        // the two are not touching where a corner would be.
        //
        // The same neighbour in the *same* half is asserted beside it, for the
        // reason the exception test gives: `straight` is also what a stair with
        // no shape rule at all comes out as, so the negative half of this is
        // green against a deleted rule and says nothing on its own.
        let table = sturdy(&[]);
        let solid = Solid::from_constants(&table).expect("columns");
        let across = default("minecraft:oak_stairs")
            .with("facing", "east")
            .expect("a stair faces");
        let other_half = across.with("half", "top").expect("a stair has a half");
        let straight = shaped(
            default("minecraft:oak_stairs"),
            Around::empty().with(Face::North, other_half),
            solid,
        );
        assert_eq!(
            straight.property("shape"),
            Some("straight"),
            "{}",
            spell(straight)
        );
        let corner = shaped(
            default("minecraft:oak_stairs"),
            Around::empty().with(Face::North, across),
            solid,
        );
        assert_eq!(
            corner.property("shape"),
            Some("outer_right"),
            "{}",
            spell(corner)
        );
    }

    #[test]
    fn shaping_a_shaped_state_changes_nothing() {
        // What lets the world call this on any write without tracking whether
        // it already has — and what makes the placement half and the
        // neighbour-change half the same rule rather than two that have to
        // agree.
        let table = sturdy(&["minecraft:stone"]);
        let solid = Solid::from_constants(&table).expect("columns");
        let around = Around::empty()
            .with(Face::North, default("minecraft:stone"))
            .with(Face::East, default("minecraft:oak_fence"));
        let once = shaped(default("minecraft:oak_fence"), around, solid);
        assert_eq!(shaped(once, around, solid), once);
    }

    #[test]
    fn a_block_with_no_shape_rule_is_not_asked_twice() {
        // The cheap half of the pair, and the answer for almost every
        // neighbour of almost every edit.
        assert!(!reads_neighbours(default("minecraft:stone")));
        assert!(!reads_neighbours(default("minecraft:oak_log")));
        assert!(reads_neighbours(default("minecraft:oak_fence")));
        assert!(reads_neighbours(default("minecraft:cobblestone_wall")));
        assert!(reads_neighbours(default("minecraft:glass_pane")));
        assert!(reads_neighbours(default("minecraft:oak_stairs")));
    }

    #[test]
    fn a_face_the_protocol_does_not_have_is_refused_rather_than_guessed() {
        assert_eq!(Face::from_protocol(6), None);
        assert_eq!(Face::from_protocol(255), None);
        assert_eq!(Face::from_protocol(0), Some(Face::Down));
        assert_eq!(Face::from_protocol(5), Some(Face::East));
    }
}
