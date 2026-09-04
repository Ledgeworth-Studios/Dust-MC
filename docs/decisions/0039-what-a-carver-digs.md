# D39 — What a carver digs

**Status:** Built, measured, wired to the socket and checked with a
third-party client. Stage four of decision record
[0012](0012-what-worldgen-is-worth-measured-first.md) is finished:
**97.3% and 95.4% of the cells Minecraft carved are open in Dust too**, and
in a 33x33 footprint at seed 0's spawn a player standing on the served world
now has 1,738 cells of air under them where they had eight.

## Context

D35 put the aquifer in and left a clean number behind it:

```text
                 cells Minecraft left open below its own surface
                 that Dust did not          ... of which Dust flooded
  seed 0                    187,614                              23
  seed 1                    221,438                              23
```

Twenty-three. Nothing was hiding in that count any more; it was caves that
were not dug. This record digs them.

## The algorithm is code, and code is reachable — again

Three things about a carver are in the operator's pack, and all three are read
at run time: `worldgen/configured_carver/*.json` (three of them in the
overworld — `cave`, `cave_extra_underground`, `canyon`), the `carvers` list in
each `worldgen/biome/*.json`, and the `#minecraft:overworld_carver_replaceables`
block tag that says what a carver may cut through.

Everything *done* with them is `CaveWorldCarver.java`, `CanyonWorldCarver.java`,
the `WorldCarver` half they share, and `ChunkGenerator.applyCarvers`. D8's route
reaches those: `javap -p -c` on the inner server jar in the operator's own
`.dust-extract`, read through the ProGuard mappings Mojang publishes beside it.
The mappings keep line numbers, so a method's bytecode lines up against the Java
it came from and reads as control flow rather than as a stack machine.

What came out of it, none of which a careful guess would have got right:

* **A chunk is carved by the carvers of 289 chunks, not by its own.**
  `applyCarvers` walks `-8..=8` in both axes and re-seeds one generator per
  neighbour with `setLargeFeatureSeed(seed + index, cx, cz)`, where `index` is
  the carver's position in the biome's list. A tunnel is up to 112 steps long,
  so one that starts nine chunks away can still arrive — and drawing all 289
  every time is what makes a chunk depend on nothing but its coordinates.
* **`setLargeFeatureSeed` throws two `nextLong` draws away**, and they are what
  make caves depend on the world seed at all: `setSeed(seed)`, draw `a` and `b`,
  then `setSeed(x*a ^ z*b ^ seed)`. Read as "hash the coordinates" and skipped,
  every world would get the same caves in different places.
* **The stream is `java.util.Random`,** not the xoroshiro modern worldgen uses.
  `nextInt` has two paths, and a power-of-two bound takes the other one: a cave
  draws `nextInt(15)` and `nextInt(16)` on its first two lines.
* **A tunnel turns by `Mth.sin` and `Mth.cos`, which are a 65,536-entry table
  indexed by a truncated `float`,** not by the real functions. The table is
  `sin(i * PI * 2 / 65536)`; the error against a real sine is small per step and
  a tunnel takes a hundred of them.
* **A cave's floor is flat because `shouldSkip` answers before the ellipsoid is
  consulted:** `relY <= floorLevel` returns "skip" outright, and only then is
  `relX² + relY² + relZ² >= 1` asked. A symmetric reading gives round tubes.
* **The fork at the halfway step replaces the rest of the tunnel rather than
  adding to it** — it returns immediately after — and both halves are given a
  thickness of `nextFloat() * 0.5 + 0.5`, which is below the `> 1.0` a fork
  needs. So a fork never forks again, and the recursion is two deep by
  construction.
* **`getCarveState` asks the aquifer at density 0.0,** so the barrier is still
  deciding inside a cave: 11,350 cells of seed 0's sample were left standing by
  it. Below `lava_level` (`above_bottom 8`, so y ≤ -56) it does not ask at all.
* `carveEllipsoid` refuses the top seven rows of the world and the bottom one,
  and its x/z bounds are asymmetric — `floor(x - r) - minX - 1` against
  `floor(x + r) - minX` — so a cave is one block wider on its low side.
* A canyon is a stack of independently widened slices: `initWidthFactors` draws
  one width per world row, redraws it every `width_smoothness` rows, and squares
  it. That is what makes a ravine ribbed rather than smooth-walled.

**Nothing Mojang's is committed.** What is in `dust_gen::carver` is this
project's own arithmetic; every number the world is generated *from* still
arrives at run time from the operator's own pack, exactly as D6, D7 and D8 say.

## Where it runs

After the surface rules and before features, because that is where vanilla's
chunk statuses put it: `noise`, `surface`, `carvers`, `features`. A tunnel cuts
through finished ground, which is the only reason `carveBlock` has a clause
about grass at all.

It reuses the `Filler`'s own `Aquifer::Flow` rather than building a second one.
`fill_with_aquifer` has already pointed that at the chunk and jittered its grid
centres; a carver with its own would pay for all of it twice and hold a second
evaluator over the same graph. Resource cost decided it.

## What was declined, with the count

**`CarvingContext.topMaterial` under a carved grass block.** When a carver eats
a grass block, vanilla sets a flag and, for each block it carves below it in
that column, re-runs the surface rules on the block underneath if that block is
dirt — so a cave that opens into a meadow has grass on its floor. Dust counts
where this would fire and does not do it: **1,163 columns on seed 0 and 1,150 on
seed 1**, out of 76,800. Doing it means running the surface rules at a single
arbitrary position with a "there is fluid here" flag, which `Painter` has no
entry point for; it is a surface-rules change and not a carver one, and it moves
no cell from solid to open. It belongs with the next stage's work. The number is
printed by `cargo xtask harness worldgen` every run so it cannot be forgotten.

**Biomes that disagree about their carvers are refused by name at boot.**
Vanilla asks the biome source at each of the 289 neighbours' own corners, per
chunk. Every biome of a vanilla overworld names the same three carvers in the
same order — all 53 of them, checked against the pack — so the lookup cannot
change the answer and is not done. A pack where it *could* is told so with both
biome names rather than quietly given the first biome's caves everywhere, which
would look right. Honouring two lists would cost 289 climate lookups a chunk;
that is the price, and it is written down here rather than paid on a guess.

## What it scores

`cargo xtask harness worldgen --version 1.21.1 --seed <n> --radius 2` over
D21's twelve scattered 5x5 squares: 300 chunks, 76,800 columns, 29,491,200
cells. Every figure is a count of things **wrong**.

```text
seed 0 — 17 biomes in view

  surface  surface     biome    caves      false      blocks  chunk    KiB
    short    block     short  missing      caves       short  cols/s  /col
    76800    76800    435459        0    9598921    10005374    6238   2.2  the flat world Dust served
    74905    74931    435459   583625     795317    10475058    1829  16.2  + the world's own sea level
    74905    74931       382   583625     795317    10475058     223  16.6  + Dust's biome source
    28796    49128       382   588215      75560     6840919     108  18.8  + Dust's terrain
    28796    32398       382   588215      75560     2529567      43  18.8  + Dust's surface rules
    28784    32393       382   187614      90892     2097950      48  18.8  + Dust's aquifers
    28349    32345       382    18433      92418     1838763      37  18.8  + Dust's carvers            <- this record
        0    60037         0   681715          0    10405644    1179  19.6  + Minecraft's surface height
        0    60037         0        0          0     9723929     871  19.6  + Minecraft's carvers
        0        0         0        0          0       12140     954  20.6  + its blocks at and below it
        0        0         0        0          0           0     746  20.7  + its blocks above it (control)

seed 1 — 20 biomes in view

    76800    76800    449472        0    9610683    10028458    6571   2.2  the flat world Dust served
    72552    74345    449472   527846     746183    10434876    1618  16.2  + the world's own sea level
    72552    74345       238   527846     746183    10434876     153  16.6  + Dust's biome source
    16678    54815       238   541571      33148     6807705     104  19.0  + Dust's terrain
    16678    23025       238   541571      33148     2376862      68  19.0  + Dust's surface rules
    16629    22969       238   221438      42614     2045421      33  19.0  + Dust's aquifers
    16064    22943       238    30431      46671     1809196      62  19.0  + Dust's carvers            <- this record
        0    47083         0   662830          0    10402908    1691  19.4  + Minecraft's surface height
        0    47083         0        0          0     9740078    1122  19.4  + Minecraft's carvers
        0        0         0        0          0       10180    1156  20.4  + its blocks at and below it
        0        0         0        0          0           0     964  20.5  + its blocks above it (control)
```

**Missing cave cells: 187,614 -> 18,433 and 221,438 -> 30,431.** As a rate,
97.296% and 95.410% of the cells Minecraft carved are open here too.

**And it is right in both directions, which is the stronger half.** Dust's
carvers changed 276,430 cells on seed 0. Missing caves fell by 169,181 and false
caves — cells Dust opens that Minecraft filled — rose by 1,526. A carver that
was one draw out of step would dig just as many holes and put them somewhere
else, and both columns would go up together. Only one did.

The rest of the ladder is unchanged to the cell: the aquifer row still reads
187,614 / 90,892 / 2,097,950 and the control still reads zero everywhere.

## What it costs

The wide run above says 48 -> 37 cols/s on seed 0 and 33 -> 62 on seed 1. The
second of those is impossible, and it is the tell: three other builds were on
this ten-core machine and the ladder's timer measures wall time. **A cost
measured under varying load is not a cost.**

Measured the way a paired comparison has to be — the same chunks, the two rungs
consecutive in the same process, three times over. Four of the twelve squares,
chosen inland so that there are caves to cut: 100 chunks, 25,600 columns, and
the carvers take 66,120 missing cave cells down to 6,316 across them.

```text
                                   cols/s
                              run 1  run 2  run 3
  + Dust's surface rules         65     65     65
  + Dust's aquifers              63     60     62
  + Dust's carvers               62     60     62
```

Repeated on the origin square alone (25 chunks), three more times: 64/64/66
against 64/63/67.

**Nothing distinguishable from noise, on either sample.** That is not luck, it is arithmetic: a
chunk costs about four seconds to build in this verb, and carving it is 867
generator seedings plus about 68 tunnel walks, most of which leave after
`canReach` says the chunk is out of range. The 17x17 neighbourhood sounds
expensive and is not, because the work per neighbour is a `setSeed` and a
`nextFloat`.

The one thing that *would* have been expensive is the per-neighbour biome
lookup, and that is the decline above.

Memory is one bit per cell of one chunk — a 12 KiB carving mask per generating
thread, reused, allocated once. KiB/col does not move: a carved chunk holds air
where it held stone and a paletted container does not care which.

## What a player gets

`tools/bot/openness.js`, a third-party mineflayer client, reading the block
under every cell of a 33x33 footprint at seed 0's spawn, y -60..59 — 130,680
cells. The same binary in both rows but for the one line that chooses the
generator stage, both release builds:

```text
                      air   of which below y 0   water    solid
  before (D35)          8                    8     572   130100
  after                1738                1451     650   128292
```

Eight cells of air became 1,738, and 1,451 of them are below y 0. That is the
whole point of the stage: underground is somewhere to go.

`check.js` is **22/29, the same seven**, unchanged — all of them seed 0's water
spawn column and none of them touched by this.

## Checks, and every one of them watched to fail

Seven new tests, each mutated and confirmed red before being believed:

| the check | the mutation that broke it |
| --- | --- |
| `legacy_is_java_util_random` | `0xB` -> `0xC` in the LCG increment |
| `sin_is_the_table_and_not_the_function` | `mth_sin` -> `f32::sin` |
| `a_carver_cuts_what_its_tag_names…` | the replaceable tag ignored |
| the same check, second half | nested tags not followed |
| `a_chunk_draws_the_carvers_of_every_chunk_within_eight` | `-8..=8` -> `-1..=1` |
| `a_chunk_is_carved_the_same_however_it_is_visited` | the mask not cleared |
| `biomes_that_disagree…_are_refused` | the disagreement accepted |
| `a_dimension_whose_biomes_name_no_carver_has_none` | the empty list kept |

The golden values in the first are a JDK's, printed by a five-line Java program
on this machine — `java.util.Random` is specified in its own javadoc and is not
Mojang's. The second is a differential that requires the two sides to
**disagree**: a version of this file that called the real sine would pass every
other test here and put every cave in the wrong place.

## What is still wrong

18,433 cells on seed 0 and 30,431 on seed 1, and the top of the "Minecraft has
where Dust is wrong" list is now tuff, andesite, diorite and granite — stone
variants, which are *features*. Mineshafts, dungeons, geodes and ancient cities
all open cells below Minecraft's surface and none of them is a carver; they are
D12's stage five and its structures. The 1,163 declined grass floors are in
there too.

Also unchanged and still open: icebergs (2,284 and 2,246 columns),
`minecraft:temperature` (asked 0 times, so the gap is still nobody's), and the
badlands bands (0 blocks).
