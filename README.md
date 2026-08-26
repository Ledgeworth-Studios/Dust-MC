# Dust

A Minecraft Java Edition server, written in Rust.

Dust is being built from nothing. It is not usable yet, and this README will say
so until it is. What exists today is the workspace, the configuration system and
the gates that keep the rest honest.

## Status

Stage 0 — groundwork. Phases 0.1, 0.2 and 0.3 are done; Phase 0.4 (CI) has its
workflow and Phase 0.5 (vanilla data extraction) has not started. The server
process skeleton exists behind the `dust server` command — lifecycle, tick
loop, clean shutdown — but nothing here accepts a connection yet.

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
