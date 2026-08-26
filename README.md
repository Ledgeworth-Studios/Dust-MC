# Dust

A Minecraft Java Edition server, written in Rust.

Dust is being built from nothing. It is not usable yet, and this README will say
so until it is. What exists today is the workspace, the configuration system and
the gates that keep the rest honest.

## Status

Stage 0 — groundwork. Phases 0.1 through 0.4 are done, and Phase 0.5 extracts
the vanilla data the rest of the server stands on: blocks, items, entity
types, fluids, tags, recipes, loot tables, commands and packets. No server
code. Nothing here accepts a connection.

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
