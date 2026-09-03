# D12 — What worldgen is worth, measured before it is written

**Status:** Measured. Nothing generated yet, and that is the point. This record
is the order of the work and the numbers the order was chosen from.

## Context

Dust generates a superflat: bedrock at the world's floor, three rows of dirt,
one of grass at y -60, air above, `minecraft:plains` everywhere, and every
column of every chunk identical. A column a real world does not contain falls
back to it, because a world is a disc in an infinite plane and a player can
walk off the edge of it. `dust_server::net::world::FlatWorld` says all of that
in its own module note and has never pretended otherwise.

Phase 6 replaces it. The temptation is to start with noise, because noise is
where a terrain generator visibly begins. This project has a method that has
been right every time instead: measure against the real thing first, run a
**ladder** — several models over the same input in one run, each row the one
above it plus a single named change — and put the number in a record. D8 sat
open for two months because the right answer had never been priced. D10 costed
a 3x3 light volume, declined it, and named the cheaper thing to build instead;
the cheaper thing turned out to close the *smaller* share and was still the
right order.

So: `cargo xtask harness worldgen`, and no noise.

## What it does

Reads a world Minecraft generated for a seed, builds the same chunks with
Dust's own generator, and counts five things a player standing in the world
would notice. Five and not one, because **a percentage hides which half it is
about**: a world that is 96% air agrees with any other world that is 96% air,
and a single "blocks match" figure is a fact about how much sky is in view.

* **surface height** — is the ground at the right y, per column, from
  `MOTION_BLOCKING`, the map `spawn_at` already stands players on.
* **surface block** — is it the right block underfoot, asked at *Minecraft's*
  surface y, because where the ground is is not a thing the model gets to
  decide.
* **biome** — per 4x4x4 cell, and how many kinds each side has in view at all.
* **caves** — of the cells Minecraft carved below its own surface, how many are
  open in Dust, and, kept separately, how many cells Dust opens that Minecraft
  filled.
* **blocks** — every cell, state for state. The total the other four are slices
  of.

Seven models, in the order vanilla's own pipeline runs. Rows three and below
each hand Dust one more of Minecraft's answers, read out of the region file;
**none of those is a mode a server could run in**, which is the same device
`harness light`'s last rung uses. The last row hands over everything and has to
be exact.

## What each stage is worth

`cargo xtask harness worldgen --version 1.21.1 --seed <n> --radius 4`, over 81
chunks: 20,736 columns, 7,962,624 cells, 124,416 biome cells. Every figure is a
count of things *wrong*.

```text
seed 0 — inland, forest and a lake (198,471 cells carved)

  surface  surface     biome    caves    false      blocks  cols/s  KiB/col
    short    block     short  missing    caves       short
    20736    20736    124416        0  2416513     2520828   10256      2.2  the flat world Dust serves
    17467    17551    124416   172558    52673     2627298    2608     16.2  + the world's own sea level
    17467    17551         0   172558    52673     2627298    2550     16.6  + Minecraft's biomes
        0    13497         0   198471        0     2632197    1835     18.9  + Minecraft's surface height
        0    13497         0        0        0     2433726    1078     18.9  + Minecraft's carvers
        0        0         0        0        0         360    1325     19.6  + its blocks at and below it
        0        0         0        0        0           0    1187     19.6  + its blocks above it (control)

seed 1 — open ocean (126,604 cells carved)

    20736    20736    124416        0  2382452     2495313    9896      2.2  the flat world Dust serves
    20736    20736    124416   126604        0     2616655    2607     16.2  + the world's own sea level
    20736    20736         0   126604        0     2616655    2483     16.5  + Minecraft's biomes
        0    20736         0   126604        0     2595919    2507     16.5  + Minecraft's surface height
        0    20736         0        0        0     2469315    1171     16.5  + Minecraft's carvers
        0        0         0        0        0           0    1052     17.3  + its blocks at and below it
        0        0         0        0        0           0    1157     17.3  + its blocks above it (control)
```

**The control is exact on both seeds and all five scores.** That is the row
that says the four above it are about the generator and not about the scorer.

### Two seeds disagree about nearly every number, which is why there are two

D10's lesson, again and immediately. Seed 0 spawns inland and seed 1 in open
ocean, and they disagree about the size of every stage:

* Surface **material** is 13,497 columns inland and **all 20,736** over ocean —
  and over the ocean it is one block. Water.
* **Caves** are 198,471 cells inland and 126,604 over ocean.
* **Plants above the surface** are 360 cells inland and **zero** over ocean.
* The sea-level rung gets 3,269 columns right inland and **not one** over
  ocean, for a reason worth its own heading.

A generator scored on seed 0 alone would be told features are 360 cells and
surface rules are two thirds done. Scored on seed 1 alone it would be told
surface rules are everything and features do not exist.

### Sea level 63 is one too high, everywhere there is water

Every one of seed 1's 20,736 columns is short by **exactly +1** under the
sea-level rung, and `+1 x 4563` is the largest single bucket on seed 0 too.
`sea_level: 63` names the level the water reaches *to*; the topmost water block
is the one below it, at y 62. A generator that writes its top water block at
`SEA_LEVEL` is one block off on every ocean column in the world, and the
harness would say so on seed 1 in a single line before anyone swam.

Recorded rather than corrected. The rung is the documented constant, not a
constant tuned until the answer came out right.

### Raising the flat world to sea level makes the block count *worse*

Seed 0: 2,520,828 cells wrong becomes 2,627,298 — **106,470 worse** — while
3,269 columns' surface becomes right and 3,185 gain the right block underfoot. A solid dirt fill to y 63 replaces stone with dirt below and air
with dirt above, and it buries 172,558 cells of cave that the flat world had
left open by having no rock there at all.

This is the strongest single argument in the record for scoring five things.
One number would have called this change a regression. It is an improvement in
the two scores a player reads with their feet and a regression in the one
nobody can see.

### The flat world "has" every cave, and that is what the false column is for

The first run of this verb printed rates, and the flat world scored **100% on
caves** on both seeds — a world with no rock above y -60 contains every cave
Minecraft carved. Beside it sit 2,416,513 cells of stone it had turned into
sky. The summary now prints counts and a **false caves** column, and no rate at
all.

### Leaves are the largest entry in the surface worklist and are not a surface rule

What Minecraft has underfoot where Dust does not, seed 0:

```text
minecraft:oak_leaves      7716
minecraft:grass_block     7239
minecraft:water           3508
minecraft:birch_leaves    1946
minecraft:sand             188
minecraft:stone             56
minecraft:gravel            22   ... and 7 more kinds
```

`MOTION_BLOCKING` counts leaves, so over a forest the block a player stands on
is a **tree**. Trees are a feature and not a surface rule, and they are handed
over by the "at and below it" rung rather than the one above it. A surface-rule
engine scored against this column without that said would be blamed for the
forest.

### Biomes are all-or-nothing, and a 9x9 cannot score them

124,416 of 124,416 biome cells are wrong on both seeds — Dust puts plains
everywhere — and **Minecraft has two kinds in each square**. Two biomes in 144
blocks by 144 is the multi-noise field being smooth at this scale, which means a
9x9 is not a test of a biome source; it is a test of whether one of two answers
came out. The sample has to be scattered before that stage is scored, and
`--at` is already the flag for it.

## What it costs, which for worldgen is not a footnote

This code runs for every chunk a player walks toward, forever, so the ladder
weighs and times every rung as well as scoring it.

**Bytes held, per column, as the paletted containers store them:**

```text
  block states + biomes    flat 2.2 KiB    real 19.6 KiB inland, 17.3 over ocean
  sky + block light        96 KiB, every column, every model, unconditional
```

A real column's terrain is **8.9x** a flat one's. And the largest single cost of
holding a column is not the terrain and never was: `LightArray` is an
unconditional `Box<[u8; 2048]>` and a section has two, so 96 KiB of every column
is light whether or not anything ever lights it — 4.9x the block storage of a
real column and 44x that of a flat one. At the default view distance a join
streams 289 columns: 0.6 MiB of blocks today, 5.5 MiB once terrain is real, and
**27 MiB of light either way**.

**Time.** The harness writes a full column at 1,000–2,600 columns per second in
release, its own writes only, region reads excluded, against about 10,000 for
cloning the shared flat template. That is the same order as `dust-world`'s own
bench — 0.5 ms to build a flat column and 0.9 ms to light it — and it is the
cost of *writing the blocks*, before a single noise sample has been taken. 289
columns at 1,300/s is **222 ms** of pure block writing on a join.

Two things follow, and both are for the person who writes the generator:

* **The template goes the moment two columns differ**, and with it the 10,000
  columns per second. That is stated in `FlatWorld`'s own note as a consequence;
  this is the number attached to it.
* **The write is already a real cost.** A density function that is free would
  still leave a join at a fifth of a second of `set_block`. Whatever the
  generator does, it should fill a section rather than walk it — a paletted
  container written cell by cell pays a palette lookup 4,096 times for a section
  that holds two values.

## The decision — the order of the work

1. **The biome source.** 124,416 of 124,416 cells, the only stage that is wholly
   absent, and every stage below it takes the biome as an input. **First widen
   the sample**, because two biomes in a 9x9 cannot tell a right biome source
   from a lucky one; several small squares at distant `--at` cost 108 s of
   vanilla each and no new code.
2. **The density functions.** 20,736 of 20,736 columns on both seeds, and the
   first thing a player sees. Score it on surface height and *not* on cells: the
   rung that hands the shape over makes the block count 4,899 worse on seed 0.
3. **Surface rules and aquifers.** 13,497 columns inland, all 20,736 over ocean,
   where the entire answer is one block. The off-by-one above is waiting here.
4. **Carvers.** 198,471 cells inland and 126,604 over ocean, and the false-cave
   column is already built to keep the score honest.
5. **Features.** 360 cells inland and none over ocean — and **cells are the
   wrong unit for them**. 322 cells of short grass is what a meadow looks like,
   and a tree is a hundred cells of the thing a player walks toward. Named last
   because it is smallest, and named explicitly because the count understates
   it more than any other row here.

### What was costed and declined

* **A wider square.** A 9x9 is 108 s of vanilla and about a second of scoring
  per seed, and going wider is linear in both. The biome finding says the
  *shape* of the sample matters more than its size, so a scattered sample is
  the cheaper next move and `--at` already exists for it. Declined until stage
  one needs it.
* **Lighting the generated chunks here.** `harness light` owns that number and
  two verbs answering for one number is how a measurement drifts. The 96 KiB is
  still counted, because it is what a column *holds*.
* **Block entities, entities and scheduled ticks.** Excluded by construction,
  as `harness capture` excludes them and for the same reason. Worldgen does not
  place them; a chest in a village will, and that is a later record.
* **Reading the region file at run time as a "generator".** Rows three and below
  do exactly that and every one of them says in its own name that no server
  could run it. They are ceilings, not candidates.

## The harness caught itself twice, which is the part worth keeping

**Once on the control.** The first run reproduced every block of seed 0 exactly
and still reported **352 columns whose surface was wrong** — `+1 x 344` and
`+2 x 8`. The built chunk's heightmaps were recomputed with `state != air`
while Minecraft's side used Minecraft's own `MOTION_BLOCKING` predicate, so the
two sides were being asked different questions and the difference was short
grass and flowers. That is D10's finding, in a second place, found the same way
and by the same kind of row. The fix is one line; the control is what made it a
line rather than a fortnight.

**Once on the rate.** See the false-caves heading. A rate said 100% about a
world made of sky.

Both are the same shape as every finding this harness has produced: **a
comparison that asks the two sides different questions measures itself**, and
the only defence is a control that hands everything over and has to come out
zero.

## Consequences

- `cargo xtask harness worldgen` exists, is a measurement and not a gate, and
  exits 0 unless the run itself failed — the same contract as `light` and
  `placement`, for the same reason: a verb that goes red for a known gap is red
  every time it runs and is read by nobody.
- **The control is checked in CI**, against chunks this harness constructs
  itself and nothing of Mojang's. Both directions: the control agrees, and one
  changed block makes it disagree in exactly the three scores that block
  touches. Watched to fail — narrowing `is_air` to `minecraft:air` alone turns
  the cave rung red, and skipping the biome copy turns the control red.
- **Adding a rung is how the next stage gets measured.** When a biome source
  lands, its rung replaces "+ Minecraft's biomes" and the row below it says
  what it bought.
- Nothing Mojang's is committed. The worlds are generated into the harness
  cache outside the repository, the block constants come from the operator's
  own jar through `cargo xtask extract`, and the run says which of the two
  surface predicates it had.

## Related

* D10 — the ladder this one is built in the shape of, and the heightmap
  predicate whose absence this verb re-found on its first run.
* D8 — why Minecraft's own constants arrive at run time from the operator's
  jar, which is the route this verb reads its surface predicate through.
* D6 — the ore baseline, the one piece of worldgen already measured, and the
  stated exception to keeping vanilla's numbers out of the tree.
