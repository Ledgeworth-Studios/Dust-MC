# D16 — What a placed block reads from the cell it lands in

**Status:** Decided, 2026-09-03. Three rules read the target cell, one item
carries two blocks, and both were measured against a vanilla server before
either was written.

## Context

Decision records 0011 and 0014 measured the placement gap twice and each time
said plainly what its survey could not see. 0011's grid varies the four numbers
a right-click carries — the clicked face, the cursor height, the yaw, the pitch
— and holds everything else still. 0014's scenes vary what is **beside** the
target and hold the click still. Between them they left one variable untouched,
and it is the one a player meets first:

> The arena clears the target cell before every sample.

Every row either survey has ever written is a placement into **air**. Minecraft
has three rules that read that cell, and all three were therefore invisible:

* a block put into water comes out **waterlogged**;
* a second layer of snow **stacks** on the first, to eight;
* a slab put into its own other half becomes a **double slab**.

The README named the first of them and a player meets it constantly — every
dock, every underwater build, every fence post in a river left a dry hole in
the water where the post went in.

## The survey: `tools/bot/placement.js --into`

The third variable, and the inverse of both the others: the click is held at
the support's top face and what varies is the block put in the cell above it,
which is where the placement lands.

**The target is walled on four sides with stone.** That is not tidiness. An
unwalled water source spreads across the arena inside two ticks and every
sample after it is measured in a puddle. The cell above is left open so that a
block taller than one is not refused for a reason that has nothing to do with
the question.

One column is added, `into`, and like `before` it is **a measurement rather
than an intention**: what was really in the target when the placement went out,
read back off the wire. That matters more here than anywhere else. A `/setblock
water[level=1]` in a sealed pocket is gone by the next tick, and a row claiming
the placement landed in flowing water would be a row about a cell that held
air. The same column is what caught `short_grass`, which cannot stand on stone
and is not in the target when the click arrives however firmly it was asked
for.

**A refusal does not look like a refusal here, and that is the one thing this
survey had to be taught.** In both older surveys the target is air, so a refused
placement leaves air and `minecraft:air` is the whole test. Here a refused
placement leaves *whatever was already there*, which is a state and reads
exactly like a successful placement of it. The test is "is the cell what it was
a tick ago", and a slab put into a double slab and a ninth layer of snow are
precisely that.

## What the surveys said

```text
  the grid, D11's survey            7,400 situations   925 items
  the scenes, D14's                 3,000 situations   221 items
  --into, 14 cells x 15 items         210 situations    15 items
  --into all, oak_fence and stone   2,120 situations     2 items
```

**2,330 situations added**, and every one of them is a question neither of the
other two surveys can ask.

```text
  --into                  108 placements into a full cell, 108 agree
  --into all               178 placements into a full cell, 178 agree
                            89 of the 1,060 cells accept a fence at all
                             4 of those 89 make it waterlogged
```

The `--into` run scores **108 of 108** placements into a non-empty cell, and
with its own column cut out of the file the same rows score 65 of 108. That is
the check watched to fail: the number the rule depends on, removed, and the
score going back. `--into all` scores **178 of 178**, and 174 of 178 with the
column removed — four rows, and they are the four this record is about.

## The three rules, and the one that is not about water

`waterlogged` is set from the cell's fluid, both ways round, and **the second
half is where most of the gain was**. A conduit, a sea pickle and every one of
the twenty coral fans carry `waterlogged=true` in their *default* state, so a
server that never touched the property put every one of them down flooded on
dry land. 122 of the 496 rows the grid called wrong were that, and not one of
them involved water.

Which cells count as water is `getFluidState` and not "is this block water".
Seagrass and a bubble column stand *in* water and report it, so a fence put
into seagrass comes out waterlogged and one put into a lily pad does not.

That is why `--into all` was run: a name list is only worth having if somebody
has asked every block whether it belongs in it. An oak fence was put into every
one of the **1,060 blocks this build knows**. 89 accepted it — the other 971
are not replaceable by a fence and the placement went to the cell above — and
of those 89, **exactly four made it wet**: water, a bubble column, seagrass and
tall seagrass. Lava did not, and lava is one of the 89, so that is a measured
answer rather than an untested one.

Kelp and a kelp plant hold water too and are deliberately **not** in the list.
Nothing can be placed into either, so no run reaches them, and a line no check
covers is a line that will be wrong one day without anybody hearing about it.
The control for the same run is the one 0014 uses: stone has one state, and it
came out stone over all 1,060 cells.

## Where the placement lands is now an item-aware question

Minecraft's `canBeReplaced` takes a placement context, and two blocks answer
differently for it: deep snow may only be replaced by more snow, and a slab
only by its own other half. `dust_sim::placement::replaces_clicked` and
`replaces_beside` are the two shapes of it, and they are two functions rather
than one because the clicked cell and the cell behind it are different
questions — a bottom slab clicked on its top face doubles, and the same slab
clicked from underneath puts a new slab below it instead.

**A ninth layer of snow is not refused, it goes on top.** Eight layers is a
full block; the drift stops taking layers and the cell above takes the snow.
The README said "refused" and the measurement says otherwise.

## An item can put down two blocks

A sign, a torch, a banner and a head each have a standing form and a wall form,
and the wall form is the largest single thing either older survey called wrong:
**152 rows across 43 items placed the wrong block entirely**.

Nothing relates the two blocks but the item that holds both. A torch and a wall
torch share no property, and `torch` -> `wall_torch`, `oak_sign` ->
`oak_wall_sign`, `black_banner` -> `black_wall_banner`, `skeleton_skull` ->
`skeleton_wall_skull` is four different name transformations and not a rule. So
it is data, and it arrives under 0008's rule: `StandingAndWallBlockItem`'s
`wallBlock` field, off the operator's own jar, in two more columns of
`dust-items.tsv`. 58 items have one.

`attachmentDirection` is the second column and it is what a rule would have got
wrong. A sign stands on the ground and attaches `down`, so the top of a block
stands one up. A **hanging** sign attaches `up` — it hangs from what is above
it — so that same face hangs nothing at all and the underside is what hangs
one. Twelve items, in the direction nobody would think to test.

A table written before the columns answers `None` for every item, and the
caller asks `has_walls()` rather than reading that `None`. That is the trap the
`replaceable` column paid for in 0011 and it is the same trap: **ask whether
the table knows, not what it says when it does not.** The harness prints which
kind of table it read, so a run against an old one says so rather than
reporting a worse score for a better build.

## What was declined

**A rule from the block's name, instead of the two columns.** It would have
been right about every sign and every banner and is exactly the "right 98% of
the time" this project has already decided is worse than a table. The columns
cost one reflection lookup at extract time and answer for a version this build
has never seen.

**The wall form of a hanging sign.** It faces *across* the wall rather than out
of it — a north face gives `west` and an east face gives `south`, which is not
a function of the clicked face — and the grid was taken at one yaw, so it
cannot say what the other input is. A wall block facing the wrong way is worse
to look at than the standing sign that is there now, so a hanging sign keeps
its old answer and 35 rows stay wrong. The survey that would settle it varies
the yaw with the face, and does not exist yet.

**Refusal as a scored quantity.** The `--into` run refuses 102 of its 210
placements and every one of them is correct — a slab, a deep drift and kelp are
not replaceable by a fence, so the block lands in the cell above and the watched
cell does not change. The harness counts refusals apart and does not compare
them, because most refusals in the older surveys are *support* rules a torch on
a ceiling would trip and Dust has no support rules at all. Scoring them would
report a thousand findings about a question this record is not about.

## The score

```text
                                 before   after
  the grid, 7,400 placements
    rows Minecraft placed and Dust would not     496      62
    items wrong in at least one situation        101      21
  the scenes, 3,000 placements
    rows wrong                                   231      92
    items wrong                                   29      18
  --into, placements into a cell holding something
    rows scored                                    —     108
    rows wrong                                     —       0
```

What is left of the grid's 62 is named rather than rounded off: 35 hanging
signs on walls, 8 a crafter's `orientation`, 6 the age of three vines, 4 a note
block's `instrument` (which is the block *below* it and needs a column of its
own), 4 redstone wire and 4 scaffolding's `distance` — both of which 0014 left
on purpose — and one pointed dripstone.
