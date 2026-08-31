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
//! [`Reach`], and nothing else yet. The rules that are still missing are stated
//! where the code for them would go rather than listed here, because a list in
//! two places is a list that disagrees with itself.

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
/// **Where the player really is.** The position is whatever their last movement
/// packet said, which this server trusts as sent. A client that lies about its
/// position lies to this check too. What this stops is the *other* shape of
/// cheat: acting on a block far from a position the player is honestly at.
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
