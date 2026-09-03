# D21 — Which biome a cell gets, and the 620 cells that are a coin toss

**Status:** Built and measured. Stage one of decision record
[0012](0012-what-worldgen-is-worth-measured-first.md) is done:
**435,459 biome cells wrong is 382, and 449,472 is 238**, over a sample that
reaches seventeen and twenty biomes rather than two. Every one of the 620 that
are left is an **exact tie**, and this record is mostly about why they are not
worth chasing.

## Context

D12 ordered the worldgen work and put the biome source first: the only stage
wholly absent, and every stage below it takes the biome as an input. It also
left a warning attached to its own number. 124,416 of 124,416 cells were wrong
on both seeds, and **Minecraft had two kinds of biome in each 9x9 square** —
so a biome source scored on that square is not being scored, it is being asked
whether one of two answers came out. The sample had to be widened first.

## First, the sample

`--at` is repeatable now, so a scattered sample is one boot and one score. The
sample this record uses is **twelve 5x5 squares** spread out to about sixteen
thousand blocks from the origin: 300 chunks, 76,800 columns, 460,800 biome
cells per seed.

What it reaches, which is the number to check before trusting any score off it:

```text
             biomes Minecraft has in the sample
  one 9x9 square at the origin (D12)        2
  twelve 5x5 squares, seed 0               17
  twelve 5x5 squares, seed 1               20
```

Seventeen and twenty. **The shape of the sample mattered far more than its
size**: the widened sample holds 300 chunks against the single square's 81, not
quite four times as many, and reaches eight to ten times as many biomes. D12
guessed that and it was right.

It is also cheap now, for reasons that turned out to be defects rather than
physics — see "what the sample cost" below.

## What it does

`dust_gen::biome` answers "which biome is this 4x4x4 cell". Minecraft samples
six climate values at the cell — temperature, humidity, continentalness,
erosion, depth, weirdness — and picks the biome whose published region of that
space is nearest. So does this:

* `noise::rng` is xoroshiro128++ with Minecraft's seed upgrade and its MD5
  positional factory. Every noise is seeded from its own **name**, so building
  the five noises a climate needs and not the other fifty-five still gives the
  five Minecraft would have given.
* `noise::perlin` is `ImprovedNoise`, `PerlinNoise` and `NormalNoise`.
* `noise::density` evaluates the density-function language — the small language
  vanilla's worldgen is written in — over an arena of node indices.
* `noise::build` compiles that graph out of whatever data pack the operator has.
* `biome` is the parameter list and the nearest-region search over it.

**Nothing of Mojang's is in the tree**, per D6, D7 and D8. The amplitudes, the
density-function graph and the noise router are read at run time from the
operator's own unpacked jar. The parameter list is 7,593 regions over 53 biomes
and arrives as `dust-biomes.tsv`, which `cargo xtask extract --only worldgen`
writes beside `dust-constants.tsv` for the operator to copy to their
`[data] path`. Every row carries the biome's **name** beside its id, and
`BiomeParameters::rebind` checks the pairing against the running registry: a
version that renumbers a biome is then caught on the row it renumbered, rather
than by a player standing in the wrong forest.

## What it scores

`cargo xtask harness worldgen --version 1.21.1 --seed <n> --radius 2` with the
twelve `--at` squares. Rung 2 was "+ Minecraft's biomes", a copy out of the
region file; it is Dust's own biome source now, so the ladder has one fewer
ceiling and one more candidate. Every figure is a count of things **wrong**.

```text
seed 0 — 300 chunks, 17 biomes in view

  surface  surface     biome    caves      false      blocks  chunk    KiB
    short    block     short  missing      caves       short  cols/s  /col
    76800    76800    435459        0    9598921    10005374   11000   2.2  the flat world Dust serves
    74905    74931    435459   583625     795317    10475058    2894  16.2  + the world's own sea level
    74905    74931       382   583625     795317    10475058     332  16.6  + Dust's biome source
        0    60037         0   681715          0    10405644    2021  19.6  + Minecraft's surface height
        0    60037         0        0          0     9723929    1313  19.6  + Minecraft's carvers
        0        0         0        0          0       12140    1242  20.6  + its blocks at and below it
        0        0         0        0          0           0    1220  20.7  + its blocks above it (control)

seed 1 — 300 chunks, 20 biomes in view

    76800    76800    449472        0    9610683    10028458   10971   2.2  the flat world Dust serves
    72552    74345    449472   527846     746183    10434876    2858  16.2  + the world's own sea level
    72552    74345       238   527846     746183    10434876     240  16.6  + Dust's biome source
        0    47083         0   662830          0    10402908    2194  19.4  + Minecraft's surface height
        0    47083         0        0          0     9740078    1434  19.4  + Minecraft's carvers
        0        0         0        0          0       10180    1538  20.4  + its blocks at and below it
        0        0         0        0          0           0    1267  20.5  + its blocks above it (control)
```

**The control is exact on both seeds and all five scores**, on this sample as on
D12's. That is the row that says the rows above it are about the generator.

Dust now has as many kinds of biome in view as Minecraft does: 17 and 17, 20
and 20.

## The 620 cells that are left are every one of them a tie

The ladder names the pairs now, because "435,459 short" is not something a
reader can act on and "forest where Minecraft has river, 96" names one boundary
to go and look at. Over both seeds the whole remaining gap is six pairs:

```text
seed 0   forest where Minecraft has river                     96
         forest where Minecraft has birch_forest              95
         forest where Minecraft has jungle                    94
         river  where Minecraft has plains                    93
         four more cells, in four more pairs                   4

seed 1   old_growth_birch_forest where Minecraft has
                                        dripstone_caves      136
         plains where Minecraft has sparse_jungle             96
         six more cells, in four more pairs                    6
```

Every pair is two adjacent biomes, and 96 is exactly the number of biome cells
in one column of the world. So these are four columns on seed 0 and a couple on
seed 1, standing on a boundary.

Measured rather than assumed: for every disagreeing cell, the squared distance
from the climate point to Dust's answer and to Minecraft's answer were both
computed. **All 382 and all 238 are exact ties — `delta = 0`, every one, on
both seeds.** Not one cell is a case of Minecraft's biome being nearer. The
climate sampling and the parameter list are right; the two sides break a tie
differently.

Dust's rule is the first row of the table. Vanilla's is not a rule about the
point at all: its search carries the previous answer forward as its starting
bound and only replaces it on a **strictly** nearer leaf, so on a tie it keeps
whatever the last cell asked for got — anywhere on the server, in whatever order
the cells happened to be asked.

### So this is declined, and the reason is priority 1, not effort

Matching those 620 cells means reproducing vanilla's R-tree *and* the order
every cell was ever asked in. That would make Dust's biome at a position depend
on what was generated before it. A biome that is a pure function of its position
is worth more to a player than 620 cells of agreement: it means a chunk comes
out the same however you walked toward it, and it means two servers with the
same seed agree. Vanilla's own answer here is not reproducible from the position;
ours is.

620 cells of 921,600 is 0.067%, all of it a 4x4x4 cell of forest where vanilla
has river, on the line between the two.

## What it costs, which for worldgen is not a footnote

The biome rung runs at **332 chunk columns per second inland and 240 over
ocean**, against 2,894 and 2,858 for the rung above it which writes the same
blocks and no biomes. So a biome is about 2.7 ms of a chunk column, and a join
that streams 289 of them is about 0.9 s of climate — paid once, when the chunk
is generated, and never again.

Getting there was the interesting part, and it is one number split in two.

**The first run cost 43 chunk columns per second and scored 0 of 124,416 wrong.**
Timed in halves over the same cells: **the six climate samples were 18 ms and
the nearest-region search was 1,710 ms.** The search was 99% of it, and every
guess about where the time went — the paletted biome writes, the noise octaves,
the loop order — was wrong. The graph's `flat_cache` nodes were doing their job
from the first run.

So the table carries a skip index: consecutive runs of 64 rows, each with the
smallest box holding all of them. The scan is still a scan in table order,
because the first of two equally near regions wins and the table's order *is*
the answer; a run whose box is already further away than the best answer so far
is skipped whole. That cannot change an answer — every term of a distance is a
square, so a floor that has passed the best cannot be beaten — and it is checked
against the unindexed scan over 500 regions in 8 runs, comparing the region
picked and not just its id.

Measured over D12's own 81-chunk square, on the same chunks and the same score,
before the run test grew its early exit:

```text
  no index                      43 chunk columns per second
  runs of 16                   228
  runs of 32                   297
  runs of 64                   290
  runs of 128                  227
  runs of 256                  182
```

Runs of 64 with the early exit: 316 on that square, and 332 and 240 on the two
widened samples above.

A small run pays its own box test more often than the rows it saves; a wide one
holds a box too loose to skip anything. The run test carries the same early exit
`Region::fitness` has, which is worth more than any run size: without it, 238
full six-axis box distances per cell were themselves the largest remaining cost.

**Costed and not kept:** a two-pass search that bought a bound from the nearest
run before scanning in order. It measured *slower* (258 against 297), which says
the ordered scan already finds its answer early and the extra pass over the runs
costs more than it saves. And vanilla's R-tree, which would be faster still and
is the same code that would fix the 620 ties; declined for the reason above.

**KiB/col is unchanged by biomes and that is the point.** 16.2 before the rung
and 16.6 after: a chunk column's biome container holds 96 cells of a palette
with a handful of entries. The 96 KiB of light D12 measured is still there,
still unconditional, and still the largest single cost of holding a column.

## What the sample cost, which was three defects and not physics

D12 priced a capture at 108 s and said a scattered sample was cheap. It was not:
a twelve-square capture could not finish at all. Three things, none of them new,
all of them reachable only once `--at` could name a square that is not the
origin.

**The RCON client had stopped working in release builds.** Vanilla's RCON reads
a whole packet with one `read` and refuses what it gets if the length field does
not account for every byte of it. `exec_delimited` wrote the command and its
delimiter back to back, so whether they arrived as one segment or two was a race
with the kernel — and a release build wins that race, which is to say loses.
Two commands in one read are not two commands to vanilla; they are a malformed
packet, and it closes the socket without a word on either side. Every capture on
this machine was failing with "the server closed the connection" and retrying
until its budget ran out; the same commit in a debug build worked. **This is the
`just bot` trap with the opposite sign**, and it is the same lesson: a build
profile is an input.

**The poll was reading the wrong file.** `region_file_path` takes chunk
coordinates and shifts them itself; `pending_chunks` handed it region
coordinates, so it shifted twice. Every chunk within four of 0,0 is in
`r.0.0.mca` either way — which is why this was right for as long as there was
one square and wrong for every square added beside it. Chunk 300,300 lives in
`r.9.9.mca`; the poll asked `r.0.0.mca` about it, read the right slot of the
wrong header, and answered about a chunk a thousand chunks away.

**The poll was waiting for an autosave.** A forceloaded chunk generates in
seconds and then stays in memory. Vanilla writes it every 6,000 ticks, and a
server generating three hundred chunks does not tick 6,000 times quickly; three
squares sat nine minutes each at 3% CPU with every chunk already generated. The
poll's criterion is "on disk", so the poll is what should be asking. It sends
`save-all` every ten seconds now.

Together: **a twelve-square capture went from never finishing to 9.2 s on seed 0
and 30.0 s on seed 1.**

The transferable part is the same sentence three times. **A stand-in only
exposes the defects its own range reaches.** The RCON fake framed its reads with
a length prefix and `read_exact`, so two packets arriving together were simply
two packets to it and it could never have caught the defect it existed to guard.
The poll's test named only chunks in region 0,0, which is exactly the range in
which its bug is invisible. Both have a second test now that reads the way the
real thing reads and asks about somewhere that is not the origin.

## Consequences

- Rung 2 of `harness worldgen` is Dust's own biome source. It no longer reads
  the region file, so it is a mode a server could run in, and the gap between it
  and rung 3 is what the biome source still gets wrong.
- **Nothing is wired into the running server yet, deliberately.** `FlatWorld`
  hands out one cloned column at about ten thousand a second, and D12 already
  said the template goes the moment two columns differ. Making biomes vary now
  would pay that whole cost for coloured grass and mob spawns on a world whose
  terrain is still four rows of dirt. The biome source lands with the terrain it
  is an input to, which is stage two.
- `cargo xtask extract --only worldgen` writes `dust-biomes.tsv`. A biome in the
  report that the registry has no id for is a hard error, not a skipped row: a
  row that can never be chosen is a hole in the world shaped like an ocean, and
  it would look like terrain rather than like a fault.
- **The climate quantisation is `f32` and 1.21.1's own report is what says so.**
  Of the 37 distinct parameter values in `overworld.json`, two — 0.7666 and
  -0.7666 — quantise to 7666 in `f32` and 7665 in `f64`, and both are real
  bounds on the temperature and weirdness axes. Splines are `f32` for the same
  reason. Widening either would be more accurate and would be wrong.
- The ladder prints the biome pairs it gets wrong, not just the count.

## What is next, in D12's order

Stage two, the density functions: 74,905 columns inland and 72,552 over ocean,
and the first thing a player sees. The vocabulary it needs is already here —
`noise::density` evaluates the language and `noise::build` compiles it — so what
is left is the rest of the router, the interpolation lattice and the block
placement above and below the surface. Score it on **surface height** and not on
cells, as D12 said: the rung that hands the shape over makes the block count
worse.

## Related

* D12 — the ladder, the order of the work, and the warning about two biomes in a
  9x9 that this record's sample exists to answer.
* D10 — the ladder's shape, and the control that has to be exact.
* D8, D7, D6 — why Minecraft's own values arrive at run time from the operator's
  jar, which is the route the parameter list and the density functions take.
