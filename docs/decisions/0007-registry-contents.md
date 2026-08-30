# D7 — Registry contents come from the operator's data, never from this repository

**Status:** Accepted, 2026-08-30.

## Context

Since Minecraft 1.20.5 a joining client is told the contents of eleven datapack
registries before it enters the world, because a datapack may have changed them
and there is no other channel that would say so. The payload per entry is
optional: a server omits it for any client that has acknowledged the server's
known packs.

Every vanilla client acknowledges `minecraft:core`, so Dust has been able to
serve them with names alone — sixty-eight biome and dimension names, no
contents. That is not a shortcut, it is exactly what a real 1.21.1 server does,
and it was captured off one to be sure.

A client that acknowledges nothing gets nothing. That is most of the bot and
proxy ecosystem, and it is not a hypothetical: `mineflayer` sends an empty pack
list, fails inside its own registry loader reading `undefined` where a dimension
type's contents should be, and never reaches the world. `mineflayer` is also the
strongest independent check this project has on its own protocol work — it found
the missing `strict_error_handling` byte in seconds — so being unable to serve
it costs more than the bots.

Serving it means sending the contents. The contents are Mojang's.

## The question

Everything committed to this repository so far has been a fact about an
interface: a packet's field order, a block state's property names, a registry's
entry *names*. A biome's fog colour and a dimension's ambient light are not
that. They are the game's content, expressed as data.

So: where do those values live?

## Options considered

**1. Commit them.** Sixty-eight JSON documents transcribed into a generated Rust
table, the way the names already are.

It is the least work and it is the one option that is clearly the wrong side of
the line the project has held since Phase 0.5. The names are a vocabulary; the
values are the game. `NOTICE` would have to say so, and the answer to "may we"
is not obviously yes.

**2. Derive them.** Ship defaults that are close enough — a plausible fog colour
per biome, a plausible height per dimension.

Worse than either alternative. It puts a number on the wire that no Minecraft
ever generated, and every one of those numbers is *invisible* when wrong: a
slightly incorrect water colour is a world that renders and is not the world the
operator has. The failure mode is a bug report nobody can attribute, which is
the exact thing Stage B exists to prevent.

**3. Read them at run time from the operator's own copy.** ✅

The operator has Minecraft. They have to: Dust reads worlds Minecraft wrote, and
`xtask extract` already requires a locally-obtained server jar for every table
in the build. `[data] path` points at a directory in the ordinary datapack
layout, Dust reads it at boot, and the entries go out with their contents.

## Decision

Option 3, and it is the same rule D6 already settled for the ore baseline: **no
Mojang content in the repository; the shape of it, yes.**

What is committed is a *schema* — `crates/dust-server/src/registries/schema.rs`
— saying that `ambient_light` is a `TAG_Float` and `coordinate_scale` beside it
is a `TAG_Double`, that `fixed_time` may be absent, that
`monster_spawn_light_level` is either a number or an object. That is a
description of a protocol, written from bytes a real server sent, and it is the
same kind of artefact as a packet definition in `dust-protocol`.

What is not committed is a single value.

## What follows from it

**A schema is needed per registry, and ten of the eleven have one.** The
missing one is `minecraft:enchantment`, and it is missing for a reason rather
than for want of an afternoon: the other ten are flat records — strings,
numbers, one list and one map — while an enchantment's `effects` is an open
codec tree of level-based values, loot conditions and entity predicates,
several levels deep and different in every entry. Writing that here would be
reimplementing a slice of Minecraft's codec graph on the way to Phase 4, which
has to do it properly anyway.

**All of a registry or none of it.** A registry with no schema is sent as names
to a client that acknowledged the core pack, and *not sent at all* to one that
did not. That is the rule tags already follow in this codebase and it is the
same reasoning one layer up: a client told nothing falls back to its own copy,
where a client told a list of names it has no definitions for believes those
things exist and are empty. It is not a theory — `mineflayer` was sent names
without contents and failed inside its own registry loader reading `undefined`.

**An unrecognised key is an error, and a server-side key is listed.** A biome's
`features`, `carvers`, `spawners`, `spawn_costs` and
`creature_spawn_probability` are real data the client is never sent, so they are
written out as dropped-on-purpose. Everything else that is neither sent nor
listed is refused by name. Without the list, a misspelled `temperture` would be
indistinguishable from `features` — it would load, send an entry without the
temperature the operator set, and give them a world that was quietly not the one
they configured.

**A fraction where an integer belongs is refused rather than rounded.**
`"height": 384.5` is valid JSON and is not a `TAG_Int`. Rounding it produces a
world of a height nobody asked for and says nothing.

**Nothing changes for a vanilla client, and nothing is required of an operator
who does not want this.** With no `[data] path`, Dust behaves exactly as it did
before: names to everyone, and a client that acknowledges no packs is
disconnected with a message naming the setting that would admit it.

## Related

* D2 — GPL-3.0. This is about Mojang's content, which no licence of Dust's
  reaches.
* D6 — the ore baseline, which established that vanilla's numbers arrive from
  the operator's jar rather than from here.
