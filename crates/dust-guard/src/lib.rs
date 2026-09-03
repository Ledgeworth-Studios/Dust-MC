//! Anti-cheat checks: the rules a server applies to what a client claims to
//! have done.
//!
//! # Where this sits, and why it is a crate of its own
//!
//! `dust-server`'s session decodes packets and `dust-world` stores blocks.
//! Neither of them is the place to decide whether a player was *allowed* to do
//! what they said they did — `dust_server::net::edits` says so in its own
//! documentation, and this crate is where that answer goes. Nothing here knows
//! about a socket or about a chunk; it takes numbers and returns a verdict.
//!
//! That is not an aesthetic split. A check that can only be run from inside a
//! session can only be tested by running a session, and a check that cannot be
//! tested cheaply is one that gets loosened until it passes.
//!
//! # What is here
//!
//! [`Reach`], which bounds how far a player may act from where they stand, and
//! [`Movement`], which bounds where they may say they are — in time, through
//! [`SpeedLimit`], and through solid ground, through [`Solidity`]. [`Pose`] is
//! what both of them measure: how tall a player is and where their eyes are,
//! derived from the handful of things a 1.21.1 client actually tells a server
//! about its own shape. The rules
//! that are still missing are stated where the code for them would go rather
//! than listed here, because a list in two places is a list that disagrees with
//! itself.

#![forbid(unsafe_code)]

/// How far a player may act on the world from where they are standing.
///
/// # What it is checking
///
/// The distance from the player's eye to the **nearest point of the block**,
/// not to its centre and not to its corner. A block is a unit cube, so a player
/// standing right against a wall is zero away from it and a player five blocks
/// back is five away from its face — which is the measurement a person means by
/// "how far can I reach", and the one vanilla makes with
/// `new AABB(pos).distanceToSqr(eyePosition)`.
///
/// Centre-to-eye would be up to 0.87 blocks longer for the same reach, which
/// reads as an inconsistent limit: a player who can break the block in front of
/// them cannot break the one diagonally past it, at the same distance from
/// their hand.
///
/// # What it is *not* checking
///
/// **Line of sight.** A player may reach a block through a wall. Vanilla does
/// not check that either — the client raycasts and the server trusts the
/// result — so this is a match rather than a gap, but it is worth stating
/// because "reach check" sounds like it covers it.
///
/// **Where the player really is.** This measures from wherever it is told to.
/// It is [`Movement`] that decides whether a claimed position is one the player
/// could have reached, and a caller that measures reach from an unchecked
/// position has a check a client can walk around by lying. `dust-server` passes
/// the position `Movement` last accepted.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reach {
    limit_squared: f64,
}

impl Reach {
    /// A limit in blocks, from the eye to the nearest point of the block.
    ///
    /// # Panics
    ///
    /// Never. A negative or non-finite limit refuses everything rather than
    /// panicking, which is the safe direction for a *check* — but it is a
    /// configuration mistake and the configuration layer is what should have
    /// caught it, so the type does not pretend to be a validator.
    #[must_use]
    pub fn new(limit: f64) -> Self {
        Self {
            limit_squared: if limit.is_finite() && limit > 0.0 {
                limit * limit
            } else {
                0.0
            },
        }
    }

    /// The limit in blocks, as it was given.
    #[must_use]
    pub fn limit(self) -> f64 {
        self.limit_squared.sqrt()
    }

    /// Whether an eye at `eye` may act on the block whose corner is at `block`.
    ///
    /// `block` is the block's own coordinates — the cube it occupies runs from
    /// there to one more on each axis.
    #[must_use]
    pub fn allows(self, eye: (f64, f64, f64), block: (i32, i32, i32)) -> bool {
        self.distance_squared(eye, block) < self.limit_squared
    }

    /// The squared distance from `eye` to the nearest point of the block.
    ///
    /// Squared, and compared squared, because the square root is the only
    /// expensive part of this and nothing needs the distance itself. Exposed
    /// rather than kept private so that a caller logging a refusal can say how
    /// far away it was.
    #[must_use]
    pub fn distance_squared(self, eye: (f64, f64, f64), block: (i32, i32, i32)) -> f64 {
        let axis = |eye: f64, low: i32| {
            let low = f64::from(low);
            // Zero inside the cube, and the gap to the nearer face outside it.
            // Written as two `max`es rather than a branch because the inside
            // case is not special: it is the one where both terms are negative.
            (low - eye).max(eye - (low + 1.0)).max(0.0)
        };
        let x = axis(eye.0, block.0);
        let y = axis(eye.1, block.1);
        let z = axis(eye.2, block.2);
        x * x + y * y + z * z
    }
}

/// How far above their feet a standing player's eyes are.
///
/// Vanilla's `Player.DEFAULT_EYE_HEIGHT`, and the eye height of
/// [`Pose::Standing`]. Every other pose has its own; see [`Pose::eye_height`].
pub const EYE_HEIGHT: f64 = 1.62;

/// What shape a player is.
///
/// # Why a server needs this at all
///
/// A player is not a point and not a fixed box. Standing they are 1.8 tall
/// with their eyes at 1.62; crouching, 1.5 and 1.27; crawling, swimming or
/// gliding, 0.6 and 0.4. Two of this crate's checks read those numbers and
/// both of them were wrong without this type: [`Reach`] measured every player
/// from a standing eye, so a crouching one was measured **0.35 too high** —
/// which is exactly the wrong direction at a ledge edge, where crouching is
/// the single most common thing a player does on purpose — and [`Movement`]
/// measured only the bottom 0.6 of everybody, so a client could put its head
/// through a wall while its feet stood in a legal cell.
///
/// # Where the numbers come from
///
/// Vanilla's `Player.POSES`, which pairs each `Pose` with an
/// `EntityDimensions`. They are constants in Minecraft's code rather than rows
/// in a table it ships, which is why they are written here and not extracted:
/// there is no file in a jar to read them out of. The width — 0.6 — is the
/// same for every pose but `SLEEPING`, and [`PLAYER_WIDTH`] holds it.
///
/// # What a server can actually know
///
/// Less than this enum can say, and that gap is the whole difficulty. A 1.21.1
/// client tells the server about **crouching** (the sneak key, as a
/// `player_command`) and about **gliding** (the elytra start), and about
/// nothing else. Swimming and crawling are not sent: vanilla derives them,
/// from water and from whether a taller pose fits. Dust does not read water on
/// the movement path, so [`Movement`] treats swimming as a thing it cannot see
/// and is permissive about — see [`Movement::measured_height`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pose {
    /// On their feet. 1.8 tall, eyes at 1.62.
    #[default]
    Standing,
    /// Holding the sneak key, and not flying. 1.5 tall, eyes at 1.27.
    ///
    /// Vanilla's condition is `isShiftKeyDown() && !abilities.flying`: a
    /// creative player who sneaks while flying descends rather than crouching,
    /// and is still their full height.
    Crouching,
    /// Swimming, crawling, or spinning through the air on a riptide trident.
    /// 0.6 tall, eyes at 0.4 — vanilla gives all three the same box.
    Swimming,
    /// Gliding on an elytra. 0.6 tall, eyes at 0.4.
    Gliding,
    /// In a bed. 0.2 tall, eyes at 0.2.
    ///
    /// Nothing in this server puts a player here yet — there are no beds — and
    /// it is written down anyway because the day there are, a sleeping player
    /// measured as 1.8 tall is a player refused for lying in a two-block
    /// bedroom.
    Sleeping,
}

impl Pose {
    /// How tall a player in this pose is, in blocks.
    #[must_use]
    pub fn height(self) -> f64 {
        match self {
            Self::Standing => 1.8,
            Self::Crouching => 1.5,
            Self::Swimming | Self::Gliding => 0.6,
            Self::Sleeping => 0.2,
        }
    }

    /// How far above their feet a player in this pose has their eyes.
    #[must_use]
    pub fn eye_height(self) -> f64 {
        match self {
            Self::Standing => EYE_HEIGHT,
            Self::Crouching => 1.27,
            Self::Swimming | Self::Gliding => 0.4,
            Self::Sleeping => 0.2,
        }
    }
}

/// What a client has said about itself, out of which a [`Pose`] is derived.
///
/// Five bits, all of them read straight off packets the server already decodes
/// — three `player_command` actions, the abilities flags, and the on-ground
/// flag every movement packet carries. Nothing here is inferred and nothing
/// here costs a world lookup; the inference lives in [`Posture::pose`] and in
/// [`Movement::measured_height`], where it can be read in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Posture {
    /// The sneak key is down. `player_command` `StartSneaking`/`StopSneaking`.
    pub sneaking: bool,
    /// The sprint key is down. `player_command`
    /// `StartSprinting`/`StopSprinting`. Not a pose by itself — it is here
    /// because vanilla's swimming pose requires it, and swimming is the one
    /// pose this server cannot see.
    pub sprinting: bool,
    /// Flying, from `player_abilities`. Cancels crouching, as it does in
    /// vanilla.
    pub flying: bool,
    /// Gliding on an elytra. `player_command` `StartFlyingWithElytra`.
    ///
    /// A client says when this *starts* and never when it stops — landing is
    /// something vanilla's server works out for itself. A stale `true` here
    /// makes a player shorter than they are, which is the direction that
    /// believes them rather than the direction that refuses them.
    pub gliding: bool,
    /// The last movement packet said the player was standing on something.
    pub on_ground: bool,
}

impl Posture {
    /// The pose these signals describe.
    ///
    /// Vanilla's `Player.updatePlayerPose` in the order it tests them, with the
    /// two branches this server cannot see — sleeping and swimming — left out.
    /// It does **not** include vanilla's "and shrink until it fits" fallback;
    /// that needs the world, and [`Movement::measured_height`] is where it
    /// happens.
    #[must_use]
    pub fn pose(self) -> Pose {
        if self.gliding {
            Pose::Gliding
        } else if self.sneaking && !self.flying {
            Pose::Crouching
        } else {
            Pose::Standing
        }
    }
}

/// The eye position of a player at `feet` in `pose`.
#[must_use]
pub fn eye_of(feet: (f64, f64, f64), pose: Pose) -> (f64, f64, f64) {
    (feet.0, feet.1 + pose.eye_height(), feet.2)
}

/// How far a player may move in one tick, in blocks.
///
/// # Where the number comes from
///
/// Measured, not reasoned about. `tools/bot/movement.js` drives a third-party
/// client through the motions a player actually makes and counts the
/// displacement in every position packet it sends. Over 1,217 packets —
/// standing, walking, sprinting, sprint-jumping, creative flight, a 300-block
/// free fall, and a walk through a 700 ms network stall — the largest single
/// tick was **3.580 blocks**, all of it the fall, which is approaching free
/// fall's asymptote of 3.92 blocks per tick. Nothing else came near one block.
///
/// The default limit is **10 blocks per tick**, which is what vanilla's own
/// server uses for a player who is not flying an elytra (its constant is 100,
/// and it is a squared one). That is 2.8 times the fastest thing an honest
/// client here produces, and the headroom is not waste: elytra, riptide,
/// knockback and TNT boosts all move a player faster than walking, and none of
/// them exist in this server *yet*. A limit tuned to what Dust can do today is
/// a limit that starts rubber-banding players the week elytra land.
///
/// # What this does not catch
///
/// A player who moves at 9 blocks a tick forever. That is 180 blocks a second
/// and it is plainly a cheat, and this will not say so — the same hole vanilla
/// has, for the same reason: the alternative is a bound tight enough to argue
/// with a legitimate fall. What it does catch is the shape the README names,
/// the client that claims to be somewhere it could not have walked to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpeedLimit {
    per_tick_squared: f64,
}

impl SpeedLimit {
    /// The most ticks of movement one packet may be charged for.
    ///
    /// A stalled connection does **not** produce one big step: the client keeps
    /// ticking and keeps writing one packet per tick, the packets queue, and
    /// then they arrive together — several packets within one tick of each
    /// other, each carrying the ordinary displacement of a single tick. That is
    /// what the 700 ms stall in the measurement shows, and it is why the budget
    /// is floored at one tick rather than scaled by elapsed time alone: charge
    /// by the clock and a bunched-up honest client is refused for being *early*.
    ///
    /// Scaling up exists for the other shape, a client that froze and resumed
    /// and simulated several ticks into one packet. Past a quarter of a second
    /// that stops being the likely explanation — a longer gap means a queue of
    /// packets rather than one big one — so the multiplier stops there.
    /// Vanilla clamps at the same five.
    pub const MAX_TICKS: u32 = 5;

    /// A limit in blocks per tick.
    ///
    /// `f64::INFINITY` is a legitimate value and turns the speed bound off
    /// entirely; the coordinate bound in [`Movement::claimed`] still applies,
    /// because a position that is not a number is malformed rather than fast.
    /// A limit that is not a positive number refuses everything, which is the
    /// safe direction for a check — but it is a configuration mistake and
    /// `dust-config` is what refuses it.
    #[must_use]
    pub fn new(blocks_per_tick: f64) -> Self {
        Self {
            per_tick_squared: if blocks_per_tick > 0.0 && !blocks_per_tick.is_nan() {
                blocks_per_tick * blocks_per_tick
            } else {
                0.0
            },
        }
    }

    /// The limit in blocks per tick, as it was given.
    #[must_use]
    pub fn blocks_per_tick(self) -> f64 {
        self.per_tick_squared.sqrt()
    }

    /// The squared distance a player may cover in `ticks` ticks.
    ///
    /// `ticks` is clamped into `1..=`[`MAX_TICKS`](Self::MAX_TICKS) and the
    /// budget grows as its **square**, because a player moving at the limit for
    /// *n* ticks covers *n* times the distance and this is a squared quantity.
    /// Vanilla multiplies its squared constant by *n* rather than *n²*, which
    /// is a tighter bound that only never fires because the constant is eight
    /// times what honest play produces; stating the relation correctly costs
    /// one multiply and does not need that excuse.
    #[must_use]
    pub fn budget_squared(self, ticks: u32) -> f64 {
        let ticks = f64::from(ticks.clamp(1, Self::MAX_TICKS));
        self.per_tick_squared * ticks * ticks
    }
}

/// A world, asked the only question a movement check has for it.
///
/// # Why this is a range and not a cell
///
/// The obvious shape is `solid(x, y, z) -> bool`, and it is the wrong one.
/// Resolving *which column* a cell belongs to costs the same as resolving the
/// column and reading eight cells out of it, and on a world read from region
/// files it can cost a file read — so a per-cell question makes the caller pay
/// that eight times for one player box. Handing the whole box over at once lets
/// the implementation hoist that work, and there is nothing else a movement
/// check ever wants to know.
///
/// # What "solid" has to mean
///
/// A block whose **collision shape is the whole cube**, and nothing looser. Not
/// "opaque", not "occludes", not "blocks motion": a stair, a slab, a fence, a
/// farmland block and a lump of soul sand all block motion and all let a player
/// stand somewhere inside the cube they occupy. Counting any of them refuses a
/// player for standing where the game put them, which is the one failure this
/// check cannot be forgiven for. Under-counting only lets a cheat through.
///
/// # What an implementation does about a chunk it does not have
///
/// Say it is not solid. A player walking into ground the server has not loaded
/// is a player the server cannot judge, and the honest answer to a question you
/// cannot answer is not "refused".
///
/// `&mut self` so that an implementation may cache the column it just resolved;
/// [`Movement::claimed`] asks once per packet for a player in the open and up
/// to four times for one standing inside terrain, and every one of those
/// questions is about the same one to four columns.
pub trait Solidity {
    /// The first solid cell in the inclusive box from `lo` to `hi`, or `None`
    /// if there is none. Which one, when there are several, is not specified —
    /// it is used to say what a refused player walked into.
    fn first_solid(&mut self, lo: (i32, i32, i32), hi: (i32, i32, i32)) -> Option<(i32, i32, i32)>;
}

/// A world with nothing solid in it.
///
/// What a server hands [`Movement::claimed`] when the collision check is turned
/// off, or when it has no table saying which block states are solid. Both are
/// real states rather than error cases: the block constants are extracted from
/// the operator's own jar and a server can legitimately be running without them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Open;

impl Solidity for Open {
    fn first_solid(&mut self, _: (i32, i32, i32), _: (i32, i32, i32)) -> Option<(i32, i32, i32)> {
        None
    }
}

/// How wide a player is, in blocks. Vanilla's `EntityDimensions.scalable(0.6F,
/// 1.8F)` — the width, which no pose changes.
pub const PLAYER_WIDTH: f64 = 0.6;

/// How much of a player, upwards from their feet, is checked whatever else is
/// true.
///
/// The shortest a player can be — [`Pose::Swimming`]'s 0.6 — and also vanilla's
/// own step height, which makes the floor of this check sayable in one
/// sentence: **a player may not put their feet somewhere they could not have
/// stepped.** The rest of their height is checked on top of this and can be
/// given up; this part never is. See [`Movement::measured_height`] for what
/// decides the rest, and [`Movement::claimed`] for why the two are asked
/// separately.
pub const FOOT_HEIGHT: f64 = 0.6;

/// How far inside a face a player has to be before they count as inside it.
///
/// A client that has resolved its own collision leaves itself against the face,
/// within about `1e-7` of it, and a player who is *exactly* against a wall is
/// not in it. A millimetre is four orders of magnitude more room than that
/// needs, and it costs at most one tick of detection: a player walking into a
/// wall at 0.216 blocks a tick is a millimetre in for a fiftieth of a tick.
const SKIN: f64 = 1.0e-3;

/// The furthest a sample may be from the one before it, in blocks on any axis.
///
/// A player box is 0.6 wide, so a full cube can hide strictly between two boxes
/// only if they are more than 1.6 apart on some axis; sampling at 1.0 leaves
/// six tenths of a block of margin and keeps the ordinary case — every step a
/// measured client takes is under 1.0 — at exactly one sample.
const SAMPLE_SPAN: f64 = 1.0;

/// The most samples one packet may be split into.
///
/// Reached only by a move the speed limit already allowed, which at the default
/// is fifty blocks after a five-tick gap, and only by an operator who set the
/// limit to `inf`. Past it the sampling is coarser than [`SAMPLE_SPAN`] and a
/// wall can be stepped over — a cheat that gets through, never an honest player
/// refused, which is the direction to be wrong in.
const MAX_SAMPLES: u32 = 64;

/// The cells a player `height` tall standing at `feet` is inside, inset by
/// [`SKIN`].
///
/// Inclusive on both ends. At most two cells across on x and z, and as many
/// high as the height reaches — one or two for a foot box, two or three for a
/// standing one. That count is the cost of the whole check: every cell in here
/// is a block state read.
#[must_use]
fn cells(feet: (f64, f64, f64), height: f64) -> ((i32, i32, i32), (i32, i32, i32)) {
    let half = PLAYER_WIDTH / 2.0 - SKIN;
    let floor = |v: f64| v.floor() as i32;
    (
        (
            floor(feet.0 - half),
            floor(feet.1 + SKIN),
            floor(feet.2 - half),
        ),
        (
            floor(feet.0 + half),
            floor(feet.1 + height - SKIN),
            floor(feet.2 + half),
        ),
    )
}

/// Whether a player `height` tall standing at `feet` is inside solid ground.
#[must_use]
fn inside(
    world: &mut impl Solidity,
    feet: (f64, f64, f64),
    height: f64,
) -> Option<(i32, i32, i32)> {
    let (lo, hi) = cells(feet, height);
    world.first_solid(lo, hi)
}

/// The furthest from the origin, on any axis, a position may claim to be.
///
/// Minecraft's own `MAX_LEVEL_SIZE`. Past it a player is outside every world
/// that can exist, and the arithmetic that turns a position into a chunk column
/// stops meaning anything. A coordinate this large is malformed rather than
/// fast, and is refused whatever the speed limit says.
pub const WORLD_LIMIT: f64 = 3.0e7;

/// What the server decided about a position a client claimed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Claim {
    /// Believe it. The player is there now.
    Accepted,
    /// Drop it and say nothing. The server has already told this client where
    /// it is and is waiting to be told the client agrees; the packets still in
    /// flight from before that describe a player who no longer exists, and
    /// answering each of them with another correction is how a rubber-band
    /// becomes a loop.
    Ignored,
    /// Refuse it, and put the player back. See [`Movement::correct`].
    Refused(Refusal),
}

/// Why a claimed position was refused.
///
/// Carries the numbers rather than a message so that the caller logging a
/// refusal can say how far the claim was and how far it was allowed to be —
/// a refusal without a number is a refusal nobody can tune.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Refusal {
    /// A coordinate that is not a finite number. Nothing downstream survives
    /// one: a NaN position compares false against every bound, and casting it
    /// to a chunk coordinate quietly produces zero.
    NotFinite,
    /// A coordinate outside every world that can exist. See [`WORLD_LIMIT`].
    OutOfWorld,
    /// Further in the time available than a player can move.
    TooFast {
        /// The squared distance claimed.
        moved_squared: f64,
        /// The squared distance the elapsed ticks allowed.
        allowed_squared: f64,
    },
    /// Into a block a player cannot be inside, from somewhere they were not
    /// already inside one. See [`Movement::claimed`].
    IntoSolid {
        /// The block they walked into. One of them, where the box they claimed
        /// covers several — enough to log, not a list.
        block: (i32, i32, i32),
    },
}

/// Where a player is, as opposed to where they say they are.
///
/// # What this is for
///
/// A movement packet is a claim, and until this existed the server believed
/// every one of them — which meant the position the reach check measured from
/// was whatever the client last asserted, and [`Reach`]'s own documentation
/// says so. This is the other half: a player who claims to be somewhere they
/// could not have travelled to is put back where they were.
///
/// # Being invisible to an honest player
///
/// This is the half that matters, and it is the harder one. A correction that
/// fires on a laggy connection has made the game worse than no check at all, so
/// the rules here are deliberately loose in the player's favour in three
/// separate places: the budget is floored at a full tick so that bunched-up
/// packets from a stalled connection are not refused for arriving early
/// ([`SpeedLimit::MAX_TICKS`]); the limit itself is nearly three times the
/// fastest thing a measured client produces ([`SpeedLimit`]); and a client that
/// never answers a correction is un-frozen by the first legal position it sends
/// rather than being held forever on a lost packet ([`Movement::claimed`]).
///
/// # Cost
///
/// One of these per player, sixteen bytes of state plus the limit, and
/// [`claimed`](Self::claimed) is a dozen floating-point operations with no
/// square root, no allocation and no lookup. It runs about twenty times a
/// second per player, which is the reason it is written that way.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Movement {
    limit: SpeedLimit,
    at: (f64, f64, f64),
    /// The teleport this client has not acknowledged yet, and where it put
    /// them. `None` is the ordinary state: a player who is where they say
    /// they are never has one of these.
    awaiting: Option<(i32, (f64, f64, f64))>,
    /// What the client has said about its own shape. Five bits; see
    /// [`Posture`].
    posture: Posture,
}

impl Movement {
    /// A player who has just arrived at `at`.
    #[must_use]
    pub fn new(limit: SpeedLimit, at: (f64, f64, f64)) -> Self {
        Self {
            limit,
            at,
            awaiting: None,
            posture: Posture::default(),
        }
    }

    /// What this client has last said about its own shape.
    ///
    /// A movement packet's on-ground flag belongs here too, and every one of
    /// them should set it before the position is judged — a client that says
    /// it is airborne is measured differently from one that says it is
    /// standing. See [`Movement::measured_height`].
    pub fn posture(&mut self, posture: Posture) {
        self.posture = posture;
    }

    /// The pose the client's own signals describe.
    ///
    /// This is what a reach check measures the eye height from — see
    /// [`eye_of`] — and it is deliberately **not** the height the collision
    /// check uses, which can be shorter than this and never taller. A reach
    /// that guessed a player shorter than they are would refuse them for
    /// looking up.
    #[must_use]
    pub fn pose(&self) -> Pose {
        self.posture.pose()
    }

    /// Where the server believes this player is.
    ///
    /// This is what the reach check measures from, and after a refusal it is
    /// the last position the player was legitimately at rather than the one
    /// they just claimed.
    #[must_use]
    pub fn at(self) -> (f64, f64, f64) {
        self.at
    }

    /// Whether this player is waiting to acknowledge a teleport.
    #[must_use]
    pub fn settled(self) -> bool {
        self.awaiting.is_none()
    }

    /// Judge a claimed position, `ticks` ticks after the last one was judged.
    ///
    /// An [`Accepted`](Claim::Accepted) claim has already been stored: `at`
    /// returns the new position on return.
    ///
    /// # The collision rule, and where the line is drawn
    ///
    /// **A player may not move from outside solid ground to inside it. A player
    /// already inside it may move anywhere.**
    ///
    /// That second sentence is the whole design, and it is not a loophole. A
    /// player legitimately ends up inside a block often: somebody places one on
    /// them, they spawn in terrain, a chunk arrives late, one day a piston
    /// pushes them. Every one of those resolves by the player *moving out*, and
    /// a rule that refused a move because it started inside a block would hold
    /// them there for as long as they were unlucky. So being inside is never
    /// itself refused — only crossing in from outside is, and that is the one
    /// thing an honest client cannot do, because its own collision stopped it.
    ///
    /// The two questions are asked of the world as it is *now*, not remembered
    /// from the last packet: a block placed into a standing player changes the
    /// answer to "were they inside one" from no to yes, and a remembered answer
    /// would refuse them for the next thing they did.
    ///
    /// A move long enough to step over a block is sampled — see
    /// [`SAMPLE_SPAN`] — so a claim that jumps a wall is judged at the points
    /// between as well as at its end.
    ///
    /// # How much of a player is measured
    ///
    /// All of them, up to the height of the pose the client's own signals
    /// describe — see [`Pose`] and [`Movement::measured_height`]. That is what
    /// closes the hole a foot-high box left open, where a client could put its
    /// head through a wall while its feet stood in a legal cell.
    ///
    /// The bottom [`FOOT_HEIGHT`] of a player is asked about separately when
    /// the taller question says "already inside", because a head in a low
    /// ceiling is an entirely ordinary player and must not be a licence to
    /// walk the rest of them through a wall. [`walked_into`](Self::walked_into)
    /// spells the order out.
    ///
    /// # What this deliberately allows
    ///
    /// Standing on a stair, a slab, a farmland block or soul sand: none of
    /// those is a full cube. Crawling through a one-block gap, because a
    /// player already inside a ceiling at their full height is believed.
    /// Swimming, because a sprinting player who says they are airborne is
    /// measured at their feet and this server cannot see water. Riding, being
    /// pushed and being knocked back, which no part of this server does yet,
    /// will all be moves the player did not make and will need a way to say so
    /// before they exist. And a chunk the server has not loaded is not solid,
    /// so a player walking into unloaded ground is believed.
    ///
    /// # Cost
    ///
    /// One [`Solidity::first_solid`] call over a box of at most twelve cells
    /// for a step under a block long, which is every step a measured client
    /// takes. The further calls, over the position they came from and over the
    /// player's feet, are only made when the first one found something — which
    /// for a player in the open is never.
    pub fn claimed(&mut self, to: (f64, f64, f64), ticks: u32, world: &mut impl Solidity) -> Claim {
        if !to.0.is_finite() || !to.1.is_finite() || !to.2.is_finite() {
            return Claim::Refused(Refusal::NotFinite);
        }
        if to.0.abs() > WORLD_LIMIT || to.1.abs() > WORLD_LIMIT || to.2.abs() > WORLD_LIMIT {
            return Claim::Refused(Refusal::OutOfWorld);
        }
        if let Some((_, target)) = self.awaiting {
            // The client is where the correction put it, or near enough to
            // have walked there — so it honoured the teleport and the
            // acknowledgement is late or lost. Freezing a player for the rest
            // of their session over a missing packet is a far worse outcome
            // than trusting a position that would have been accepted anyway,
            // and this cannot be exploited to skip the check: it accepts only
            // what the check already allows.
            //
            // The collision rule still applies, and has to. Without it a
            // player refused for walking into a wall could answer the
            // correction with a position *inside* the wall — one step, well
            // within the budget from where they were put — and arrive at the
            // one state this check never refuses: already inside. Every road
            // into solid ground is the same road.
            if distance_squared(target, to) <= self.limit.budget_squared(ticks) {
                let back = Self {
                    limit: self.limit,
                    at: target,
                    awaiting: None,
                    posture: self.posture,
                };
                if back.walked_into(to, world).is_none() {
                    self.awaiting = None;
                    self.at = to;
                    return Claim::Accepted;
                }
            }
            return Claim::Ignored;
        }
        let moved_squared = distance_squared(self.at, to);
        let allowed_squared = self.limit.budget_squared(ticks);
        if moved_squared > allowed_squared {
            return Claim::Refused(Refusal::TooFast {
                moved_squared,
                allowed_squared,
            });
        }
        if let Some(block) = self.walked_into(to, world) {
            return Claim::Refused(Refusal::IntoSolid { block });
        }
        self.at = to;
        Claim::Accepted
    }

    /// How much of this player, upwards from their feet, the collision check
    /// measures.
    ///
    /// Never taller than the pose the client's own signals describe, and it is
    /// the two places it is *shorter* that decide whether an honest player is
    /// ever refused. Both are stated as permissions rather than discovered as
    /// bugs:
    ///
    /// **A player who says they are not on the ground, while sprinting, is
    /// measured at [`FOOT_HEIGHT`].** That is vanilla's own swimming
    /// condition — `isSprinting() && isInWater()` — with the water left out,
    /// because this server does not read fluids on the movement path and
    /// cannot. A swimmer is 0.6 tall and a client never says so, so the choice
    /// is between believing a sprinting airborne player is short and
    /// rubber-banding every player who swims through a one-block gap in a
    /// ravine or a kelp forest. It costs a cheat one bit — set the on-ground
    /// flag false and hold sprint — and that cheat is exactly what every
    /// client could already do before any of this existed, so the check is
    /// still strictly a gain. Note what it does *not* give away: the feet are
    /// checked whatever this returns.
    ///
    /// **A player whose taller box does not fit where they already are is
    /// measured shorter**, and that is not here — it falls out of
    /// [`claimed`](Self::claimed)'s already-inside rule for free, because a
    /// crawler in a one-block tunnel has a standing box that is inside the
    /// ceiling at both ends of every move they make. Vanilla does the same
    /// thing explicitly, in `updatePlayerPose`: if the pose does not fit, try
    /// crouching, then the 0.6 box.
    #[must_use]
    pub fn measured_height(&self) -> f64 {
        if self.posture.sprinting && !self.posture.on_ground {
            return FOOT_HEIGHT;
        }
        self.posture.pose().height().max(FOOT_HEIGHT)
    }

    /// The block a move from `at` to `to` puts the player inside, having not
    /// been inside one to start with.
    ///
    /// # Two boxes, asked in order, and why not one
    ///
    /// The whole player is asked about first. If nothing is in that box there
    /// is nothing more to ask, and that is the case every walking player in
    /// the open is in — **one world question, the same as before pose
    /// existed.**
    ///
    /// When something *is* in it, the same box is asked about where the player
    /// came from, and a player who was already inside one is believed. That
    /// pair is the rule this crate has always had, now applied to the player's
    /// real height rather than to their bottom 0.6.
    ///
    /// But "already inside" cannot be allowed to mean "and therefore anything
    /// goes". A player with their head in a low ceiling is a completely
    /// ordinary player — under a slab, in a cave, on a staircase — and if that
    /// state licensed the rest of them to walk through a wall, then standing
    /// under an overhang would be a cheat's front door. So when the tall pair
    /// says "already inside", the [`FOOT_HEIGHT`] pair is asked as well, and a
    /// player who walks their *feet* into a block is refused however blocked
    /// their head was. Four world questions at the very worst, for a player
    /// who is genuinely stuck inside terrain, and one for everybody else.
    ///
    /// # The sampling
    ///
    /// By the largest single-axis displacement rather than by the distance,
    /// and that is not an approximation of the distance: what has to stay
    /// under a block's width is the step on each axis, and the largest of the
    /// three is exactly what bounds all of them.
    fn walked_into(
        &self,
        to: (f64, f64, f64),
        world: &mut impl Solidity,
    ) -> Option<(i32, i32, i32)> {
        let height = self.measured_height();
        let from = self.at;
        let d = (to.0 - from.0, to.1 - from.1, to.2 - from.2);
        let span = d.0.abs().max(d.1.abs()).max(d.2.abs());
        let samples = ((span / SAMPLE_SPAN).ceil() as u32).clamp(1, MAX_SAMPLES);
        // Where the player came from does not change between samples, so
        // neither do these two answers. Asked at most once each, and only
        // once a sample has found something — a player walking in the open
        // never asks either.
        let mut was_blocked: Option<bool> = None;
        let mut feet_were_blocked: Option<bool> = None;
        for i in 1..=samples {
            let t = f64::from(i) / f64::from(samples);
            let at = (from.0 + d.0 * t, from.1 + d.1 * t, from.2 + d.2 * t);
            let Some(block) = inside(world, at, height) else {
                continue;
            };
            let blocked = *was_blocked
                .get_or_insert_with(|| inside(world, from, height).is_some());
            if !blocked {
                // Somewhere clear, into somewhere that is not. Refused.
                return Some(block);
            }
            // Already inside something at their full height, and on their way
            // out — believed, *unless* the part of them that was clear is the
            // part that just went into a block. See this method's own note.
            if height <= FOOT_HEIGHT {
                continue;
            }
            let Some(feet) = inside(world, at, FOOT_HEIGHT) else {
                continue;
            };
            let feet_blocked = *feet_were_blocked
                .get_or_insert_with(|| inside(world, from, FOOT_HEIGHT).is_some());
            if !feet_blocked {
                return Some(feet);
            }
        }
        None
    }

    /// Start correcting a player, and say where to put them.
    ///
    /// The caller sends that position to the client as a teleport carrying
    /// `teleport_id` — a real correction the client honours, not a log line —
    /// and every position packet until the client acknowledges that id is
    /// [`Ignored`](Claim::Ignored).
    pub fn correct(&mut self, teleport_id: i32) -> (f64, f64, f64) {
        self.awaiting = Some((teleport_id, self.at));
        self.at
    }

    /// A client acknowledging a teleport. True if it is the one being waited
    /// for.
    ///
    /// A stale id is not an error: a client acknowledges every teleport it is
    /// sent, including the one that placed it on join, so most of these are
    /// about nothing.
    pub fn confirmed(&mut self, teleport_id: i32) -> bool {
        if self.awaiting.is_some_and(|(id, _)| id == teleport_id) {
            self.awaiting = None;
            true
        } else {
            false
        }
    }
}

/// Squared distance between two positions. No square root: nothing here wants
/// the distance itself, and the comparison is the same comparison squared.
fn distance_squared(from: (f64, f64, f64), to: (f64, f64, f64)) -> f64 {
    let dx = to.0 - from.0;
    let dy = to.1 - from.1;
    let dz = to.2 - from.2;
    dx * dx + dy * dy + dz * dz
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Standing at the origin, eyes at 1.62.
    fn eye() -> (f64, f64, f64) {
        eye_of((0.5, 0.0, 0.5), Pose::Standing)
    }

    #[test]
    fn the_block_underfoot_is_measured_from_the_eye_and_not_from_the_feet() {
        // 1.62 away, not zero: the block a player is standing on is a whole
        // eye-height below their eyes, and this is the case that says the check
        // is not quietly measuring from the feet. It also sets the floor for
        // any usable limit — a server configured under 1.63 could not break the
        // ground it was standing on.
        let d = Reach::new(1.0).distance_squared(eye(), (0, -1, 0));
        assert!((d - EYE_HEIGHT * EYE_HEIGHT).abs() < 1e-9, "{d}");
        assert!(!Reach::new(1.0).allows(eye(), (0, -1, 0)));
        assert!(Reach::new(2.0).allows(eye(), (0, -1, 0)));
    }

    #[test]
    fn distance_is_to_the_nearest_face_and_not_to_the_centre() {
        // The block ten east. Its nearest face is at x = 10, and the eye is at
        // x = 0.5, so the distance is 9.5 — not 10.5 to its centre and not
        // 11.0 to its far corner. A limit of 10 therefore allows it and a limit
        // of 9 does not.
        let far = (10, 1, 0);
        assert!(Reach::new(10.0).allows(eye(), far));
        assert!(!Reach::new(9.0).allows(eye(), far));
        let d = Reach::new(1.0).distance_squared(eye(), far);
        // dx 9.5, dy 1.62 - 1 = 0 (the eye is inside the cube's y range),
        // dz 0.5 - 1 = 0 (likewise for z, since the cube spans 0..1).
        assert!((d - 9.5 * 9.5).abs() < 1e-9, "{d}");
    }

    #[test]
    fn a_player_inside_a_block_is_zero_from_it() {
        // Both `max` terms are negative on every axis, which is the case the
        // `.max(0.0)` exists for — without it the distance would come out
        // negative on one axis and the squares would still be positive, which
        // is a wrong answer that looks plausible.
        let reach = Reach::new(0.5);
        assert_eq!(reach.distance_squared(eye(), (0, 1, 0)), 0.0);
        assert!(reach.allows(eye(), (0, 1, 0)));
    }

    #[test]
    fn the_limit_is_exclusive_at_exactly_the_limit() {
        // Vanilla compares `<`, and matching it matters only at the boundary —
        // but the boundary is where every reach complaint comes from.
        let reach = Reach::new(4.0);
        let eye = (0.0, 0.0, 0.0);
        assert!(!reach.allows(eye, (4, 0, 0)), "exactly 4.0 away");
        assert!(reach.allows(eye, (3, 0, 0)), "3.0 away");
    }

    #[test]
    fn distance_is_symmetric_in_every_direction() {
        // The `max` pair is the one place a sign can be wrong, and it is wrong
        // in only one direction when it is — so the block west has to be as
        // far as the block east.
        let reach = Reach::new(100.0);
        let eye = (0.5, 0.5, 0.5);
        let east = reach.distance_squared(eye, (7, 0, 0));
        let west = reach.distance_squared(eye, (-7, 0, 0));
        assert!((east - west).abs() < 1e-9, "east {east}, west {west}");
        let up = reach.distance_squared(eye, (0, 7, 0));
        let down = reach.distance_squared(eye, (0, -7, 0));
        assert!((up - down).abs() < 1e-9, "up {up}, down {down}");
    }

    #[test]
    fn a_block_across_the_map_is_refused() {
        // The whole point, stated as the README states it.
        let reach = Reach::new(6.0);
        assert!(!reach.allows(eye(), (500, 64, -500)));
    }

    /// The limit `dust-config` ships. Every movement test below uses it, so a
    /// change to the default is a change these tests argue with.
    const DEFAULT: f64 = 10.0;

    fn walker() -> Movement {
        Movement::new(SpeedLimit::new(DEFAULT), (0.0, 64.0, 0.0))
    }

    #[test]
    fn every_step_a_measured_client_took_is_accepted() {
        // The per-phase maxima out of `tools/bot/movement.js`, in blocks moved
        // in one packet: walking, sprinting, sprint-jumping, creative flight
        // up, creative flight forward, and a 300-block free fall. 1,217
        // packets produced these six numbers and nothing larger.
        //
        // This is the half of the check that matters. A validator that refuses
        // any of these has made the game worse for somebody who did nothing
        // wrong, and there is no threshold worth that.
        for step in [0.216, 0.281, 0.742, 1.000, 0.283, 3.580] {
            let mut player = walker();
            let to = (0.0, 64.0 - step, 0.0);
            assert_eq!(
                player.claimed(to, 1, &mut Open),
                Claim::Accepted,
                "a measured client moved {step} blocks in one tick and was refused"
            );
            assert_eq!(player.at(), to);
        }
    }

    #[test]
    fn free_falls_asymptote_is_inside_the_limit_with_room_over() {
        // The measured 3.580 was still accelerating; a fall long enough
        // converges on 3.92 blocks a tick, which is where drag balances
        // gravity. That is the real ceiling on honest movement in a server
        // with no elytra, and the limit has to clear it by enough that adding
        // elytra later is a decision rather than an emergency.
        let mut player = walker();
        assert_eq!(
            player.claimed((0.0, 64.0 - 3.92, 0.0), 1, &mut Open),
            Claim::Accepted
        );
        let headroom = DEFAULT / 3.92;
        assert!(headroom > 2.5, "only {headroom}x over free fall");
    }

    #[test]
    fn a_teleport_across_the_map_is_refused() {
        // The cheat the README names, and the one shape of it this can see.
        let mut player = walker();
        let Claim::Refused(Refusal::TooFast {
            moved_squared,
            allowed_squared,
        }) = player.claimed((500.0, 64.0, 500.0), 1, &mut Open)
        else {
            panic!("a 707-block step was not refused");
        };
        assert!(moved_squared > allowed_squared);
        // And the player is still where they were, not where they claimed.
        assert_eq!(player.at(), (0.0, 64.0, 0.0));
    }

    #[test]
    fn a_bunched_up_client_is_not_refused_for_arriving_early() {
        // The stall case, which is the one an over-tuned validator gets wrong.
        // A connection that stops for 700 ms does not produce one big step: the
        // client keeps ticking, the packets queue, and then fourteen of them
        // arrive inside the same tick, each carrying the 0.216 blocks a walking
        // player covers. Zero elapsed ticks has to buy a whole tick of room or
        // every one of them after the first is a refusal.
        let mut player = walker();
        let mut y = 64.0;
        for _ in 0..14 {
            y -= 0.216;
            assert_eq!(player.claimed((0.0, y, 0.0), 0, &mut Open), Claim::Accepted);
        }
    }

    #[test]
    fn a_pause_buys_more_room_but_only_a_quarter_second_of_it() {
        // Five ticks of budget is five ticks of distance — 50 blocks, not the
        // 22.4 a linear scaling of the squared limit would give. And a gap of a
        // second buys no more than a gap of a quarter, because past that the
        // honest explanation is a queue of packets rather than one large one.
        let mut player = walker();
        assert_eq!(
            player.claimed((49.0, 64.0, 0.0), 5, &mut Open),
            Claim::Accepted
        );
        let mut player = walker();
        assert!(matches!(
            player.claimed((51.0, 64.0, 0.0), 5, &mut Open),
            Claim::Refused(Refusal::TooFast { .. })
        ));
        let mut player = walker();
        assert!(
            matches!(
                player.claimed((51.0, 64.0, 0.0), 200, &mut Open),
                Claim::Refused(Refusal::TooFast { .. })
            ),
            "a four-second gap bought more than the clamp allows"
        );
    }

    #[test]
    fn a_position_that_is_not_a_number_is_refused_on_every_axis() {
        // Not a speed problem. A NaN compares false against every bound it is
        // measured with, so a claim carrying one passes a check written the
        // obvious way — and then casts to chunk zero.
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            for axis in 0..3 {
                let mut to = (0.0, 64.0, 0.0);
                match axis {
                    0 => to.0 = bad,
                    1 => to.1 = bad,
                    _ => to.2 = bad,
                }
                let mut player = walker();
                assert_eq!(
                    player.claimed(to, 1, &mut Open),
                    Claim::Refused(Refusal::NotFinite),
                    "{bad} on axis {axis}"
                );
            }
        }
    }

    #[test]
    fn a_position_outside_every_world_is_refused_however_long_it_took() {
        let mut player = Movement::new(SpeedLimit::new(DEFAULT), (WORLD_LIMIT - 1.0, 64.0, 0.0));
        assert_eq!(
            player.claimed((WORLD_LIMIT + 1.0, 64.0, 0.0), 5, &mut Open),
            Claim::Refused(Refusal::OutOfWorld),
            "two blocks east of the edge of the world is still off the map"
        );
    }

    #[test]
    fn an_unlimited_speed_still_refuses_a_position_that_is_not_a_number() {
        // Turning the speed bound off is a legitimate thing for an operator to
        // want and a malformed coordinate is not a speed.
        let mut player = Movement::new(SpeedLimit::new(f64::INFINITY), (0.0, 64.0, 0.0));
        assert_eq!(
            player.claimed((1e6, 64.0, 1e6), 1, &mut Open),
            Claim::Accepted
        );
        assert_eq!(
            player.claimed((f64::NAN, 64.0, 0.0), 1, &mut Open),
            Claim::Refused(Refusal::NotFinite)
        );
    }

    #[test]
    fn a_correction_is_not_answered_with_another_correction() {
        // The packets already in flight when the correction went out describe a
        // player who no longer exists. Refusing each of them sends another
        // teleport, which the client answers with more stale packets: that is
        // how a rubber-band becomes a loop, and it is what `Ignored` exists to
        // stop.
        let mut player = walker();
        assert!(matches!(
            player.claimed((500.0, 64.0, 500.0), 1, &mut Open),
            Claim::Refused(_)
        ));
        let back = player.correct(7);
        assert_eq!(back, (0.0, 64.0, 0.0));
        assert!(!player.settled());
        for _ in 0..5 {
            assert_eq!(
                player.claimed((501.0, 64.0, 501.0), 1, &mut Open),
                Claim::Ignored
            );
        }
        assert!(player.confirmed(7));
        assert!(player.settled());
        assert_eq!(player.at(), (0.0, 64.0, 0.0));
    }

    #[test]
    fn acknowledging_some_other_teleport_changes_nothing() {
        // A client acknowledges every teleport it is sent, the one that placed
        // it on join included, so most of these are about nothing.
        let mut player = walker();
        player.correct(7);
        assert!(!player.confirmed(1));
        assert!(!player.settled());
        assert_eq!(
            player.claimed((500.0, 64.0, 500.0), 1, &mut Open),
            Claim::Ignored
        );
    }

    #[test]
    fn a_lost_acknowledgement_does_not_freeze_a_player_forever() {
        // The client honoured the teleport — it is standing where it was put —
        // and the packet saying so never arrived. Holding that player still for
        // the rest of their session is a worse outcome than believing a
        // position that passes the check on its own merits, which this one
        // does.
        let mut player = walker();
        player.correct(7);
        assert_eq!(
            player.claimed((0.3, 64.0, 0.0), 1, &mut Open),
            Claim::Accepted
        );
        assert!(player.settled());
        assert_eq!(player.at(), (0.3, 64.0, 0.0));
    }

    #[test]
    fn a_speed_limit_that_is_not_a_speed_refuses_everything() {
        for bad in [0.0, -1.0, f64::NAN] {
            let mut player = Movement::new(SpeedLimit::new(bad), (0.0, 64.0, 0.0));
            assert!(
                matches!(
                    player.claimed((0.001, 64.0, 0.0), 1, &mut Open),
                    Claim::Refused(_)
                ),
                "a limit of {bad} allowed a millimetre"
            );
        }
    }

    #[test]
    fn the_budget_grows_as_the_square_of_the_ticks() {
        // Distance is speed times time and this is a squared quantity, so two
        // ticks is four times the budget and not two. Stated as a test because
        // vanilla's own check gets this wrong in the tighter direction and it
        // would be easy to copy.
        let limit = SpeedLimit::new(DEFAULT);
        assert!((limit.budget_squared(1) - 100.0).abs() < 1e-9);
        assert!((limit.budget_squared(2) - 400.0).abs() < 1e-9);
        assert!((limit.budget_squared(0) - limit.budget_squared(1)).abs() < 1e-9);
        assert!(
            (limit.budget_squared(99) - limit.budget_squared(SpeedLimit::MAX_TICKS)).abs() < 1e-9
        );
    }

    /// A world made of whichever cells the test names, and a count of how many
    /// boxes it was asked about — which is what the cost claims in
    /// [`Movement::claimed`] are checked against.
    #[derive(Default)]
    struct Ground {
        solid: std::collections::HashSet<(i32, i32, i32)>,
        asked: u32,
    }

    impl Ground {
        /// A floor at y = -1 and nothing else, from -8 to 8 on both axes.
        fn floor() -> Self {
            let mut world = Self::default();
            for x in -8..8 {
                for z in -8..8 {
                    world.solid.insert((x, -1, z));
                }
            }
            world
        }

        /// A wall two blocks tall across x = 2.
        fn with_wall(mut self) -> Self {
            for z in -8..8 {
                for y in 0..2 {
                    self.solid.insert((2, y, z));
                }
            }
            self
        }
    }

    impl Solidity for Ground {
        fn first_solid(
            &mut self,
            lo: (i32, i32, i32),
            hi: (i32, i32, i32),
        ) -> Option<(i32, i32, i32)> {
            self.asked += 1;
            for y in lo.1..=hi.1 {
                for z in lo.2..=hi.2 {
                    for x in lo.0..=hi.0 {
                        if self.solid.contains(&(x, y, z)) {
                            return Some((x, y, z));
                        }
                    }
                }
            }
            None
        }
    }

    /// A player standing on the floor at the origin.
    fn stander() -> Movement {
        Movement::new(SpeedLimit::new(DEFAULT), (0.5, 0.0, 0.5))
    }

    #[test]
    fn a_player_walking_into_a_wall_is_refused_at_the_face() {
        // The defect this exists for, at the pace it is actually done at: a
        // walking client, 0.216 blocks a tick, straight at a wall. It is
        // believed for every step up to the face and refused by the one that
        // puts a foot through it.
        let mut world = Ground::floor().with_wall();
        let mut player = stander();
        let mut x = 0.5;
        let mut refused = None;
        for _ in 0..20 {
            x += 0.216;
            match player.claimed((x, 0.0, 0.5), 1, &mut world) {
                Claim::Accepted => {}
                Claim::Refused(Refusal::IntoSolid { block }) => {
                    refused = Some((x, block));
                    break;
                }
                other => panic!("{other:?} at x {x}"),
            }
        }
        let Some((at, block)) = refused else {
            panic!("walked all the way through a wall at a walking pace");
        };
        assert_eq!(block, (2, 0, 0), "refused for the wrong block");
        // The wall's west face is at x = 2 and the player is 0.6 wide, so the
        // first position with a foot in it is just past 1.7. Nothing before
        // that was refused, which is the half that matters.
        assert!((1.7..1.95).contains(&at), "refused at {at}");
        // And they are left standing where they legitimately got to, not
        // where they claimed and not back at the origin.
        assert!(
            player.at().0 < 2.0 && player.at().0 > 1.4,
            "{:?}",
            player.at()
        );
    }

    #[test]
    fn the_same_walk_with_no_world_is_not_refused() {
        // The negative control for the test above, in the form the brief asks
        // for: the same twenty steps against a world with nothing solid in it
        // go all the way through. If `Open` ever grew an opinion this goes red.
        let mut player = stander();
        let mut x = 0.5;
        for _ in 0..20 {
            x += 0.216;
            assert_eq!(player.claimed((x, 0.0, 0.5), 1, &mut Open), Claim::Accepted);
        }
        assert!(player.at().0 > 4.0, "{:?}", player.at());
    }

    #[test]
    fn a_player_may_walk_out_of_a_block_they_are_already_inside() {
        // Somebody placed a block on them, or they spawned in terrain. The
        // rule is that being inside is never itself refused — including the
        // move that takes them further in, because a player who has to pick
        // the right direction to be believed is a player being punished for
        // somebody else's block.
        let mut world = Ground::floor();
        world.solid.insert((0, 0, 0));
        let mut player = Movement::new(SpeedLimit::new(DEFAULT), (0.5, 0.0, 0.5));
        assert_eq!(
            player.claimed((0.4, 0.0, 0.5), 1, &mut world),
            Claim::Accepted
        );
        assert_eq!(
            player.claimed((0.2, 0.0, 0.5), 1, &mut world),
            Claim::Accepted
        );
        // And out the west side, which is where they were heading.
        assert_eq!(
            player.claimed((-0.4, 0.0, 0.5), 1, &mut world),
            Claim::Accepted
        );
        assert_eq!(player.at(), (-0.4, 0.0, 0.5));
    }

    #[test]
    fn a_block_placed_onto_a_standing_player_does_not_freeze_them() {
        // The same case reached the way it actually happens: the player was
        // outside solid ground last packet and the *world* changed, not them.
        // The check asks the world as it is now on both sides, so the position
        // they came from is inside too and nothing is refused. A remembered
        // answer would refuse this.
        let mut world = Ground::floor();
        let mut player = stander();
        assert_eq!(
            player.claimed((0.5, 0.0, 0.6), 1, &mut world),
            Claim::Accepted
        );
        world.solid.insert((0, 0, 0));
        assert_eq!(
            player.claimed((0.5, 0.0, 0.7), 1, &mut world),
            Claim::Accepted
        );
    }

    #[test]
    fn standing_on_the_floor_is_not_standing_in_it() {
        // Feet exactly on the top face of a block, which is where a client's
        // own collision leaves them and therefore what nearly every packet
        // this check ever sees looks like. Touching is not inside.
        let mut world = Ground::floor();
        let mut player = stander();
        for step in [0.216, 0.281, 0.742] {
            let to = (player.at().0 + step, 0.0, 0.5);
            assert_eq!(
                player.claimed(to, 1, &mut world),
                Claim::Accepted,
                "a {step} block step along the floor was refused"
            );
        }
    }

    #[test]
    fn a_jump_over_a_wall_is_believed_and_a_dash_through_it_is_not() {
        // Two claims of the same length either side of the same wall. The one
        // that goes over the top is believed; the one that goes through is
        // refused by a sample between the ends, which is the whole reason the
        // long ones are sampled at all.
        let mut world = Ground::floor().with_wall();
        let mut over = Movement::new(SpeedLimit::new(DEFAULT), (0.5, 2.0, 0.5));
        assert_eq!(
            over.claimed((4.5, 2.0, 0.5), 1, &mut world),
            Claim::Accepted
        );
        let mut through = stander();
        assert!(
            matches!(
                through.claimed((4.5, 0.0, 0.5), 1, &mut world),
                Claim::Refused(Refusal::IntoSolid { .. })
            ),
            "a four-block dash straight through a wall was believed"
        );
    }

    #[test]
    fn a_correction_cannot_be_answered_with_a_position_inside_the_wall() {
        // The exploit the recovery path would otherwise have. Walk into the
        // wall, get put back, and then answer the teleport with a position in
        // the wall — one step, well inside the speed budget from where the
        // correction put them. Accepting it would land the player in the one
        // state this check never refuses, and every subsequent step through
        // the wall would be believed.
        let mut world = Ground::floor().with_wall();
        let mut player = Movement::new(SpeedLimit::new(DEFAULT), (1.5, 0.0, 0.5));
        assert!(matches!(
            player.claimed((2.5, 0.0, 0.5), 1, &mut world),
            Claim::Refused(Refusal::IntoSolid { .. })
        ));
        player.correct(4);
        assert_eq!(
            player.claimed((2.5, 0.0, 0.5), 1, &mut world),
            Claim::Ignored
        );
        assert!(
            !player.settled(),
            "a position in the wall settled the player"
        );
        // A legal one still clears the correction, which is the behaviour the
        // recovery path exists for and which this must not have broken.
        assert_eq!(
            player.claimed((1.4, 0.0, 0.5), 1, &mut world),
            Claim::Accepted
        );
        assert!(player.settled());
    }

    #[test]
    fn ground_the_server_does_not_have_is_not_solid() {
        // `Ground` holds a floor from -8 to 8 and nothing beyond it, which is
        // what a server that has not loaded a chunk looks like from here. A
        // player walking off the edge of what is loaded is believed rather
        // than refused: an answer nobody has is not a refusal.
        let mut world = Ground::floor().with_wall();
        let mut player = Movement::new(SpeedLimit::new(DEFAULT), (0.5, 0.0, 20.5));
        assert_eq!(
            player.claimed((2.5, 0.0, 20.5), 1, &mut world),
            Claim::Accepted
        );
    }

    #[test]
    fn one_ordinary_step_asks_the_world_once() {
        // The cost claim, as a test rather than as a sentence. A player in the
        // open costs one box question per packet — the second one is only
        // asked when the first found something, and for a player walking
        // across a floor it never does.
        let mut world = Ground::floor();
        let mut player = stander();
        world.asked = 0;
        for i in 1..=100 {
            let to = (0.5 + f64::from(i) * 0.216, 0.0, 0.5);
            assert_eq!(player.claimed(to, 1, &mut world), Claim::Accepted);
        }
        assert_eq!(world.asked, 100, "a step in the open asked more than once");
    }

    /// A ceiling of solid blocks at `y`, from -8 to 8 on both axes.
    fn roofed(mut world: Ground, y: i32) -> Ground {
        for x in -8..8 {
            for z in -8..8 {
                world.solid.insert((x, y, z));
            }
        }
        world
    }

    #[test]
    fn a_crawling_player_is_believed_for_every_packet_of_the_crawl() {
        // The permissive half, and the one that matters. A player in a
        // one-block gap — floor under them, ceiling on top of them — is 0.6
        // tall in vanilla and their client never says so. What says so here is
        // that their standing box is inside the ceiling at *both* ends of
        // every move they make, which is `claimed`'s already-inside rule doing
        // the job vanilla does with `updatePlayerPose`'s shrink-until-it-fits.
        let mut world = roofed(Ground::floor(), 1);
        let mut player = stander();
        for i in 1..=10 {
            let to = (0.5 + f64::from(i) * 0.216, 0.0, 0.5);
            assert_eq!(
                player.claimed(to, 1, &mut world),
                Claim::Accepted,
                "a crawling player was refused for having a ceiling"
            );
        }
    }

    #[test]
    fn a_crawling_player_may_not_put_their_feet_through_the_wall_of_the_tunnel() {
        // The other side of the rule above: being already inside something at
        // full height is not a licence to walk. A head in a ceiling is an
        // ordinary player; a body through a wall is not, and the feet are
        // checked whatever the head is doing.
        let mut world = roofed(Ground::floor().with_wall(), 1);
        let mut player = stander();
        let mut x = 0.5;
        let mut refused = None;
        for _ in 0..20 {
            x += 0.216;
            if let Claim::Refused(Refusal::IntoSolid { block }) =
                player.claimed((x, 0.0, 0.5), 1, &mut world)
            {
                refused = Some(block);
                break;
            }
        }
        assert_eq!(
            refused,
            Some((2, 0, 0)),
            "a player under a ceiling walked their feet into a wall"
        );
    }

    #[test]
    fn a_client_may_not_put_its_head_through_a_wall_while_its_feet_are_legal() {
        // The defect pose exists to close. A wall two blocks tall standing on
        // the floor, and a cheat that claims a position where the cell its
        // feet are in is open air and the cell its head is in is the wall.
        let mut world = Ground::floor();
        for z in -8..8 {
            for y in 1..3 {
                world.solid.insert((2, y, z));
            }
        }
        let mut player = stander();
        // Every step up to the face is believed: the wall starts a block above
        // the feet, so nothing at foot height ever refuses this.
        let mut x = 0.5;
        let mut refused = None;
        for _ in 0..20 {
            x += 0.216;
            if let Claim::Refused(Refusal::IntoSolid { block }) =
                player.claimed((x, 0.0, 0.5), 1, &mut world)
            {
                refused = Some((x, block));
                break;
            }
        }
        let (x, block) = refused.expect("a head walked through a wall unrefused");
        assert_eq!(block.0, 2, "refused by something that is not the wall");
        assert!(
            (1.7..1.92).contains(&x),
            "refused at {x}, and the first step past the face at 1.7 is 1.796"
        );
    }

    #[test]
    fn a_sprinting_airborne_player_is_measured_at_their_feet() {
        // The permission this server takes deliberately, because it cannot see
        // water: a sprinting player who says they are not on the ground may be
        // swimming, and a swimmer is 0.6 tall. Same wall, same claim, and the
        // one that says it is swimming through is believed.
        let mut world = Ground::floor();
        for z in -8..8 {
            for y in 1..3 {
                world.solid.insert((2, y, z));
            }
        }
        let mut player = stander();
        player.posture(Posture {
            sprinting: true,
            on_ground: false,
            ..Posture::default()
        });
        let mut x = 0.5;
        for _ in 0..20 {
            x += 0.216;
            assert_eq!(
                player.claimed((x, 0.0, 0.5), 1, &mut world),
                Claim::Accepted,
                "a swimmer was refused for a wall above their head"
            );
            if x > 1.9 {
                break;
            }
        }
        assert!(x > 1.9, "the walk stopped before it reached the wall");
    }

    #[test]
    fn a_crouching_player_is_a_foot_and_a_half_of_player() {
        // The number the reach check was getting wrong, and the height the
        // collision check now uses. Vanilla's own `Player.POSES`.
        assert!((Pose::Standing.height() - 1.8).abs() < 1e-9);
        assert!((Pose::Crouching.height() - 1.5).abs() < 1e-9);
        assert!((Pose::Standing.eye_height() - Pose::Crouching.eye_height() - 0.35).abs() < 1e-9);
        // And a crouching player really is measured shorter. An overhang one
        // block thick at y = 2, and a player part-way through a jump with
        // their feet at 0.4: standing they are 2.2 of the way up and inside
        // it, crouching they are 1.9 of the way up and clear of it. The step
        // into the overhang's column is refused for one of them and not for
        // the other, and nothing else about the two runs differs.
        for (posture, expected) in [
            (Posture::default(), Claim::Refused(Refusal::IntoSolid { block: (2, 2, 0) })),
            (
                Posture {
                    sneaking: true,
                    ..Posture::default()
                },
                Claim::Accepted,
            ),
        ] {
            let mut world = Ground::floor();
            for z in -8..8 {
                world.solid.insert((2, 2, z));
            }
            let mut player = Movement::new(SpeedLimit::new(DEFAULT), (1.5, 0.4, 0.5));
            player.posture(posture);
            assert_eq!(
                player.claimed((1.75, 0.4, 0.5), 1, &mut world),
                expected,
                "a player with {:?} stepping under an overhang 1.6 above their feet",
                posture.pose()
            );
        }
    }

    #[test]
    fn sneaking_while_flying_is_not_crouching() {
        // Vanilla's own condition. A creative player who holds shift while
        // flying goes down; they do not get shorter, and a server that thought
        // they did would measure their reach from 0.35 too low.
        let flying = Posture {
            sneaking: true,
            flying: true,
            ..Posture::default()
        };
        assert_eq!(flying.pose(), Pose::Standing);
        assert_eq!(
            Posture {
                sneaking: true,
                ..Posture::default()
            }
            .pose(),
            Pose::Crouching
        );
    }

    #[test]
    fn a_limit_that_is_not_a_length_refuses_everything() {
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let reach = Reach::new(bad);
            assert!(
                !reach.allows(eye(), (0, 0, 0)),
                "a limit of {bad} allowed a block the player is standing in"
            );
        }
    }
}
