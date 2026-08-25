# D6 — Ore density is configured as multipliers over the world's own placements

**Status:** Accepted, 2026-08-25.

## Context

Operators want to change how much ore a world has. It is the single most common
thing a server owner reaches for a plugin or a datapack to do, and both of the
existing ways of doing it are unsatisfying: a datapack means hand-editing JSON
against a schema and re-editing it every time the base pack changes, and a
plugin means an ore regenerator running after the fact, which fights the
generator instead of configuring it.

Dust generates its own terrain, so it can offer this as a setting. The question
is what the setting should *be*, and there are four decisions inside it.

## Options considered

**1. What the number means — a multiplier or an absolute count.**

| | |
| --- | --- |
| **Multiplier** ✅ | `frequency = 3.0` means three times as much as this world would otherwise have. |
| Absolute count | `attempts = 21` means twenty-one attempts per chunk, whatever the world was doing. |

An absolute count has to be written against one specific baseline. The moment a
datapack such as Terralith changes the baseline underneath it, the number
silently stops meaning what the person who wrote it thought it meant — and it
does so without any error, because 21 is still a perfectly valid number. A
multiplier composes with whatever the world generates; a count fights it.

The cost of the multiplier is that an operator cannot state an exact quantity,
and has to tune by feel against a baseline they cannot see. That is accepted,
and it is why the resolver reports what it computed.

**2. What the number is attached to — an ore, or a placement.**

Vanilla generates diamond through four separate placed features: an ordinary
one, a medium one, one that makes large veins, and one that only generates
fully enclosed in stone. Iron has three, coal has two, gold has three.

The knob is keyed by **ore group** — one entry named `diamond` covering all
four — because an operator who says "more diamond" means all four, and
because scaling all of an ore's placements by the same factor preserves the
*character* of its distribution while changing its quantity. Exposing the raw
placements would let someone triple only the buried variant, which is a real
thing to want and a bad default to design around.

Datapacks may add ore groups beyond vanilla's, so the setting accepts any name
rather than a fixed list. The names are Dust's, not Mojang's — an ore group is
a Dust concept that gathers several placements under one knob.

**3. Where the vanilla numbers live — in this repository, or extracted.**

Not in this repository. Mojang's data may not be redistributed, so the baseline
placements reach Dust through `xtask extract` running against a server jar
obtained on the operator's own machine. See the project's Code Provenance
document.

This constraint turned out to improve the design rather than compromise it. A
resolver that knew vanilla's numbers would be quietly wrong on every modded
world and right only in the case that needs it least. Because it cannot know
them, it works on whatever the loaded world actually has.

**4. Whether it is safe to have at all — the vanilla parity question.**

Phase 6's exit criterion is that Dust's output is block-identical to vanilla
1.21.1 across ten seeds. A feature that perturbs ore placement is a direct
threat to that test, and the wrong answer would be to compile it out for the
comparison — a feature that is absent during the test that would catch it
breaking is not being tested.

The answer instead is an **identity property**: with default settings the
resolver returns the baseline unchanged, exactly, not approximately. The
default path is an early return rather than arithmetic that happens to come out
the same, because "happens to come out the same" is a floating-point claim.
`worldgen.ores.enabled = false` is identity too, so the parity run has a switch
that is proven identical rather than a build that is proven absent.

## Decision

`[worldgen.ores]` in `dust.toml`, with:

- `enabled` — the master switch, and the switch vanilla parity testing uses.
- `default_frequency` — a multiplier applied to every ore with no entry.
- `overrides.<ore>` — per-ore `frequency`, `vein_size`, `min_y`, `max_y` and
  `enabled`, each independently optional, each meaning "leave this alone" when
  omitted.

The arithmetic lives in `dust-gen::ore_density`, which takes the world's own
placements and returns scaled ones. The configuration vocabulary and validation
live in `dust-config::ore`.

**Both forms of "how often" collapse into one representation.** Vanilla writes
frequency two ways — a count of attempts per chunk, and a rarity filter meaning
"one attempt in one chunk out of N". A multiplier has to work on both, and
scaling a 1-in-9 filter by 27 has to produce three attempts per chunk rather
than "one chunk in zero". So both are converted to *expected attempts per
chunk*, scaled, and returned as a whole number of attempts plus a probability
of one more. At a multiplier of 1.0 that is exactly the rarity filter it came
from, which is what makes the identity property hold for both forms.

## Consequences

- **Changing an ore setting does not change chunks already on disk.** Terrain
  is generated once and written; the setting applies to chunks generated after
  it. This is the expectation most likely to be violated in a bug report, so it
  is a distinct reload marker on the type (`hot, new chunks only`), it appears
  in the generated reference, and a test asserts it appears there.
- **A misspelled ore name cannot fail silently.** Nothing at parse time knows
  which ores a world has, so validation happens in two stages: the file is
  checked for well-formedness when it loads, and the names are checked against
  the world's actual ore groups when the world does. An unknown name is an
  error naming the nearest match. The alternative — ignoring a key that matches
  nothing — produces a server that started and a setting that did nothing,
  which is the worst outcome available.
- **Some requests are only knowable as impossible once the world is loaded.**
  `min_y = 100` is a valid number that validation passes, and it is above the
  entire range diamond generates in. The resolver reports it at the point it
  becomes knowable rather than failing the boot on a value that might be fine
  for a different ore.
- **A vein-size multiplier can ask for more than the ore feature can place.**
  Vanilla caps a vein at 64 blocks. The resolver clamps and says so, rather
  than clamping in silence.
- **The setting cannot be tested properly yet.** Everything above is arithmetic
  over a baseline, and the generator that consumes it does not exist. The tests
  assert precedence, validation and the identity property; none of them place a
  block. The test that would catch this being wrong in a way that matters is
  the Phase 6 seed-for-seed differential, and it is worth stating plainly that
  it does not exist yet.
- **X-ray protection is unaffected, and is worth noting here** because it is
  the one adjacent feature an ore setting invites confusion with. Phase 14's
  answer to x-ray is obfuscating ore positions in the chunk packet, which is
  about what the client is told, not about what generates. The two are
  independent.

## Correction, 2026-08-25

This record originally said diamond had **three** placed features and named an
ordinary one, a large-vein one and a buried one. It has four: `ore_diamond`,
`ore_diamond_medium`, `ore_diamond_large` and `ore_diamond_buried`. The counts
given for iron, coal and gold were right.

The decision is unaffected — if anything the miscount argues for it, since a
number written from memory went wrong in exactly the way keying by placement
would have gone wrong in a datapack. What corrected it was the extractor
built in Phase 0.5, which derives the grouping from the block states each
feature places rather than from anything written down here; the four are one
group because all four place `minecraft:diamond_ore` and
`minecraft:deepslate_diamond_ore`.

The same miscount was in `dust-config/src/ore.rs` and is corrected there too.
