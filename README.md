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

**It can serve a world Minecraft made.** Point `[server].world_source` at a
region directory and Dust reads the columns out of it — blocks, their
properties, biomes — and streams them. Without one it generates a superflat and
does not pretend otherwise: worldgen is Phase 6, and a column a real world does
not contain falls back to the flat one, because a world is a disc in an infinite
plane and a player can walk off the edge of it.

What exists either way is the whole path from the socket to the block table —
framing, compression, encryption, the four connection states, the paletted
section codec, the chunk packet, the light engine.

**Not yet**, and each of these is stated where the code for it would go: no
physics, block updates, drops, tool checks or reach validation, so a player may
break bedrock from across the map; no inventory, so there is one placeable
block; no tags; no light across chunk boundaries and no block light; no plugins;
and **Anvil is read-only** — Dust opens a world Minecraft wrote and saves its own
changes in its own format beside it, because writing Anvil means answering
questions there is no way to check the answers to, and a writer that guessed
would produce worlds that open until the day one does not.

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
entry counts, the offline-mode UUID derivation, and a chunk section decoded
field by field until its 18,779 bytes were consumed exactly.

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
argue with ours. The groundwork for that is the harness — three verbs that
provision a vanilla server, fingerprint a world it generates, and compare
fingerprints:

```
cargo xtask harness provision --version 1.21.1 --seed 0 --yes
cargo xtask harness capture --version 1.21.1 --seed 0 --radius 2
cargo xtask harness compare captures/a captures/b
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
