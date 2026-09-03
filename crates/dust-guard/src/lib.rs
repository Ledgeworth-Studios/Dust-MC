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
//! [`Movement`], which bounds where they may say they are. The rules that are
//! still missing are stated where the code for them would go rather than listed
//! here, because a list in two places is a list that disagrees with itself.

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
/// Vanilla's `Player.DEFAULT_EYE_HEIGHT`. A crouching player's are 1.27 and a
/// swimming one's 0.4, and **this does not track either** — which is why the
/// configured limit is documented as needing slack rather than as being
/// vanilla's number exactly. Half a block of slack covers the whole range of
/// poses and then some; see `[server] interaction_range`.
pub const EYE_HEIGHT: f64 = 1.62;

/// The eye position of a player standing at `feet`.
#[must_use]
pub fn eye_of(feet: (f64, f64, f64)) -> (f64, f64, f64) {
    (feet.0, feet.1 + EYE_HEIGHT, feet.2)
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
}

impl Movement {
    /// A player who has just arrived at `at`.
    #[must_use]
    pub fn new(limit: SpeedLimit, at: (f64, f64, f64)) -> Self {
        Self {
            limit,
            at,
            awaiting: None,
        }
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
    pub fn claimed(&mut self, to: (f64, f64, f64), ticks: u32) -> Claim {
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
            if distance_squared(target, to) <= self.limit.budget_squared(ticks) {
                self.awaiting = None;
                self.at = to;
                return Claim::Accepted;
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
        self.at = to;
        Claim::Accepted
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
        eye_of((0.5, 0.0, 0.5))
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
                player.claimed(to, 1),
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
        assert_eq!(player.claimed((0.0, 64.0 - 3.92, 0.0), 1), Claim::Accepted);
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
        }) = player.claimed((500.0, 64.0, 500.0), 1)
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
            assert_eq!(player.claimed((0.0, y, 0.0), 0), Claim::Accepted);
        }
    }

    #[test]
    fn a_pause_buys_more_room_but_only_a_quarter_second_of_it() {
        // Five ticks of budget is five ticks of distance — 50 blocks, not the
        // 22.4 a linear scaling of the squared limit would give. And a gap of a
        // second buys no more than a gap of a quarter, because past that the
        // honest explanation is a queue of packets rather than one large one.
        let mut player = walker();
        assert_eq!(player.claimed((49.0, 64.0, 0.0), 5), Claim::Accepted);
        let mut player = walker();
        assert!(matches!(
            player.claimed((51.0, 64.0, 0.0), 5),
            Claim::Refused(Refusal::TooFast { .. })
        ));
        let mut player = walker();
        assert!(
            matches!(
                player.claimed((51.0, 64.0, 0.0), 200),
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
                    player.claimed(to, 1),
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
            player.claimed((WORLD_LIMIT + 1.0, 64.0, 0.0), 5),
            Claim::Refused(Refusal::OutOfWorld),
            "two blocks east of the edge of the world is still off the map"
        );
    }

    #[test]
    fn an_unlimited_speed_still_refuses_a_position_that_is_not_a_number() {
        // Turning the speed bound off is a legitimate thing for an operator to
        // want and a malformed coordinate is not a speed.
        let mut player = Movement::new(SpeedLimit::new(f64::INFINITY), (0.0, 64.0, 0.0));
        assert_eq!(player.claimed((1e6, 64.0, 1e6), 1), Claim::Accepted);
        assert_eq!(
            player.claimed((f64::NAN, 64.0, 0.0), 1),
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
            player.claimed((500.0, 64.0, 500.0), 1),
            Claim::Refused(_)
        ));
        let back = player.correct(7);
        assert_eq!(back, (0.0, 64.0, 0.0));
        assert!(!player.settled());
        for _ in 0..5 {
            assert_eq!(player.claimed((501.0, 64.0, 501.0), 1), Claim::Ignored);
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
        assert_eq!(player.claimed((500.0, 64.0, 500.0), 1), Claim::Ignored);
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
        assert_eq!(player.claimed((0.3, 64.0, 0.0), 1), Claim::Accepted);
        assert!(player.settled());
        assert_eq!(player.at(), (0.3, 64.0, 0.0));
    }

    #[test]
    fn a_speed_limit_that_is_not_a_speed_refuses_everything() {
        for bad in [0.0, -1.0, f64::NAN] {
            let mut player = Movement::new(SpeedLimit::new(bad), (0.0, 64.0, 0.0));
            assert!(
                matches!(player.claimed((0.001, 64.0, 0.0), 1), Claim::Refused(_)),
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
