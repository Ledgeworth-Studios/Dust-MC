# D20 — What a movement check really costs on a saved world

**Status:** Measured, 2026-09-03. **8.8 microseconds a packet, not 408
nanoseconds**, and 97% of it is rebuilding a chunk column out of a region file
inside the network path. [D15](0015-what-a-movement-check-asks-the-world.md)'s
number and [D19](0019-how-tall-a-player-is.md)'s were both measured on a bench
whose player was not moving, and this record replaces them. No behaviour
changed; the bench did.

## What was wrong with the number

`benches/movement.rs` walks a player back and forth across 64 blocks and judges
2,000 movement packets. It reported 408 ns a packet on a world read from region
files, and both records above quoted it.

The player was not walking. `Movement::at` only advances on an accepted claim,
and the bench sends the next packet from where its *walk* says rather than from
where the server put the player — so the first refusal froze the player at the
start of the walk while the walk carried on. Every later packet was a claim from
the origin to somewhere far away, which is:

- **split into more samples**, up to `MAX_SAMPLES` of 64, and
- **answered by the first sample**, because the first sample is right next to
  the frozen origin, which is inside the terrain the row deliberately put it in.

So the row measured a short box question in one already-resident column, 1,800
times, and never touched another column. It was a measurement of the sample
loop and of nothing this bench exists to find out.

Two things said so in the output and were read past. Only **118 of 2,000**
packets were accepted, in a row named for a walk. And when
[D19](0019-how-tall-a-player-is.md) added a pose ladder, **all three poses read
the same on that row** while differing by a fifth on the flat one — a row that
cannot tell a 1.8-high box from a 0.6-high one is not reading the box.

The fix is one line: a refused packet re-seeds the player where the walk says
they are, which is what a server that corrected them and got an answer would
leave behind. Every packet is then a 0.216-block step and every one of them is
judged.

## What it actually costs

`DUST_BENCH_CONSTANTS=... DUST_BENCH_REGION=... cargo bench -p dust-server
--bench movement`, median of five rounds of 2,000 packets, on a world Minecraft
wrote:

|                                       | ns/packet |
| --- | --- |
| no world | 3 |
| flat, in the open, standing (1.8) | 35 |
| flat, in the open, feet only (0.6) | 28 |
| flat, into the ground | 55 |
| region files, into the ground, standing (1.8) | 8,798 |
| region files, in the open, standing (1.8) | 8,835 |
| region files, in the open, feet only (0.6) | 8,690 |

**8.8 microseconds, and it is the same number for every pose and for both the
walk through the terrain and the walk over it.** That is the shape of a cost
that is not about cells: the box, its height, and whether anything is in it make
no difference to it at all.

`Ground` now counts what it builds, and the bench prints it: **one 2,000-packet
walk — 432 blocks — builds 19 columns.** At 8.8 us a packet the walk costs
17.6 ms; 19 builds at the ~0.9 ms a column build takes is 17 ms of that. The
cell reads are the other 5%.

Raising the per-session cache from 4 columns to 16 takes the row from 8.8 us to
2.3 us, which is the same finding from the other side: **the misses are the
cost, and they happen because a player who walks 432 blocks visits more columns
than four.**

## Why the cache cannot just be made bigger

A built column is about a megabyte, and this cache is **per session**. Four is
already 4 MB a player and a hundred players is 400 MB of duplicated chunks —
duplicated because two players standing in the same place each hold their own
copy. Sixteen would be 1.6 GB. The cache is the right size for what it is and
the wrong thing to be.

`net/collide.rs` already said the answer in its own "What is still wrong":

> the real answer is that a server should keep the columns its players are
> standing in, and Dust does not keep any column at all. That is chunk
> residency and it is not this.

This record is the number for it. **Chunk residency would take a walking
player's movement check from 8.8 us to about 0.4 us and take the per-player
column memory to zero**, because a resident column is shared and a player
walking into a chunk another player is standing in would build nothing.

## What was declined

**Raising `CACHED_COLUMNS`.** Measured — 4.3x for 12 more megabytes a player —
and refused for the memory. Priority 2, and it is the wrong axis: the fix is one
copy of a column for the server, not more copies per player.

**Leaving the bench alone and quoting the old number with a caveat.** A number
that is wrong by 21x is not a number with a caveat.

**Making the walk follow the terrain's surface** so that every packet is
accepted without re-seeding. Tried, and it does not work: the player's box is
0.6 across and straddles two columns, so a step beside a rise is refused for
walking into it, and the row went back to two thirds refused. Re-seeding is
both simpler and closer to what a corrected client does.

## What this does not change

Nothing about what the check decides. The rule, the poses, the permissions and
every `collide.js` case are exactly as [D19](0019-how-tall-a-player-is.md) left
them; only the cost of running them was misreported.

The flat rows were never wrong — a superflat lends one template column to every
position and builds nothing, which is why they are 35 ns and why the pose ladder
reads sensibly on them.
