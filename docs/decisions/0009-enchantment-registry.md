# D9 — `minecraft:enchantment` is not a schema table

**Status:** Decided. Dust does not send `minecraft:enchantment`, and the reason
is not that nobody has spent the afternoon. Serving it needs a codec
interpreter, which is Phase 4's datapack loader; serving *part* of it is worse
than sending nothing, which is the part of this that was not obvious.

## Context

Eleven registries are synced to a client that acknowledges no data packs. Dust
serves ten of them from a table in `dust-server::registries::schema`: one row
per key, naming the NBT type it takes and whether every entry carries it. D7
settles why that table may be committed while the values may not — a table of
types is a fact about a protocol, and the numbers in it come from the
operator's own copy of the game.

The eleventh has been a stated omission since the day the other ten landed,
described in the code as "an open codec tree of level-based values, loot
conditions and entity predicates". That description was written from an
impression rather than from a measurement, and this record replaces it with the
measurement.

## What is actually there, measured

`cargo xtask harness registries --version 1.21.1 --dump minecraft:enchantment`
boots a real 1.21.1 server, reads the registry off the wire, and reports every
key path with the NBT tag it holds and how many entries carry it.

Forty-two entries. **470 distinct key paths, eleven levels deep.**

Nine of those paths are the whole of the flat part, and it is genuinely flat:

```text
anvil_cost        TAG_Int        max_level        TAG_Int
weight            TAG_Int        supported_items  TAG_String
slots             TAG_List<TAG_String>            exclusive_set    TAG_String  18/42
description       TAG_Compound { translate }      primary_items    TAG_String   5/42
min_cost/max_cost TAG_Compound { base: TAG_Int, per_level_above_first: TAG_Int }
```

The tenth key is `effects`, and it holds the other 461 paths. Under it:

* **54 effect-component entries** below a compound with 29 keys and nothing
  required — a map keyed by component id, not a record.
* **A recursive condition grammar.** `requirements` is a loot condition, whose
  `terms` are loot conditions, whose `terms` are loot conditions. The deepest
  in vanilla's own data is `minecraft:location_changed`, five conditions down.
* **Predicates as an open second grammar** — entity, block, fluid, movement,
  location — each an object dispatched on its own `type` key.

## Why the table cannot express it, in one number

Three of the 79 floating-point paths are `TAG_Double` and the other 76 are
`TAG_Float`. `movement.horizontal_speed.min` is a double; `value.base` beside
it is a float. **Both are written `0.6` in the JSON on disk**, so no rule over
the file's own text separates them, and no rule over the value separates them
either — which is the same observation the schema module was built on, arriving
one level deeper than the table can reach.

What separates them is what *dispatched* the object: a `LevelBasedValue`'s
fields are floats and a `MovementPredicate`'s are double ranges. That is
Minecraft's codec graph, and reading types out of it is the work — not
transcribing 470 rows.

And transcribing them would not even be right. A table built from these 470
paths is built from the paths *vanilla's forty-two enchantments happen to
exercise*. A datapack enchantment using an effect component none of them uses
would meet a schema that has no row for it, and the schema's own rule — an
unrecognised key is an error naming it — would refuse a valid enchantment.
**That is D6's lesson exactly**: a resolver that knows vanilla's figures is
quietly wrong on every modded world and right only in the case that needs it
least.

## Why a partial send is worse than no send

The tempting middle path is to send the nine flat keys and leave `effects` out.
It parses: one of the forty-two vanilla enchantments has no `effects` key at
all, so the client's own codec already treats it as optional.

It is still the wrong thing, and by the rule already written beside `SERVED`. A
client sent nothing for a registry **falls back to its own copy, which is
correct**. A client sent an entry with the effects missing believes that
enchantment exists and does nothing — Protection stops protecting, Mending
stops mending — and it believes it silently, because a valid entry with an
absent optional key is not an error anywhere.

So the status quo is not the absence of a feature. It is the better of the two
available behaviours, and moving off it requires the whole tree rather than a
part of it.

## What serving it would take

In order, and each piece is Phase 4 work that the datapack loader needs anyway:

1. **A dispatched-object mechanism** in the schema vocabulary: an object whose
   shape is chosen by a key inside it (`type`, `condition`). Every one of the
   three grammars above is that mechanism applied to a different table.
2. **`LevelBasedValue`** — the union at `effects[].effect.amount`, which is a
   bare float or a dispatched object, and the only union the dump found in the
   whole registry.
3. **The loot condition table**, recursively.
4. **The predicate tables** — entity, block, fluid, movement, location.

Nothing about that is blocked. It is a size, and it is Phase 4's size.

## Consequences

- **`minecraft:enchantment` stays a stated omission**, printed by
  `harness registries` on every run rather than left for a reader to notice.
- **A client keeps its own enchantments and is right about them.** No player
  or third-party client sees a wrong enchantment; they see Dust decline to
  have an opinion.
- **A datapack that adds or changes an enchantment does nothing**, and this is
  the real cost of the decision. It is bounded by Phase 4 rather than open.
- **The dump is the input to doing it**, and it is a command rather than a
  paragraph: rerun it against any version and it reports that version's tree.

## Related

* D6 — the ore baseline. The rule that a table of vanilla's own figures is
  right only for vanilla appears here for the second time.
* D7 — registry contents. This record is D7 meeting a registry whose schema is
  a grammar rather than a list.
