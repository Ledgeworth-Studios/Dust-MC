# D31 — How a join streams its chunks

**Status:** Built and measured, 2026-09-03. A join sends the same 289 columns in
the same order as before, and **the session's own task no longer builds any of
them**. On a generated world — which is what a server with no `world_source`
serves since [D26](0026-the-terrain-dust-serves-and-what-it-does-at-the-seam.md)
— that removed **2,293.9 ms of blocking work from a tokio worker**, and the
players already standing in the world felt it: with four bots joining at once, a
settled player's chat round trip went from a worst case of **279, 875 and 500 ms
over three runs to 24, 69 and 87 ms**, and from two thirds of its pings never
coming back inside the window to all of them.

[D25](0025-who-keeps-a-chunk-column.md) named this as the next measurement in
its own last section. It is the third caller of the store D25 built, and
deliberately not a third builder of columns.

## Which world a number is about

Since D26 there are three, and one number for "a column" hides which. Measured
by `benches/join.rs`, which runs a ladder over the same 289 columns on each —
each row the one above it plus a single named change, because a total cannot say
which input owns which part of it:

| | build only | build and encode | encode only, resident |
| --- | --- | --- | --- |
| **flat** | 0.0 ms | 5.5 ms | (keeps nothing) |
| **generated** | 2,293.9 ms cold | 1,300.5 ms warm | **16.0 ms** |
| **region files** | 104.0 ms cold | 76.6 ms warm | **5.8 ms** |

Two things fall out of the ladder and neither is visible in a single figure.

**Encoding a column is 19 us and building one is 3.8 ms generated, 0.24 ms out
of a region file.** The build is 99.5% of the stream on a generated world and
92% of it on a region one. So moving the *build* off the session task is the
whole of the change worth making, and moving the encode is not worth making:
5.8 ms spread over 289 columns is not something a player can feel, and a scheme
that also moved it would have to move the socket with it.

**The world that needed D25's residency sixteen times more than the other was
the only one without it.** `Source::Generated` had no store at all; every
column, for a movement check as much as for the stream, was built on whatever
thread asked. That was correct when a generated world was a curiosity behind a
flag and wrong the day it became the default.

## What is built and by whom

**One `ColumnStore` per world, and both real worlds have one.** The residency,
the channel and the warming thread came out of `AnvilWorld` and now sit behind a
`Columns` trait with two implementors. That is the right seam: the rules for who
may build a column belong to the *threads*, not to where the blocks come from.
A session runs on a tokio worker and the item loop runs on the engine's own
`std` thread, and neither may block, whether the block came off a disk or out of
a noise function.

**One thread per store, not a pool.** What that thread serves is the ring ahead
of a walking player — nine columns, 34 ms of generated terrain against the
1,600 ms [D17](0017-how-fast-a-player-may-say-they-moved.md)'s speed limit gives
a player to cross the column they are standing in. A margin of 47 to one does
not need a second thread, and the one caller that wants 289 columns at once does
not come through here.

**The join's first twenty-five are built with `tokio::task::spawn_blocking`.**
This is the one place in this server where that door is the right one, and the
reason is the same reason D25 gave for the warming thread belonging to the
world: a session is on tokio and the item loop is not, so `spawn_blocking` is
available to one caller in two. A join is that caller. It has no movement packet
held up behind it, it wants the ground under the player's feet before the
loading screen ends, and blocking its worker for 95 ms of noise blocks every
other session that worker is running.

## The three rules the stream keeps, which only work together

**Nearest first**, which `View` already did. A client renders what it has, and
the column underfoot arriving before the far corner is the difference between
walking forward and waiting.

**A claim on a bounded window ahead of the send point.** `View::peek` names the
batch and sixteen more *without recording them* — a peek that marked columns
loaded would credit a client with a world it was never sent — and one
`ColumnClaim` per session holds them in the store and asks its thread for them.
Held, so a column built for one session is kept: four bots joining at the same
place now arrive **within 20 ms of each other** where they used to be staggered
across 400, because the store builds their columns once between them.

**A prefix, never a hole.** `Source::built_prefix` counts how many of the window
are ready, *counted from the front*, and the pass sends exactly that many. That
is the ordering guarantee and the back-pressure at the same time: the window
advances only over columns that have gone out, so a client that cannot keep up,
or a player who outruns the store, asks for nothing new. There is no queue to
grow.

A flat world and a world whose warming thread would not start both answer
`columns.len()`, and neither is a shortcut: one has nothing to wait for and the
other has nobody to wait *on*. A stream pacing itself against a thread that does
not exist is a hole in the world forever.

## What it costs

**24 columns per streaming session, 2.7 MB** at the 111 KB a column of a real
world measures — the batch of eight plus sixteen of runway, each released as it
goes out. D25 declined a claim the size of the view for this exact reason: 289
columns is 32 MB a player, and a stream does not need to hold what it has
already sent.

The total work is unchanged. The same columns are built once, in the same order,
and finish at about the same moment. What changed is which thread pays and who
waits behind it.

## The defect this uncovered, which is the transferable part

Giving a column back **scanned the whole resident map** to ask how many columns
nobody holds, under the write lock. That was affordable for its two original
callers: a player crossing a chunk boundary, and a heap of items despawning.
The chunk stream releases a column for every column it sends — fifty times a
second per session — and on a region-file world, where a column is cheap enough
that four joins move two thousand of them a second, it starved every reader. A
settled player's worst chat round trip was **758 ms**, worse than before the
change.

`Kept` now carries the count and every path that moves a holder across zero
maintains it, so the test is a comparison; the `retain` is still a walk and
still happens, once every sixty-four retirements rather than on every release.

**The lesson is not about locks.** An O(n) step is a policy decision about how
often it is allowed to run, and that decision lives in the *callers*, not in the
code that does it. This one was correct for the whole life of the module and
became wrong the moment a third caller arrived — with nothing in the module
changing, and nothing to see at the call site.

## What was measured, and how

Two instruments, because they answer different questions.

`benches/join.rs` is the ladder above: what a column costs and where the cost
is. `cargo bench -p dust-server --bench join` with `DUST_BENCH_CONSTANTS`,
`DUST_BENCH_DATA` and `DUST_BENCH_REGION` set.

The player-facing number is four bots joining at once while a settled player
times a chat round trip twenty times a second across a **fixed three-second
window**. The window has to be fixed: a first version measured until the last
joiner had its columns, which scored a faster build on fewer samples — a
measurement whose sample size is the variable it is measuring.

**The two builds were run interleaved, alternating on the same machine**, and
that is not fastidiousness. Three other builds were running throughout; a
sequential A then B would have attributed the machine's mood to the change.
Worst round trip, and how many of the ~150 pings completed:

|  | before | after |
| --- | --- | --- |
| **region files** | 170 / 276 / 403 ms (107, 93, 40 samples) | 109 / 221 / 125 ms (113, 105, 114) |
| **generated** | 279 / 875 / 500 ms (77, 59, 54 samples) | 24 / 69 / 87 ms (151, 135, 149) |

Four joiners, time until the last of their 289 columns arrived: on a generated
world **2,017–2,400 ms before and 1,663–1,684 ms after**, and tightly clustered
where they used to be spread. On a region world the two are the same within the
noise, **720–1,273 ms before and 713–981 ms after** — a region column is cheap
enough that there was never much to move, which is the point of measuring both.

## What was declined

**A pool of warming threads.** It would make a cold generated join finish in
about a quarter of the time. It also takes cores the tick loop wants, and the
number that matters — when the loading screen ends — is already 25 columns and
not 289. Priority 2, once priority 1 is satisfied. If it is ever wanted, the
caller to give threads to is `spawn_blocking` at the join, not the store.

**Moving the encode off the session task too.** 5.8 ms across 289 columns, and
it would put the socket write behind another hop.

**Holding the whole view.** D25's number stands: 32 MB a player.

**Letting the session build a column the store has not got to yet.** It is the
obvious floor and it is the wrong one here: with a single builder the session
would win that race on nearly every column and the store would build everything
twice. The floor that is kept is narrower and exact — *a world with no warming
thread* builds on the caller, as every caller did before D25.

## What this does not change

Nothing about what any check decides. `tools/bot/collide.js` is **10 of 10 on a
superflat, 10 of 10 on generated terrain and 10 of 10 on a world Minecraft
wrote**, five runs each on both builds.

## What is still wrong

**A cold generated join is still 1.7 seconds of world arriving**, and about
1.6 of that is one thread evaluating noise. The loading screen ends at 145 ms
and the player can walk, so this is terrain filling in around somebody who is
already playing rather than a wait — but it is the number a thread pool would
move, and it is the reason the pool is declined rather than rejected.

**The join warms twenty-five columns and the play loop then streams the other
264 at eight per twenty milliseconds.** On a generated world the store cannot
keep up with that cadence, so the stream is paced by the builder and the batch
size is not doing anything. That is not wrong, but it means `STREAM_BATCH` is
now a bound on the region case only.
