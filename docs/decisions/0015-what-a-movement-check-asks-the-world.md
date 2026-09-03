# D15 — What a movement check asks the world

**Status:** Decided, 2026-09-03. A player may not move from outside a solid
block to inside one. A player already inside one may move anywhere. The
question is asked of the world as it is at that instant, of a box 0.6 across
and 0.6 high measured up from the feet, and it costs 32 ns on a flat world and
408 ns on a world read from region files.

**Its cost figures are superseded by [D20](0020-what-a-movement-check-really-costs-on-a-saved-world.md)**: 408 ns was measured on a bench whose player had stopped moving, and the real number for a walking player on a world read from region files is 8.8 us.

**Superseded in part by [D19](0019-how-tall-a-player-is.md), 2026-09-03**: the
box is now the height of the pose the player's own packets describe, and 0.6 is
the floor rather than the whole of it. Everything else here still holds.

## Context

[D17](0017-how-fast-a-player-may-say-they-moved.md) put a speed limit on
movement packets and said, in its last section, that collision was a separate
decision rather than an omission: a speed check is arithmetic on two positions
and a collision check needs the world, twenty times a second, for every player
online. This is that decision.

The README's "Not yet" said the same thing more plainly — a client could walk
into a wall so long as it walked at a walking pace — and that is what a wall
hack is.

## The rule, and the half of it that looks like a loophole

One sentence: **a player may not move from outside solid ground to inside it,
and a player already inside it may move anywhere.**

The second half is the design. Every legitimate way to end up inside a block
resolves by moving *out* of it: a block placed onto a standing player, a
player logging in where the world changed under them, a piston or a boat or a
minecart moving somebody who did not ask to be moved. None of those exist in
this server yet and all of them will, and a rule that refused those moves would
hold a player wherever somebody else's block put them — stuck, with no way out
and no message saying why. Refusing to be inside is a worse game than allowing
it; refusing to *enter* is the whole of the cheat.

It also cannot be exploited into a free pass. Getting inside is the thing that
is refused, so "already inside" is a state a cheat cannot reach by cheating.
The one road that looked open was answering a correction with a position inside
the wall — one short step from where the teleport put them, well inside the
speed budget — and the collision rule is applied to that path too, for exactly
that reason.

Both questions are asked of the world **as it is now** rather than remembered
from the last packet. That is what makes a block placed onto a standing player
cost them nothing.

## The box, and what is deliberately allowed

A player is 0.6 blocks across and 1.8 high. The box this check measures is 0.6
across and **0.6 high from the feet**, not 1.8, because pose is not tracked: a
crawling or swimming player is 0.6 high and a full-height box would refuse
every one of them, constantly, for playing correctly. What the short box costs
is a cheat whose head is through a wall while its feet stand in a hole. That is
a worse trade in the abstract and a much better one for anybody actually
playing, and it stops being a trade the day pose is read off the packet.

Deliberately allowed, and each of these is a real client doing a real thing:

* **Stepping up half a block without jumping.** The destination is air; only
  the cell the feet end in is asked about.
* **Cornering against a block edge.** The box is the player's, not the
  block's, and a corner clipped by a hair is a position outside the block.
* **A block broken underneath a player a tick ago.** The world is read live,
  so the cell is already air by the time the packet is judged.
* **A chunk the server has not finished loading.** Nothing there is solid, so
  a player walking into unloaded ground is believed. Refusing them would be a
  rubber-band on exactly the players whose connection is worst.
* **A move that starts inside a block**, for the reasons above.
* **A table with no `full_collision` column at all** — the check turns itself
  off and the boot line says so, rather than reading an absent column as
  "nothing is solid".

Not allowed, and each of these is measured red in `tools/bot/collide.js`: a
step down into the ground, a five-block dash into it, and half a block down
into it.

## What was measured

`cargo bench -p dust-server --bench movement`, on a release build with
Minecraft's own block table (26,684 states, 2,990 of them solid). Four rows
over the same 2,000-packet walk, each the one above it plus a single named
change, because one number cannot say which input owns which part of a cost.
Median of five rounds, nanoseconds per movement packet:

| row | ns/packet | what it adds |
| --- | --- | --- |
| no world | **3** | the speed check alone — what a movement packet cost before this |
| flat, in the open | **32** | one box question against a flat world, nothing found |
| flat, into the ground | **40** | the box finds something, so the second question is asked too |
| region files | **408** | the same walk over a world read from `.mca` |

Four hundred nanoseconds twenty times a second is **8.2 µs of CPU per player
per second**: a hundred players standing in a real world spend 0.08% of one
core between them on this. The check is affordable at the worst row and the
number is why it is on by default.

The 408 is not column rebuilding. Instrumented, the four-column cache in
`Ground` built **6 columns across 10,000 packets** — the walk crosses a chunk
boundary every 74 steps and the cache holds every column the walk touches. What
is left is the per-cell reads themselves: a flat world lends one template
column whose sections are uniform, and a real column is a paletted, bit-packed,
400-kilobyte structure. That is the honest shape of the cost and it is a
per-cell cost, not a per-packet one.

## What was declined

* **Sweeping the whole path rather than sampling it.** The move is sampled at
  intervals no longer than a block, which is enough that no step can pass
  through a block without landing in one; a real swept-box test costs more and
  buys a case no client produces.
* **The full 1.8-high box**, above.
* **Refusing a player who is already inside a block.** The heart of the rule.
* **Holding more than four columns per player.** Four is the most one player
  box can span, and a player standing on a chunk corner asks about exactly
  four. It is the number the geometry gives, not a guess, and the measurement
  says it does not thrash.
* **Invalidating the column cache.** It holds the column *as generated*, a pure
  function of its position, and every edit is read live ahead of it. There is
  nothing for an invalidation to invalidate.
* **A per-tick physics simulation of the player.** That is the eventual right
  answer and it is a different project; this is a validity check on a claim, on
  the packet path, and it has to stay cheap enough to run on every packet.

## How it is known to work

`tools/bot/collide.js`, driven by mineflayer — whose physics is
`prismarine-physics`, an independent reimplementation sharing no code with this
project. Six checks. Against a release server with the rule in, **6 of 6 pass
on a flat world and 6 of 6 on a world read from region files**. With the two
lines that refuse the move removed and nothing else changed, **3 of 6 pass**,
and it is exactly the three refusals that go red; the three controls — three
seconds of ordinary walking, a step of the same length upwards, and a dash of
the same length through open air — stay green in both builds, which is what
says the pass is collision and not the speed limit wearing its name.

The first version of that file failed on a real world for being right: its
"open air" control dashed five blocks sideways into a hillside. It now asks the
client's own copy of the world where the open air is and says which direction
it chose. A control that only holds on a superflat is not a control.

## What is still wrong

* A cheat can put its head through a wall while its feet are in a legal cell.
  Fixed by tracking pose, which is a packet field nobody reads yet.
* Nothing tracks *why* a player is inside a block, so the day pistons and boats
  land, "already inside" will be doing work it should not have to.
* A movement check on a region-file world is 13× one on a flat world and all of
  the difference is reading cells out of a real column. The answer is chunk
  residency — a server keeping the columns its players are standing in — and
  Dust does not keep any column at all.
