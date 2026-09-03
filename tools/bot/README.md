# The third-party client check

`mineflayer` implements the Minecraft client protocol independently and shares
no code with this project. It is the strongest check available short of a real
Minecraft client, and **it should be the first thing run after any protocol
change**.

Its record here is not theoretical. The first time it was pointed at Dust it
could not log in: `dust-net` wrote Login Success by hand and had left off
`strict_error_handling`, the trailing bool 1.20.5 added — one byte, no visible
truncation, and every test in that crate read the packet the way that crate
wrote it. Since then it has found a missing `set_health` (and the fact that
*where* that packet sits in the join burst is load-bearing), and it is what a
`player_command` decoder one VarInt short was caught failing.

## The movement recorder

`movement.js` is the same idea pointed at a different question. Rather than
checking what the server said, it records what the *client* sent: it hooks
mineflayer's own packet writer and counts the displacement in every position
packet, while the bot walks, sprints, sprint-jumps, flies, falls three hundred
blocks and walks through a simulated 700 ms network stall.

```
node movement.js 25581           # print the distribution
node movement.js 25581 --check   # and assert the server corrects a liar
```

It exists because `[server] movement_speed_limit` is a number somebody has to
choose, and a number chosen without this one is a number that rubber-bands
players on bad connections. Decision record 0017 is its output. The `--check`
run does both halves: a bot that claims to be 707 blocks away has to be put
back, and a bot that takes an ordinary 0.3-block step has to be left alone.

**The stall row is the one worth reading.** A connection that stops for 700 ms
and then delivers everything it queued produces exactly the same displacements
as an unstalled walk — the packets bunch, the steps do not. A validator that
charges a movement budget by the clock refuses an honest client for arriving
early, and this is where that would show up.

## The click differential

`clicks.js` is a third idea again: it does not check the server and it does not
watch the client. It **records**, so that the same recording can be taken from
Minecraft's own server and the two diffed.

```
node clicks.js 25603 --out dust.json          # a running Dust server
node clicks.js 25703 --out vanilla.json       # a real 1.21.1 server, same script
node clicks.js --compare vanilla.json dust.json
node clicks.js 25603 --predict                # the one thing the diff cannot see
```

A hundred clicks over a seeded container, one snapshot of every slot per click.
The comparison is the measurement; a recording on its own is not a result, which
is why nothing in the first two commands asserts anything.

**It is the reason decision record 0016 exists.** The first version of the
script reached only the ordinary inventory and reported 58 of 58 — a number
about the fifty-eight situations it reached, not about the container. Twenty-five
more clicks over the armour slots, the offhand and the crafting grid took it to
60 of 83, and eighteen more seeded with a wearable that stacks to 64 took the
repaired 83 of 83 back to 84 of 101. It is now 101 of 101.

Three things had to be learned to make the recording honest, and each is a way
of getting a confident wrong answer:

- **Claim nothing.** `window_click` carries the client's own prediction and a
  server only corrects what the prediction got wrong, so every click here claims
  "nothing changed and my hand is empty". That makes both servers tell us
  everything they changed, which is what makes two recordings comparable.
- **Read the raw packets.** mineflayer drops `set_slot` for window -1 — its
  handler resolves a window by id, finds none, and returns — so the cursor is
  invisible through `bot.inventory` and a drag read that way looks like it moved
  nothing. (Decision record 0013 hit the same wall from the other side with
  window -2.) The window id also arrives as 255 rather than -1, because
  minecraft-data types that field unsigned on 1.21.1.
- **`--predict` exists because the diff cannot see everything.** Since every
  click claims nothing changed, a click the server *refuses* is one where both
  ends already agree and no packet is needed — two servers that both send
  nothing are two recordings that agree, at 101 of 101, while a real client's
  prediction stands uncorrected and the player sees a block on their head.
  `--predict` sends that prediction and requires the contradiction.

## Running it

```
cd tools/bot
npm install          # once; see the note on dependencies below
node check.js 25565  # the port your dust server is bound to
```

It exits `0` if every check passed and `1` with a named failure otherwise, so
it works as a gate as well as a thing to watch.

Point it at a server started from a `dust.toml` with `online_mode = false` —
`mineflayer` here is not logging in to Mojang — and with `[data] path` set, so
that a client acknowledging no data packs can be served at all. Without that
path the server refuses the connection and says so, which the check reports as
a failure rather than as a crash.

**Put both of the oracle's tables in that directory**, or the last five checks
fail for a reason that is not a defect: without `dust-constants.tsv` a placed
block makes no sound, and without `dust-items.tsv` every placement is the
world's own surface block. `cargo xtask extract --only constants` prints the two
`cp` lines.

**Serve a real world from a release build.** Every check that involves a
*second* bot has a deadline, and a debug build streaming a real Minecraft world
misses them: pointed at seed 1's ocean spawn, `cargo build`'s binary fails five
of the nine and `cargo build --release`'s passes all nine, on the same commit
and the same world. That was measured on this checkout and again on `main`
before it, which is the only reason it is written down as a build fact rather
than chased as a regression. Against the flat world a debug build passes nine
of nine, so the deadline is the chunk streaming and not the protocol.

A failing check is either a defect or a deadline, and the two look identical
from here. If the failures are the second-bot ones and nothing else, rebuild
with `--release` before believing them.

**Do not check light through `bot.world.getSkyLight` or `getBlockLight`.**
They do not report what the server sent. Chased down on 2026-08-31: on seed 1's
spawn column the bot read sky light 0 for four cells of open air above the
sand, and the same four cells read **15** in Dust's own arrays, in the bytes of
the `map_chunk` packet the bot itself received, *and* in the light Minecraft
computed into its own region file. `cargo xtask harness light --at 7,11` agrees
with vanilla cell for cell there.

So the bytes are right on both sides of the wire and `prismarine-chunk`'s
nibble accessor is reading them back permuted — its `BitArray` is a
`Uint32Array` built for long-packed containers, and a light array is a flat
byte sequence. Dumping `p.skyLight[9]` out of the raw packet and reading it by
hand is what settled it, and is the way to check light from here if you need to.

That is worth the paragraph because the failure is silent and convincing: it
produces a plausible-looking dark band at a chunk edge, which is exactly what a
real lighting bug looks like. **An independent client is only independent where
it is right.**

## What it checks

1. It **joins** — through login, configuration and into the world.
2. The **dimension it was told about is the one it is in**, with the height and
   floor Dust sent in the registry contents rather than defaults of its own.
3. It **has the biome registry**, all sixty-four.
4. It can **read a block** under its feet, which means the chunk packet decoded
   and the palette resolved.
5. A **second bot's chat line** reaches the first, *with the sender's name on
   it* — the name is the server's to add and not the client's to send, and a
   server that relayed the raw line would let anybody speak as anybody.
6. A **second bot's arm swing** reaches the first.
7. A **second bot's crouch** reaches the first, as both the entity flag and the
   pose — the two are separate and a client told only one renders a player
   half-doing it.
8. A **second bot breaking a block** reaches the first as a `world_event`, with
   the *broken* block's state in it rather than the air left behind — the
   client makes the particles and the sound out of that, and the air's id gives
   a silent puff of nothing.
9. A **second bot placing a block** reaches the first as a `sound_effect`. A
   placement has no particles and no level event, so the sound is the only
   packet and it has to name the sound itself. Three things are checked about
   it and each fails differently:
   - it **arrives at all**, which needs a `dust-constants.tsv` beside
     `[data] path` — without one a server places blocks in silence and this
     check says so rather than reporting a protocol fault;
   - it is the **sound that block makes**, resolved through minecraft-data's own
     table rather than Dust's. The block is whatever check 10 put in the bot's
     hand — cobblestone, which sounds like `block.stone.place` — so this check
     and the one below it fail together if the held item never arrives, and
     that is deliberate: a sound that is right about a block the server placed
     by accident is not a sound that is right;
   - it plays **where the block went**, which is the check that matters most and
     the one nothing on either side of the wire can do. That packet's position
     is in *eighths of a block* and its field is called `x`. A server that wrote
     the block coordinate would put the sound an eighth of the way to the
     origin: legal, decodable, and audible only as "that sounded far away".

   The bot writes `block_place` by hand rather than through `bot.placeBlock`.
   That was once because the server kept no inventory for mineflayer to take a
   held item from; it now does, and the packets stay hand-written for the
   reason everything else here is — they come from a library that has never
   seen Dust's encoder.
10. **What the second bot was holding is what went into the world.** It writes
    `set_creative_slot` and `held_item_slot` — the two packets a creative client
    uses, and the only inventory writes this server understands — and the first
    bot reads back the block that landed. Two items, for two different failures:
    - **cobblestone**, the ordinary case, which fails if the held item never
      reaches the server or is looked up in the wrong table;
    - **wheat seeds**, which place `minecraft:wheat`. That row is the one that
      says the server read a table rather than matching item names against
      block names — a rule that is right about nine hundred items and wrong
      about sixteen, and `minecraft:wheat` the *item* places nothing at all.

    Each cell is broken before it is placed into, because this server keeps its
    edits across a restart and a placement into a cell that already holds that
    block is correctly silent.
15. **A block is not placed into one that is already there.** The bot clicks
    the *down* face of a buried block, whose far side is more ground, and the
    first bot reads that cell before and after. It must not change and no sound
    must arrive. This used to replace it, silently, for every solid cell in the
    world — a player could hollow a wall out from the outside without breaking
    anything. Watched to fail: with the rule taken out the check reports
    `dirt -> wheat, and a sound was heard`.
12. **A stair faces the way the player was standing**, and is the bottom half
    because the top of a block was clicked. The bot looks **west** on purpose:
    a stair's default state faces north, so a check that expected north would
    pass against a server that had never read the click at all.
13. **The hole the break check dug is filled back in**, which is what makes a
    run leave the world as it found it — see the note below.
14. **A player cannot break or place fifty blocks away.** Both verbs, because
    they are two packets down two paths and a check covering one would pass
    while the other stayed open. The block is read from the first bot before
    and after, so what is checked is what reached the world rather than what
    either client believes. Watched to fail: with
    `interaction_range = 5000.0` in the server's `dust.toml` it reports
    `grass_block -> wheat`.

## Asking Minecraft what it places

```
# start vanilla with its console on a pipe
mkfifo /tmp/mc-console
( tail -f /tmp/mc-console | java -jar server.jar nogui > /tmp/mc.log 2>&1 & )

# then, from here
DUST_SERVER_CONSOLE=/tmp/mc-console node placement.js 25565 > answers.tsv
DUST_SERVER_CONSOLE=/tmp/mc-console node placement.js 25565 items.txt --survey
```

`placement.js` asks a **vanilla** server what state it puts down for a given
item, clicked face, cursor position and player look — one placement at a time,
reading the answer out of the block-change packets the server pushes back.

It exists because that answer is out of reach any other way.
`Block.getStateForPlacement` needs a `Level`, `Level` is an abstract class
rather than an interface, and so the reflection the block oracle uses — which
got the light, sound, replaceable and item-to-block tables out of the same jar —
cannot construct one. A running server can be asked instead.

What it writes is Minecraft's own answers, so it belongs on the operator's disk
under the same rule as everything else the extractor produces, and no row of it
is committed here. Point it at **vanilla and only vanilla**: comparing Dust
against the answers is a different job, and a measurement that also has an
opinion is not a measurement.

`--survey` trades the full 144-situation grid for eight, chosen to answer only
"does this block's placement read anything at all?" — the question worth asking
of every placeable item where the full grid is worth asking of a handful.

`--neighbours` varies what is **beside** the target instead of the click, and
`--into` varies what is **in** it. Those are the three variables a placement
has and each survey holds the other two still.

```
DUST_SERVER_CONSOLE=/tmp/mc-console node placement.js 25565 oak_fence,snow,oak_slab --into
DUST_SERVER_CONSOLE=/tmp/mc-console node placement.js 25565 oak_fence --into all
```

Two things about `--into` that neither of the others has to deal with:

- **The target is walled on four sides with stone.** An unwalled water source
  spreads across the arena within two ticks and every sample after it is
  measured in a puddle. The cell above is left open so a block taller than one
  is not refused for a reason that is not the question.
- **A refusal does not leave air.** Everywhere else the target is empty, so a
  refused placement leaves air and `minecraft:air` is the whole test. Here it
  leaves whatever was already there, which is a state and reads exactly like a
  successful placement of it. The test is "is the cell what it was a tick ago",
  and a slab put into a double slab and a ninth layer of snow are precisely
  that.

What comes out is scored by `cargo xtask harness placement --answers <file>`,
which asks Dust the same questions and counts. That verb is the whole point of
this one: decision record 0011 chose rules in Dust over a table on the
operator's disk, and rules are worth exactly what their check says they are.

### The control, and what it cannot catch

Every run measures `minecraft:stone` first, whatever else it was asked for.
Stone has one state, so every situation has to give the same answer, and a run
where they do not stops before printing anything else. It is not a test of the
server; it is a test of *this tool*, and it has caught every one of the faults
below before anything downstream could be believed.

**Stone has no properties, and that is the shape of what the control misses.** A
state is decoded out of its id by dividing through each property's value count,
and an `int` property's values need not start at zero — `snow[layers]` runs 1..8,
`candle[candles]` 1..4, a leaf's `distance` 1..7. Printing the *index* instead of
the value reported `snow[layers=0]`, which is not a state Minecraft has, and a
run then disagreed with a server that was right about all nineteen of them. The
control could not see it and never will. What did see it was the score
disagreeing with a second, independent reading of the same file — which is the
argument for having two.

### Six things that cost time

Each is a comment in the file too, so the next person does not pay again.

- **Read both block-change packets.** A server sends `block_change` when exactly
  one block in a section changed in a tick and `multi_block_change` when more
  than one did. So the arena's own `/fill` arrives the second way — and so does
  a **door**, which puts down two blocks. Listening to one of them makes every
  door read as refused and every arena read as never settling.
- **Read the packet, never `bot.blockAt`.** The bot's own world lags a placement
  by an unbounded amount; read that way the tool reported the *previous*
  sample's block, convincingly.
- **The first change is the placement; the last one may not be.** A door with
  nothing under it is put down and then breaks. Reading the last change recorded
  `air` for forty-seven of a door's situations. Whether the block survived is a
  support rule and not a placement rule, so it is a column of its own.
- **A refusal arrives as air.** The client predicted a block and the server
  answers by telling it what is really there. No item places air, so there is
  nothing to confuse it with.
- **Forget the arena before changing it, not after.** The barrier is "wait until
  the support turns to stone", and console changes arrive in the order the
  console ran them, so seeing that means the fill before it has landed too.
  Waiting *without* forgetting first matches the support's stone from the
  previous sample and returns immediately, which put `air` in twenty-two rows of
  a run — every one a down-face placement, because that is the cell the fill
  happened to reach last.
- **Turn as rarely as possible, and set the look with `bot.look()`.**
  mineflayer's physics loop sends a position every tick and overwrites a
  hand-written `position_look`, so a hand-written one makes every stair face the
  same way whatever was asked for. And a look reaches the server on the next
  physics tick, so a placement sent too soon after one is measured against the
  *previous* sample's rotation: the first version of this tool turned before
  every sample and produced exactly one poisoned row in 2,448 — a piston facing
  west, which is where the sample before it had been looking. The rotation is
  now the outer loop and the face the inner one.

The arena is built from the server console rather than by the bot, which is why
the pipe is needed: the bot is not opped and does not need to be.

**One run at a time per server.** Both runs share a username, so a second one
kicks the first with `multiplayer.disconnect.duplicate_login` — which is the
right outcome and not a bug to fix: two bots on one server would share one arena
and quietly corrupt each other's samples, where a kick is loud and immediate.

### The question to ask of the answers

"Does the placed state depend on the four numbers a right-click carries" is the
question `--survey` answers, and it is **not** the question that matters. The
one that matters is "is the placed state the block's *default* state", because
the default is what Dust puts down.

They are different, and twenty-six blocks live in the gap: ten kinds of leaves
go down with `persistent=true` where the default is `false`, thirteen corals and
conduits and sea pickles default to `waterlogged=true` and are placed dry,
`scaffolding` and `redstone_wire` read their neighbours and `weeping_vines` rolls
a random age. Every one of them reads none of the four numbers, so the first
question calls them fixed — and every one of them is a block Dust currently gets
wrong. Decision record 0011 has the counts.

### Varying the surroundings instead of the click

```
DUST_SERVER_CONSOLE=/tmp/mc-console node placement.js 25565 items.txt --neighbours
DUST_SERVER_CONSOLE=/tmp/mc-console node placement.js 25565 oak_fence --neighbours --against all
```

The grid above is one stone block in a cleared volume, so it cannot see a rule
that reads the cell **next door** — and sixty-one of the hundred and sixty items
it reported wrong were exactly that. `--neighbours` holds the click still and
varies the surroundings: nothing at all, a full block, a full block that does
not occlude, a block with no full side, something above, the straight run that
decides a wall's post, then the block's own kind on one side, two sides and all
four, and where it has a four-valued `facing`, five more with the neighbour
turned across it and one of those in the other half. Eleven scenes, or sixteen
for the blocks that face.

`--against all` replaces the scenes with one per block the build knows, put to
the north of the target. That is how "does a fence connect to X" is answered for
every X at once rather than argued about; a run is 1,060 rows and takes about
seven minutes.

Two columns come back that the grid's rows do not have, and **both are read off
the wire rather than copied from the commands that built the scene**. `before`
is what the six cells around the target actually held: a `/setblock` naming a
property the block does not have is refused and leaves air, and a scene written
down as what was *asked for* would score a rule against a neighbourhood that was
never there. `after` is which of those six the placement changed, which is the
half of a neighbour rule a survey of placed states alone cannot see — a fence
has to connect when the block beside it arrives later, not only when it was
there first.

Three things cost time here and every one of them looked like a wrong rule:

* **A neighbour with nothing to stand on falls the tick after it is set.** The
  floor was one block wide, so a rail set beside the target emptied itself
  before the click. The fill now lays stone under all four side cells.
* **A barrier that is not last is not a barrier.** The grid loop sets the
  support first because nothing follows it; here the scene does, so the support
  goes down last.
* **`before` has to be true at the moment of the click.** A ladder goes wherever
  `/setblock` puts it and falls on the next tick. One row in 799 read the ladder
  in `before` and air in `after`; the wait before the read is why there are not
  more, and the scorer reads a side that `after` says was emptied as the air the
  click actually saw.

The control is the same one and asks the same question of a different variable:
`minecraft:stone` has one state, so no arrangement of neighbours may change it.
It agreed with itself over 1,060 neighbourhoods.

### What neither survey measures

Neither varies the **fluid** already in the cell, so the twenty-two blocks that
default to `waterlogged=true` are still unmeasured, and so is every stair, slab,
fence and sign that would waterlog. Nothing here goes more than one cell out
either, so a rail's rise to a rail a block higher and a scaffolding's distance
to the ground are both out of reach of these scenes.

## The long one

```
node soak.js 25565 10
```

`check.js` answers "does this work". `soak.js` answers "does it keep working",
which is a different question and the one Phase 3's exit criterion asks: a bot
that stays for ten minutes, walks a square, digs at every corner and talks,
while nothing ends and nothing goes quiet.

What it watches for is **ending and stopping**, not a wrong value in one packet:
a keep-alive that stops being answered, a connection dropped, thirty seconds of
silence. Those are the failures that only appear after a while, and they are the
reason a soak exists beside a check.

It reports what it saw either way — packets, columns streamed, columns
forgotten, keep-alives answered — because "it survived" with no numbers beside
it is indistinguishable from "it sat there".

## Why the dependency is not in the licence gate

`cargo xtask licenses` audits what a Dust *build* incorporates. This is a
development tool that is npm-installed by whoever runs it, ships in no
artefact, and is linked into nothing. Nothing here is vendored: `package.json`
names a version range and `node_modules/` is ignored.

## Why the run puts the world back

This server keeps its edits across a restart *and* remembers where each player
left, and both of those turn a repeated check into a drifting one.

The break check used to dig the block under the actor's own feet. The actor
dropped a block, that position was saved, and the next run started a block
lower. After enough runs against one world the actor is standing on bedrock,
and the checks that read the terrain around it are reading somewhere else
entirely — one of them had quietly become vacuous and still said `ok`.

So the run now digs *beside* the actor and fills the hole back in, and the
checks that need a particular arrangement of blocks **build it** rather than
look for it. Run it three times in a row against one world and it says 21/21
three times; that is the property being aimed for, and it is worth more than it
sounds, because a check that decays over runs decays into a green tick.

## The inventory checks, and what the third-party client found

A third bot, `Carrier`, clears all forty-five writable slots, fills three of
them, clicks a stack from one slot to another, leaves, and comes back under the
same name. Six checks:

- **a stack larger than that item allows is refused** — sixty-four water
  buckets, which stack to one. An *empty* bucket stacks to sixteen, which is
  why the number has to come from the item table and not from a constant.
- **a click moves a stack to the slot it was dropped in**, and out of the one it
  came from. The bot writes `window_click` with an **empty** changed-slot list,
  so it predicts nothing and the server's push-back is the only thing that can
  put the right answer in this bot's model.
- **what a player was carrying is still there after a relog**, with the count,
  plus their armour, their offhand, the hotbar slot they had in hand, and a slot
  they emptied still empty. The count is `2 + (Date.now() % 40)` rather than a
  fixed number, so a server that ignored every write above cannot pass on what
  the last run left in the world.

Watched to fail. With `record_inventory` made a no-op the run reports 25 of 29,
and the four that go red are exactly the relog ones.

**What it found on the way: mineflayer ignores `set_slot` on window `-2`.** The
protocol gives that packet a signed window id so `-2` can mean "the player's own
inventory, ignore the state id", Mojang's client honours it, and it reads like
the right id for a correction. mineflayer's handler resolves a window by id,
finds none, and returns — no error and no log line, on either side. Dust now
corrects on window `0`, which is what vanilla's own synchronizer sends for a
player's own menu and which both clients honour. See decision record 0013.

The cursor is still sent on window `-1`, because there is no second spelling of
the cursor to prefer. mineflayer ignores that too and keeps its own, which is
why these checks read slots and never the cursor.

**A `const` inside `main` shadows a module-level one for the whole function
body.** The first version of these checks called its slot-describing helper
`named`, `main` already declares `const named` for sound events, and every
detail string came out as the literal `undefined` while the checks themselves
passed. The helper is called `carrying` now. A detail string that lies is worse
than none: it is what somebody reads when a check fails.
