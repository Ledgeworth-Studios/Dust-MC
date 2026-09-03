# D32 — What the ground is made of

**Status:** Built, measured, wired to the socket and checked with a
third-party client. Stage three of decision record
[0012](0012-what-worldgen-is-worth-measured-first.md) is done: **49,128
columns with the wrong block underfoot becomes 32,398, and 54,815 becomes
23,025** — and nearly all of what is left is a tree. A player joining Dust
lands on grass over dirt, not on stone.

## Context

D12 ordered the worldgen work by what each stage is worth to a player and put
surface rules third: *13,497 columns inland, all 20,736 over ocean, where the
entire answer is one block.* D26 built the terrain under them and declined
them in one sentence: "inventing a rule for the top block — grass above the
sea, sand beside it — would be right most of the time and wrong in a way no
test here could name, and it would poison the measurement meant to replace
it."

That is exactly right, and it is also why the rules are not invented here.
Vanilla's surface rules are **data**. `noise_settings/overworld.json` carries
a `surface_rule` of thirty-two kilobytes — 137 conditions over 100 result
blocks — and it arrives at run time from the operator's own unpacked data
pack, the same road the biome parameter list took in D21 and the density
functions in D26. D6, D7 and D8 hold.

## What it does

`dust_gen::surface` compiles that tree and runs vanilla's own column walk over
the materials the noise stage wrote. Fifteen node types, all of them in the
overworld pack: `sequence`, `condition`, `block`, `bandlands`; `biome`,
`noise_threshold`, `vertical_gradient`, `y_above`, `water`, `stone_depth`,
`hole`, `above_preliminary_surface`, `steep`, `temperature`, `not`.

**The walk is the rule, not an implementation of it.** A surface rule is not a
function of a position. It is a function of a position *and how deep into the
rock that position is*, so each column is walked from its top down carrying
three running numbers — how many solid blocks have passed since the last air,
how many are left before the rock ends, and where the last fluid surface was.
Every `stone_depth` and `water` condition reads one of those. Sampling the
tree at a point would answer a different question and would look almost right.

Three things this needed that were not in the tree:

* **A positional stream at a block.** `getSurfaceDepth` rolls a die per
  column (`noise * 2.75 + 3.0 + nextDouble() * 0.25`) and every
  `vertical_gradient` rolls one per block. Both go through `Mth.getSeed`.
* **Java's 24-bit `nextFloat`.** `vertical_gradient` compares it against a
  chance computed in `f64`. A 53-bit draw would agree with Minecraft about the
  bedrock roof almost everywhere and disagree at the edges.
* **SHA-256**, because the biome is not read off the quart grid.

### The biome a rule sees is blurred, and that is what makes a coast wobble

`BiomeManager.getBiome` offsets a block position by two, then picks whichever
of the eight surrounding quart-cell corners wins a hash-fiddled distance,
fiddled from a **zoom seed** that is the SHA-256 of the world seed. Asking the
grid directly would put beach sand in a straight line down a coast.

Measured rather than assumed, by running the same sample both ways: the blur
is worth **63 columns on seed 0 and 224 on seed 1** of 76,800, and 357 and
1,466 cells. Small, and kept, because it costs nothing measurable — the rules
ask for a biome only above the preliminary surface, which is a shell and not a
column — and because it is the rule. A number that small is exactly the kind
nobody would have noticed was missing.

### A material grew a fourth answer

`Material` was three codes: air, the dimension's block, the dimension's fluid.
It is now those three plus `Surface(index)` into the rules' **own palette**,
which the caller resolves against its registry once at boot. The alternative
was for `dust-gen` to hand back block names and for the server to look one up
about ninety thousand times per column, or for the generator to know a
registry, which it does not and should not.

## What it scores

`cargo xtask harness worldgen --version 1.21.1 --seed <n> --radius 2` over
D21's twelve scattered 5x5 squares: 300 chunks, 76,800 columns, 29,491,200
cells. Every figure is a count of things **wrong**.

```text
seed 0 — 17 biomes in view

  surface  surface     biome    caves      false      blocks  chunk    KiB
    short    block     short  missing      caves       short  cols/s  /col
    76800    76800    435459        0    9598921    10005374   10228   2.2  the flat world Dust served
    74905    74931    435459   583625     795317    10475058    2582  16.2  + the world's own sea level
    74905    74931       382   583625     795317    10475058     317  16.6  + Dust's biome source
    28796    49128       382   588215      75560     6840919     181  18.8  + Dust's terrain
    28796    32398       382   588215      75560     2529567      93  18.8  + Dust's surface rules     <- this record
        0    60037         0   681715          0    10405644    1987  19.6  + Minecraft's surface height
        0    60037         0        0          0     9723929    1394  19.6  + Minecraft's carvers
        0        0         0        0          0       12140    1431  20.6  + its blocks at and below it
        0        0         0        0          0           0    1233  20.7  + its blocks above it (control)

seed 1 — 20 biomes in view

    76800    76800    449472        0    9610683    10028458   10425   2.2  the flat world Dust served
    72552    74345    449472   527846     746183    10434876    2660  16.2  + the world's own sea level
    72552    74345       238   527846     746183    10434876     220  16.6  + Dust's biome source
    16678    54815       238   541571      33148     6807705     148  19.0  + Dust's terrain
    16678    23025       238   541571      33148     2376862      84  19.0  + Dust's surface rules     <- this record
        0    47083         0   662830          0    10402908    2047  19.4  + Minecraft's surface height
        0    47083         0        0          0     9740078    1414  19.4  + Minecraft's carvers
        0        0         0        0          0       10180    1456  20.4  + its blocks at and below it
        0        0         0        0          0           0    1212  20.5  + its blocks above it (control)
```

**The control is exact on both seeds and all five scores.** And the surface
*height* column does not move at all, on either seed — which is the check that
says the rules replaced blocks rather than moving them. A stage that improved
the block underfoot by raising the ground would have shown up there.

The block score falls by **4,311,352 cells inland and 4,430,843 over ocean**,
which is deepslate, dirt, sand, gravel and snow arriving at once. It is the
largest single change any rung of this ladder has made.

### What is left in the column score is a tree

```text
seed 0, of 32,398 wrong        seed 1, of 23,025 wrong
  oak_leaves      12599          oak_leaves       6335
  spruce_leaves    6572          birch_leaves     4919
  birch_leaves     3478          grass_block      4495
  grass_block      3343          spruce_leaves    3333
  jungle_leaves    3341          ice              2246
  packed_ice       2284          acacia_leaves     852
  ... 17 more       781          ... 18 more       845
```

Leaves are **25,990 of seed 0's 32,398 and 15,689 of seed 1's 23,025**. The
surface block is asked at *Minecraft's* surface y, and over a forest that is a
tree; D12 names features last and says explicitly that a cell count
understates them. The `grass_block` rows are the columns whose ground Dust put
at the wrong height in the first place, which is D26's remaining gap and not
this one's.

**`packed_ice` and `ice` are icebergs**, and they are the one nameable surface
gap this record leaves: see below.

## What it costs, which for worldgen is not a footnote

**3.9 milliseconds a chunk column**, against the density stage's 5.3 and the
climate's 2.4. Split by taking the walk without the rules: the column walk and
its noises are 1.7 ms and the rule evaluations 2.2. A chunk holds about
98,000 solid blocks to visit and each visit is about 39 ns.

Two thirds of that first arrived as an **eighteen-fold** slowdown — 181 chunk
columns per second to 10 — and the cause is the same shape as the one D21 paid
1,710 ms for. A biome is a climate search, and the walk visits every solid
block of every column; looking one up per block is 76,800 searches a chunk
where the biome grid needs 96. Vanilla memoises the lookup per block and so
does this, and the fix is checked the way D26 checked its cell skip: **all five
scores on both seeds are the same number with the memo and without it**,
because the rules ask for a biome only above the preliminary surface. Reading
whole rows to find where a chunk's sky stops, rather than 256 columns of sky,
is the other third.

## What was declined, each with the number that says it can be

* **The eroded-badlands and frozen-ocean extensions.** These are not surface
  rules; they run beside them in `SurfaceSystem` and are what makes a badlands
  pillar and an **iceberg**. The frozen-ocean one is worth **2,284 columns on
  seed 0 and 2,246 on seed 1** — the `packed_ice` and `ice` rows above — and
  is the largest thing this record leaves on the table. Declined for time, not
  for doubt, and named here so the next agent does not have to find it.
* **`minecraft:temperature`.** It asks the biome whether it is cold enough to
  snow at that block, which is a legacy simplex noise and two temperature
  modifiers. It answers `false` without looking, and the harness **counts
  every time it was asked: zero, on both seeds, over 76,800 columns.** It is
  reachable only under `biome in {frozen_ocean, deep_frozen_ocean}` and
  `hole`. A gap nobody's world reaches is not a gap, and the count is what
  says which it is rather than a comment claiming it.
* **The clay bands are built and reached zero times.** `minecraft:bandlands`
  is the one part of the rules whose table vanilla does *not* put in the pack —
  seven terracotta colours named in Java — so it is the one place a mistake
  could not be blamed on the data. It is implemented from the draw order
  (five `nextInt` per orange run, three `makeBands` passes in a fixed order,
  then a white run whose stride is another draw) and the harness reports that
  **a badlands band decided 0 blocks on both seeds**. It is built and unproven,
  and the count says so.
* **Aquifers**, and this record found the number that reorders them.

### Most of what reads as a missing carver is a missing aquifer

D12 ordered carvers fourth and aquifers with the surface rules, on a "caves
missing" count that could not tell them apart. It can now:

```text
                 cells Minecraft left open below its own surface
                 that Dust did not          ... of which Dust flooded
  seed 0                    588,215                         400,638
  seed 1                    541,571                         320,233
```

**Two thirds and three fifths.** Vanilla runs its noise caves through an
aquifer that leaves most of them dry; a generator without one fills every
pocket below the sea level with water. So the aquifer is the larger half of
what looks like the carver's job, and it is the half a player *drowns* in. The
remaining 187,577 and 221,338 cells are carvers proper.

That is D12's own lesson in a third place: **a single count cannot say which
stage owns it.** The harness now prints both.

## The served world

A generated column now goes through the rules before it reaches a player. The
neighbours generated for their sky floors do not, and for a reason rather than
to save the time: a rule replaces the block at a y, it does not move it, so a
column's sky floor is the same either way. The exception is the handful of
rules that write air into a hole in a frozen ocean floor.

Checked against the same binary, seed 0, view distance 6:

```text
  node check.js 25604            22/29 — the same seven D26 left failing, for
                                 the same reason: seed 0's origin column is
                                 water and those seven need dry flat ground
  1,089 columns around spawn     682 grass_block, 407 water
  one of them, downward          grass_block / dirt / dirt / dirt / dirt / dirt
```

Before this record every one of those 682 was stone.

## Consequences

- `cargo xtask harness worldgen` has a ninth rung and it is the last one a
  server could run in. Everything below it reads the region file.
- `Material` is four answers, and a caller resolves the rules' palette once.
  A pack that names a block this build's registry does not have is refused at
  boot with the name, not defaulted.
- A biome name a `biome_is` asks about that the registry does not have is
  **reported and left unbound**, so it matches nothing. A name that matched
  everything would put a beach across a continent.
- The boot line says how many result blocks the dimension's rules can write.
  Zero means the settings carried none and the ground is bare stone, which is
  still a correct answer for a pack that has no rules.
- Five checks were watched to fail before they were believed, and one of them
  taught its own lesson: **the first attempt to break it was a no-op.** The
  "a rule that always answers still leaves air and the fluid alone" check is
  defended by two guards, and breaking either alone leaves it green. A check
  that cannot be made red by the edit you *thought* would do it has not been
  watched to fail; it has been watched to pass twice.

## Related

* D26 — the terrain these rules are painted over, the seam, and the sentence
  that declined them for a stated reason that has now expired.
* D21 — the biome source they read, and the finding that a climate search is
  expensive enough to change the shape of the code around it.
* D12 — the order of the work, the five scores, and the "caves missing" count
  this record splits in two.
* D8, D7, D6 — why every number in the rules arrives at run time from the
  operator's own copy of Minecraft.
