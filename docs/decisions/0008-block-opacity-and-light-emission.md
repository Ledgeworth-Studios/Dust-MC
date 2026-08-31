# D8 — The block constants Minecraft keeps in code

**Status:** Open, with the options costed and the cost of the gap measured.
Decided when somebody chooses a source; nothing is blocked on it that is not
already stated as a known gap.

## Context

Dust's light engine asks one question of every block: how much light does
entering this cell cost? Today the answer is **zero for air and fifteen for
everything else**. Minecraft's answer is not binary — glass costs nothing,
water and leaves cost one, ice costs three — and neither is its light
*emission*, which Dust has no model for at all. There is no block light in
Dust, and every block but air is a wall to sky light.

Both numbers are code constants in Minecraft. They are in no `--reports`
output, in no data pack, and in nothing `xtask extract` can reach. That is the
whole of the problem: this is not a piece of work nobody has done, it is a
piece of data nobody can currently get.

## What the gap actually costs, measured

`cargo xtask harness light --version 1.21.1 --seed 0 --radius 4` reads the
light Minecraft computed into its own region files, lights the same chunks with
Dust's engine, and compares cell by cell.

**Every single disagreement is Dust being darker** — the direction both known
gaps point in — and the shortfalls are one block list:

```text
seed 0, radius 2/4/6      99.4% agree     oak leaves, water, birch leaves,
                                          short grass, seagrass, flowers
seed 1, radius 2/3        96.4% agree     water (168,428 of 169,480),
                                          seagrass, kelp
```

**The percentage is a property of the world and not of the engine**, and that
matters for how this decision is argued. Seed 1 spawns in deep ocean, so its
shortfall is an even 12,544 cells at each level from fourteen downwards — one
per column per level, the water column marching down — and the server is the
same server that reads 99.4% on seed 0. What is invariant is the *shape*: one
direction, one block list, every one a block Minecraft gives an opacity of one
or two.

So the cost of this decision is known block by block, and its size is known to
depend on how much water and foliage a world has. That is an unusually good
position from which to not have decided something.

### And opacity owns nearly all of it (2026-08-31)

The measurement above had a confound while it was written: sky light has *two*
known gaps, and a percentage cannot say which one it is reporting. `harness
light` now splits the shortfall by how far each cell sits from its column's
edge. Light arriving from a neighbour enters at a face and loses a level per
step inward; opacity does not care where in a column it is.

```text
distance from a face   0      1      2      3      4      5      6      7
seed 0, radius 2    0.660  0.595  0.561  0.548  0.530  0.510  0.530  0.581
seed 1, radius 3    3.522  3.521  3.521  3.516  3.512  3.513  3.516  3.516
```

**Flat on both worlds**, and on seed 0 the rate *rises again* at the centre,
which no neighbour effect produces. Reading seed 0's interior as the opacity
floor puts everything the edge carries above it at roughly 750 cells of 14,276.

**So this record is not choosing between two comparable causes. It is the whole
question**, and the multi-column light volume — the other outstanding item — is
worth about five per cent of the gap, on a world where the gap is 0.6%.

The measurement is a rate per ring and not a count, and that is the load-bearing
part: a column has `60 - 8d` columns at distance `d`, sixty at the face against
four in the middle. Counted raw, a perfectly uniform cause reads as "it is all
at the edges". The histogram would have confirmed the impression it was built
to test.

## Options

**1. Extract it from the server jar. — BUILT, and it works.** A small Java
program on the jar's classpath, using the ProGuard mappings downloaded beside
it, reflecting over the block registry and reading `getLightBlock` and
`lightEmission` per state. See the costing and the results below.

The right answer in principle — it produces Minecraft's own numbers, at the
operator's machine, from the operator's jar, which is exactly D6 and D7's rule.
The cost is that Minecraft's static initialisation has to run, which means
`Bootstrap.bootStrap()` through obfuscated names, which is what mod-loader-based
extractors exist to avoid doing by hand.

**Costed, 2026-08-31, and it is cheaper than that paragraph says.** Everything
the oracle would reflect over is in the published mappings for 1.21.1, checked
name by name:

| what | mapped to |
| --- | --- |
| `Block.BLOCK_STATE_REGISTRY` (an `IdMapper`) | `dfy.q` |
| `BlockBehaviour$BlockStateBase` | `dtb$a` |
| &nbsp;&nbsp;`int lightEmission` — a **field**, not a call | `b` |
| &nbsp;&nbsp;`getLightBlock(BlockGetter, BlockPos)` | `b` |
| &nbsp;&nbsp;`canOcclude()` / `propagatesSkylightDown(..)` | `p` / `a` |
| `EmptyBlockGetter`, `BlockPos`, `Bootstrap` | `dcl`, `jd`, `akt` |

Two things follow. `BLOCK_STATE_REGISTRY` is an id-to-state map, so the
extraction comes out **keyed by the same state ids Dust already generates
against** rather than by name — no matching step, and no place for one to be
subtly wrong. And `lightEmission` being a field means emission needs no world
at all; only `getLightBlock` takes one, and `EmptyBlockGetter.INSTANCE` with
`BlockPos.ZERO` is what it wants.

**Half the machinery is also already written.** `xtask/oracle/dustoracle/Names.java`
on `wip/0.5-worldgen` looks Minecraft's classes and members up from a
key-to-name properties file the extractor writes, and **contains no Minecraft
identifier of its own** — so it does not need rewriting when a version renames
everything. What is missing is the Rust half: a parser for the mappings file and
the small table of keys above.

So the honest cost is a mappings parser, a properties file, one Java class and
an `xtask extract` verb — not a research project. **This does not decide
anything below; it removes the reason option 1 looked expensive.**

### Built, 2026-08-31. `cargo xtask extract --only light`

Six and a half seconds from a clean cache, and it reports **26,684 block
states**:

```text
opacity: 0=14616  1=9552  15=2516
1,588 state(s) emit light
```

Spot-checked against names, and these are Minecraft's own numbers:

| block | opacity | emission |
| --- | --- | --- |
| air | 0 | 0 |
| stone | 15 | 0 |
| glass | 0 | 0 |
| water, oak leaves, seagrass, **ice** | 1 | 0 |
| torch | 0 | 14 |
| glowstone, sea lantern | 15 | 15 |
| lava | 1 | 15 |

**Two things this corrects in the record above.** "Ice costs three" — it costs
**one**; that figure was not measured and is wrong. And opacity takes exactly
**three** values in 1.21.1, not a spread: a state either occludes (15), or lets
sky light through unattenuated (0), or costs one. So the whole of the gap this
record is about is 9,552 states that Dust charges 15 for and Minecraft charges
1 — which is precisely the block list `harness light` reported, arrived at from
the other end.

The pipeline is three steps and one of them is worth knowing: the jar is a
*bundle*, and a nested jar cannot go on a classpath, so Mojang's own unpacker
runs first — `-DbundlerMainClass=` with an empty value unpacks and exits.
Passing the oracle's own class there instead does **not** work: the bundler
builds a classloader over the jars it unpacked and nothing else, so it cannot
see the class it is told to start.

**Nothing extracted is committed.** The table lands in `.dust-extract/` beside
the jar, behind the same `.gitignore` line, and is regenerated by anyone who
runs the verb against their own copy.

**2. Derive it from tags.** Dust now holds all thirteen tag registries. A rough
opacity model could be built from `minecraft:leaves`, `minecraft:impermeable`
(glass), the flower and replaceable groups, and the fluid registry — which is
very nearly the list the measurement above names.

Rejected as a *silent* approximation by D6 and D7, both of which refuse derived
values on the grounds that a wrong number is invisible. **That objection is
weaker than it was**, because `harness light` makes exactly this kind of error
visible: a derived table can be adopted and its accuracy reported as a
percentage against Minecraft's own answer. What it cannot do is stop being an
invention. A tag is not an opacity, and mapping one to the other is choosing
numbers.

**3. Take them from the operator, like the registry contents.** A file in the
`[data]` directory. Honest, and it moves the problem to whoever has to write the
file, which is nobody.

**4. Leave it.** What is happening now. Sky light is 99.41% right, wrong in one
direction, and wrong in a way that is written down where the code lives.

## Why this is not decided here

**Read the costing above first, and then note that option 1 now exists.** The
reason this record sat open was that option 1 was the right answer nobody had
priced, which left options 2 and 3 — the ones that need a judgement — looking
like the only live ones. It is built, it runs in seconds, and it produces
Mojang's own numbers from the operator's own jar without committing any of
them.

**So there is no judgement left to make between the options**, and what remains
is a smaller and different question: whether `opacity_of` should consume the
table when one is present, and what a server with no `[data]`-side table should
do — which is the ordinary question every other operator-supplied input in this
project has already answered. The paragraphs below are kept because they are
the reasoning that got here, not because the choice is still open.

Option 2 is an afternoon's work and would visibly improve the world a player
sees. It is also the one that puts numbers in this repository that no
measurement produced and no extraction justified, against two decision records
that say values come from the operator's own copy of the game. That is a
judgement about the project's line and not about lighting, and it wants a
decision rather than a commit.

The measurement is the input. This record consumes it, exactly as D4 waits on
Phase 10's.

## The same wall, one block over: sounds

Placing a block should make a noise for everybody else. Captured from a real
1.21.1 server — a bot placing stone — it sends `sound_effect` with a sound
registry id, category `block`, the block's centre in eighths of a block, volume
1.0, pitch 0.8 and a seed.

Every part of that is reachable except the one that matters: **which sound**.
Minecraft holds a `SoundType` per block in code, exactly like the opacity and
the emission, so a server without it can either say nothing or play the same
noise for glass and gravel. Dust says nothing.

That is worth recording here rather than in its own file because it is not a
second decision. It is the same decision, and whichever way it goes, it goes the
same way for all three.

## Consequences of leaving it

- **No block light at all.** Torches, lava and glowstone light nothing, and
  there is no engine work outstanding for it — `dust_world::propagation` runs
  the same walks vanilla does. It is waiting on emission values and nothing
  else.
- **Sky light stops at the surface of an ocean and under a tree**, which is
  visible to a player and is nearly all of the shortfall. On an ocean spawn it
  is 3.5% of every cell in view.
- **No sound when a block is placed**, for the reason above. Breaking one is
  fine — the break effect carries the block's *state* and the client picks the
  sound itself, which is why that could be built and this cannot.
- **`opacity_of` is the one place this is decided** for light, and it says so.
  Whichever option is taken changes that function, the emission model beside it,
  and a block-to-sound table, and nothing else.

## Related

* D6 — the ore baseline, which established that vanilla's numbers arrive from
  the operator's jar rather than from here.
* D7 — registry contents, which extended the same rule to the wire.
