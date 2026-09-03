# D33 — What a grid of items makes

**Status:** Decided, 2026-09-03. The operator's own recipe files, read out of
`[data] path` at boot, indexed by ingredient, and matched on every click that
moves a grid slot. **No new file, no new extraction step and no table of
Mojang's recipes anywhere in the repository.** The 2x2 grid a player carries
crafts; the 3x3 a table opens does not yet, and the last section says what is
missing.

## Context

Dust stored the five crafting slots and never filled the output. A player with
an inventory of oak logs could not make a plank, which is not a survival game
missing a feature — it is the loop the rest of the game hangs off. Decision
record 0013 named those five slots and said in its own words that nothing here
crafts.

## What was measured, before anything was written

All 1,290 recipe files vanilla 1.21.1 ships, counted:

```text
  crafting_shaped                                634
  crafting_shapeless                             253
  stonecutting                                   250
  smelting / blasting / smoking / campfire       112
  smithing_transform / smithing_trim              28
  crafting_special_* and crafting_decorated_pot   14
```

And of the 887 that are made in a grid:

```text
  ingredient shapes         3   {"item": id}, {"tag": id}, and a list of the first
  result keys               2   `id` and `count`, on every one of the 887
  distinct item tags used  19
  pattern rows              1..=3, and rows 1..=3 wide
  shapeless ingredients     1..=9
  result counts             1, 2, 3, 4, 6, 8, 9, 16
```

**That is the whole decision**, and it is decision record 0022's finding again:
three ingredient shapes and two result keys, with every argument enumerable, is
a language that can be *implemented* rather than approximated. So there is no
rule in `dust_sim::crafting` and no name matching anywhere in it.

## Options

**1. A hard-coded table of recipes.** Mojang's data in the repository by a
route the provenance line does not allow, and a data pack that adds a recipe
would not work. Rejected outright; records 0006, 0007 and 0008 all say so.

**2. A new `dust-recipes.tsv` beside `dust-constants.tsv`.** The route record
0008 took for opacity and emission, and it is right when the data is a Java
*constant*. A recipe is not: it is a data pack, in a directory the operator is
already holding because record 0007 asks them to produce it. Taking this route
would mean asking twice and inventing a flat spelling for a tree that is
already written down.

**3. Read `recipe/*.json` at boot. — TAKEN.** A data pack that changes what a
log makes changes what Dust makes, because there was never a second copy of the
answer to disagree with it.

**4. Interpret the files per lookup.** Rejected on the second priority; see
the index below.

## What was built, and what it costs

`crates/dust-sim/src/crafting.rs` compiles and matches;
`crates/dust-server/src/registries/recipes.rs` walks each namespace. On this
machine's data:

```text
  1,290 files, 887 craftable in a grid, 0 refused
  389 not made in a grid   (smelting, stonecutting, smithing)
   14 code rather than data (the crafting_special_* markers)
  147 item tags read and resolved
  2,713 (item, recipe) index pairs, 6,959 ingredient slots
  128 ms to build at boot, about 36 kB held
  100,000 lookups on a full 2x2: 3.5 ms — 35 ns each
```

**The index is the second priority's whole answer.** A lookup runs on every
click that moves a grid slot, which is every click a player makes arranging
ingredients. Scanning 887 recipes each time would be 887 pattern matches to
answer a question that usually has one candidate, so the recipes are indexed by
ingredient item — a flat `u32` array with a range per item id, both built once
— and a lookup tests only the candidates of the grid's *rarest* item.

**Refusal is counted, never guessed.** An ingredient shape, a result key or a
recipe type this compiler has not heard of makes the recipe refuse and say so.
The two that vanilla itself produces are counted apart from defects, because
"this server cannot make a firework yet" and "your data pack has a recipe I
cannot read" are different sentences to an operator.

**The thirteen `crafting_special_*` shapes and `crafting_decorated_pot` are
declined.** On 1.21.1 they are marker files carrying `type` and `category` and
nothing else, because the recipe is a Java class — a firework's flight duration
comes from how much gunpowder is in the grid, a shulker box keeps its contents
through a dye. There is nothing in the file to compile. They are refused **by
their declared type**, never by what a name implies.

**Item tags come from the data pack, not from `dust_registry::tags`.** That
table would answer `#minecraft:planks` correctly today. A recipe's tags are the
one place a pack's *additions* matter most: a pack that adds a wood adds it to
that tag and expects a crafting table to notice, and a server resolving the tag
out of its own compiled copy would load the pack's recipes and then refuse to
make anything with its planks.

## The three places this differs from vanilla, and why

Every one is priority 1, and the direction is the same each time: **crafting is
where item loss is most visible and least forgivable, so where a choice exists,
take the one that cannot destroy a player's items.**

**A right click on the output hands over the whole result.** Vanilla's
`doClick` computes `(count + 1) / 2` before the result slot sees it, and
`ResultSlot.onTake` spends the grid whatever that number came out as. Measured
against a real server, that path happens to hand over all four planks — Dust
agrees with it on this row, and would have chosen the same behaviour if it had
not.

**A shift-click stops on the first craft whose result does not fit whole.**
Vanilla's `moveItemStackTo` reports success when it moved *any* of the stack
and `onQuickCraft` spends the grid anyway. This is the one measured
disagreement: with sixty-two planks in the last free slot and a log in the
grid, a real 1.21.1 server moves two planks, destroys two, and takes the log;
Dust refuses the craft and leaves the log where the player can see it.
`tools/bot/crafting.js` step 28 is that row and it is deliberate.

**Q over the output crafts once and the result leaves the container.** Vanilla
crafts once for both Q and control-Q — measured, one log per press, and not
what reading the code alone would have predicted. What reaches the *floor*
differs between the two buttons on a real server and reaches no floor at all
here, which is what every other Q in this container already does.

## Crafting remainders, which are a Java constant

The bucket a cake gives back is `Item.Properties.craftRemainder` — in no
report, no data pack and no registry, the same shape as `Block.getLootTable` in
record 0022 and `Mob.getEquipmentSlotForItem` in record 0016. So it is twelve
written pairs in `dust_sim::crafting::REMAINDERS`, and it is the only
Minecraft-authored relation in the file. Three of the 887 vanilla recipes touch
one, and one of the three is reachable in a 2x2: four honey bottles make a
honey block and give four glass bottles back. A real 1.21.1 server puts those
four bottles back in the grid slots they came from, and so does Dust.

## Where an item lands when a player is given one, which was wrong

Measured on the way past, and it was not a crafting bug: closing a window with
a log in the grid put that log in the **main inventory** here and in the **first
hotbar slot** on a real server. Vanilla's `Inventory.add` looks for a partial
stack in the slot in hand, then the offhand, then the hotbar, then the main
inventory, and then for an empty slot in the hotbar before the inventory — and
the offhand is never chosen for an empty slot, because `getFreeSlot` scans only
the thirty-six. `Inventory::place` is that order now, and it is what an item
picked up off the ground follows too. The one snapshot that caught it
distinguishes hotbar-first from inventory-first; it does not separately pin the
"slot in hand" and "offhand" clauses, which are written from vanilla's own
search order.

## What says this is right

`tools/bot/crafting.js`, a new third-party instrument in the shape of
`clicks.js`: it records what the server says the container became, one
snapshot per step, and the same recording is taken from a real 1.21.1 server
and diffed.

```text
  node crafting.js 25703 --out vanilla.json    (a real 1.21.1 server)
  node crafting.js 25603 --out dust.json
  node crafting.js --compare vanilla.json dust.json

  28 of 29 snapshots agree
```

The one that does not is the shift-click that only half fits, above.

**A recording alone would not have been a check.** Every click it sends claims
"nothing changed", so a click the server *refuses* is a click both ends already
agree about and neither server sends anything — the trap `clicks.js --predict`
was built for. `crafting.js --refuse` tells the lie a real client tells,
drawing the cobblestone it dropped into the output, and requires the
contradiction. **6 of 6 on a real server and 6 of 6 on Dust**; rebuilt with the
output's own pickup path removed it reports **3 of 6**, so the check can fail.

Four unit tests were each watched failing with their rule removed: mirroring
off, the grid offset ignored, the shapeless assignment made greedy, and the
shift-craft loop cut to one pass. The greedy control is the one worth writing
down — the first draft of that test **passed** under greedy, because the case
only bites when the ingredient asked first can take the item the ingredient
asked second uniquely needs. A test of a search that any order satisfies is not
a test of the search.

## What is still wrong, with numbers

**The 3x3 does not open.** 887 recipes compile and the 2x2 can reach the ones
that fit in four slots; every three-wide recipe — a pickaxe, a bed, a chest,
bread — needs a `minecraft:crafting` window the server opens on a right-click,
which is a second container with its own window id and its own slot numbering,
and this container is built around the player's forty-six. That is the next
piece and it is most of a PR on its own.

**14 recipes are code and stay refused** until there is somewhere for a Java
recipe to live. A player cannot dye leather armour, make a firework, copy a
map, or colour a shulker box.

**389 recipes want a furnace, a stonecutter or a smithing table**, all of which
are the same missing thing as the 3x3: a block that opens a window.

**Nothing rate-limits the lookup per player.** A client sending clicks as fast
as the socket allows makes one 35 ns lookup each, which is nothing, but it is
one more thing a click can cost and nobody has measured a hostile client.
