# D8 — The block constants Minecraft keeps in code

**Status:** Decided, 2026-08-31. The numbers come from the operator's own jar,
asked of Minecraft by an oracle (option 1), and reach a running server as a file
in `[data] path` (option 3). Both are built. What the decision bought is
measured below, and it turned out to be larger than this record predicted, for
a reason this record had no way to see.

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

### And then it was measured, and it was not what this record thought (2026-08-31)

`harness light` now runs **both** opacity models over the same chunks in the
same run. Adopting Minecraft's own numbers, on their own, moved seed 0 from
99.419% to **99.423%** — a hundred and seven cells of fourteen thousand.

That is not a small win. It is a **wrong prediction by this record**, and what
was wrong with it is worth more than the number.

The cells were still short. What changed was *how* short: 6,128 cells short by
fourteen became nineteen cells short by thirteen. Light was reaching under the
water and into the leaves after all, and arriving at half the level Minecraft
says it should. The cause was in the engine, not the data —
`dust_world::propagation` charged `1 + opacity` for a step where Minecraft
charges `max(1, opacity)`, so every block of opacity one cost two.

**Nothing could see it while the only opacity model answered 0 or 15**, because
the two rules agree at both ends: at 0 they are both one, and at 15 they both
take everything. A wrong constant hidden by another wrong constant, for the
whole of the light engine's life.

With both fixed:

```text
                          agree      cells short
  seed 0, radius 2
    air only, stand-in   99.419%          14,276
    Minecraft's own      99.975%             611
  seed 1, radius 3
    air only, stand-in   96.482%         169,480
    Minecraft's own     100.000%               0
```

**Seed 1 is exact** — 4,816,896 sky-light cells of an ocean world, and not one
of them disagrees with the light Minecraft wrote. The world that was *worst*
under the stand-in is the one that comes out right.

And the residual has changed identity, which the ring histogram says out loud:

```text
distance from a face    0      1      2      3      4      5      6      7
seed 0, air only     0.660  0.595  0.561  0.548  0.530  0.510  0.530  0.581
seed 0, Minecraft's  0.072  0.021  0.008  0.007  0.005  0.005  0.006  0.018
```

Flat under the stand-in, because opacity does not care where in a column a cell
is. Falling by an order of magnitude from the face inwards under Minecraft's
numbers, which is the shape a **neighbour** effect makes and the first time this
verb has seen one. Seed 0's remaining 611 cells are 400 cells of air near an
edge with some grass and leaves beside them, and they belong to the multi-column
light volume — the other outstanding item, which this record had costed at "five
per cent of the gap" and which is now very nearly all of what is left of it.

**The transferable part is not about lighting.** The ring measurement above was
built to separate two known causes and it did that correctly. It could not
separate either of them from a *third* cause nobody had proposed, and it read
"flat, therefore opacity" — which was true, and which was also true of a step
cost that doubled every opacity that was not 0 or 15. The stand-in did not just
under-light the world; it made a second defect unobservable, because it never
produced the input under which the two rules differ. A guard can only fail on a
question somebody thought to ask, and a stand-in can only expose the defects its
own range reaches.

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

### Built, 2026-08-31. `cargo xtask extract --only constants`

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

## Why it was not decided here for so long

Kept because the shape of the delay is the useful part. Option 1 was the right
answer nobody had priced, which left options 2 and 3 — the ones that need a
judgement — looking like the only live ones, and a judgement is easy to
postpone. Pricing option 1 took an afternoon and found that everything it needed
was in the published mappings; building it took another and produced Mojang's
own numbers in six and a half seconds.

Option 2, deriving opacity from tags, was rejected as a *silent* approximation
and stays rejected — not because the objection held (it weakened the day
`harness light` could report a derived table's accuracy as a percentage) but
because it stopped being needed. A wrong number that nobody can see is the
hazard D6 and D7 legislate against, and the whole point of the oracle is that
there is no number to invent.

The measurement was the input. This record consumed it, exactly as D4 waits on
Phase 10's — and then the measurement corrected the record, which is the section
above.

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
second decision. It is the same decision, and it went the same way for all
three: the oracle already holds the object a `SoundType` hangs off, and the
route the numbers travel is settled below. What is left for sound is a column in
the oracle's output and a place to read it, which is work rather than a
judgement.

## How the numbers reach an operator — decided

`cargo xtask extract --only constants` is a **developer** command. It runs from a
Rust checkout. An operator has a `dust` binary, a `dust.toml`, and — per D7 — a
`[data] path` directory they produced with Minecraft's own `--server`
generator. They have a JDK and a jar already, so the oracle asks nothing new of
them; what it needed was a *route*, and there were four.

**1. A new `dust` subcommand** that runs the oracle. Honest and discoverable,
and it costs the thing `cli.rs` opens by stating: the grammar is "one
subcommand with three flags, and nothing composes". It also drags the Java
sources and a `javac` dependency into the shipped artefact.

**2. The server runs the oracle at boot**, given a jar path in `dust.toml`. No
new grammar, but it puts a JDK, a `javac` and a subprocess on the boot path of
every server that wants correct light, and turns a class of Java failures into
a class of server-start failures.

**3. The table travels with `[data] path`. — TAKEN.** One more file in a
directory the operator already populates, read at boot if present and absent
otherwise, which is exactly how `[data] path` itself behaves. Costs nothing at
run time and needs no grammar. Its open end was who generates the file, and the
answer is option 1 today and option 4 the day there are releases — the *format*
decision and the *producer* decision are separate, and only the format had to be
made now.

**4. Publish the oracle as a small standalone jar** beside each release. An
operator runs `java -jar dust-oracle.jar <server.jar> <out>` once. No Rust
checkout, no new `dust` grammar, nothing Mojang's in the release. Costs a second
release artefact and a second thing to version — and there are no releases yet,
so it could not be the answer to "how does an operator do this today".

### What was built

The file is `dust-constants.tsv`, beside `minecraft/` rather than inside it:
everything under `minecraft/` is Minecraft's own output in Minecraft's own
layout, and a bare `light.tsv` in there would look like one more of them. The
name says who wrote it and who reads it.

```text
<[data] path>/
  dust-constants.tsv
  minecraft/
    worldgen/biome/…
```

Absent is not an error, and the server says which case it is in at boot with the
measured cost of going without. **Present and wrong is** an error that stops the
server: the alternative is a server that reads the operator's file, puts it
down, and runs with lighting quietly worse than they asked for. `xtask extract
--only constants` prints the one `cp` line that puts the file where it belongs, at
the moment somebody has just produced one.

Nothing about this ships a Mojang value. The repository holds the question, the
oracle that asks it, and the reader for the answer.

**What is left:** nothing this record opened. Sky light is done, block light is
done, the block-place sound is done, and so is which block an item puts down —
the last two by the sections directly below, which are the same route carrying a
third and a fourth kind of constant.

## And the sound a block makes going down (2026-08-31)

`SoundType` is the same shape of problem as `getLightBlock`: a value handed to a
block's properties in Java, in no report, no data pack and nothing the
generators emit. It reaches a server the same way — three more columns in the
same file, off the same object, through the same oracle — and the route needed
no argument because this record already made it.

**Two things in it are decisions and not applications of one.**

**The column holds the sound's name, not its id.** `minecraft:block.stone.place`
rather than 1366. An id is a *position* in the sound registry of whichever
Minecraft the extractor ran against, and a table carried over a version bump
would have handed the server a number that is in range, resolves, and is a
different sound. A name is a string `dust-registry`'s own generated
`minecraft:sound_event` table already holds independently, so the two sides meet
on something each of them knows — exactly what the heightmap columns already do
with their serialization keys, and exactly why `BLOCK_STATE_REGISTRY` was chosen
over the `Blocks` class at the top of this record. All 109 of 1.21.1's names
resolve; one that did not would be named, and would be the version skew rather
than a quiet substitution.

**The table holds the group's own volume and pitch, unscaled.** Vanilla plays a
placement at `(volume + 1) / 2` and `pitch * 0.8`; a step off the same group
scales differently. A table that had already applied the placement's arithmetic
could not serve the step, and the day a step sound is wanted the file an
operator extracted a year ago would be the wrong file. The scaling belongs to
the caller and is written where the caller is.

**A sound is not a light level and the check is weaker, which is worth saying
out loud.** The opacity column has a real guard: every light level fits in a
nibble, so a value above fifteen is proof the oracle read the wrong Java member.
Volume and pitch have no such ceiling — `sound_volume` and `sound_pitch` are
refused if they are not finite numbers under ten, which catches a misaligned
read and does not catch reading `pitch` where `volume` was meant. What does
catch that is the distinct-group count: 112 sound groups over 26,684 states,
109 of them distinct once volume and pitch are counted in, and a `SoundType`
field resolved to the wrong member answers the same thing for every state. The
extractor refuses a table that reports one group, for the same reason it refuses
one with no heightmap columns.

## And which block an item places (2026-08-31)

The fourth value through this route, and the first that needed a **second
file**. `dust-items.tsv` sits beside `dust-constants.tsv` in `[data] path`,
because the two are keyed by different things — one row per block state, one row
per item — and a table whose rows meant two different things depending on which
column was filled would be a format nobody could check.

`BlockItem.block` is a Java field. It is in no report and no data pack, and the
oracle reads it in the same run, off the same jar, as everything above.

**The reason it is a table and not a rule.** 925 of 1.21.1's 1,333 items place a
block, and 909 of those place the block of their own name. It is tempting to
stop there and write `Block::from_name(item.name())`, and it would be wrong
sixteen times:

```text
minecraft:redstone       -> minecraft:redstone_wire
minecraft:string         -> minecraft:tripwire
minecraft:wheat_seeds    -> minecraft:wheat
minecraft:powder_snow_bucket -> minecraft:powder_snow
minecraft:cocoa_beans    -> minecraft:cocoa
minecraft:pumpkin_seeds  -> minecraft:pumpkin_stem
minecraft:melon_seeds    -> minecraft:melon_stem
minecraft:carrot         -> minecraft:carrots
minecraft:potato         -> minecraft:potatoes
minecraft:torchflower_seeds -> minecraft:torchflower_crop
minecraft:pitcher_pod    -> minecraft:pitcher_crop
minecraft:beetroot_seeds -> minecraft:beetroots
minecraft:sweet_berries  -> minecraft:sweet_berry_bush
minecraft:glow_berries   -> minecraft:cave_vines
```

— and, the other way round, **`minecraft:air` and `minecraft:wheat` are items
that share a block's name and place nothing at all.** The second of those is the
sharp one: `minecraft:wheat` is what bread is made of, the crop of that name
comes from the seeds, and a name-matching server would let a player put a crop
down by holding a handful of harvest. That is a bug found by a player and not by
a test, which is the same argument option 2 of this record already lost.

**The item's own name is in the file beside its id, and it is load bearing.**
The light table can only check its row *count*, so it catches a version with a
different number of block states and nothing finer. This one checks every row's
name against the name this build gives that id, so a version that renumbered a
single item is caught on the row where it happened, by name. It is the strongest
version-skew check anything on this route has, and it is there because the
column was free.

**What arrives is a block, not a block state.** A stair placed by a player faces
the way they were standing; that is `getStateForPlacement`, it needs a placement
context, and it is a different problem in a different place. The caller takes
`Block::default_state` and the gap is stated where it is taken.

## What is still consequent

- **Block light — built, 2026-08-31, and this record was right about it for
  once.** The emission values were the whole of it: `dust_world::propagation`
  ran vanilla's walks already, and what was missing was seeds. A torch, lava and
  glowstone now light what they should, exactly — `harness light` compares block
  light against Minecraft's own arrays and finds no disagreement at all once the
  volume is wide enough, on both seeds.

  Read the measurement above before taking that as a pattern. This record said
  the same about opacity, where the data turned out to be the smaller half, and
  the reason it was right this time is not a better prediction — it is that
  block light had already been measured against nothing, so there was no
  stand-in in the way. **The `EmissionModel` with no table is not a stand-in
  either**: a server with no constants table says nothing emits, which is a
  refusal to invent how bright a torch is rather than a guess at it.
- **Sky light stops at the surface of an ocean and under a tree** on a server
  with no table, and that is now a configuration state rather than a property of
  Dust: 3.5% of every cell in view on an ocean spawn without one, and nothing at
  all with one.
- **A placed block makes a sound — built, 2026-08-31.** Breaking one always
  could: the break effect carries the block's *state* and the client picks the
  sound itself. A placement has no such packet, so the sound has to be named,
  and naming it needed the table. A server with no table places blocks in
  silence, which is what every server did before this and is a refusal to invent
  what a block sounds like rather than a guess at it — every block would have
  been stone.
- **A player places what they are holding — built, 2026-08-31.** Not an
  inventory: nine hotbar slots and a selected one, written by
  `set_creative_mode_slot`, which is the single inventory write a creative
  client makes without a container open. Dust puts every player in creative, so
  that is the whole path from the creative menu to a block going down. A server
  with no item table places the world's own surface block whatever is held,
  which is what every server here did before.
- **`opacity_of` is the one place this is decided** for light, and it says so.
  It takes the table, the emission model beside it takes the table,
  `net::play::block_placed` takes it for the sound, and
  `net::session::held_block` takes the item table for the block. Four readers,
  one route.

## Related

* D6 — the ore baseline, which established that vanilla's numbers arrive from
  the operator's jar rather than from here.
* D7 — registry contents, which extended the same rule to the wire.
