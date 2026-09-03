# D19 — How tall a player is

**Status:** Decided, 2026-09-03. A player is the height of the pose their own
packets describe — 1.8 standing, 1.5 crouching, 0.6 gliding — and their eyes
are that pose's eye height. The collision check measures that whole height and
the reach check measures from that eye. Two heights are given up deliberately
and both are named below. On a world read from region files the check still
costs 405 ns a packet, which is the number [D15](0015-what-a-movement-check-asks-the-world.md)
reported and the number this was not allowed to move.

**Its region-file figures are superseded by [D20](0020-what-a-movement-check-really-costs-on-a-saved-world.md)**, which found that the bench row they came from was measuring a player who had stopped walking. The conclusion below survives it — the three poses read the same on that row before and after — but the numbers are 8.8 us and not 411 ns.

## Context

[D15](0015-what-a-movement-check-asks-the-world.md) refused a player who walked
from outside a solid block to inside one, and measured a box **0.6 high** to do
it. Its own record says why: pose was not tracked, so a full-height box would
have refused every player crawling through a one-block gap, which is ordinary
play. It named the cost of that choice — a cheat whose head goes through a wall
while its feet stand in a legal cell — and left it open.

The reach check had the other half of the same gap. `Reach` measured every
player from a standing eye at 1.62, so a crouching player, whose eyes are at
1.27, was measured **0.35 too high**. That is not a symmetric error: it makes
the block a crouching player is looking *down* at further away than it is, and
crouching at the edge of a ledge to place a block under yourself is the single
most common thing a player does deliberately in this game.

Both are the same missing fact, which is why they are one decision.

## What a 1.21.1 client actually tells a server

Less than a pose. Measured against the protocol rather than assumed:

| Pose | Height | Eye | How the server learns it |
| --- | --- | --- | --- |
| Standing | 1.8 | 1.62 | the default |
| Crouching | 1.5 | 1.27 | `player_command` `StartSneaking` / `StopSneaking` |
| Gliding | 0.6 | 0.4 | `player_command` `StartFlyingWithElytra` — start only |
| Swimming, crawling | 0.6 | 0.4 | **nothing** |
| Sleeping | 0.2 | 0.2 | a bed, and this server has none |

The numbers are vanilla's `Player.POSES`. They are constants in Minecraft's own
code rather than rows in a file it ships, so unlike everything
[D6](0006-ore-density-configuration.md), [D7](0007-registry-contents.md) and
[D8](0008-block-opacity-and-light-emission.md) cover, there is no jar to extract
them from and they are written down here.

Swimming and crawling are the difficulty. Vanilla derives them — swimming from
`isSprinting() && isInWater()`, crawling from a pose that does not fit — and a
client never says either. Dust does not read fluids on the movement path.

## What was decided

**The collision box is the pose's whole height.** A client that puts its head
through a wall is now refused for it.

**A player already inside something at their full height is believed**, which
is [D15](0015-what-a-movement-check-asks-the-world.md)'s rule unchanged, and it
turns out to be the whole crawling answer: a player in a one-block tunnel has a
standing box that is inside the ceiling at *both* ends of every move they make,
so every packet of the crawl is believed. Vanilla does the same thing
explicitly, in `updatePlayerPose`, by shrinking the pose until it fits. Getting
it for free is why this decision needed no new state and no second world
question.

**Being already inside is not a licence to walk.** A head in a low ceiling is an
entirely ordinary player — under a slab, in a cave, on a staircase — and if that
state let the rest of them through a wall, then standing under an overhang would
be a cheat's front door. So when the tall question says "already inside", the
bottom 0.6 is asked about separately, and a player who walks their *feet* into a
block is refused however blocked their head was.

**A sprinting player who says they are airborne is measured at their feet.**
This is the one deliberate hole. It is vanilla's swimming condition with the
water left out, because this server cannot see water on the movement path, and
the alternative is rubber-banding every player who swims through a one-block gap
in a ravine or a kelp forest. Priority 1: a refused swimmer is a defect a player
feels, and a cheat's head through a wall is one they do not.

It costs a cheat one bit — hold sprint, send `onGround: false` — and that is
precisely what every client could already do before any of this existed, so the
check is still strictly a gain and never a loss. What it does *not* give away is
the feet, which are checked whatever the client claims about itself.

**The reach check uses the pose's eye and not the speculation.** A sprinting
airborne player is measured for *collision* as if they might be swimming and for
*reach* as if they are standing, and the two differ on purpose: guessing a
player shorter than they are is permissive for collision and refusing for reach.
A reach check that believed a sprint-jumping player was 0.4 tall would refuse
them for looking up.

## What was measured

`cargo bench -p dust-server --bench movement`, median of five rounds of 2,000
packets, on a superflat and on a world read from region files. The world rows
now run at all three heights in the same run, because a single percentage
cannot say which input owns which part of a cost:

|                                | ns/packet |
| --- | --- |
| flat, in the open, standing (1.8) | 36 |
| flat, in the open, crouching (1.5) | 35 |
| flat, in the open, feet only (0.6) | 28 |
| region files, standing (1.8) | 411 |
| region files, crouching (1.5) | 396 |
| region files, feet only (0.6) | 386 |

Against `origin/main`, run interleaved three times on one machine because the
first attempt was not and a single reading of main came back 867 ns against a
true 420: **flat in the open 32 → 38 ns, flat into the ground 40 → 57 ns,
region files 420 → 405 ns.**

The head costs 8 ns of 36 on a superflat and nothing measurable on a real world,
and the reason is worth writing down: a movement check on a world read from
region files spends its time **building a column**, not reading cells out of one.
Adding cells to the box therefore costs almost nothing there and costs a fifth
of the whole check on a superflat, where building is free. 411 ns x 20 packets a
second is 8.2 microseconds of CPU per player per second — the number
[D15](0015-what-a-movement-check-asks-the-world.md) reported, unmoved.

Part of that came back from a change that is not about pose at all:
`EditedWorld::edited_block_at` took the edit map's read lock **once per cell**,
so one movement packet acquired the same lock up to twelve times.
`EditedWorld::edits_now` takes it once for the box. On a superflat that is 5 ns
of 43 in the open and 10 ns of 67 into the ground.

`node tools/bot/collide.js`: **6 of 6 before, 10 of 10 now**, on a superflat and
on a world read from region files. The file builds the one shape a superflat
does not contain — a block with air under it — and claims a position whose foot
cell is open and whose head cell is not. Watched failing twice, one line removed
each time: without the pose height, **9 of 10** and it is the head case and only
the head case that goes red; without the sprinting-and-airborne permission,
**9 of 10** and it is the swimming control and only that one.

## What was declined

**Reading water, so that swimming could be known rather than guessed.** It is
the honest fix and it is a new column in `dust-constants.tsv`, a re-extraction
by every operator, and a `has_x()` branch for a table that predates it — for a
check whose failure mode without it is a cheat keeping a capability it already
had. Reconsider it when fluids exist for their own sake.

**Deriving the pose by asking the world whether a taller one fits**, which is
what vanilla does explicitly. It is a second world question on the hot path
every packet, and the already-inside rule already produces the same answer for
free in the only case that matters.

**Refusing a player whose pose does not fit where they are.** Vanilla shrinks
them; refusing would pin a player wherever somebody else's block put them, which
is the failure [D15](0015-what-a-movement-check-asks-the-world.md) was written
to avoid.

**Measuring reach from the shortest pose the player might be in.** Symmetrical
with the collision rule and wrong: for reach, short is the refusing direction.

## What is still open

A **swimming or crawling player who does not sprint** is measured at their full
height. Vanilla's swimming pose requires sprint, so a swimmer always does; a
crawler in a tunnel is covered by the already-inside rule. The case left is a
player crawling in the open, which vanilla cannot produce.

**Sleeping** is in the type and nothing sets it, because there are no beds. The
day there are, a sleeping player measured as 1.8 tall is one refused for lying
down in a two-block bedroom.

**Gliding never ends.** A client says when a glide starts and never when it
stops; vanilla's server works the landing out from physics Dust does not have.
A stale `gliding` makes a player shorter than they are, which believes them
rather than refusing them, so it is the safe direction to be stuck in.
