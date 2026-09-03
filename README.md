# Dust

A Minecraft Java Edition server, written in Rust.

Dust is being built from nothing and is not finished — but you can play on it.

## Status

**Two people can connect, walk around a shared world, break and place blocks,
see each other doing it, and talk.** They place what they are holding — any of
the nine hundred and twenty-five blocks Minecraft has an item for, picked out of
the creative menu — where Minecraft would put it: into the tall grass they aimed
at, and not into the wall behind the face they clicked. **What goes down is the
state Minecraft would put there for 89.5% of the ways a block can be placed**,
measured against a real server rather than claimed: a stair faces the way you
stood, a furnace faces back at you, a lever takes the wall it is on, a piston
points where you looked and an observer looks back. They see each other
swing, crouch and break blocks,
particles and sound out of the block that broke, and hear each other put blocks
down, each block with its own sound. What they change is still there after a
restart, along with where they were standing. The world is lit the way Minecraft
lights it: sky light and block light both, from Minecraft's own numbers, read
out of the operator's own jar.

`dust server` binds `[server].bind`, answers the server-list ping with the MOTD,
player count and favicon from `dust.toml`, runs login in either offline or
online mode, syncs the eleven datapack registries a 1.21.1 client needs, streams
chunks as players move out to `[server].view_distance`, and keeps the connection
up. That distance is a ceiling: the client asks for one of its own during
configuration and is served the smaller of the two.

The loading screen ends once the near square has arrived rather than once the
whole view has, and the rest of the view is sent by the play loop a batch at a
time instead of in one burst. Measured A/B on one binary at the default
distance, timing the first keep-alive *after* the screen ends:

```text
           screen ends   first keep-alive after it   all 289 columns
  burst        648 ms       1,733 ms (1.1 s later)        1,731 ms
  batched      411 ms          428 ms (17 ms later)       1,768 ms
```

The same work, in the same order, finishing at the same moment — with the
session answering throughout instead of at the end. A player who joins and walks
immediately used to have their movement packets sit in the socket for a second.

A player who changes their render distance mid-game is served the new one, which
the pacing made cheap enough to bother with: the view forgets or sends the
difference on its next move, out of the same computation it already does.

**What a player waits for no longer depends on the view distance**, which is why
the default is Minecraft's own ten: 404/421 ms at a distance of 8, 396/415 at
10, 376/394 at 12. What the number still costs is the streaming behind them and
the memory of holding it — a bill paid while playing rather than while waiting.

**Tags go out, all thirteen registries of them** — 514 tags flattened to
6,362 registry ids, which is exactly what a real 1.21.1 server sends, compared
tag by tag and id by id against one. Nothing went out while five of the
thirteen were extracted, because a partial tag set is worse than none: a client
told `minecraft:mineable/pickaxe` holds eleven blocks believes the other nine
hundred are not mineable, where a client told nothing falls back to its own
copy.

**`mineflayer` joins it.** That matters more than it sounds: a client that does
not track data packs has no copy of the registry contents to fall back on, and
until now Dust had none to send it, so most of the bot and proxy ecosystem was
refused at configuration. Point `[data].path` at a copy of Minecraft's data —
the one the operator already has, since none of it is shipped here — and Dust
sends the two registries such a client cannot manage without. See decision
record [0007](docs/decisions/0007-registry-contents.md) for where the line
between a protocol fact and Mojang's content falls, and why it falls there.

Ten of the eleven synced registries go that way. The eleventh,
`minecraft:enchantment`, is **declined out loud**: a client sent nothing for a
registry falls back to its own correct copy, and one sent an enchantment
without its effects believes Protection does nothing. Decision record
[0009](docs/decisions/0009-enchantment-registry.md) measures what serving it
would take — 470 key paths, eleven levels deep — and
`cargo xtask harness registries --dump minecraft:enchantment` is the command
that measured it, against any registry and any version.

**It can serve a world Minecraft made, and hand one back.** Point
`[server].world_source` at a region directory and Dust reads the columns out of
it — blocks, their properties, biomes, heightmaps — and streams them. It also
writes Anvil: `cargo xtask harness rewrite` puts every chunk of a real world
through Dust's reader and writer and then boots a vanilla server on the result,
which reads back the world it started as and says nothing about it that it did
not say about its own. Without a world source Dust generates a superflat and does
not pretend otherwise: worldgen is Phase 6, and a column a real world does not
contain falls back to the flat one, because a world is a disc in an infinite
plane and a player can walk off the edge of it. How far that superflat is from
the world Minecraft generates for the same seed is measured rather than
estimated — `cargo xtask harness worldgen` scores it in five parts, and
decision record
[0012](docs/decisions/0012-what-worldgen-is-worth-measured-first.md) is what
each stage of vanilla's pipeline is worth and the order to build them in.

What exists either way is the whole path from the socket to the block table —
framing, compression, encryption, the four connection states, the paletted
section codec, the chunk packet, the light engine.

A player only reaches as far as their arms: breaking and placing are refused
past `[server] interaction_range`, measured from the eye to the nearest point of
the block, and the check lives in `dust-guard` rather than in the session
because a rule that can only be run from inside a session can only be tested by
running one.

And a player is where they say they are, or they are put back. A movement
packet claiming a position further than `[server] movement_speed_limit` — ten
blocks a tick, which is what vanilla allows — is answered with a teleport to
the last position the server believed, not with a log line. The limit was
measured rather than chosen: `just movement` counts what a real client's
packets contain, and over 1,217 of them covering walking, sprinting,
sprint-jumping, creative flight, a 300-block free fall and a walk through a
700 ms network stall, the largest single tick was 3.58 blocks. Decision record
[0012](docs/decisions/0012-how-fast-a-player-may-say-they-moved.md) has the
table and says what it declined.

And a player cannot walk into a wall. A movement packet is checked against the
blocks the player would be standing in, not only against how far they claim to
have come: a player may not move from outside a solid block to inside one, and
a player already inside one may move anywhere, because every honest way to end
up inside a block — a block placed onto somebody standing still, a piston, a
boat — resolves by moving out of it. Which blocks count is Minecraft's own
`isCollisionShapeFullBlock`, read out of `dust-constants.tsv`, because every
proxy for it in the table is wrong in the direction that refuses honest play.
The world is asked as it is at that instant, so a block broken underneath a
player a tick ago costs them nothing, and an unloaded chunk is not solid, so a
player outrunning their own chunk loading is believed. `just collide` is the
third-party check: six cases, and the three refusals go red with the two lines
that refuse the move taken out. It costs 32 ns a packet on a flat world and 408
on a world read from region files, which at twenty packets a second is 0.08% of
one core across a hundred players. Decision record
[0015](docs/decisions/0015-what-a-movement-check-asks-the-world.md) has the
ladder and says what it declined.

And a player carries what they were carrying. All forty-six slots of the
player's own container — the twenty-seven main slots, the nine hotbar, four
armour, an offhand and the crafting grid — with counts bounded by each item's
own `max_stack_size` out of the generated component table, never by a 64
written here. A survival client's clicks are replayed over it: left, right,
shift, the number keys and F, creative clone, Q and control-Q, the three drags
and double-click-to-collect, with only the slots the client got wrong sent
back. **And it survives a relog and a restart**, written by name into the same
file beside the world that already held the block edits. `just bot` checks that
with a third-party client: set slots, leave, come back, look. Decision record
[0013](docs/decisions/0013-where-a-players-inventory-lives.md) says what the
record does and does not promise.

And a fence connects to what it touches, whichever way the wall was built. A
fence, a wall, a glass pane and a stair take their shape from the six cells
around them — when they are placed, and again whenever anything beside them is
placed or broken, because a fence that only connected in the direction it was
built would look worse than one that never connected. What a block asks of its
neighbour is whether that neighbour has a full square face on the side they
share, which is six columns off the operator's own jar and not one: the back of
a bottom stair is a full square and its front is not. Measured against a
vanilla server over 5,120 situations the old grid could not ask, an oak fence
now agrees with Minecraft about all 1,060 blocks it can stand beside, and the
sixty-one items placed wrongly for a neighbour reason are down to two. Decision
record [0014](docs/decisions/0014-what-a-block-reads-from-the-cell-next-door.md)
has the counts and says what it declined.

**Not yet**, and each of these is stated where the code for it would go: no
physics, block updates, drops or tool checks, so a player is stopped from
entering a block and never pushed out of one, and **no pose**, so the box a
movement check measures is 0.6 high rather than 1.8 and a cheat's head may pass
through a wall its feet may not; **no item entities**, so a dropped stack is destroyed rather than
thrown, and nothing can be picked up off the ground; no crafting, so the grid is
five slots that store and never combine; **no data components on a stack**, so a
renamed block or an enchanted tool is stored and given back as the plain item,
and shift-clicking a helmet moves it rather than wearing it — that last one
needs one more column of `dust-items.tsv`, and 0013 says which; no
**redstone wire** and no **scaffolding distance**, which are the last two
neighbour rules of the sixty-one decision record
[0014](docs/decisions/0014-what-a-block-reads-from-the-cell-next-door.md)
found, and no **rail bend**; a hundred and one of the eight hundred and
fifty-six items that place anything still go down in a state Minecraft would
not, which is `cargo xtask harness placement`'s number rather than this
paragraph's; a block placed
into water replaces it rather than waterlogging, player-placed leaves decay,
a sign never becomes a wall sign, and eight layers of snow are replaced where
Minecraft would refuse; light that
crosses a chunk boundary — sky light from a neighbour it would have to travel
*through*, and any light at all from a torch on the far side of one — which is
an engine gap and not a data one, and is now the *only* thing between a served
world's light and Minecraft's own: 435 sky cells and 1,163 block cells of 2.4
million inland, costed and declined in decision record
[0010](docs/decisions/0010-how-wide-the-sky-light-volume.md); no plugins;
and the running server still saves its own edits in its own format beside a
world rather than back into it — writing Anvil works, but a chunk's block
entities and scheduled ticks survive a round trip by being *copied*, not because
Dust models them, and a server that edited a chest would be writing a record it
does not understand.

## Try it

```
cargo run -p dust-server -- server
```

Then add `localhost` to a 1.21.1 client's server list. Set `online_mode = false`
in `dust.toml` first unless you want Mojang consulted, and point
`world_source` at a `region` directory if you have a world to serve.

For light, block sounds and placing what you are holding, put Minecraft's own
answers beside your data:

```
cargo xtask extract --version 1.21.1 --only constants
cp .dust-extract/oracle-1.21.1/constants.tsv <[data] path>/dust-constants.tsv
cp .dust-extract/oracle-1.21.1/items.tsv     <[data] path>/dust-items.tsv
```

How much light a block stops, how much it gives off, which of the six heightmaps
count it, whether a block placed there goes into it, what it sounds like going
down, and which block each item puts down are Java code inside Minecraft rather than data, so they are in no report, no data
pack and no copy here — the extractor asks your own server jar for them and
writes them to your own disk. Without the files, every block but air stops sky
light, the sky starts above the grass rather than through it, a placed block
makes no sound, and a right-click puts the world's own surface block on the face
it clicked whatever the player is holding and whatever is already there; the
server says so at boot. Decision record
[0008](docs/decisions/0008-block-opacity-and-light-emission.md) is why they
arrive this way rather than in the binary — including why both tables key on
*names* rather than registry ids, and why sixteen items on 1.21.1 make the
difference between a table and a name match.

The console takes `stop`, `list` and `say`, with or without a leading slash.

## How it is checked

Two ways, and the second is the one that matters.

The protocol tests **speak the wire by hand** — their own VarInts, their own
length prefixes, their own zlib — sharing no code with the server. A test client
built on Dust's own framing would agree with Dust by construction, under any
convention including a wrong one.

And the formats are **captured from a running Minecraft 1.21.1 server** rather
than read off a wiki: the configuration order, the eleven registries and their
entry counts, the NBT type of every field in a dimension type and a biome, the
offline-mode UUID derivation, and a chunk section decoded field by field until
its 18,779 bytes were consumed exactly.

Doing that found three defects in an afternoon, each with passing tests over it:

- **A player command was one VarInt short.** The jump boost reads as though it
  should be conditional — only the horse-jump actions mean anything by it — and
  the packet was modelled that way. Vanilla reads three VarInts whatever the
  action: sent two it disconnects naming the packet, sent three it carries on.
  Every sneak and every sprint a real client sends carries a zero there, so
  Dust refused all of them.

- **Login Start's shape was inverted.** The transport expected an optional
  profile id behind a presence flag — true in 1.20.2–1.20.4, wrong since
  1.20.5 — so it accepted exactly the two shapes vanilla refuses. *No real
  client could have logged in.* The protocol crate's definition had been right
  the whole time; nothing tied the two together, and now a test does.
- **The offline profile id was derived from a lowercased name.** Vanilla hashes
  the name as typed, so every offline player on Dust had a different identity
  from the one they have on every other server.
- **The status document carried two keys vanilla omits**, and both justifying
  comments had the reasoning backwards.

The lesson is the rule now: a test written from the same understanding as the
code agrees with the code, not with Minecraft.

Underneath: Stage 0's workspace, configuration system and gates; the vanilla
data extractor; and the crates the rest stands on — NBT, world storage with
paletted containers, heightmaps and a light engine, the 1.21.1 protocol codec,
the datapack loader, and the network transport.

## Vanilla data

Dust ships no Mojang data and no Mojang assets. What the repository holds is
the extractor, and the Rust that results from running it:

```
cargo xtask extract --version 1.21.1
```

That resolves the version through Mojang's manifest, downloads the server jar
to a gitignored cache, verifies its SHA-1 against the manifest **on every
run** — including when the jar is already cached — runs Minecraft's own data
generators and regenerates the tables in `dust-registry`, `dust-protocol` and
`dust-gen`. It needs a network and a JDK 21 or newer, runs by hand a few times
per Minecraft release, and is deliberately not part of `just verify` — what CI
checks is the generated code.

The work is split into domains: blocks, items, entities, fluids, tags,
recipes, loot, commands, packets, worldgen and light. A full run regenerates
everything; each domain prints what it found and how long it took.

`light` is the odd one and is odd on purpose. How much light a block state
costs to enter and how much it gives off are Java code in Minecraft — in no
report, no data pack and nothing the generators emit — so that domain runs an
**oracle**: a small Java program on the jar's own classpath that boots
Minecraft's static initialisation and reads `getLightBlock` and `lightEmission`
off every state in the block-state registry. It writes a table to
`.dust-extract/` and, unlike every other domain, **generates no Rust**: those
are Mojang's numbers and they stay on the operator's disk. Decision record
[0008](docs/decisions/0008-block-opacity-and-light-emission.md) is why, and
`harness light` is what checks the result. Two things
make re-runs cheap:

- **The generator output is cached.** The `--reports` and `--server` trees are
  kept under `.dust-extract/`, keyed by version, and reused until deleted —
  running Minecraft's generators is the slow part, and nothing about reading
  them gets faster by repeating it.
- **`--only` extracts one domain at a time**: `cargo xtask extract --version
  1.21.1 --only tags` reads the cached trees and rewrites just that domain's
  table. A misspelled domain is refused rather than quietly extracting
  everything.

A full cold run — download plus both generators plus every table — takes a few
minutes, almost all of it inside Java. A warm run against the cache takes
seconds.

## Point a third-party client at it

```
cd tools/bot && npm install
just bot 25565
```

`mineflayer` implements the client protocol independently and shares no code
with this project, which is why it finds what a test suite agrees with itself
about. `tools/bot/check.js` joins, checks that the dimension it was told about
is the one it is in, that it has all sixty-four biomes, that it can read a
block, that a second bot's chat line arrives with the sender's name on it, and
that its swing, crouch, block-break and block-place all reach the first, that a
stair goes down facing the way it stood, and that a block fifty blocks away
reaches nothing —
twenty-one checks, exit 0 or 1. `tools/bot/README.md` has the list and what it has
caught.

`just soak <port> <minutes>` is the long version, and a different question:
`check` asks whether this works, `soak` asks whether it keeps working. A bot
stays for ten minutes, flies a forty-block square, digs at every corner and
talks, and what it watches for is *ending and stopping* — a connection dropped,
a keep-alive that stopped arriving, thirty seconds of silence. Ten minutes on a
real world: no failures, 15,634 packets, 7,633 columns streamed and 7,344
forgotten across 144 legs — about five and a half thousand blocks flown.

Both are deliberately outside `just verify`: they need a server already running,
an npm install and a `[data] path`, and `verify` is CI's list in CI's order. The
short one has already earned its keep — the break check caught the dig path
firing twice, the second one breaking air and sending a puff of particles made
of nothing.

## Differential testing

Testing against vanilla is the highest-value test this project will have: run
the real server and Dust over identical inputs and let Mojang's implementation
argue with ours. The harness provisions a vanilla server,
fingerprints a world it generates, compares fingerprints — and puts Dust's own
code in the loop:

```
cargo xtask harness provision --version 1.21.1 --seed 0 --yes
cargo xtask harness capture --version 1.21.1 --seed 0 --radius 2
cargo xtask harness compare captures/a captures/b
cargo xtask harness rewrite --version 1.21.1 --seed 0 --radius 2
cargo xtask harness registries --version 1.21.1
cargo xtask harness light --version 1.21.1 --seed 0 --radius 2
cargo xtask harness worldgen --version 1.21.1 --seed 0 --radius 4
```

`provision` resolves the server jar through the same manifest-and-SHA-1 path
the extractor uses (verified on every run, including cache hits), writes a
run directory tuned for headless determinism into the harness cache, and —
only with `--yes` — accepts Minecraft's EULA on your behalf by writing
`eula.txt`. Without that flag the file is left unwritten and vanilla refuses
to boot until you have read the EULA and chosen; agreeing to a licence is an
act, and the flag keeps it visible in your shell history where it belongs.

`registries` is the same idea one layer up, over the protocol rather than the
world. It boots Minecraft, boots Dust in the same process as the command, and
points one hand-written client — its own VarInts, its own zlib, sharing no code
with either server — at both, acknowledging no data packs so that both send the
registries' *contents* rather than their names. As of 2026-08-30 it reports no
differences: ten registries agree entry for entry and field for field, and all
thirteen tag registries agree over all 6,362 ids. The eleventh registry,
`minecraft:enchantment`, is listed as a stated omission rather than a
difference — Dust has no schema for it and says so in code, and the day one is
added and is wrong, this goes red. Watched to fail: changing one field's type
from `TAG_Double` to `TAG_Float` produced four findings naming the field.

`light` puts a number on how close Dust's light is, **both kinds, reported
apart** — a single figure covering both would read as "the lighting is 99.9%
right" while hiding which half of it was. A chunk Minecraft wrote carries the
light Minecraft computed, so the same chunks can be lit again with Dust's engine
and compared cell by cell.

It measures a **ladder**: five models over the same chunks in the same run,
each row the one above it plus a single named change, and it now covers **both
kinds of light**.

```text
seed 0, radius 2                                sky short   block short
  no table at all                                  14,276         7,185
  + Minecraft's own opacity and emission              611         1,163
  + Minecraft's own heightmap predicates              435         1,163   <- a server
  + a 3x3 volume of columns                             0             0
  + the heightmaps Minecraft wrote                      0             0
```

On seed 1 the second row's sky light is already zero, over 4,816,896 cells.

Block light's percentages need reading with care and the counts do not: most of
a world has no block light, so a server that computed none still "agrees" with
99.7% of cells, and the 7,185 in that first row is *every lit cell in view*.

**Every disagreement is accounted for, in both kinds of light.** Given
Minecraft's own answers to the questions Dust asks about a block, and a volume
wide enough for light to cross, the walks reproduce the light Minecraft computed
exactly — on both seeds and at every radius. Lighting has four inputs and only
one of them is the engine; that row is what says the walks are right and
everything above it is something Dust is *told* about the world.

The fifth row is a control and agrees with the fourth. It skips the recompute
and takes the heightmaps out of the chunk as Minecraft wrote them — not a mode
a server can run in, since an edited chunk has a heightmap its file does not.
Its agreeing is the statement that Dust's recompute, given Minecraft's
predicates, *is* Minecraft's heightmap and not merely close to it.

A server with a `dust-constants.tsv` stands on the third row. What separates it
from exact is the multi-column volume, costed and declined in decision record
[0010](docs/decisions/0010-how-wide-the-sky-light-volume.md); why the numbers
come from the operator's jar rather than from here is
[0008](docs/decisions/0008-block-opacity-and-light-emission.md).

**Getting there found a defect the stand-in had been hiding for the whole of
the light engine's life.** Minecraft's numbers on their own moved seed 0 by a
hundred and seven cells. The shortfall did not shrink; it got *shallower* —
6,128 cells short by fourteen became nineteen short by thirteen. Light was
reaching under the water and arriving at half the level it should, because the
engine charged `1 + opacity` for a step where Minecraft charges
`max(1, opacity)`. Nothing could see it while the only opacity model answered
0 or 15, because the two rules agree at both ends. A wrong constant hidden by
another wrong constant.

**A 5x5 volume buys exactly nothing** over a 3x3 — which is the argument for a
finite volume confirmed rather than assumed. Light loses a level
a block and a chunk is sixteen of them, so one ring of neighbours is not an
approximation of the infinite volume; it is the infinite volume.

**The report splits the shortfall by how far each cell sits from its column's
edge**, because light from a neighbouring column enters at a face and fades
inward while opacity does not care where in a column a cell is.

```text
distance from a face    0      1      2      3      4      5      6      7
seed 0, air only     0.660  0.595  0.561  0.548  0.530  0.510  0.530  0.581
seed 0, Minecraft's  0.072  0.021  0.008  0.007  0.005  0.005  0.006  0.018
```

Flat, then falling by an order of magnitude — two different causes, each
visible as a different shape. What it cannot do is separate a cause nobody
proposed: it read "flat, therefore opacity" and was right, and was equally
right about the step cost and about a heightmap predicate that put the sky
floor above a flower. The rate and not the count is the whole measurement: a column has `60 - 8d` columns at distance `d`, sixty at the face
against four in the middle, so a raw count reads as "it is all at the edges"
for a perfectly uniform cause.

Under the stand-in the percentage was a **property of the world rather than of
the engine**, which was worth knowing before quoting it: seed 0 read 99.4% and
seed 1 read 96.4% with the same server, because 168,428 of seed 1's 169,480
shortfalls were water. The world that was worst is the one that comes out exact
first.

It is a measurement and not a gate — a verb that failed for a known gap would be
red every time it ran — and **there are no over-lit cells, at any radius, either
seed or either model.** Getting to that took three corrections and every one was
the harness rather than the engine. It lit each column against *itself* on all
four sides: 805 over-lit cells. It compared chunks vanilla had not finished
lighting: 167,000 more, and the agreement fell to 98.1% with no change to the
engine. And it took sky floors from *neighbours* vanilla had not finished: the
last thirty-two, every one within a step of a chunk edge. Separating over-lighting
from under-lighting is what made all three visible instead of letting them hide
inside a number that already looked good.

`worldgen` asks the same question of the terrain, and asks it in five parts.
Dust generates a superflat; this counts how far that is from the world
Minecraft generates for the same seed, and which stage of vanilla's pipeline
owns which part of the gap. Seven models over the same chunks in one run, each
row the one above it plus a single named change, and every figure a count of
things **wrong**:

```text
seed 0, radius 4 -- inland

  surface  surface     biome    caves    false      blocks  KiB/col
    short    block     short  missing    caves       short
    20736    20736    124416        0  2416513     2520828      2.2  the flat world Dust serves
    17467    17551    124416   172558    52673     2627298     16.2  + the world's own sea level
    17467    17551         0   172558    52673     2627298     16.6  + Minecraft's biomes
        0    13497         0   198471        0     2632197     18.9  + Minecraft's surface height
        0    13497         0        0        0     2433726     18.9  + Minecraft's carvers
        0        0         0        0        0         360     19.6  + its blocks at and below it
        0        0         0        0        0           0     19.6  + its blocks above it (control)
```

Out of 20,736 columns, 7,962,624 cells and 124,416 biome cells. The last row is
a control: it hands over every block and every biome, so a non-zero anywhere in
it is the harness's fault and not the generator's. It is exact on both seeds,
and it is checked in CI against chunks the harness builds itself — watched to
fail in both directions.

**Five scores and not one, because a percentage hides which half it is about.**
Putting the flat world's grass at sea level fixes 3,269 columns' surface height
and makes the block count **106,470 worse**; one number would have called that
a regression. The flat world scores 100% on caves — a world with no rock above
y -60 contains every cave Minecraft carved — which is why the summary prints
counts and a "false caves" column and no rate at all.

**Two seeds, because one cannot see.** Seed 1 spawns in open ocean: every one
of its 20,736 columns has water underfoot, all of them short by *exactly one*
under the sea-level rung, because `sea_level: 63` names the level water reaches
*to* and the topmost water block is at 62. Plants above the surface are 360
cells inland and zero there.

Cost is scored beside accuracy, because this code runs for every chunk a player
walks toward: a real column's blocks are 19.6 KiB against a flat one's 2.2, and
**light is 96 KiB more per column whatever the terrain** — at the default view
distance a join is 5.5 MiB of blocks once terrain is real and 27 MiB of light
either way. Decision record
[0012](docs/decisions/0012-what-worldgen-is-worth-measured-first.md) is what
the ladder was built to write: what each stage is worth, what it costs, and the
order to build them in.

`capture` boots the provisioned server headless, watches its own log for the
readiness line, force-generates the square of chunks within `--radius` chunks
of spawn with `forceload`, waits until every chunk has reached disk, flushes,
stops the server over RCON, and then reads the region files directly — anvil
layout, chunk decompression, a minimal NBT walk — to produce one digest per
chunk: a block-state multiset hash (order-independent), a biome hash, and
per-heightmap hashes. Output lands as `chunks.bin` plus a human-readable
`chunks.tsv`. `harness rcon` stands alone for talking to a running server.

`compare` diffs two capture sets and prints one row per chunk that is missing,
extra or divergent, with both digests side by side:

```
$ cargo xtask harness compare 1.21.1-seed-0-radius-2 1.21.1-seed-0-radius-2-rerun
comparing seed 0 data version 3953: 25 chunks vs 25 chunks
identical
```

Its exit codes are for scripts: **0** when identical, **1** when they differ
(a finding, not a failure), **2** when the comparison could not run at all.

`rewrite` is Phase 2's exit criterion made runnable. It copies the provisioned
world, rewrites every chunk through Dust's Anvil reader and writer, boots vanilla
on the copy, and compares what vanilla read back against the capture of the world
it started as. It found a defect on its first run that nothing in the test suite
could have: the reader had never read `Heightmaps` at all, which is invisible
in-process because the one caller that serves chunks recomputes them first.

**The digest is not the whole check.** Vanilla does not fail on a chunk it
cannot read — it logs, discards it and regenerates it from the seed, so the
server boots, the capture completes, and nothing in the digests says anything
went wrong. Measured, by scrambling 200 bytes of one chunk and leaving its
header intact: vanilla logged four errors about it and then printed
`Done (4.392s)!` and ran.

So the criterion's other words are checked separately: everything vanilla says
is kept, and the transcript of the run over Dust's world is diffed against the
transcript of the run over vanilla's own. Anything new is a finding — a diff
rather than a list of known-bad strings, because a list can only fail on what
whoever wrote it already thought of.

The two checks overlap more than expected in that experiment: the regenerated
chunk digested differently as well, because regenerating one chunk into a world
whose neighbours are already finished loses the decoration those neighbours
would have contributed. Where they do *not* overlap is the failure this writer
can actually cause. A digest covers blocks, biomes and heightmaps and nothing
else, so a carried block entity whose block Dust has since broken — a record
vanilla drops and logs about, with every block still exactly where it was —
is visible only in the transcript.

Two honesty notes. First, what is seed-stable: terrain, biomes, ore and
structure placement are stable for a fixed seed and version, and that is
exactly what the digest covers; everything clock-shaped (mob cycles, weather,
container loot) is excluded by construction rather than filtered afterwards.
Second, where things live: nothing Mojang ships and nothing vanilla generates
is ever committed. Jars, worlds and digests stay under the harness cache —
outside the repository, shared by all worktrees, movable with
`DUST_HARNESS_CACHE`. Each verb's own usage (`cargo xtask harness`) carries
the operational details.

## Building

```
just verify
```

That is CI's command list in CI's order — formatting, lints, tests, the
generated configuration reference, the dependency licence audit and the build.
It is deliberately not a subset of what CI runs, because a local gate that skips
steps produces confidence in exact proportion to what it skipped.

## Configuration

One `dust.toml`. See [`dust.toml.example`](dust.toml.example) to start, and
[`docs/configuration.md`](docs/configuration.md) for every setting Dust has.

That reference is generated from the server's own types by `cargo xtask docs`,
and a setting with no documentation does not compile. There is no third place
for a setting to hide.

## Decisions

The reasoning behind the things that are hard to change later is in
[`docs/decisions/`](docs/decisions/): why Dust is written from scratch, why it is
GPL-3.0, why it targets 1.21.1 first, why it is Rust throughout, why ore
density is configured the way it is, which of Minecraft's numbers may live here
rather than on the operator's disk, how wide a volume the sky light is computed
over — a record of a thing measured and deliberately *not* built — and how a
placed block gets its state, which is the first value the oracle route cannot
carry and the first one to be answered with rules and a check instead of a
table.

## Licence

GPL-3.0-only, copyright Ledgeworth Studios. See [LICENSE](LICENSE) and
[NOTICE](NOTICE).

Dust ships no Mojang data and no Mojang assets. Minecraft is a trademark of
Mojang Synergies AB; Dust is not affiliated with or endorsed by Mojang or
Microsoft.
