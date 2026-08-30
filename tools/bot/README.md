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

## What it checks

1. It **joins** — through login, configuration and into the world.
2. The **dimension it was told about is the one it is in**, with the height and
   floor Dust sent in the registry contents rather than defaults of its own.
3. It **has the biome registry**, all sixty-four.
4. It can **read a block** under its feet, which means the chunk packet decoded
   and the palette resolved.
5. A **second bot's arm swing** reaches the first.
6. A **second bot's crouch** reaches the first, as both the entity flag and the
   pose — the two are separate and a client told only one renders a player
   half-doing it.

## Why the dependency is not in the licence gate

`cargo xtask licenses` audits what a Dust *build* incorporates. This is a
development tool that is npm-installed by whoever runs it, ships in no
artefact, and is linked into nothing. Nothing here is vendored: `package.json`
names a version range and `node_modules/` is ignored.
