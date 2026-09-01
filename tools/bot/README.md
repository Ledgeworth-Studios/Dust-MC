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

   The bot writes `block_place` by hand rather than through `bot.placeBlock`,
   which wants a held item mineflayer can only get from an inventory this
   server does not keep.
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
11. **A block is not placed into one that is already there.** The bot clicks
    the *down* face of a buried block, whose far side is more ground, and the
    first bot reads that cell before and after. It must not change and no sound
    must arrive. This used to replace it, silently, for every solid cell in the
    world — a player could hollow a wall out from the outside without breaking
    anything. Watched to fail: with the rule taken out the check reports
    `dirt -> wheat, and a sound was heard`.
12. **A player cannot break or place fifty blocks away.** Both verbs, because
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

### The control, and why there is one

Every run measures `minecraft:stone` first, whatever else it was asked for.
Stone has one state, so every situation has to give the same answer, and a run
where they do not stops before printing anything else. It is not a test of the
server; it is a test of *this tool*, and it has caught every one of the faults
below before anything downstream could be believed.

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

### What it does not measure

The arena is one stone block in a cleared volume, so nothing here varies a
block's **surroundings**. A stair's `shape` comes from the stairs beside it, a
chest becomes half of a double chest next to another, a fence connects to what
it touches, and redstone wire reads all four neighbours. A block this tool calls
context-free is one whose *placement* reads nothing — it may still owe a
neighbour rule, which is a different problem worth measuring separately rather
than folding in and losing.

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
