# D25 — Who keeps a chunk column

**Status:** Built and measured, 2026-09-03. The server keeps the columns its
players and its falling items are near, **once, shared**, instead of every
caller rebuilding its own copy out of a region file on whatever thread it
happens to be on. A movement packet at the worst moment went from **11.28 ms to
0.034 ms**; a falling item's tick went from **555,197 ns to 75 ns**. It costs
**more memory, not less**, and the reason that is still the right trade is the
first half of this record.

[D20](0020-what-a-movement-check-really-costs-on-a-saved-world.md) is the
measurement that asked for this and it named the wrong reason.

## What D20 got right, and the one number it had not measured

D20 was right that 97% of a movement check on a saved world is a column build,
and right that a bigger per-session cache is the wrong axis. It was wrong about
the size of a column, and so about the whole memory case:

> A built column is about a megabyte ... Four is already 4 MB a player and a
> hundred players is 400 MB of duplicated chunks.

**A column of a real world is 111 KB.** `benches/movement.rs` now counts the
heap under 256 of them with a counting allocator — the same instrument
`dust-nbt/benches/allocation.rs` uses — and the number is 29,103,608 bytes.
"About a megabyte" had been in three modules' documentation since the project
started and nothing had ever measured it.

So the per-session cache was 0.44 MB a player, not 4 MB, and 44 MB at a hundred
players, not 400. **The efficiency case for residency was nine times weaker than
it looked.** This record keeps residency anyway, and priority 1 is why.

Resident-set size was tried first and does not work: an allocator that has just
freed a thousand columns hands the next thousand the same pages, so `ps` reports
**zero bytes a column** for any measurement taken after another row has run.

## What a player feels, which is the actual case

`benches/movement.rs` grew a row that runs in **real time** — 300 packets at
twenty a second, the rate a walking client sends — into a world nobody had been
in. Every other row in that file sends packets as fast as the machine will judge
them, which crosses a chunk boundary every three microseconds and is a player
moving about a million times faster than a client can claim to; those rows
cannot answer this question and the first draft of this change read them as if
they could.

The number is not a mean. A stall is not felt as an average:

| | mean | **worst single packet** | columns built on the network path |
| --- | --- | --- | --- |
| before | 193,261 ns | **19,184,166 ns** | 5 |
| after | 6,884 ns | **74,083 ns** | 0 |

Repeated across the session's runs the worst packet before was 11.3–19.2 ms and
after was 0.032–0.074 ms. **That is the change: between 150x and 350x off the
one number a player can perceive, and not one region file read on the session's
task.**

The steady-state row agrees from the other side. A walk over terrain the server
is keeping is **7,786 ns a packet before and 39 after** — 200x — and the count
beside it says why: 19 columns built by the check before, 0 built and 19 shared
after.

## The second caller, which has different rules

[D23](0023-what-shape-an-entity-has.md)'s item entities hit the same defect in a
different place: `ItemWorld::tick` builds a `Ground` inside every tick and its
four-column cache dies with it. `benches/items.rs`, over region files:

| | before | after |
| --- | --- | --- |
| 1 falling item | 555,197 ns/tick | **75 ns/tick** |
| 100 falling items | 4,642,769 ns/tick | **5,300 ns/tick** |
| 1 item at rest | 10 ns/tick | 26 ns/tick |
| 100 items at rest | 4,061 ns/tick | 4,148 ns/tick |

Three things in that table are the design.

**The item loop runs on the engine's own `std` thread and a session runs on a
tokio worker.** Neither may block on a region file and `spawn_blocking` exists
only for the second, so the warming thread belongs to the *world* — one thread
started with `AnvilWorld`, ended when it drops — and neither caller knows it is
there. Both call `want`, which sends a list and returns.

**A settled item never asks the world anything.** `step` returns on its first
line for an item that has landed, which is why the at-rest rows are the same
either side: neither reads a column. So the item claim covers **only the drops
still in the air** — a second or two after a block breaks — and is given up as
they land. Claiming columns for a heap lying on the floor would be a megabyte
apiece held for the five minutes until it despawns, bought for nothing.

**The claim names the column the item will read, not the one it is in.** `step`
moves the item and *then* reads the cell under where it moved to, so the column
that matters is the one at `x + vx`. Claiming `x` is right for every item that
does not cross a boundary and wrong for exactly the ones that do — and an item
is given a random horizontal push when it pops, so crossing is what they spend
their first second doing. Measured, a hundred falling items: **2,020,855 ns a
tick claiming `x`, 5,300 claiming what is read.** It looked like the claim
needed a margin around it and it was an off-by-one-tick.

## Who owns a column, and when it is dropped

Two kinds of holder, because there are two access patterns and one of them is
not a ring around anybody.

- **A player is a moving window.** `Residence` holds the nine columns around the
  one a session is standing in and slides them as it walks. Nine is arithmetic:
  a player box is 0.6 across, so the cells one check reads are always inside the
  ring around the player's own column.
- **Falling items are a static set.** `ColumnClaim` holds whatever columns
  `items::footprint_into` names and gives up the ones it stops naming.

Both are refcounts on the same column. Two players and a bouncing pile of
cobblestone over one column keep **one** copy of it between them, which is the
whole point and the thing a per-session cache cannot do.

A column that loses its last holder is **retired, not dropped**, and the retired
ones go wholesale once there are more than 64 of them — the policy the sky-floor
cache already uses, for the same reason. It is there for one real player: the
one walking back and forth across a boundary in a corridor or a doorway, who
would otherwise rebuild the same column every few seconds.

**Nothing is evicted while anybody holds it, and the bound is players and items
rather than a cap**, so there is no number to tune and no way for the set to
grow without somebody standing in it.

## What it costs, at 1, 10 and 100 players

At 111 KB a column, nine a player, plus a flat 64 columns of retired tier for
the whole server:

| | before, 4 a session | after, 9 shared |
| --- | --- | --- |
| 1 player | 0.4 MB | 1.0 MB |
| 10 players together | 4.4 MB | 1.0 MB |
| 10 players apart | 4.4 MB | 10 MB + 7 MB |
| 100 players apart | 44 MB | 100 MB + 7 MB |

**This is more memory for players who are spread out — about 2.4x at a hundred
of them.** It is taken deliberately. Priority 1 is a 19 ms stall inside the
network path and a server that stops answering when a hundred players walk into
new terrain at once; priority 2 is 63 MB on a machine that has gigabytes. The
`Ground` caches cost nothing on top, because their entries are now `Arc` handles
on the server's own copy rather than a second one.

Zero of all of it on a flat world, which lends one template column to every
position and has nothing to be resident.

## Thread safety, and what it is serialised against

One `RwLock` over one map, and **the lock is never held across a region read**.

- **Movement checks**, on every session's task on every tokio worker thread,
  take the read lock, clone an `Arc` and drop it.
- **The item tick**, on the engine's own thread, takes the write lock for the
  length of a handful of hash lookups. It does no I/O.
- **Holds and releases** are refcounts, which is exactly why they are safe on
  both of those threads: only the *build* is expensive, and it is a separate
  call.
- **The warming thread** takes a snapshot of what is cold, builds with nothing
  held, and takes the write lock for one insert.

It is deliberately **not** serialised against `AnvilWorld`'s region-file mutex.
Two threads warming the same cold column both build it and the second insert
finds it already there and drops its copy — duplicated work, bounded to once per
column per simultaneous arrival, and never a lock held across a disk.

An `Arc<Chunk>` already handed out **outlives eviction**: the last holder
leaving drops the map's entry, not the chunk. A movement check reading a column
that was retired mid-read sees the whole column it started with.

Residency holds the column **as generated**, so it needs no invalidation, for
the same reason `Ground`'s cache never did: every edit is read live out of
`EditedWorld` ahead of any column, on every lookup.

## Why a player never waits for a column that is not there

Warming a cold ring of nine is **20.2 ms, measured; 2.25 ms a column**. A player
who has just crossed a boundary is standing at the edge of their new column with
their box still over the two that were already resident, and has to walk the
width of a column — 16 blocks, **1,600 ms even at the speed limit
[D17](0017-how-fast-a-player-may-say-they-moved.md) allows** — before they can
touch one of the five new ones.

**The margin is 79 to one and it is bounded by the speed limit rather than by a
lock.** The only way to reach a column that is not built is to move faster than
the server will believe, which is separately refused. And a caller that does
reach one builds it, exactly as every caller did before this existed: the floor
of this path is the old behaviour, never a hole in the world.

The join is the one place that warms synchronously — 49.9 ms — and it is the
right place: there is no movement packet held up behind it, and the nine columns
it builds are nine of the first twenty-five the join is about to stream anyway,
so the stream finds them resident and the warm costs the join nothing it was not
already paying. Without it, the first movement packet of a session read a region
file on the spot: **6.4 ms, measured by leaving it out.**

## What was declined

**A residency the size of the view.** 289 columns at the default view distance
is 32 MB a player, and the movement check has never needed more than the ring.

**A radius of two, 25 columns.** 2.8x the memory to take a margin of 79 to one
up to about 500 to one. Priority 2 decides it once priority 1 is satisfied.

**A cap on the resident set.** A cap is a number to tune and a way to evict a
column somebody is standing on. Holders bound this already.

**Claiming columns for settled items.** Measured to buy nothing — the at-rest
rows are the same either side — and it would be the only part of this scheme
that pinned memory for minutes.

**Making `ItemWorld::tick` take the claim, so the entity list is walked once
instead of twice.** `footprint_into` takes the item mutex and scans, and then
`tick` does it again; at a thousand items that is about 1% of the tick. Left,
because the signature is shared with the bench and the tests and the number does
not justify moving them.

## What this does not change

Nothing about what any check decides. `tools/bot/collide.js` is **10 of 10 on a
superflat and 10 of 10 on a world Minecraft wrote**, the same ten
[D19](0019-how-tall-a-player-is.md) left. Every guarantee in this record was
watched failing: the four new checks in `net/residency.rs` were each run against
a build with the thing they are about removed, and each went red on its own and
named a different defect.

## What is still wrong

**The chunk stream is now the biggest blocking region-file cost left, and it is
larger than the one this record removed.** A join builds and sends 289 columns
at the default view distance, a batch at a time, on the session's own task —
and unlike a movement check it does it for columns nobody will hold. Residency
does not touch it. That is the next measurement.
