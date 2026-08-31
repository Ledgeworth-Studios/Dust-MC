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
