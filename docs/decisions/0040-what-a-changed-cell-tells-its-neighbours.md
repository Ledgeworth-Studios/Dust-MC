# D40 — What a changed cell tells its neighbours

**Status:** Decided and built, 2026-09-04. The world reacts to being changed:
a block whose support is gone breaks and drops, a block that falls becomes an
entity and lands, and a leaf that has lost its tree comes down. **No Mojang
value in this repository**; the support, falling and leaf columns arrive in
`dust-constants.tsv`, which `cargo xtask extract --only constants` writes from
the operator's own jar. Fluids are **not** in this record and are named at the
end as the next one.

## Context

`README.md`'s "Not yet" said the world does not move. Sand hung in the air
where the block beneath it was mined. A felled tree left a canopy floating over
the whole landscape. A torch stayed stuck to a wall that was gone. Every one of
those is visible in the first minute of play, and the canopy is visible from
across a valley.

The mechanism the rest need is the **neighbour update**: the news, delivered to
a cell, that something beside it changed. Decision record 0014 built the
placement half of that — a fence that grows an arm towards a fence put down
beside it — and could not build this half, because a placement rule runs when a
player clicks and this one runs when a block goes away.

## The three rules, and where each came from

```text
  can it stay?     BlockStateBase.canSurvive, per state, through reflection
  does it fall?    FallingBlock.isInstance, 32 states
  how far is the   LeavesBlock.getOptionalDistanceAt, and the block tag it
  nearest log?     consults, which is the half reflection cannot answer
```

`canSurvive` had been out of reach for the reason `getStateForPlacement` still
is: it takes a `LevelReader` and there is no world. `Level` is an abstract
class and cannot be faked, which is the wall `tools/bot/placement.js`
documents. **`LevelReader` is an interface**, so the oracle hands it a
`java.lang.reflect.Proxy` that answers `getBlockState` out of a map of seven
cells and delegates everything else to `EmptyBlockGetter` — the level Minecraft
itself passes where there is no world.

Measured over all 26,684 states, none of which threw:

```text
  20,110  survive with every neighbour air
   6,574  need something
   6,195  of those name a side the probe could resolve
     379  do not, across 28 named blocks — 232 multiface lichen and sculk
          vein, 40 crops that also want a light level the proxy cannot
          answer, 24 piston heads, and the rest
      32  fall when nothing is under them
     280  are leaves and carry a distance
```

### The probe sweeps fourteen materials, and the first version did not

A first pass probed with stone alone and reported that a sapling has no support
at all, because a sapling wants dirt. **A rule reading that column would have
deleted every flower in the world.** The probe now sweeps one material per
family of `mayPlaceOn` predicate and ends with the state's own block — which is
what answers the top half of a door, a stalk of sugar cane and a shoot of
bamboo — and stops at the first that holds the state up. The column therefore
means *which side is load bearing*, not what may be underneath. That is the
question a neighbour update actually asks, because what happens to a support in
play is that it is mined.

### The oracle answered half of the leaf rule and was silent about the other

`LeavesBlock.getOptionalDistanceAt` is Minecraft's whole relation in one static
method: zero for anything in `BlockTags.LOGS`, the state's own `distance` for a
leaf, absent for everything else. Asking it produced the finding worth carrying
forward. **A tag's contents come from a data pack, and the oracle runs
Minecraft's static initialisation with no server and no pack loaded**, so
`BlockTags.LOGS` is empty. Every log in the game fell through to the property
test and came back as "no answer".

A rule reading that column alone would have put every leaf in the world at
distance seven and decayed the tree it was still attached to — a server that
eats builds, which is the failure this whole task exists to avoid. The oracle
prints `log_states=0` and **that zero is the evidence**. The log half now comes
from Dust's own tag table, which is extracted data and is where a tag belongs;
the column carries only the half that is Java.

This is the standing warning about a stand-in only exposing the defects its own
range reaches, in a shape it had not taken before: the stand-in was not
*narrow*, it was **half absent**, and the absent half read exactly like a
legitimate answer. What caught it was printing a count that had no reason to be
anything but zero.

### One column and not a property read, because scaffolding has a `distance`

`BlockStateProperties.DISTANCE` is the leaf's, 1 to 7. `STABILITY_DISTANCE` is
scaffolding's, 0 to 7, counted from a different thing, and **both are spelled
`distance` in a block state**. A rule that read the property instead of asking
the column would read a scaffold as a log and hold a whole canopy up. The
column says which states the number *means* something for; the number itself
comes from the protocol table both sides already share, and the extractor
checks that claim rather than assuming it —
`leaf_distance_disagreed=0` over all 280.

## What it costs

`cargo bench -p dust-server --bench updates`, median of five rounds, against a
50,000,000 ns tick:

```text
  an idle tick                              291 ns          0.0006%
  one break, a torch on a mined wall      1,271 ns/tick     0.0025%
    19 positions examined, 1 broken
  a 64-block sand column                  2,346 ns/tick     0.0047%
    1,223 examined, 64 fell, 64 landed, 237 ticks to settle
  a 32x32 raft of sand, 1,024 blocks     75,827 ns/tick     0.152%
    10,880 examined, 1,024 fell, 1,024 landed, 231 ticks
  an oak felled under its canopy            329 ns/tick     0.0007%
    1,790 examined, 387 relabelled, 90 leaves decayed, 5,057 ticks
```

Felling a tree costs 1.7 milliseconds in total and spends it over four minutes,
because the leaves are waiting on their own draws and not on the server.

The cheap half is what makes the rate affordable. 20,110 of 26,684 states
survive alone, do not fall and are not leaves, and for those a queued position
costs one block read and three bit tests. Only the 6,854 that need something
pay for the six reads that build a neighbourhood.

## Two ceilings, and the row that found one of them bounding the wrong thing

Vanilla delivers neighbour updates synchronously and recursively inside the
write, bounded by `max-chained-neighbor-updates`, which ships at a million: one
break may legitimately touch a million positions before the server does
anything else. Dust queues them and drains at most `PER_TICK` — 4,096 — a tick,
which turns an unbounded stall into a bounded rate. The visible cost is that a
torch falls off its wall on the tick after the wall went rather than in the same
instant, which is fifty milliseconds and which nobody can see.

The second ceiling is `MAX_ENTITIES`, 512 falling blocks at once. The
thirty-two by thirty-two raft of sand was written to prove that ceiling was in
the right place and instead found it in the wrong shape: it measured
`fell 512, landed 512` out of 1,024, and **the other 512 hung in the air for
ever**, because `launch` returned quietly on a refusal and nothing ever queued
those cells again. That is the defect this record exists to remove, reproduced
by its own ceiling at a scale an ordinary gravel pit reaches.

A refused cell now goes back on the schedule. All 1,024 fall, with 29,184
deferrals in the middle of it, and the raft takes 231 ticks instead of a
hundred.

**A ceiling that bounds a rate is a server that stays up. A ceiling that bounds
an outcome is sand hanging in the air.** That is the general form, and it is
the one to carry to the next ceiling somebody adds.

## Why the queue is not the edit channel

The sessions already listen to a `broadcast` of edits and having this listen
there too would have cost no new field and no new lock in the write path. It is
wrong, and the reason is the same failure mode: **a `broadcast` channel drops
the oldest for a receiver that lags, and the receiver that lags is exactly the
one draining a cascade.** A dropped edit is a torch that stays in the air for
ever. The queue is explicit, deduplicated by position so a wall of sand is not
queued six times per cell, and says out loud when it overflows.

## Why leaves are not random-ticked

Vanilla decides a leaf's fate on a random tick: three positions are drawn out
of each sixteen-cubed section every tick, so a decaying leaf waits a mean of
1,365 ticks — about a minute — before it goes. **That wait is the whole look of
a felled tree.** A canopy that vanishes with the trunk reads as having been part
of the trunk; one that pops out over a minute reads as a tree dying.

A random tick over every loaded section is an O(loaded world) step run twenty
times a second whose only caller here is a few hundred leaves. Drawing each
leaf's wait from the same geometric distribution the moment it becomes decayable
gives a player the identical thing to look at and costs one draw per leaf. The
first decision-rule priority could not tell the two apart; the second decided
it. Five minutes is the horizon, which cuts the 1.2% tail.

## Measured against a real server, and watched to fail

`tools/bot/updates.js` stands a block in a shell of six, takes one neighbour
away, and records what a **vanilla** server did.
`cargo xtask harness updates` asks `dust_sim::updates` the same six questions of
the operator's own constants table:

```text
                                        as it is    support columns cut
  rows scored, of 255 read                   243                    243
  agree with the server                      231                    195
  Minecraft broke and Dust keeps              12                     48
  Minecraft kept and Dust breaks               0                      0
  never stood the block up                    24                     24
  reshaped rather than broke (D14's)           9                      9
  the arena and not a rule                    12                     12
```

**Zero rows in the dangerous direction**, in both columns. The 12 that remain
are 10 vine — a multiface state, one of the 379 the probe could not resolve —
and 2 `oak_door`, which vanilla breaks through `updateShape` rather than
`canSurvive` and which is a different mechanism this record does not build.

Cutting the support columns is the negative control and it moves 36 rows, which
is the check biting.

### The arena is not a rule, and counting it as one was a defect in the scorer

`/setblock` places a state without asking `canSurvive`, so a dandelion can be
stood in a shell of stone it could never legitimately occupy — and it then dies
at the first update whichever side moved. Five of the first run's disagreements
were that, and five more were an oak sapling doing the same thing. A shell that
refused the block on **all six sides** measured the arena; no state in 1.21.1
needs all six of its neighbours. Those rows are now named and set apart rather
than counted against Dust. In the dirt shell the same two blocks agree with
Minecraft on all twelve rows.

### The gate that runs against Dust rather than against the answers

The scorer cannot see the queue, the tick loop, the entity or a single packet.
**A rule that is right in a crate nobody calls is a world that does not move.**
`node updates.js <port> --check` does everything a player does — a creative
inventory write, a right-click, a dig — and reads the answer out of the
block-change packets rather than out of `bot.blockAt`, which lags by an
unbounded amount:

```text
  10/10 against a release build on a flat world
   6/10 with `Rules::reacts` forced to false
```

The four that stay green under that mutation are the controls, and one of them
had to be repaired to be one: the cobblestone control originally failed under
the mutation because the torch above it never broke and the cobblestone had
nowhere to go. **A control that fails when its own subject is fine has stopped
being a control**; the arena is cleared before it now.

Six unit checks in `dust_sim::updates` were each watched to fail: the log tag
ignored, the leaf column ignored, `persistent` ignored, `reacts` forgetting
leaves, the step from a neighbour costing nothing, and a missing column meaning
every state is a leaf. The last of those found a **vacuous check** —
the test helper writes the leaf column whatever is in it, so "a table with no
leaf column" was a table of zeroes and the mutation slid straight past. It cuts
the column now.

## Options considered

**Deliver updates synchronously inside the write, as vanilla does.** ❌ Correct
and unbounded. One break legitimately touching a million positions is a
sixteen-second stall on this machine at the measured rate, during which the
server answers nobody.

**Break the 379 states the probe could not resolve.** ❌ Player experience
decided it and it is not close. A world that keeps a block it should have
dropped is a bug in one block; a world that drops blocks it should have kept is
a server that eats builds. They never break.

**Ship a table of block names for the falling and leaf sets.** ❌ Thirty rows of
Mojang's data in this repository, stale the first time a version adds a colour.
`FallingBlock.isInstance` and the leaf column are the same answer, extracted.

**Recompute a leaf's distance by flood fill from the trunk.** ❌ It is the same
argument decision record 0014 made about a wall's post: right, and an unbounded
number of block reads for one break. The cascade through the queue reaches the
same fixed point one ring per pass and costs 387 writes for a whole oak.

**Random ticking, so leaves decay the way vanilla decays them.** ❌ See above.
Rejected on cost for an outcome a player cannot distinguish, and it is worth
saying that a random tick will be wanted eventually for crops and grass — this
record declines it *for leaves*, not for ever.

## What is still wrong

**Fluids are not built and a bucket still does nothing.** This is deliberate
and it is the next record. Water and lava need a flow state Dust has no
representation for, a fluid tick queue separate from this one, and the source
rules that make cobblestone and obsidian; they are the classic way a Minecraft
server is killed and they are larger than everything above put together. The
measurement asked for was a bucket emptied on a flat plain: today it costs
**0 ns, because nothing happens**, and that is the defect rather than the
result.

**Two `oak_door` rows** break through `updateShape` rather than `canSurvive`.
The top half of a door whose bottom half is mined is the visible one.

**The 379 unresolved states** keep blocks a vanilla server would drop. 232 of
them are lichen and sculk vein, which a player rarely mines the wall out from
under.

**A refused position is lost, not deferred.** `MAX_PENDING` is 16,384 and past
it new positions are counted and dropped. The falling ceiling was fixed by
deferring; this one cannot be, because there is nowhere to defer *to* — the
queue being full is the condition. It has not been reached in any measurement
here and the counter is what would say it had.

**Placement's tail is unchanged and still parked**: 21 items wrong on the grid,
18 on the scenes, 35 hanging-sign rows.
