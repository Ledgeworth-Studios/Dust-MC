# D22 — What a broken block yields

**Status:** Decided, 2026-09-03. The operator's own loot tables, read out of
`[data] path` at boot and evaluated per break. **No new file, no new extraction
step, and no table of Mojang's values anywhere in the repository.**

## Context

Dust had no drops at all. A player could mine for an hour and receive nothing,
which is not a survival game missing a feature — it is a survival game missing
the loop. `README.md` listed it under "Not yet" beside physics and block
updates, and of the four it is the one a player notices in the first ten
seconds.

What a block yields is not a rule anybody can state. Stone yields cobblestone.
Wheat yields wheat when it is fully grown and seeds otherwise. An ore yields a
variable count that a fortune tool multiplies. Leaves yield nothing on nearly
every break, a sapling sometimes, and an apple rarely. Snow yields one snowball
per layer. A chest yields a chest *and the contents it was holding*.

This project has already learned the general shape of that once, and the note
is in the Obsidian vault and in `docs/decisions/0008`: **a rule that is right
98% of the time is worse than a table**, and the case that taught it was
`minecraft:wheat` — the item `minecraft:wheat` places nothing, the block
`minecraft:wheat` comes from seeds, and a server matching names to names is
confidently wrong about the one relation a farmer touches every day.

## What was measured, before anything was written

Every one of the 982 block loot tables vanilla 1.21.1 ships, read and counted:

```text
  pools whose `rolls` is not exactly 1.0        0 of 1,022
  pools whose `bonus_rolls` is not 0.0          0 of 1,022
  entry types                                   3   item, alternatives, dynamic
  condition types                               9
  function types                                7
  deepest nesting of `alternatives`             2
  distinct items any block table can yield    918
```

The nine conditions are `survives_explosion` (741 uses), `match_tool` (137),
`block_state_property` (85), `table_bonus` (27), `any_of` (15), `inverted`
(12), `random_chance` (7), `location_check` (4) and `entity_properties` (2).
The seven functions are `set_count` (212), `explosion_decay` (139),
`copy_components` (49), `apply_bonus` (31), `limit_count` (5) and `copy_state`
(2).

**That is the whole decision.** Nine and seven, with every argument shape
enumerable, is a language that can be *implemented* rather than approximated.
The general loot language is much larger — the full 1,178 tables use more, and
chest and entity loot far more — but block drops do not, and block drops are
what a player mining feels.

## Options

**1. A block-to-item map, extracted once.** The obvious shortcut and the one
this project has already been burned by. It cannot say that wheat depends on
its age, that an ore's count varies, that a silk-touch tool changes the answer,
or that leaves usually yield nothing — and every one of those is visible in the
first minute of play. Rejected on the first priority: it is a survival game
that lies about what you just mined.

**2. A new `dust-drops.tsv` beside `dust-constants.tsv`.** The route decision
record 0008 took for opacity, emission and sound. It works, and it is what
would be needed if the data were a *code* constant. It is not: loot is a data
pack. Taking this route would mean asking the operator to run an extractor over
a directory they are already holding, and inventing a flat spelling for a tree
that is already written down.

**3. Read the operator's `loot_table/blocks/*.json` at boot. — TAKEN.** They
are already in `[data] path`; decision record 0007 asks the operator to produce
that directory with Minecraft's own `--server` generator, and the generator
writes the loot tables into it. So a server that can already sync its
registries can already say what a broken block yields, with no new file, no new
step and nothing committed. It also means **a data pack that changes what stone
drops changes what Dust drops**, because there was never a second copy of the
answer to disagree with it.

**4. Interpret the tables lazily, per break.** Rejected on the second priority:
a break would parse JSON. The tables are compiled once at boot into
`dust_sim::drops::Table` — a flat vector of pools and entries indexed by the
block's own protocol id — and a break walks it.

## What was built

`crates/dust-sim/src/drops.rs` is a compiler and an evaluator for the loot
language, and `crates/dust-server/src/registries/drops.rs` is the reader that
walks each namespace under `[data] path`. On this machine's data:

```text
  982 files, 982 compiled
  1 entry refused
  51 functions want a block entity
  78 of 1,060 blocks have no table of their own name
  22,448 of 26,121 block states yield something to a bare hand
```

**Refusal is counted, never guessed.** A condition this compiler has not heard
of could mean anything, so the entry carrying it yields nothing *and says so* —
it is never read as false, because a condition quietly read as false is a drop
quietly deleted. The one refusal on 1.21.1 is `decorated_pot`'s dynamic sherds
entry. A *function* it has not heard of is different, because a function
modifies a stack that is dropping either way: `copy_components` on a chest
copies the chest's custom name, and a chest with no name is still a chest, so
those 51 are counted apart.

**`Tables::table` answers `None` for a block the data says nothing about, and
`None` is not "drops nothing".** `minecraft:bedrock` has no table because it
yields nothing; `minecraft:oak_wall_sign` has none *under its own name* because
Minecraft points it at `minecraft:oak_sign`'s. Those are opposite facts and the
table refuses to conflate them — the same `has_x()` discipline
`BlockConstants::has_replaceable` follows.

## What is still wrong, with numbers

**78 blocks have no table of their own name, and about sixty of them do drop
something.** Minecraft's block-to-table relation is a *code* constant,
`Block.getLootTable`, in no report and no data pack — the exact shape decision
record 0008 built the oracle for. Almost all sixty are wall variants: every
`*_wall_sign`, `*_wall_banner`, `*_wall_head`, `*_coral_wall_fan` and
`*_wall_hanging_sign` points at the standing form's table. **The fix is one more
oracle column, not a rule about names**, and writing the rule instead is exactly
the mistake this record exists to avoid: "strip `_wall`" is right about sixty
blocks and silently wrong the day Mojang adds a sixty-first that does not follow
it.

**Silk touch and fortune take their unenchanted branch, always.** Not because
the tables were not read — every `match_tool` and `table_bonus` in every table
is compiled and evaluated — but because a stack carries no data components yet
(`README.md`'s own "Not yet", and decision record 0013 says what it needs).
`Break::tool.enchantments` is the seam, it is a borrowed slice, and the day a
stack knows it is silk-touched every branch starts working with no change here.

**A creative player's break drops, and vanilla's does not.** Vanilla checks the
game mode before it rolls the table. Dust's join packet says creative because
Dust has no game modes, so gating on it would mean nothing ever drops and the
whole feature would be invisible. The gate belongs with game modes; putting it
in first would have been correct and unplayable.

**No tool requirement.** Breaking stone with a bare hand yields cobblestone
here and nothing in Minecraft, because `requiresCorrectToolForDrops` is another
code constant and wants the same oracle column as the paragraph above. This one
errs generous rather than mean, which is the better direction to be wrong in
while it is being fixed.

**No block entities**, so the 51 `copy_components` and `copy_state` functions
do nothing: a broken chest drops a chest and not its contents, and a named one
drops an unnamed one.

## Related

- 0007 — what `[data] path` is and who produces it.
- 0008 — the constants that *are* code and needed an oracle. The two oracle
  columns this record asks for belong there.
- 0013 — the data components a stack needs before silk touch can work.
- 0023 — the item entity the drop becomes.
