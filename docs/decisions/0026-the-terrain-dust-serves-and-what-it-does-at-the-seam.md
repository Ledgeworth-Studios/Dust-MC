# D26 — The terrain Dust serves, and what it does at the edge of a world

**Status:** Built, measured and wired to the socket. Stage two of decision
record [0012](0012-what-worldgen-is-worth-measured-first.md) is done:
**74,905 columns at the wrong height becomes 28,796, and 72,552 becomes
16,678** — and on both seeds nearly every column that is left has a tree on
it. Dust no longer serves a superflat.

## Context

D12 ordered the worldgen work by what each stage was worth to a player and put
the density functions second: 20,736 of 20,736 columns wrong on both seeds, and
the first thing anybody sees. It also attached two warnings to its own number.
**Score the shape on surface height and not on cells**, because the rung that
hands the shape over makes the block count 4,899 *worse* on seed 0. And **the
flat template goes the moment two columns differ** — 10,000 columns per second
of cloning one shared chunk, gone, and with it the only reason `FlatWorld`
existed.

D21 built the biome source and deliberately did not wire it in, for a stated
reason: paying eight times a column's cost for coloured grass on a world that
is still four rows of dirt is the wrong order. That reason expires here.

## What it does

`dust_gen::terrain` is vanilla's **noise stage**, which is one sign change.
`final_density` is positive where the world is the dimension's `default_block`
and not positive where it is air or the `default_fluid`, and every mountain,
overhang, sea floor and noise cave in the overworld is that one function.

Getting there needed nine more density-function types than a climate does. The
climate half of the noise router uses fourteen; `final_density` uses
twenty-six. `square`, `cube`, `half_negative`, `quarter_negative`, `squeeze`,
`clamp`, `range_choice`, `weird_scaled_sampler` and `old_blended_noise` are the
nine. Two more were not missing so much as **wrong**, and both are answers
rather than speeds:

* **`flat_cache` is not `cache_2d`.** Vanilla's `FlatCache` fills a table over
  the chunk's *quart* grid at y = 0, so the value at x = 5 is the value
  computed at x = 4. Both were compiled to one column memo, which is a smoother
  world than Minecraft's. It made no difference to D21's biome score, and that
  is the finding: **a biome is sampled at quart positions already**, so the
  only model that had ever exercised the node could not tell the two apart.
* **`interpolated` was a passthrough.** Minecraft evaluates the function it
  wraps at the corners of a cell four blocks wide and eight tall — both read
  from the dimension's own settings — and lerps trilinearly inside. Evaluating
  the noise at every block instead is more samples of the same noise and a
  *different world*: smoother, without the flat shelves and straight cliff
  faces a player recognises. The lattice is the terrain, not an approximation
  of it.

`old_blended_noise` is the one node that cannot be spelled in the language:
three legacy Perlin stacks, forty octaves, drawn as three consecutive stretches
of one stream hashed from `minecraft:terrain`, with a main stack that picks per
point which of the other two answers. The draw order is the whole contract,
which is why `BlendedNoise::new` takes the stream and not a seed.

**Nothing of Mojang's is in the tree**, per D6, D7 and D8. The graph, the
amplitudes, the splines, the cell size, the sea level and the two block names
all arrive at run time from the operator's own unpacked data pack; the biome
parameter list arrives as `dust-biomes.tsv` from their own server jar.

## What it scores

`cargo xtask harness worldgen --version 1.21.1 --seed <n> --radius 2` over
D21's twelve scattered 5x5 squares: 300 chunks, 76,800 columns, 29,491,200
cells. Every figure is a count of things **wrong**.

```text
seed 0 — 17 biomes in view

  surface  surface     biome    caves      false      blocks  chunk    KiB
    short    block     short  missing      caves       short  cols/s  /col
    76800    76800    435459        0    9598921    10005374   10438   2.2  the flat world Dust served
    74905    74931    435459   583625     795317    10475058    2620  16.2  + the world's own sea level
    74905    74931       382   583625     795317    10475058     311  16.6  + Dust's biome source
    28796    49128       382   588215      75560     6840919     176  18.8  + Dust's terrain          <- this record
        0    60037         0   681715          0    10405644    1954  19.6  + Minecraft's surface height
        0    60037         0        0          0     9723929    1390  19.6  + Minecraft's carvers
        0        0         0        0          0       12140    1366  20.6  + its blocks at and below it
        0        0         0        0          0           0    1188  20.7  + its blocks above it (control)

seed 1 — 20 biomes in view

    76800    76800    449472        0    9610683    10028458   10432   2.2  the flat world Dust served
    72552    74345    449472   527846     746183    10434876    2641  16.2  + the world's own sea level
    72552    74345       238   527846     746183    10434876     225  16.6  + Dust's biome source
    16678    54815       238   541571      33148     6807705     146  19.0  + Dust's terrain          <- this record
        0    47083         0   662830          0    10402908    2015  19.4  + Minecraft's surface height
        0    47083         0        0          0     9740078    1410  19.4  + Minecraft's carvers
        0        0         0        0          0       10180    1418  20.4  + its blocks at and below it
        0        0         0        0          0           0    1213  20.5  + its blocks above it (control)
```

**The control is exact on both seeds and all five scores**, on this sample as
on D21's and D12's.

D12's warning held and was worth having. Block agreement improved by 3.63M
cells inland and 3.63M over ocean, and **false caves fell from 795,317 to
75,560** — but the surface *block* score is 49,128 against the 60,037 of the
rung above it, which hands over Minecraft's own heights and then puts grass on
them. The two rows are not comparable and the ladder does not pretend they are:
this one has water where there is water and stone where there is stone, and
that one has grass at the right y and dirt under it everywhere.

### Nearly every column that is left has a tree on it

`MOTION_BLOCKING` counts leaves, so a column whose ground is exactly right
reads five blocks short with an oak on it. The ladder now names Minecraft's own
surface block in the columns whose **height** disagrees — a different list from
the one it already printed for the block underfoot, and the difference is which
stage owns the gap.

```text
seed 0, of 28,796 short          seed 1, of 16,678 short
  oak_leaves      12547            oak_leaves       6333
  spruce_leaves    6572            birch_leaves     4919
  birch_leaves     3478            spruce_leaves    3330
  jungle_leaves    3341            acacia_leaves     852
  packed_ice       2279            grass_block       578
  stone             263            jungle_leaves     250
  ... 11 more kinds  316           ... 14 more kinds  416
```

**Every delta is negative** — Dust is never too high, on either seed. Leaves
and ice are 28,217 of seed 0's 28,796 and 15,684 of seed 1's 16,678, which
leaves at most 579 and 994 columns of 76,800 that are a disagreement about the
rock itself. Trees are a feature and D12 names them last; the ice is a surface
rule and D12 names it next.

Without that list, a terrain generator with no trees in it would be blamed for
the forest. This is D21's finding in a second place: **a count nobody can act
on is worth less than a name.**

## What it costs, which for worldgen is not a footnote

Split three ways over one chunk column, measured rather than guessed at, which
is the lesson D21 paid 1,710 milliseconds for:

```text
  the six climate samples and the biome search   2.4 ms
  final_density over the lattice                 5.3 ms
  writing the blocks into the paletted container 0.38 ms
```

So the density was two thirds of the rung, and the guesses about which third it
would be were again worth nothing.

### A skip that provably cannot change an answer takes 5.3 ms to 2.5

A trilinear interpolation never leaves the interval its eight corners span. So
an **interval walk** over the graph — stopping at every `interpolated` node,
where the eight corners have already been sampled, and answering "unknown" for
anything whose range is not known — bounds `final_density` over a whole 4x8x4
cell. A cell whose bound cannot reach zero holds no rock anywhere, and its 128
blocks are decided by their y alone. Above a mountain that is most of the
column.

Nothing about that walk is specific to vanilla's graph: a spline and the old
blended noise both answer with an infinite interval, and an infinite interval
never satisfies the test. Both sit under an `interpolated` in every dimension
vanilla ships, so the cost of admitting ignorance about them is nothing.

Checked against `fill_without_skipping`, which is public because a control that
lives only inside the thing it checks is not a control — and confirmed on the
real world rather than only on a scratch pack: all five scores are the same
number on both seeds with the skip and without it.

The rung runs at **176 and 146 chunk columns per second** against 311 and 225
for the biome rung above it. A column holds 18.8 KiB of terrain and biome
against the flat world's 2.2, which is D12's 8.9x arriving on schedule.

## Wiring it in, and what a served world does at the seam

A world file is a disc in an infinite plane and a player can walk off the edge
of it. `FlatWorld` was the answer everywhere: for a server with no world file
it *was* the world, and for a server with one it was what a missing column
became. Both change.

A server with no `[server] world_source` generates every column from
`[worldgen] seed`. A server with one generates the columns off the edge — and
**which fallback an operator gets is not a setting**: it is whether the world's
own seed could be read out of the `level.dat` beside its region directory.

* **With the seed**, the far side of the edge is the terrain that world would
  have had, so the seam is a discontinuity in the *materials* — stone where
  vanilla has grass, no trees — and not in the shape. A player walking off the
  edge keeps walking on the same hill.
* **Without it**, the plain runs on as before. Generating from a seed that is
  not the world's own would put a cliff exactly where the disc ends, and a
  wrong answer that looks right is worse than an obviously artificial one.
  `[worldgen] seed` is deliberately *not* consulted for a saved world for the
  same reason.

Reading that seed is deliberately softer than reading the spawn point:
`spawn_beside` refuses to start on a `level.dat` it cannot read, and
`seed_beside` returns `None` on the same file. A missing spawn puts every
player in the wrong place in a world that is otherwise right; a missing seed
costs a plain at the edge, which is what Dust served everywhere until now.

### The light needs the four columns around it

A flat world could be lit with its own floors on all four sides because every
column of it is the same column. A real one cannot, and a cliff at x 16 lit as
though the next chunk were the same shape is a seam a player sees. So a
column's neighbours are generated for their sky floors — terrain only, no
biomes and no light — and remembered, exactly as `AnvilWorld` remembers the
floors it reads. Each position's floor is then computed once: a view of 289
columns costs 72 more around its edge, not 289 times four.

### What a player gets, measured against the same binary serving the plain

```text
  a join at view distance 6, 169 columns   flat   398 ms   generated 1,627 ms
  node check.js 25604                      flat  29/29     generated  22/29
  node soak.js 25604 2                     949 columns over 30 legs of 40 blocks,
                                           no stall, every keep-alive answered
```

**The seven are the checks and not the server**, and the project had already
written down why: *a control that only holds on a superflat is not a control.*
Every one of the seven is a placement check that needs dry flat ground under
the bot, and seed 0's origin column is water — "the block this check needs is
where it was put — water". They are left failing rather than moved to dry land,
because moving a check until the answer comes out right is not measuring
anything. What they need is ground of their own, and that is a change to
`tools/bot`.

## What was declined

* **Surface rules.** The ground is the default block, so a generated world is
  stone. Inventing a rule for the top block — grass above the sea, sand beside
  it — would be right most of the time and wrong in a way no test here could
  name, and it would poison the measurement meant to replace it. D12 puts them
  third and 49,128 columns is what they are worth.
* **Aquifers.** Every pocket below the sea level holds water, which is exactly
  what vanilla does with `aquifers_enabled: false` and not what the overworld
  does. So a noise cave under a hill floods where vanilla leaves it dry. Named
  here rather than fixed because it is the same stage as the surface rules and
  it needs the four aquifer entries of the noise router, which this record
  compiles and does not read.
* **Bedrock's five-row band.** Vanilla writes its floor with a die, which is a
  surface rule. One row of bedrock at the world's floor is not that rule; it is
  the floor, and without it a player digs into the void. Every rung of the
  ladder including the control writes it, because at y -64 vanilla's own
  gradient is true without a die being rolled.
* **Reproducing vanilla's cell iteration order.** The fill walks x and z inside
  a cell the way `NoiseChunk` does, and the answer is a pure function of the
  position either way, so this is a statement about cache locality and not
  about the world.
* **A chunk cache.** Every column a player walks toward is generated on demand
  and thrown away, which is what Dust did with a template clone and is now
  9.6 ms rather than 0.1. That is chunk residency and it is somebody else's
  record.

## Consequences

- `FlatWorld` is no longer what a player stands on wherever the operator has
  extracted their own data. It stays as the source of the block palette and the
  world height, and as the honest answer at a seam whose seed is unknown.
- **`[worldgen] seed` is new**, defaults to zero rather than to a random
  number, and is a restart setting. Two servers started from the same
  configuration should serve the same world.
- The ladder has an eighth rung, and it is the last one a server could run in.
  Everything below it reads the region file and says so in its own name.
- A generated column is a pure function of its position. It comes out the same
  however a player walked toward it, and two servers with the same seed and the
  same data pack agree — which is the same property D21 declined 620 biome
  cells to keep.

## Related

* D21 — the biome source this stands on, the skip index this one is shaped
  after, and the rule that a wrong pair is worth naming rather than counting.
* D12 — the order of the work, the five scores, and the warning about scoring
  a shape on cells.
* D10 — the ladder both are built in the shape of, and the sky-light volume
  whose 3x3 is why a generated column asks its four neighbours where their sky
  reaches.
* D8, D7, D6 — why every number in the graph arrives at run time from the
  operator's own copy of Minecraft.
