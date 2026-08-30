# Dust

A Minecraft Java Edition server, written in Rust.

Dust is being built from nothing and is not finished — but you can play on it.

## Status

**Two people can connect, walk around a shared world, break and place blocks,
see each other doing it, and talk.** What they change is still there after a
restart, and so is where they were standing.

`dust server` binds `[server].bind`, answers the server-list ping with the MOTD,
player count and favicon from `dust.toml`, runs login in either offline or
online mode, syncs the eleven datapack registries a 1.21.1 client needs, streams
chunks as players move, and keeps the connection up.

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

**It can serve a world Minecraft made, and hand one back.** Point
`[server].world_source` at a region directory and Dust reads the columns out of
it — blocks, their properties, biomes, heightmaps — and streams them. It also
writes Anvil: `cargo xtask harness rewrite` puts every chunk of a real world
through Dust's reader and writer and then boots a vanilla server on the result,
which reads back the world it started as and says nothing about it that it did
not say about its own. Without a world source Dust generates a superflat and does
not pretend otherwise: worldgen is Phase 6, and a column a real world does not
contain falls back to the flat one, because a world is a disc in an infinite
plane and a player can walk off the edge of it.

What exists either way is the whole path from the socket to the block table —
framing, compression, encryption, the four connection states, the paletted
section codec, the chunk packet, the light engine.

**Not yet**, and each of these is stated where the code for it would go: no
physics, block updates, drops, tool checks or reach validation, so a player may
break bedrock from across the map; no inventory, so there is one placeable
block; no block light, and sky light that crosses a chunk boundary from a
neighbour open to the sky but not from one it would have to travel through; no
plugins;
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
recipes, loot, commands, packets and worldgen. A full run regenerates
everything; each domain prints what it found and how long it took. Two things
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
```

`provision` resolves the server jar through the same manifest-and-SHA-1 path
the extractor uses (verified on every run, including cache hits), writes a
run directory tuned for headless determinism into the harness cache, and —
only with `--yes` — accepts Minecraft's EULA on your behalf by writing
`eula.txt`. Without that flag the file is left unwritten and vanilla refuses
to boot until you have read the EULA and chosen; agreeing to a licence is an
act, and the flag keeps it visible in your shell history where it belongs.

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
GPL-3.0, why it targets 1.21.1 first, why it is Rust throughout, and why ore
density is configured the way it is.

## Licence

GPL-3.0-only, copyright Ledgeworth Studios. See [LICENSE](LICENSE) and
[NOTICE](NOTICE).

Dust ships no Mojang data and no Mojang assets. Minecraft is a trademark of
Mojang Synergies AB; Dust is not affiliated with or endorsed by Mojang or
Microsoft.
