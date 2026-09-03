# D35 — What a cave holds

**Status:** Built, measured, wired to the socket and checked with a
third-party client. Stage four of decision record
[0012](0012-what-worldgen-is-worth-measured-first.md) turns out to be two
stages, and this is the larger one: **588,215 missing cave cells become
187,614, and 541,571 become 221,438.** A player who walks into a cave under
the sea now walks; before this they drowned.

## Context

D32 built the surface rules and, at the end of doing so, found the number that
reordered the rest of the work:

```text
                 cells Minecraft left open below its own surface
                 that Dust did not          ... of which Dust flooded
  seed 0                    588,215                         400,638
  seed 1                    541,571                         320,233
```

D12 had ordered carvers fourth on a single "caves missing" count that could not
tell a hole nobody dug from a hole full of water. Two thirds of seed 0's and
three fifths of seed 1's was the second one. So the aquifer went first.

D32 declined it in one clause: *"their algorithm is not in the pack."* That is
true, and it is not a reason to stop. It is a reason to go and read it.

## The algorithm is code, and code is reachable

The pack carries the four noises an aquifer reads — `aquifer_barrier`,
`aquifer_fluid_level_floodedness`, `aquifer_fluid_level_spread`,
`aquifer_lava`, all four named in the overworld's `noise_router` — and
`aquifers_enabled`, and nothing else. Everything that is *done* with them is
`Aquifer.java`.

`javap -p -c` on the inner server jar in the operator's own `.dust-extract`,
read through the ProGuard mappings Mojang publishes beside that jar, recovers
`Aquifer$NoiseBasedAquifer` constant for constant and branch for branch — the
route D8 established and the one D32's own surface constants came out of. The
mappings keep line numbers, so a method's bytecode can be lined up against the
Java it came from and read as control flow rather than as a stack machine.

What came out of it, none of which a careful guess would have got right:

* The grid is **16 x 12 x 16** and a block's cell is `floorDiv(x - 5, 16)`,
  `floorDiv(y + 1, 12)`, `floorDiv(z - 5, 16)` — the offsets are what stop the
  aquifer boundaries lining up with the chunk grid, and the asymmetric search
  window (`0..=1` in x and z, `-1..=1` in y) only covers the block *because* of
  them.
* Each cell's centre is jittered by `nextInt(10)`, `nextInt(9)`, `nextInt(10)`
  from a factory that is the world's own positional random hashed by the name
  **`minecraft:aquifer`** and forked — not the world seed, and not the same
  stream the noises come from.
* `similarity(a, b) = 1 - |b - a| / 25`, over **squared** distances.
* The pressure ramp is **asymmetric about the midpoint** — `/1.5` and `/2.5`
  above it, `3.0 +` then `/3.0` and `/10.0` below — which is why an aquifer's
  surface is a lid and not a bubble. A symmetric reading of it would put rock
  in the wrong half of every wall in the world.
* A dry aquifer is not a special case. It is given
  `DimensionType.WAY_BELOW_MIN_Y`, which is `-2048 << 4`, so `at(y)` answers
  air everywhere *and* `calculate_pressure` measures a real distance between a
  dry aquifer and a wet one.
* The surface level is sampled at **thirteen chunk offsets**, in an order whose
  first entry is the centre — and the centre's own early return is what lets
  every block of open sky answer after one lookup.
* The two deep-dark thresholds are `float` constants widened to `double`:
  `-0.22499999403953552` and `0.8999999761581421`. A `double` `-0.225` is not
  the same number and would move the boundary of every ancient city.
* Lava sits under `min(-54, sea_level)`, and a deep aquifer turns to lava when
  its level is at or below -10 and a noise sampled on a **64 x 40 x 64** grid
  exceeds 0.3 in absolute value.

**Nothing Mojang's is committed.** What is in `dust_gen::aquifer` is this
project's own arithmetic; every number the world is generated *from* still
arrives at run time from the operator's own pack, exactly as D6, D7 and D8 say.

## Where it runs

Inside the noise stage and under the surface rules, because that is where
vanilla puts it: a `water` condition in a surface rule reads what the aquifer
decided. `Material` grew a fourth code, `Lava`, resolved by the caller against
its own registry like the dimension's other two — and refused by name at boot
rather than defaulted, because a generator that quietly filled a deep cave with
air would look right.

`minecraft:lava` is the one block name in this file that no pack carries.
`Aquifer.java` names `Blocks.LAVA` directly. It is a name and not a table, and
it lives in exactly one function.

One place this file knowingly says something narrower than vanilla: vanilla's
lava-meets-water wall names `Blocks.WATER`, and this names *the dimension's own
default fluid*. Every dimension whose settings turn aquifers on has water as
its default fluid, so the two agree today; a pack that made them differ would
get the more sensible of the two answers.

## What it scores

`cargo xtask harness worldgen --version 1.21.1 --seed <n> --radius 2` over
D21's twelve scattered 5x5 squares: 300 chunks, 76,800 columns, 29,491,200
cells. Every figure is a count of things **wrong**.

```text
seed 0 — 17 biomes in view

  surface  surface     biome    caves      false      blocks  chunk    KiB
    short    block     short  missing      caves       short  cols/s  /col
    76800    76800    435459        0    9598921    10005374   11187   2.2  the flat world Dust served
    74905    74931    435459   583625     795317    10475058    2797  16.2  + the world's own sea level
    74905    74931       382   583625     795317    10475058     328  16.6  + Dust's biome source
    28796    49128       382   588215      75560     6840919     171  18.8  + Dust's terrain
    28796    32398       382   588215      75560     2529567      87  18.8  + Dust's surface rules
    28784    32393       382   187614      90892     2097950      76  18.8  + Dust's aquifers          <- this record
        0    60037         0   681715          0    10405644    2021  19.6  + Minecraft's surface height
        0    60037         0        0          0     9723929    1361  19.6  + Minecraft's carvers
        0        0         0        0          0       12140    1438  20.6  + its blocks at and below it
        0        0         0        0          0           0    1172  20.7  + its blocks above it (control)

seed 1 — 20 biomes in view

    76800    76800    449472        0    9610683    10028458   10404   2.2  the flat world Dust served
    72552    74345    449472   527846     746183    10434876    2700  16.2  + the world's own sea level
    72552    74345       238   527846     746183    10434876     245  16.6  + Dust's biome source
    16678    54815       238   541571      33148     6807705     156  19.0  + Dust's terrain
    16678    23025       238   541571      33148     2376862      87  19.0  + Dust's surface rules
    16629    22969       238   221438      42614     2045421      76  19.0  + Dust's aquifers          <- this record
        0    47083         0   662830          0    10402908    2134  19.4  + Minecraft's surface height
        0    47083         0        0          0     9740078    1414  19.4  + Minecraft's carvers
        0        0         0        0          0       10180    1456  20.4  + its blocks at and below it
        0        0         0        0          0           0    1217  20.5  + its blocks above it (control)
```

**The control is exact on both seeds and all five scores.**

### The strongest evidence here is a prediction that was made before the code

D32 counted the flooding a *different way* — by asking, cell by cell, which of
Minecraft's open cells held Dust's water — and from that count named what the
carvers alone would be worth: **187,577 cells on seed 0 and 221,338 on seed 1**.

This record's aquifers, written from the jar and not from that count, leave
**187,614 and 221,438**. Thirty-seven and a hundred cells apart, on scores of
588,215 and 541,571.

The same thing said from the other side: the "of which Dust flooded" counter
this record inherited reads **23 on seed 0 and 23 on seed 1**, down from
400,638 and 320,233. It is the same counter, over the same chunks, and it is
the check that the drying happened where the prediction said it would rather
than somewhere else of the same size.

### The false-cave column went up, and that is a real finding

75,560 becomes 90,892 and 33,148 becomes 42,614. Those are cells below
Minecraft's surface that Dust leaves open and Minecraft filled — and every one
of the 15,332 and 9,466 new ones was **already** a disagreement, hidden.
Dust's terrain said "not rock" where Minecraft's says rock; before this record
Dust filled it with water, which does not count as open. Drying it did not
create the defect, it uncovered one. That is D12's own lesson in a fourth
place: a count that moves is not always a count that means what it says, and
the two directions have to be printed apart.

## What it costs, which for worldgen is not a footnote

The naive answer was **7.3 milliseconds a chunk column** — 87 columns per
second to 57 — and the reason to profile it before optimising is that only 2.0
of those 7.3 milliseconds are the aquifer.

The other 5.3 are a skip that stopped working. D26's whole-cell skip answers a
lattice cell in one step when the eight corners of `final_density` cannot
straddle zero. With an aquifer running, the all-rock half is untouched — the
aquifer's own first line is "positive density is rock" — but the all-*air* half
dies, because what fills an empty cell is a function of the density at each
block and not of its sign: the pressure between two aquifers is *added* to the
density, and a large enough one turns air into rock.

Measured rather than assumed, by running the same sample with the aquifer's
body removed and the skip still lost, twice, and reading the ratio inside each
run rather than between them.

The half is recoverable exactly where **every aquifer the cell could belong to
is the dimension's own global one**. Two aquifers at the same level have a
`calculate_pressure` of zero by that function's own second line, so nothing is
added to the density and the substance is the pre-aquifer rule, block for
block. That is not an approximation and it is the common case: a cell of open
sky is twelve grid lookups, all cached, instead of a hundred and twenty-eight
density evaluations.

With it, **1.7 milliseconds a chunk column and 76 columns per second**, and
**all six scores on both seeds are the same number with it and without it** —
plus a byte-for-byte control in the crate that fills the same chunks with the
skip and without it and requires them equal.

## The served world

`cargo xtask` is not the only reader. A generated column now goes through the
aquifers before it reaches a player, and the neighbours generated only for
their sky floors still do not — a rule that decides between air and water
cannot move a column's top block, which is what a sky floor is.

Seed 0, view distance 6, the same release binary:

```text
  node check.js 25604            22/29 — the same seven D32 left failing, for
                                 the same reason: seed 0's origin column is
                                 water and those seven need dry flat ground
  33x33 columns, y -60..59       580 cells the noise stage left open
    before                       580 water, 0 air
    after                        572 water, 8 air
  of those, below y 0            8 open cells, and all eight are now air
```

Spawn on seed 0 is an ocean, and **under an ocean vanilla's aquifer *is* the
ocean** — the thirteen-offset walk returns the sea's own status for any
aquifer within twelve blocks of a sea floor that is below the sea level. So the
572 that stayed wet are the rule working and not the rule missing, and the
number that says which is the second row: every open cell below y 0, where no
sea floor is in reach, dried.

## What was watched to fail

Four checks, and **two of them were vacuous when first written**. The mutations
are what said so, and both failures have the same shape — a check that passes
for a reason other than the one it is named for:

* *"an aquifer leaves a pocket dry"*, counted over the whole column, stayed
  green when `surface_level` was made to answer the global level
  unconditionally. Below -54 the global status is lava at level -54, so even a
  generator with no aquifers at all leaves air between -54 and the sea. The
  band is now y -36 upward, where the lowest centre a block can belong to is
  -48 and the pre-aquifer answer is unambiguously water.
* *"the deep dark has no aquifers"*, over one world, stayed green when either
  of its two comparisons was flipped — because that fixture's aquifers are dry
  either way. It is now a differential: the same pack built twice with only
  `erosion` and `depth` moved, requiring fluid in one and none in the other.

The other two bite as written: removing the global-lava branch turns the floor
of the world to water, and making `box_is_global` answer `true` for every
rock-free cell breaks the byte-for-byte skip control at the first chunk.

A third lesson, smaller: **narrowing `box_is_global`'s grid window by one row
left the control green.** A mutation that does not bite is not evidence that
the code is right, and it is not evidence that the check is wrong either — it
says the fixture never puts a differing aquifer in that row. Both facts are
written at the check.

## What was declined, each with the number that says it can be

* **The carvers.** 187,614 cells on seed 0 and 221,438 on seed 1, and now that
  is a clean number: the flooding it was mixed with is 23. That is the next
  stage and it is the last one before features.
* **`shouldScheduleFluidUpdate`.** Vanilla sets a flag on the aquifer saying
  whether the fluid it just placed should be given a tick, and the chunk
  generator turns that into a scheduled tick. Dust has no fluid ticks, so the
  flag would have exactly one reader and it would be a comment. It is not
  implemented, and this sentence is where it goes when fluids move.
* **The remaining 23 flooded cells**, on both seeds. Twenty-three of 29,491,200
  is not a stage, and a number that small is worth having written down rather
  than chased.
* **Icebergs**, still: 2,284 and 2,246 columns, and still the largest nameable
  thing the surface stage leaves. Unchanged by this record.

## Consequences

- `cargo xtask harness worldgen` has a tenth rung and it is the last one a
  server could run in. Everything below it reads the region file.
- `Material` is five answers and a code is `4 + i` into the surface palette,
  not `3 + i`. The rules' palette ceiling drops from 253 to 252.
- The surface rules' column walk treats lava as a fluid, which is what
  vanilla's `!getFluidState().isEmpty()` asks and not "is it water".
- `NoiseSettings` carries `aquifers_enabled`, and a settings file that does not
  say is refused rather than defaulted to `false`. A default of `false` there
  would silently drown every cave on a pack this build has not seen.
- A dimension whose settings turn aquifers on and whose router is missing one
  of the six routes is refused at boot with the route's name, rather than
  generating a world with half an aquifer.

## Related

* D32 — the surface rules this runs under, and the count that sent for this.
* D26 — the terrain and the whole-cell skip whose second half this record buys
  back with a proof rather than a guess.
* D12 — the order of the work, and the "caves missing" count now split three
  ways.
* D8, D7, D6 — why the numbers come from the operator's jar, and the `javap`
  route for the ones that are code rather than data.
