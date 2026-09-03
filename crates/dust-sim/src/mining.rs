//! How long a block takes to break, and when the server agrees it is broken.
//!
//! Six numbers and no world. The rule is Minecraft's, spread over four of its
//! own methods, and it is written here rather than in the session for the
//! reason [`crate::placement`] is: it can then be asked the same question by
//! `cargo xtask harness` and by a test, instead of by a running server only.
//!
//! # The rule
//!
//! With `H` the block state's hardness ([`dust_registry::BlockConstants::destroy_speed`],
//! Minecraft's `BlockStateBase.destroySpeed`) and `S` the held item's mining
//! speed against that block ([`dust_registry::mining::speed`]):
//!
//! ```text
//!   divisor  = 30 if the tool is correct for this block's drops, else 100
//!   progress = S / H / divisor          per tick, Minecraft's getDestroyProgress
//! ```
//!
//! `H = -1` is unbreakable: progress is zero and nothing gets through.
//! `H = 0` is a block that comes away on the first click, and so is any pair
//! whose progress reaches one in a single tick — Minecraft's "insta mine".
//!
//! `S` is then adjusted by the player, in `Player.getDestroySpeed`. Two of
//! those adjustments are implemented here and three are not; [`Digger`] says
//! which and why.
//!
//! # The two thresholds, which are not the same number
//!
//! **A break the server runs on its own clock completes at progress 1.0. A
//! break the *client* says it has finished completes at 0.7.** That gap is not
//! sloppiness, it is the whole of how Minecraft keeps a predicted break from
//! being undone: the client animates its own progress locally and sends
//! `STOP_DESTROY_BLOCK` the moment its animation is done, and by then the
//! server — which started counting when the packet arrived and not when the
//! click happened — is one round trip behind. Refusing that stop would shatter
//! a block on the player's screen and put it back, which is the worst outcome
//! available here and worse than never having timed the break at all.
//!
//! And a stop that arrives *earlier* than 0.7 is still not a refusal. It arms
//! a delayed destroy that completes on the server's own count, at 1.0. So a
//! block a player asked for always goes; the only question the two thresholds
//! decide is whether it goes now or a moment later.
//!
//! See [`Progress::stop_accepted`] and [`Progress::server_done`].

/// Minecraft's divisor when the held tool is the one this block's drops want.
const CORRECT_DIVISOR: f32 = 30.0;

/// The divisor otherwise. Not a penalty on the drops — that is
/// [`crate::drops`]'s question — a penalty on the *time*, and it applies to a
/// block that drops the same thing either way.
const WRONG_DIVISOR: f32 = 100.0;

/// What the server accepts from a client that says it has finished.
///
/// Minecraft's own number, in `ServerPlayerGameMode.handleBlockBreakAction`.
/// Thirty per cent of a break is the latency allowance, and it is generous on
/// purpose: the cost of being too strict is a block that visibly comes back.
const STOP_THRESHOLD: f32 = 0.7;

/// A player being off the ground divides their mining speed by this.
const AIRBORNE_DIVISOR: f32 = 5.0;

/// How many milliseconds a tick is. A break is counted in ticks and measured
/// in wall time; see [`Progress::ticks_elapsed`].
const TICK_MILLIS: u128 = 50;

/// The player's side of one break: everything that is not the block.
///
/// # What is here and what is not
///
/// `Player.getDestroySpeed` applies five multipliers to the held item's speed.
/// Two are here:
///
/// - **Efficiency**, `+ level² + 1`, and only when the base speed is already
///   above one — a bare hand is never made faster by an enchanted pickaxe in
///   the other slot, and Minecraft's guard for that is the same `> 1.0` test.
/// - **Not on the ground**, `÷ 5`. The client applies this itself while it
///   animates, so a server that skipped it would disagree with the screen of
///   every player who mined the block under their feet after jumping.
///
/// Three are not, and each for a stated reason rather than for want of time:
///
/// - **Haste** and **mining fatigue** need status effects, and Dust has no
///   status effects at all — not the packet, not the store, not the tick that
///   expires them. A field here that is always zero would be a guess dressed
///   as a number.
/// - **Eye in water without aqua affinity**, `× 0.2`, needs the fluid state of
///   the cell at the player's eye, and Dust has no fluid level for a cell that
///   is not a full source block. Being wrong about this one is worse than
///   omitting it: it is a five-fold error, sixteen times the 30% latency
///   allowance in [`STOP_THRESHOLD`], so a wrong answer would shatter blocks
///   and put them back. Omitting it makes an underwater player mine at their
///   dry speed, which the 0.7 stop threshold absorbs in the player's favour.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Digger {
    /// The held item's mining speed against the block being broken, straight
    /// out of its `minecraft:tool` component. One for a bare hand.
    pub speed: f32,
    /// The `minecraft:efficiency` level on the held stack, zero for none.
    pub efficiency: u32,
    /// Whether the block the drops want is the block in the hand. Decides the
    /// divisor, and nothing else here.
    pub correct: bool,
    /// Whether the player's feet are on something.
    pub on_ground: bool,
}

impl Digger {
    /// A bare hand, standing on the ground, with no tool and no enchantment.
    #[must_use]
    pub fn bare() -> Self {
        Self {
            speed: 1.0,
            efficiency: 0,
            correct: false,
            on_ground: true,
        }
    }

    /// The speed Minecraft's `Player.getDestroySpeed` arrives at.
    #[must_use]
    fn adjusted_speed(&self) -> f32 {
        let mut speed = self.speed;
        // Minecraft's guard, kept exactly: efficiency is added only when the
        // base speed already beats a bare hand. Enchanting a hoe does not make
        // it dig stone.
        if speed > 1.0 && self.efficiency > 0 {
            let level = self.efficiency as f32;
            speed += level * level + 1.0;
        }
        if !self.on_ground {
            speed /= AIRBORNE_DIVISOR;
        }
        speed
    }
}

/// One break's progress per tick, and the two questions asked of it.
///
/// Built once, when the click arrives, from one read of the world. It is four
/// bytes and it is what a session holds for the length of a break, so that the
/// tick that finishes the break does not have to ask the block or the item
/// anything again.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Progress(f32);

impl Progress {
    /// Minecraft's `BlockStateBase.getDestroyProgress`, per tick.
    ///
    /// `hardness` is the state's own, where `-1` is unbreakable and `0` is
    /// instant. A caller with no hardness column at all should not call this:
    /// see [`Progress::instant`] for what to do instead, and
    /// [`dust_registry::BlockConstants::has_destroy_speed`] for how to ask.
    #[must_use]
    pub fn of(hardness: f32, digger: &Digger) -> Self {
        if hardness < 0.0 {
            // Unbreakable. Bedrock, barriers, the portal frame. Zero progress
            // per tick never reaches either threshold, at any elapsed count,
            // which is exactly the behaviour wanted and needs no second branch.
            return Self(0.0);
        }
        if hardness == 0.0 {
            // A torch, a flower, a button. Minecraft divides by this and gets
            // an infinity that compares above one; saying so directly is the
            // same answer without asking a reader to trust float division.
            return Self(f32::INFINITY);
        }
        let divisor = if digger.correct {
            CORRECT_DIVISOR
        } else {
            WRONG_DIVISOR
        };
        Self(digger.adjusted_speed() / hardness / divisor)
    }

    /// Progress per tick, for a caller that wants the number itself.
    #[must_use]
    pub fn per_tick(self) -> f32 {
        self.0
    }

    /// Whether this break is over before it starts.
    ///
    /// Minecraft's "insta mine": the server destroys the block on the
    /// `START_DESTROY_BLOCK` packet and never counts. It is not only the
    /// zero-hardness blocks — a netherite pickaxe on dirt reaches one in a
    /// single tick too, and the player expects the same instant answer for
    /// both.
    #[must_use]
    pub fn instant(self) -> bool {
        self.0 >= 1.0
    }

    /// Whether the block can ever be broken this way at all.
    #[must_use]
    pub fn possible(self) -> bool {
        self.0 > 0.0
    }

    /// Whether a `STOP_DESTROY_BLOCK` after `elapsed` ticks is honoured.
    ///
    /// Seventy per cent, which is Minecraft's, and the reason is in this
    /// module's own header: the client counted from the click and the server
    /// counted from the packet, so the client is always ahead and refusing it
    /// puts a broken block back on the screen.
    #[must_use]
    pub fn stop_accepted(self, elapsed: u32) -> bool {
        self.0 * (elapsed as f32 + 1.0) >= STOP_THRESHOLD
    }

    /// Whether the server's own count has finished the break.
    ///
    /// One hundred per cent. This is the path a stop that came in too early
    /// falls back to, and the path a client that never sends a stop at all
    /// ends up on.
    #[must_use]
    pub fn server_done(self, elapsed: u32) -> bool {
        self.0 * (elapsed as f32 + 1.0) >= 1.0
    }

    /// How many ticks this break takes with nobody helping — the number the
    /// wiki prints and the client animates.
    ///
    /// `None` for a block this pair cannot break. This is the reporting form:
    /// the server itself asks [`Progress::server_done`] once a tick rather
    /// than comparing against this, because the two are the same test and only
    /// one of them is a division.
    #[must_use]
    pub fn ticks(self) -> Option<u32> {
        if !self.possible() {
            return None;
        }
        if self.instant() {
            return Some(1);
        }
        Some((1.0 / self.0).ceil() as u32)
    }

    /// How many ticks have passed, from wall time.
    ///
    /// **Wall time and not a server tick count, on purpose.** The client
    /// animates a break on its own twenty-a-second clock whatever the server
    /// is doing, so on a server that is behind, counting server ticks would
    /// make the break take longer on screen than the player was shown. The
    /// player's clock is the one that decides whether a break feels right, so
    /// it is the one that is counted.
    #[must_use]
    pub fn ticks_elapsed(millis: u128) -> u32 {
        (millis / TICK_MILLIS).min(u32::MAX as u128) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(speed: f32, correct: bool) -> Digger {
        Digger {
            speed,
            efficiency: 0,
            correct,
            on_ground: true,
        }
    }

    /// The numbers the wiki prints, which are also what a real 1.21.1 server
    /// was measured doing — see decision record 0028.
    #[test]
    fn the_published_break_times() {
        // Stone, hardness 1.5. Bare hand: 1 / 1.5 / 100, so 150 ticks.
        assert_eq!(Progress::of(1.5, &tool(1.0, false)).ticks(), Some(150));
        // Wooden pickaxe, speed 2 and correct: 1.5 * 30 / 2, so 23 ticks.
        assert_eq!(Progress::of(1.5, &tool(2.0, true)).ticks(), Some(23));
        // Netherite pickaxe, speed 9 and correct: 1.5 * 30 / 9, so 5 ticks.
        assert_eq!(Progress::of(1.5, &tool(9.0, true)).ticks(), Some(5));
        // Obsidian, hardness 50, netherite pickaxe: 50 * 30 / 9, so 167.
        assert_eq!(Progress::of(50.0, &tool(9.0, true)).ticks(), Some(167));
    }

    /// The two ends of the hardness range, which is the range a stand-in
    /// cannot reach: a block that is never broken and one that is always
    /// broken at once.
    #[test]
    fn unbreakable_and_instant() {
        let bare = Digger::bare();
        let bedrock = Progress::of(-1.0, &bare);
        assert!(!bedrock.possible());
        assert!(!bedrock.instant());
        assert_eq!(bedrock.ticks(), None);
        // No elapsed count ever finishes it.
        assert!(!bedrock.server_done(u32::MAX));
        assert!(!bedrock.stop_accepted(u32::MAX));

        let flower = Progress::of(0.0, &bare);
        assert!(flower.instant());
        assert_eq!(flower.ticks(), Some(1));
    }

    /// A block soft enough for the tool in hand is instant even though its
    /// hardness is not zero. Dirt is 0.5, a netherite shovel is 9 and correct:
    /// 9 / 0.5 / 30 is 0.6 per tick, which is not instant — but a diamond
    /// shovel on a snow layer (0.1) is.
    #[test]
    fn insta_mine_is_about_the_pair_and_not_the_block() {
        assert!(!Progress::of(0.5, &tool(9.0, true)).instant());
        assert!(Progress::of(0.1, &tool(8.0, true)).instant());
    }

    /// The gap this module exists to be explicit about. Stone with a wooden
    /// pickaxe takes 23 ticks on the server's own clock and is accepted from
    /// the client at 15 — eight ticks, 400 ms, which is a round trip on a bad
    /// connection and is exactly what the allowance is for.
    #[test]
    fn the_client_is_believed_before_the_server_finishes() {
        let stone = Progress::of(1.5, &tool(2.0, true));
        let first_stop = (0..).find(|e| stone.stop_accepted(*e)).unwrap();
        let first_done = (0..).find(|e| stone.server_done(*e)).unwrap();
        assert_eq!((first_stop, first_done), (15, 22));
        // And the reporting form is one more than the server's own count,
        // because the tick the click landed on counts too.
        assert_eq!(stone.ticks(), Some(first_done + 1));
    }

    /// Efficiency is added to the speed and not multiplied, and it does
    /// nothing to a bare hand. Efficiency V on a diamond pickaxe (speed 8) is
    /// 8 + 25 + 1 = 34.
    #[test]
    fn efficiency_adds_and_only_to_a_tool() {
        let enchanted = Digger {
            speed: 8.0,
            efficiency: 5,
            correct: true,
            on_ground: true,
        };
        assert_eq!(enchanted.adjusted_speed(), 34.0);
        let hand = Digger {
            speed: 1.0,
            efficiency: 5,
            ..enchanted
        };
        assert_eq!(hand.adjusted_speed(), 1.0);
    }

    /// Mining in mid-air is five times slower, and the client already knows
    /// it. Stone with a wooden pickaxe: 23 ticks standing, 113 jumping.
    #[test]
    fn airborne_is_five_times_slower() {
        let mut jumping = tool(2.0, true);
        jumping.on_ground = false;
        assert_eq!(Progress::of(1.5, &jumping).ticks(), Some(113));
    }

    /// The wrong tool costs time even where it costs no drops. Oak planks are
    /// hardness 2 and drop themselves to anything; an axe takes 15 ticks and a
    /// pickaxe of the same speed takes 50.
    #[test]
    fn the_wrong_tool_is_slower_even_when_the_drops_do_not_care() {
        assert_eq!(Progress::of(2.0, &tool(4.0, true)).ticks(), Some(15));
        assert_eq!(Progress::of(2.0, &tool(4.0, false)).ticks(), Some(50));
    }

    #[test]
    fn wall_time_becomes_ticks() {
        assert_eq!(Progress::ticks_elapsed(0), 0);
        assert_eq!(Progress::ticks_elapsed(49), 0);
        assert_eq!(Progress::ticks_elapsed(50), 1);
        assert_eq!(Progress::ticks_elapsed(1_150), 23);
    }
}
