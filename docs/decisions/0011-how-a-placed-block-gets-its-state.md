# D11 — How a placed block gets its state

**Status:** Decided, 2026-09-01. Rules in Dust, checked against answers asked of
the operator's own jar. **Not** a shipped table, and the measurement below is
why.

## Context

A block goes down on Dust in its **default state**. A stair faces north
whichever way the player was standing, a log always lies on its end, a slab is
always the bottom half. Placing any block at all is new — decision record 0008's
item table is a week old — and this is the thing that is most obviously still
wrong about it.

Minecraft computes the state per block, in Java, in
`Block.getStateForPlacement(BlockPlaceContext)`. It is the same *kind* of value
as the light constants and the sound groups, and it is the first one this
project's oracle **cannot reach**: the method needs a `Level`, and `Level` is an
abstract class rather than an interface, so there is nothing to construct and
nothing to proxy.

## What was measured, and how far it can be trusted

`tools/bot/placement.js` asks a running vanilla server instead, one placement at
a time, over a grid of clicked face, cursor height, player yaw and player pitch.
Its own account of what it took to make it trustworthy is in
`tools/bot/README.md` and is worth reading before any number here is quoted:
five separate faults, every one of them caught by the control — `minecraft:stone`
has one state, so a run where its situations disagree stops before printing
anything else.

**All 925 items that place a block, over eight situations each.** 7,400
placements; five arena hiccups, reported rather than hidden.

```text
                                                     of 925
  places its default state in every situation           375
  places something else                                 481
      because the state depends on how it was placed    455
      because the default is simply not what it places   26
  never placed at all in this arena                      69
```

**So Dust is wrong about 481 of the 925 blocks a player can put down** — a bit
over half — and right about 375.

The 69 are unmeasured rather than correct: the arena's only support is stone, so
every sapling, flower, crop, mushroom, cactus and lily pad refuses, and five more
are the command blocks, the jigsaw and the structure block, which need an
operator.

### Scored, and scored twice

`cargo xtask harness placement --answers <file>` is the verb that reads those
answers, asks Dust the same questions and counts. On the same survey:

```text
  6,323  situations where Minecraft placed a block
  4,530  of them Dust would place the same state (71.6%)
  1,793  of them it would not (28.4%)
  1,076  Minecraft refused, so there is nothing to compare

  481 of the 856 items that placed anything come out wrong in at least
  one situation
```

**481 both times, from two readings that share no code.** They did not agree at
first, and what separated them was a fault in the measuring tool rather than in
either server: a state is decoded out of its id by dividing through each
property's value count, which gives an *index*, and for an `int` property whose
values do not start at zero the index is not the value. `snow[layers]` runs
1..8. The tool printed `snow[layers=0]`, a state Minecraft does not have, and
then disagreed with a server placing the one it does — nineteen blocks, every
candle among them.

The control could not have caught it. `minecraft:stone` has no properties, so a
decoder wrong about every property in the game still agrees with itself over
stone. **A control's blind spot is a list of the defects it will let through**,
and what caught this one was two readings of the same file disagreeing.

## The 455, by what they read

```text
   169  the clicked face          logs, torches, ladders, buttons, end rods,
                                  hoppers, hanging signs, vines, tripwire hooks
    92  the player's yaw          furnaces, chests, doors, repeaters,
                                  glazed terracotta, banners
    76  face, cursor and yaw      stairs, trapdoors
    60  face and cursor           slabs
    50  face and yaw              levers, signs
     8  yaw and pitch             pistons, observers, dispensers, droppers
```

Six behaviours, and every one of the 455 falls into one of them. That is the
whole argument for the decision below.

## The 26, which are the reason this record has a number in it at all

The survey's question was "does the placed state depend on the four numbers a
right-click carries". Twenty-six blocks answer *no* and are still not placed in
their default state, and a reading that stopped at the first question would have
certified all of them as already correct:

- **Ten kinds of leaves** go down with `persistent=true` where the default is
  `false`. A player building with leaves on Dust watches them decay.
- **Thirteen water-dwellers** — the corals, the dead corals, `sea_pickle`,
  `conduit` — default to `waterlogged=true` and are placed dry in a dry arena.
  That is a **fifth input**, the fluid already in the cell, and it is invisible
  here because nothing in the arena is wet. It reaches far past these thirteen:
  every stair, slab, fence and sign waterlogs too.
- **`scaffolding`** takes `distance` from its neighbours, **`redstone_wire`**
  takes four connection properties from its, and **`weeping_vines`** takes a
  random `age`.

## What is still not measured

The arena is one stone block in a cleared volume, so nothing varies a block's
**surroundings**. A stair's `shape` comes from the stairs beside it, a chest
becomes half of a double chest next to another, a fence connects to what it
touches. A block this survey calls context-free is one whose *placement* reads
nothing; it may still owe a neighbour rule. `scaffolding` and `redstone_wire`
above are the two that leak through anyway, which is a hint about the size of
what is behind them.

## Options considered

**1. Ship a table, the way D8 ships the light and item tables.** ❌

The situation space is the four inputs and the fluid: six faces, four horizontal
directions, six nearest-looking directions, two cursor halves, wet or dry. Call
it 576 situations against 925 items — half a million rows, twenty megabytes an
operator has to fetch and copy, to encode six rules.

It is also the wrong shape for what was measured. A table is what you ship when
the data is irreducible, which is exactly what the light constants are: 26,684
numbers with no rule behind them. Here there *is* a rule behind it, and the
measurement found it — six of them, covering all 455.

**2. Rules in Dust, and no check.** ❌

The trap this project has already been caught by twice. A rule keyed on property
*names* is right most of the time and wrong in a way nobody tests: `facing` means
"opposite the player's horizontal look" on a stair, "the clicked face" on a
torch, and "the nearest looking direction including the vertical" on an observer.
The item table's own record makes the same argument about a rule that is right
about 909 items and wrong about sixteen.

**3. Rules in Dust, checked against answers asked of the operator's jar.** ✅

The rules are the data path and no operator has to copy anything for a block to
face the right way. The answers are the *check*: a file in the harness cache,
produced by `placement.js` against a vanilla server, that says for every item and
situation what Minecraft did — and a verb that reports how many of them Dust
reproduces.

That is the shape `cargo xtask harness light` already has, and it earns the same
thing: a number that goes down when the rules improve and a list of what is still
wrong, rather than a gate that is red for a known gap.

## Decision

Option 3.

**What is committed is the rules and the question. What is not committed is a
single one of Minecraft's answers** — same rule as D6, D7 and D8, for the same
reason, and this time the answers are far too large to want here anyway.

## What follows from it

- **The order the rules go in is the order the counts give.** The face is 169
  blocks and the yaw is 92; stairs and slabs together are 136 and need the cursor
  as well. Waterlogging is not in the 455 at all and is behind every one of them.
- **`net::session::held_block` is where the default state is chosen today**, and
  it says so. It is the function that grows.
- **A rule set is worth exactly what its check says it is worth.** Until
  `harness placement` exists and reports a number, "Dust places stairs correctly"
  is a claim and not a measurement.

## Related

* D8 — the oracle route, and the four tables that come down it. This is the
  first value that route cannot carry.
* D10 — a thing measured and deliberately not built, which is the shape this
  record would have had if the six behaviours had turned out to be sixty.
