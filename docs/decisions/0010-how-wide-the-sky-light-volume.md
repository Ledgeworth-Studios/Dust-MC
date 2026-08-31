# D10 — How wide the sky-light volume should be

**Status:** Measured and **not adopted**, and re-measured on 2026-08-31 when
block light landed — it is the whole of that gap too, and the larger share of
it. A 3x3 volume is now the *whole* of what is left of Dust's lighting gap — 435 cells of 2.4 million on seed 0,
nothing at all on seed 1 — and costs roughly six times the work. The cheaper
input this record sent people to build instead has been built, and the numbers
below are from after it. This record exists so that the next person to look at
the seam finds them rather than the intuition.

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
seed 0, radius 2                                       short   sweep
  air alone, one column, `not air` heightmaps        14,276    304 ms
  + Minecraft's own opacity                             611    267 ms
  + Minecraft's own heightmap predicates                435    252 ms   <- a server
  + a 3x3 volume of columns                               0  1,482 ms
  + the heightmaps Minecraft wrote                        0  1,328 ms
```

On seed 1 the second row is already zero, over 4,816,896 cells.

**Sky light has four inputs and only one of them is the engine.** The fourth row
is a hundred per cent on both seeds — 2,457,600 cells and 4,816,896 cells, and
not one of them disagrees with the light Minecraft computed. Whatever is short
of that is something Dust is *told* about the world, and each row names one.

The fifth row is a control: it skips the recompute entirely and takes the
heightmaps out of the chunk as Minecraft wrote them. Not a mode a server can run
in, since an edited chunk has a heightmap its file does not. **Its agreeing with
the row above is the statement that Dust's recompute, given Minecraft's
predicates, *is* Minecraft's heightmap** rather than merely close to it.

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

### The 179 that survived the volume were not the volume — and are gone

Before the heightmap predicates existed, a 3x3 volume left 179 cells short. They
were all short by **exactly one**, standing in short grass (103), air (62),
poppies, dandelions and a few oak leaves, and the ring histogram was flat and
even rose slightly towards the middle — not a shape a neighbour effect makes.

The cause was the fourth input. Dust recomputes a chunk's heightmaps rather than
trusting the file — it has to, because a server that has edited a block has a
heightmap the file does not — and the predicate it recomputed *all six* with was
**"anything that is not `minecraft:air`"**. Vanilla has six different ones:
`MOTION_BLOCKING` is "blocks motion, or holds a fluid", and short grass and
flowers are the difference. Vanilla lets daylight fall through them at fifteen,
Dust put its sky floor above them, and the cell they stood in came out at
fourteen. `crates/dust-server/src/net/source.rs` had called this "a known
approximation" since it was written.

**Asked of Minecraft, they are six different predicates and none of them is
`not air`:**

```text
WORLD_SURFACE               26,681 of 26,684 states
WORLD_SURFACE_WG            26,681
MOTION_BLOCKING             23,189
MOTION_BLOCKING_NO_LEAVES   22,909
OCEAN_FLOOR                 22,759
OCEAN_FLOOR_WG              22,759
```

`not air` counts 26,683, so it was wrong about 3,494 states in
`MOTION_BLOCKING` — and wrong even for `WORLD_SURFACE`, where it looks right:
vanilla's `isAir` also says yes to `cave_air` and `void_air`, which Dust's
comparison against `minecraft:air` alone does not.

The oracle reads them the same way it reads opacity, off the same object in the
same pass: `Heightmap$Types` is an enum whose members each carry a
`Predicate<BlockState>`, and each is asked for its own serialization key —
`MOTION_BLOCKING` and the rest, the strings a chunk's NBT already uses and
`HeightmapKind::nbt_key` already returns. **Nothing matches by position or by
ordinal**, on either side.

## The decision

**The 3x3 volume is not adopted. The heightmap predicates were built instead,
and are what a server now stands on.** Both closed part of the same 611 cells,
and they differ by two orders of magnitude in what they cost.

* **The heightmap predicates** cost six columns in a table Dust already reads at
  boot and one array index per cell. They are code constants in Minecraft, in no
  report and no data pack — D8's problem exactly, and D8's route was already
  built and already carrying opacity to the same place.
* **The volume** costs roughly six times the lighting work. A server would read
  nine columns to serve one, or hold a cache of lit columns nine times the size
  of the one it holds now. The harness's own timings are not a server's, but the
  ratio is the ratio.

The cheap one was done first, which was the right order even though it turned
out to close the *smaller* share: 176 cells against the volume's 435. The
reason it was still right is that the two are not alternatives — a server needs
both to reach exact, and doing the expensive one first would have spent the
larger cost to reach 99.993% and then still needed the cheap one.

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

- **Dust's light stays one column wide**, with the skirt for sky light and
  nothing at all for block light, and under-lights where light would have to
  cross a boundary. 435 sky cells and 1,163 block cells of 2.4 million on an
  inland world; nothing at all in sky light on an ocean one. It is now the only
  thing between a served world and Minecraft's own light, in either kind.

- **Block light's share of it is the larger one, and it looks worse.** A
  neighbour's *sky floor* is a complete description of what that neighbour
  shines in, which is why the sky-light skirt works and closes most of that
  gap. A neighbour's *emitters* are not — what reaches the shared face depends
  on what the light travelled through — so block light has no skirt at all and
  a torch a block into the next chunk lights nothing. The sky-light seam is a
  shade across a cliff face; this one is a hard edge at a chunk border with a
  lit room on one side of it. Seeding the boundary with `emission - distance`
  would close it and would **over-light**, which is the one kind of wrong this
  project's light harness treats as unexplained.
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
