# D10 — How wide the sky-light volume should be

**Status:** Measured and **not adopted**. A 3x3 volume closes 71% of what is
left of Dust's sky-light gap and costs roughly six times the work; the cheaper
remaining input closes the other 29% and costs nothing. This record exists so
that the next person to look at the seam finds the numbers rather than the
intuition.

## Context

Dust lights one chunk column at a time. Its four neighbours enter the walk as a
**skirt** — their sky floors used as light sources along the four faces — which
is exact where a neighbour is open to the sky and under-lights where the light
would have to travel *through* one: around the mouth of a cave three blocks
into the next chunk. `dust_world::column_light` has said so in its own module
note since the skirt was written, and `propagation::LightGraph` was given
`contains` precisely so the wider version would be a bigger volume rather than
a rewrite.

For most of the project's life this was one of two known gaps and the smaller
of them. Decision record 0008 costed it at "about five per cent of the gap" and
then, when opacity arrived and the gap collapsed, at "very nearly all of what
is left of it". Both were readings of a ring histogram, and neither was a
measurement of the volume itself.

## What it buys, measured

`cargo xtask harness light` now measures a **ladder**: four models over the
same chunks in the same run, each one the row above it plus a single named
change.

```text
seed 0, radius 2                              short   sweep
  air only, one column, Dust's heightmap     14,276    102 ms
  + Minecraft's own opacity                     611    101 ms
  + a 3x3 volume of columns                     179    611 ms
  + the heightmaps Minecraft wrote                0    544 ms
```

```text
seed 1, radius 3                              short
  air only, one column, Dust's heightmap    169,480
  + Minecraft's own opacity                       0
  + a 3x3 volume of columns                       0
  + the heightmaps Minecraft wrote                0
```

**Sky light has four inputs and only one of them is the engine.** The last row
is a hundred per cent on both seeds: 2,457,600 cells and 4,816,896 cells, and
not one of them disagrees with the light Minecraft computed. Whatever is short
of that is something Dust is *told* about the world, and each row names one.

### A 5x5 buys exactly nothing

`--volume 2` measures it, and the answer is 179 short — the same 179, not
fewer.

That is the argument for a finite volume, confirmed rather than assumed. A
level of light travels one block per step, so light entering the volume's outer
boundary has lost all fifteen before it has crossed the sixteen blocks of a
single chunk. One ring of neighbours is not an approximation of the infinite
volume; **it is the infinite volume**, for a light that attenuates this fast.

Worth stating because it also bounds the cost. Whatever a wider volume costs,
it costs it once and never grows.

### And the 179 that are left are not the volume

They are all short by **exactly one**, and they stand in short grass (103),
air (62), poppies, dandelions and a few oak leaves. The ring histogram is flat
and even rises slightly towards the middle, which is not a shape a neighbour
effect makes.

The cause is the fourth input. Dust recomputes a chunk's heightmaps rather than
trusting the file — it has to, because a server that has edited a block has a
heightmap the file does not — and the predicate it recomputes `MOTION_BLOCKING`
with is **"anything that is not air"**. Vanilla's is "blocks motion, or holds a
fluid". Short grass and flowers are the whole difference: vanilla lets daylight
fall through them at fifteen, Dust puts its sky floor above them, and the cell
they stand in comes out at fourteen. `crates/dust-server/src/net/source.rs`
has called this "a known approximation" since it was written; this is what the
approximation costs.

## The decision

**The 3x3 volume is not adopted, and the heightmap predicate is the next thing
to build.** Both close part of the same 611 cells; they differ by two orders of
magnitude in what they cost.

* **The volume** closes 432 of 611 — 0.018% of all cells on seed 0, nothing at
  all on seed 1 — and costs roughly six times the lighting work. A server would
  read nine columns to serve one, or hold a cache of lit columns that is nine
  times the one it holds now. The harness's own timings are not a server's, but
  the ratio is the ratio.
* **The heightmap predicate** closes the other 179 and costs a column in a
  table Dust already reads. `blocksMotion()` and the fluid test are code
  constants in Minecraft, in no report and no data pack — which is D8's problem
  exactly, and D8's route is already built and already carrying opacity to the
  same place.

So the cheaper of the two is also the one that takes seed 0 to exact. Doing the
volume first would spend the larger cost to reach 99.993% and then still need
the smaller one.

**This is not a rejection.** The volume is real, it is worth 432 cells, and the
day Dust holds lit columns for other reasons — a tick loop that keeps a
player's view resident, which is coming — the marginal cost of lighting them
together falls a long way. What this record refuses is spending it *now*,
before the free thing beside it.

### What was written, and what was not

`xtask/src/harness/area.rs` is the wider volume, and it lives in the harness.
It is a real `LightGraph` over a `(2k+1)²` block of columns, it produces the
numbers above, and it is not on any path a server takes.

That placement is the point. D8 sat open for two months because the right
answer had never been priced, and the lesson it left was to take the number
before building the thing. A production multi-column volume is a change to how
`AnvilWorld` reads and caches; a harness one is a hundred and eighty lines that
answer whether that change is worth making. The answer was "not yet, and here
is what to do instead", which no amount of reasoning about ring histograms had
produced in either direction.

## Consequences

- **Dust's sky light stays one column wide**, with the skirt, and under-lights
  where light would have to travel through a neighbour. 0.018% of cells on an
  inland world; nothing measurable on an ocean one.
- **`harness light`'s ladder is the record of it.** Adding a rung is how the
  next input gets measured, and the last rung is the engine with every input
  handed to it — a hundred per cent, which is the number that says the walks
  themselves are right.
- **`area.rs` is dead code by design** and says so in its own module note. If
  it stops producing numbers nobody reads them, and the ladder is what keeps it
  alive.

## Related

* D8 — the block constants Minecraft keeps in code, whose route this record's
  next step reuses, and whose history is why this one measured first.
