# D27 — Which tool a block wants, and which file a block draws from

**Status:** Decided, 2026-09-03. Two tables, both read at run time from the
operator's own copy of the game. **One new column group in the oracle's output
and one new file beside it; no Mojang value in this repository.**

## Context

Decision record 0022 gave Dust drops. It left two things wrong and named both.

**Sixty blocks yielded nothing.** `registries::drops` matched a loot file to
the block of its own name, which is right for 982 of the 1,060 blocks on
1.21.1. The other 78 draw from somewhere else, and about sixty of those draw
from *another block's* file: `minecraft:oak_wall_sign` yields an `oak_sign` out
of `blocks/oak_sign.json`. A player who breaks a wall sign, a wall banner, a
wall head, a wall torch or a coral wall fan got nothing back at all.

**Every tool was the right tool.** `minecraft:snow` wants a shovel and
`minecraft:cobweb` wants shears, and those were the only two rows of 46 that
the vanilla survey and Dust disagreed on. The rule behind them is much bigger
than two rows: stone bare-handed yields nothing, a wooden pickaxe on diamond
ore yields nothing, and a server that hands them out is a creative sandbox
wearing survival's clothes.

## The two relations, and where each of them actually lives

### Which loot table a block draws from

`Block.getLootTable()` — a `ResourceKey<LootTable>` handed to
`BlockBehaviour.Properties` in Java. It is in no `--reports` output and in no
data pack, which is the same shape of problem decision record 0008 solved for
opacity and emission, and it takes the same route: the block oracle asks the
operator's own jar and writes `dust-blocks.tsv`, 1,060 rows of block id, block
name and table id, which the operator copies beside their data.

**There is no rule about names that gets there, and this is the third time
this project has proved that to itself.** The 58 borrowers spell it four
different ways:

```text
  oak_wall_sign              -> blocks/oak_sign            drop "wall_"
  oak_wall_hanging_sign      -> blocks/oak_hanging_sign    drop "wall_" from the middle
  dead_tube_coral_wall_fan   -> blocks/dead_tube_coral_fan drop "wall_" from elsewhere
  wall_torch                 -> blocks/torch               drop a leading "wall_"
```

and 20 more point at `minecraft:empty`, which is a **table id meaning nothing**
rather than an absence. That distinction is the reason this is a table and not
a special case: `bedrock` has been answered, and `oak_wall_sign` had not been
asked.

The reader is `dust_registry::loot::BlockLoot`. It refuses a table that does
not describe every block this build knows, and refuses one that numbers a block
differently from this build — the failure being guarded against is not a
corrupt file but a file extracted from **a different version**, where every row
parses and every name means something else.

`dust_sim::drops::Tables` now holds an index per block rather than a table, so
one file compiled once serves both `oak_sign` and `oak_wall_sign`. 982 files
cover 1,042 blocks.

### Which tool is the right one

This one was already in the repository, and finding that out cost two false
starts worth writing down.

Since 1.20.5 a tool is the `minecraft:tool` data component and nothing else: an
ordered list of rules, each naming a set of blocks and optionally a mining
speed and optionally a verdict on whether the drops are correct. There is no
`PickaxeItem` and no tier ladder anywhere — a wooden pickaxe is refused diamond
ore by a rule naming `#minecraft:incorrect_for_wooden_tool`.

**First attempt: ask the jar to apply the rules.**
`Tool.getMiningSpeed(state)` and `Tool.isCorrectForDrops(state)` are the two
methods the game itself calls, so asking them over every item and every block
should be the whole answer. It gives **nine rows for the entire game**. A bare
`Bootstrap` leaves every block tag empty, so only the two rules that name their
blocks outright — cobweb, vine and glow lichen — can match anything. Binding
the tags would mean loading a data pack inside the oracle.

**Second attempt: extract the rules and resolve them against Dust's tags.**
This works, and produced a 97-row `dust-tools.tsv`. It was then thrown away,
because `Item::components()` already holds the component — it arrives in
Mojang's own item report, with its rules and its tag names intact, and is
generated into `ITEM_COMPONENTS`. A second extraction of one relation is two
answers that can disagree and nothing to say which is right.

So `dust_registry::mining` reads the crate. It resolves each rule's block set
against this crate's own tag table, **following tag references** —
`#minecraft:mineable/axe` names fifteen other tags and some of those name more
— and builds one byte per (tool, block) saying which rule matched. 33 tools,
4,523 pairs where the tool is the correct one. Built once, because the answer
cannot change while the server runs and a block break is on the interaction
path.

The byte is *which rule matched*, not the answer, because the two questions are
separable: shears set a speed on leaves and give no verdict, so they cut leaves
fifteen times faster and are still not "the correct tool" for them.

## Where the gate lives

In `dust_sim::drops::Table::roll`, before any pool is rolled, and not in the
server. Two reasons.

It is what Minecraft does: `ServerPlayerGameMode.destroyBlock` calls
`playerDestroy` only when `hasCorrectToolForDrops`, so no pool is rolled, no
function runs and no `survives_explosion` is asked. Writing it as one more
loot condition would give the same answer for every vanilla table today and a
different one the day a table has a pool nobody expected to be reachable.

And it means everything asking what a break yields asks *one* implementation:
the server, the tests, and `cargo xtask harness drops`, which scores Dust
against a real vanilla server and would otherwise have been scoring a rule the
server had and it did not.

`Break` carries `requires_tool` — the block state's own
`requiresCorrectToolForDrops`, a new flag column in `dust-constants.tsv`, true
for 13,778 of 26,684 states. Whether the *tool* is right is not passed in;
`drops` asks `mining` itself. The two halves have different sources and a
caller that conflated them would either hand a bare-handed player cobblestone
or refuse a shovel its dirt.

## The edges, and what was decided at each

- **A tool below the tier still breaks the block and yields nothing.** What
  vanilla does. A server that left the block standing would feel broken rather
  than strict, and a player would read it as a bug in the server rather than a
  rule of the game.
- **A block that does not require a tool yields to anything**, including a
  bare hand, including a tool that is "incorrect" for it. Most of what a new
  player breaks in the first minute is dirt.
- **`minecraft:empty` yields nothing and is not an error.** Bedrock has an
  answer; it is "nothing".
- **The tool takes no durability.** Not deferred out of laziness: a `Stack` in
  this server is an item and a count, with no data components, so there is
  nowhere for damage to live. It is the same seam decision record 0022 named
  for silk touch and fortune, and it is being widened on another branch.
- **Creative does not exempt anything, because Dust has no game modes.** The
  join packet says creative and every player is in it, so gating on the mode
  would mean the rule never applies to anybody. Decision record 0022 made the
  same call about drops firing at all, for the same reason and in the same
  direction: the rule that makes the game a game wins over the flag that says
  nobody is playing it.
- **An operator who has not re-run the extractor keeps the server they had.**
  No `dust-blocks.tsv` means a file is matched to the block of its own name, as
  before; no `requires_tool` column means no block asks for a tool. Both
  defaults are the generous direction, and the boot log says which one is in
  force rather than leaving an operator to notice that sixty blocks are quiet.

## What was measured

`node tools/bot/drops.js 25701 <blocks> --survival --tool <list>` breaks blocks
on a real vanilla 1.21.1 server, in survival, holding each tool in turn, and
`cargo xtask harness drops` scores `dust-sim` against the answers over 2,000
rolls a row.

On decision record 0022's own 50-block, one-tool survey: **46 of 46 scorable
rows agree, where 44 did.** The two that moved are `snow` and `cobweb`.

The wider survey is 36 blocks — sixteen that want a tool, fourteen that do not
and six that draw from another block's file — against seven tools including a
bare hand: **252 rows.** Its numbers are in the session report and in the PR,
because they are facts about a version's data rather than about this
repository.

The survey grew a **negative control** to go with its positive one: `stone`
broken bare-handed has to break and yield nothing. A run where the hand was
never empty measures nothing about the tool requirement, and a run where
everything silently yielded nothing would have looked identical to a correct
one.

## Watched failing

`without_the_oracle_column_the_wall_block_has_no_table_at_all` in
`crates/dust-server/tests/constants_route.rs` is the same data with
`dust-blocks.tsv` removed: `minecraft:oak_sign` keeps its table and
`minecraft:oak_wall_sign` has none, which is exactly the defect this record is
about. Scoring the survey with the file removed puts the count back where it
was.
