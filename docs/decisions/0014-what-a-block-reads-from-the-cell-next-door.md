# D14 — What a block reads from the cell next door

**Status:** Decided, 2026-09-03. One rule, applied at placement **and** on every
neighbour of every write. Measured against a vanilla server both times.

## Context

Decision record 0011 measured the placement gap with a grid: for every one of
the 925 placing items, eight right-clicks varying the clicked face, the cursor
height, the player's yaw and the player's pitch. It closed the gap from 481
items wrong to 160, and it said plainly what it could not see:

> The arena is one stone block in a cleared volume, so nothing varies a block's
> **surroundings**.

Sixty-one of the remaining 160 were that. A fence does not connect to what it
touches, a wall does not, a glass pane does not, a rail does not bend towards
the rail beside it, and a stair does not become an inner or an outer corner.
No arrangement of yaw and pitch can make any of them happen, so the first half
of this work is a second survey and not a rule.

## The survey, and how many situations it added

`tools/bot/placement.js --neighbours` holds the click still and varies what is
**beside** the target, which is the exact inverse of the grid. Its scenes are:
nothing at all; a full block; a full block that does not occlude (glass, which
a fence joins and a rule keyed on opacity would miss); a block with no full
side (a bottom slab, which a fence refuses and a rule keyed on "is it solid"
would get wrong); something above; the straight run that decides a wall's post;
then the block's **own kind** on one side, two sides and all four, because that
is the case a player builds all day. Where the block has a four-valued `facing`
there are five more, with the neighbour turned across it and one of them in the
other half — a stair only takes a corner from a stair in its own half, and a
scene using the neighbour's default facing would report `straight` and call the
rule correct.

`--against all` replaces the scenes with one per block the build knows, each
put to the north of the target. That is how "does a fence connect to X" is
answered for every X rather than argued about.

```text
  the grid, D11's survey                    7,400 situations   925 items
  the scenes, 11 to 17 per item             3,000 situations   221 items
  --against all, oak_fence and the control  2,120 situations     2 items
```

**5,120 situations added**, and every one of them is a question the grid could
not ask.

Two columns were added to the answers file and both are **measurements rather
than intentions**. `before` is what the six cells around the target actually
held when the placement went out, read back off the wire; a `/setblock` naming
a property the block does not have is refused by the server and leaves air, and
a scene written down as what was *asked for* would score a rule against a
neighbourhood that was never there. `after` is which of those six the placement
changed, which is the other half of the rule and is the reason this record has
a second number in it at all.

## What it cost to make the survey trustworthy

Three faults, in the order they were found, and each one looked like a wrong
rule until it was not:

1. **A neighbour with nothing to stand on falls the tick after it is set.**
   The arena's floor was one block under the target, so a rail set beside it
   emptied itself before the click and the row recorded `north=minecraft:air`
   against a shape rule scored on a rail that was not there. The fill now lays
   a floor under all four side cells.
2. **A barrier that is not last is not a barrier.** The grid loop sets the
   support first because nothing follows it. Here the scene follows it, so the
   support goes down last and seeing it turn to stone means the scene landed
   too.
3. **`before` has to be true at the moment of the click, and the barrier only
   says the commands were delivered.** A ladder is put wherever `/setblock` is
   told and falls on the next tick. One row in 799 of a fence-against-every-
   block run read the ladder in `before` and air in `after`, and looked exactly
   like a wrong connection rule. The scorer now reads a side that `after` says
   was emptied as the air the click actually saw, and prints how many rows that
   was rather than folding it into the rate.

The control is the same one the grid uses and it is what makes any of this
readable: `minecraft:stone` has one state, so no arrangement of neighbours may
change it. It agreed with itself over 1,060 neighbourhoods.

## The one thing the rules need that no report carries

Every connection rule asks the same question of the block beside it: **does it
present a full square face on this side?** Minecraft answers from the block's
collision shape, which is Java. It appears in no report and no data pack, and
it comes down decision record 0008's oracle route with the opacity and the
sound groups — six columns, `STURDY_DOWN` through `STURDY_EAST`.

**Six and not one, and this is the whole argument.** A block can have a full
face on one side and not another, and the commonest such block is a stair: the
back of a bottom stair is a full square and its front is not, so a fence joins
a stair from behind and refuses it from in front. A single "is this a full
cube" column would answer no to both and would be wrong in a place a player
looks at all the time.

Nothing Mojang's is committed. The columns arrive at run time from the
operator's own jar, and the extractor refuses a table where all six columns
count the same states — a bottom slab is sturdy underneath and not on top, so
six equal counts means the six `Direction` fields resolved to one constant.

## The decision: one rule, run twice

**A block's shape is recomputed when it is placed and again whenever anything
beside it changes.** Placement-only was considered and rejected outright.

A fence shaped only where the click landed connects to what was already there
and not to what arrives later, so a wall built west to east has arms and the
same wall built east to west does not, and breaking a block leaves the fence
beside it reaching at nothing. That is decision-rule priority 1: a
half-connected fence is worse to look at than one that never connects, and
"the placement path is the only one that has to change" is a statement about
the code and not about the game.

The cost is priority 2 and it is small. Every write reads its six neighbours
and asks each one a property-shape question that answers `false` for the stone,
dirt and wood that almost every neighbour of almost every edit actually is.
Only a neighbour that *is* a fence, a wall, a pane or a stair costs six more
reads. It is **one ring and not a search**, which is enough because a fence's
connection reads whether its neighbour is a fence and never how that fence is
connected. The single exception is named rather than hidden: a wall's post
reads the post of the wall above it, so retopping a stack three walls high
leaves the bottom one a tick behind.

The rule is idempotent — running it on its own answer changes nothing — which
is what lets the world call it on any write without tracking whether it already
has, and is what makes the two call sites one rule rather than two that have to
agree.

**A world with no constants table runs no connection rule at all.** That is the
`has_x()` question rather than the "what does it answer when it does not know"
one, and it is the same trade as above: bare is better than half-connected.

## What was measured, before and after

`cargo xtask harness placement --answers <file>`, with the rule and with a
constants table whose six columns have been cut off — which is exactly the
"remove the code the check is about and watch it go red" step, because without
the columns `Solid` cannot be built and no connection rule runs.

```text
                                          before     after
  the grid, 7,400 placements
    situations Minecraft placed            6,319     6,319
    Dust would place the same state        5,654     5,823
                                           89.5%     92.2%
    items wrong somewhere                    160       101

  the scenes, 3,000 placements
    situations Minecraft placed            2,905     2,905
    Dust would place the same state        1,955     2,674
                                           67.3%     92.0%
    items wrong somewhere                    140        29

  oak_fence beside each of 1,060 blocks
    situations Minecraft placed            2,120     2,120
    Dust would place the same state        1,695     2,120
                                           80.0%    100.0%
```

**Sixty-one neighbour-rule items down to two.** The fifty-five fences, walls
and panes are gone from the grid's wrong list and nothing joined it. A fence
now agrees with Minecraft about all 1,060 blocks it can stand beside, which is
a stronger statement than any rate: there is no block left that it joins and
should not, or refuses and should not.

And the other half, which the grid could not have scored at all:

```text
      524  neighbours the placement changed
      495  of them Dust changes the same way (94.5%)
       29  of them it does not
```

## The rules, and the two that could not be read off a shape

Keyed on the property *shape* and not on block names, the way D11's click rules
are, so one rule covers every fence, every wall, every pane and every stair
without a list of any of them. Two clauses are behaviour rather than shape and
are written out:

* **A fence joins its own kind, and "its own kind" is a strange rule.** A
  wooden fence joins wooden fences and a nether brick fence joins nether brick
  fences, and the two never join each other. The test is not "are they both
  fences" but "do they answer `#minecraft:wooden_fences` the same way".
* **A pane joins panes, iron bars and walls; a fence joins none of the three.**
  One shape, two rules, and nothing in the property table tells them apart.
  A fence joins a fence *gate* turned across it and a pane does not.
* **A full face is not enough.** Leaves, a barrier, a melon, the two carved
  pumpkins and the shulker boxes are full cubes a fence refuses. This is
  Minecraft's `isExceptionForConnection` and there is no shape to read it off.

The wall's post is the one that was measured wrong twice and is worth the
paragraph:

**A wall keeps its post until it runs through, and "runs through" is a line in
either axis.** A wall alone has a post. A wall in the middle of a north-south
line does not. A wall connected on **all four sides** does not either — a
crossroads is not a straight run, and the phrase "a straight run" is what the
rule was first written from. A block on top of a line through does **not** put
the post back, because the connections it makes `tall` already reach the top of
the wall; what does put it back is a wall above with its own post, and the odd
list in `#minecraft:wall_post_override` — a torch, a button, a lantern, the
small things a player stands on top of a wall and which need something under
them. Two of those five clauses were red in the survey before they were right,
and the survey is the only reason anybody would know.

## The rail, which was in the 61 and should not have been

Four of the sixty-one were the rails, and the grid's own rows say the rail's
grid failure is **the yaw** and not the neighbours: one situation of the eight,
the one at yaw 90, where Minecraft lays the rail east-west and Dust lays it
north-south. That is a click rule, it is four lines, and it had been filed
under neighbours on the strength of the property's name — which is the same
mistake D11 warns about for `facing` and it was made in D11's own summary.

What a rail still does not do is **bend**. A rail beside another rail turns
towards it, rises to one a block higher, and rewrites that rail in turn. That
reaches further than one ring and rewrites in both directions, so it is not
this rule with a different table; it is a different rule. It is left, it is
named here, and the scenes to measure it already exist.

## What is left, with counts

Of the grid's 101:

```text
    43  signs, banners        the item places one of two blocks
    22  corals, conduit       `waterlogged`, and the arena is dry
    10  leaves                `persistent`
     7  skulls                `rotation`, sixteen of them
     6  facing                what is left of it
     2  redstone, scaffolding neighbour rules this record did not write
```

`redstone_wire` reads four connections *and* a power level, and Dust has no
redstone to give it one; `scaffolding` takes a `distance` that propagates up to
seven blocks, which is the cascade this rule deliberately does not do. Both are
one item each and both are named in the README.

Of the scenes' 29 and the 29 neighbour changes:

```text
    14  leaves, vines, lichen  `persistent` and multiface, not a connection
    11  hanging signs          the item places one of two blocks
     3  mushroom blocks        six faces, and the rule is the inverse of a
                               fence's: a face is drawn unless the neighbour is
                               the same block. Shape alone cannot tell a
                               mushroom block from a chorus plant, which has
                               the same six properties and a different rule.
     1  rail                   the bend
```

And two rows of the neighbour half that are not shape rules at all: a big
dripleaf tilts to `unstable` when something lands beside it, and a calibrated
sculk sensor goes to `power=15` because it **heard** the click. Both are other
systems, and the scorer counts them where they are so that they cannot be
mistaken for this one.

## Options considered

**Recompute the whole connected component on every change.** ❌ A wall's post
depends on the wall above it, so a strictly correct rule is a flood fill. It
would be right and it would turn one block placement into an unbounded number
of block reads on a server holding thousands of chunks. The one case it buys is
a stack of walls three high whose top changed, which is one tick behind and
self-corrects the next time anything near it moves.

**Ship a table of connections.** ❌ Same argument D11 makes and more so: the
situation space is six neighbours against 26,684 states.

**Placement only, and a second pull request for the update half.** ❌ Named in
the task as an option and refused, for the reason in the decision above. There
is no version of this that leaves a player with a fence connected in one
direction.

## Related

* D8 — the oracle route. `isFaceSturdy` is the fourth value to come down it.
* D11 — the click rules and the survey these two extend.
