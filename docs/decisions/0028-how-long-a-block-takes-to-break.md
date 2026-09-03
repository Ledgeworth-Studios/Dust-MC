# D28 — How long a block takes to break

**Status:** Decided and built, 2026-09-03. Break time is Minecraft's own, from
the operator's own jar, on a server whose `[server] game_mode` is survival.
**No Mojang value in this repository**; the hardness arrives in
`dust-constants.tsv`'s `destroy_speed` column, which decision record 0027's
oracle pass already writes.

## Context

Every block in Dust came away on the first click, however hard it was and
whatever was in the hand. Decision record 0027 gave the *yield* a tool rule;
the *time* is the other half, and it is the feedback loop a survival player
touches most — hundreds of times an hour, on every block, all day. A wrong
break time is felt before a wrong drop is.

## The rule, in full

Four Minecraft methods. With `H` the block state's hardness
(`BlockStateBase.destroySpeed`) and `S` the held item's mining speed against
that block:

```text
  divisor  = 30 if Player.hasCorrectToolForDrops, else 100
  progress = S / H / divisor          per tick
```

`H = -1` is unbreakable and progress is zero. `H = 0`, and any pair whose
progress reaches one in a single tick, is Minecraft's "insta mine" and is
destroyed on the start packet.

`Player.getDestroySpeed` then applies five multipliers. Two are implemented:
**efficiency** (`+ level² + 1`, only when the base speed already beats a bare
hand) and **not on the ground** (`÷ 5`). Three are not:

- **Haste** and **mining fatigue**: Dust has no status effects at all — no
  packet, no store, no expiry tick. A field that is always zero would be a
  guess dressed as a number.
- **Eye in water without aqua affinity** (`× 0.2`): needs the fluid state at
  the player's eye, and Dust has no fluid level for a cell that is not a full
  source block. This one is *deliberately* omitted rather than approximated: it
  is a five-fold error, sixteen times the latency allowance below, so guessing
  it would break the very agreement that allowance exists to protect. Omitting
  it makes an underwater player mine at their dry speed, which the allowance
  absorbs in the player's favour.

Efficiency reads off the held stack. That took a decoder — see
*Reading an enchantment* below — and it is the largest single number a player
can change about a break: efficiency V takes a diamond pickaxe from speed 8 to
34, which is stone in 2 ticks rather than 6.

## What client and server do when they disagree — the two thresholds

**The server's own count finishes a break at progress 1.0. A stop the client
sends is believed at 0.7.** They are different numbers on purpose, and the
reason is the whole design:

- The client animates its own break locally, from the click.
- The server starts counting when the *packet* arrives.
- So the client is always a round trip ahead, and a server that demanded 1.0
  from the client's stop would refuse a break the player had already watched
  finish. The block shatters on their screen and comes back. That is the worst
  outcome available here and it is worse than never having timed the break.

And a stop that arrives *earlier* than 0.7 is still not a refusal. Minecraft
arms a delayed destroy that completes on the server's own count, at 1.0, from
the tick the start arrived on. In Dust that rides the pickup tick, which
already runs once a tick, so a player who is not mining pays one `Option` test.

So: **a block a player asked for always goes.** The only thing the two
thresholds decide is whether it goes now or a moment later. A cancel — the
player letting go — is the one packet that takes it back.

## What a real 1.21.1 server was measured doing

`tools/bot/drops.js --survival --times`, five blocks against four tools, no
haste (haste is the largest single term in the formula and a run that has it is
measuring the effect). Ticks:

```text
  block          bare  wooden_pick  netherite_pick  iron_axe
  stone           150           23               5       150
  dirt             15           15              15        15
  oak_planks       60           60              60        10
  iron_ore        300          150              10       300
  sand             15           15              15        15
```

Scored with `cargo xtask harness break`, which asks `dust_sim::mining` the same
question of the operator's own constants:

```text
  20/20 scored row(s) agree within 1 tick, 0 do not
```

## Eight of those twenty rows caught a real defect

Read the `dirt` and `sand` rows. Bare-handed dirt is **15** ticks, not 50.
`Player.hasCorrectToolForDrops` is

```java
!state.requiresCorrectToolForDrops() || item.isCorrectToolForDrops(state)
```

— **a block that asks for no tool is correctly tooled by anything**, a bare
hand included, and takes the 30 divisor. `dust_registry::mining::correct_for_drops`
answers the other question, the one about drops, and says `false` to a bare
hand on dirt. Its own documentation says so in as many words, and the standing
lesson it was written from is *a default that is right for one caller is a trap
for another*. Passing it straight into the divisor makes dirt 50 ticks instead
of 15 and oak planks 200 instead of 60 — every soft block in the game 3.3 times
slower, on the most-touched block there is.

`dust_sim::mining::tool_is_correct(requires_tool, correct_for_drops)` composes
the two facts and both callers go through it. The transferable part is that the
measurement found it and no amount of reading would have: both halves of a
name-matched differential would have agreed on the wrong divisor.

## The checks, and watching them fail

`node tools/bot/drops.js <port> --check-times` (`just break`) is five rows
against a running Dust server in survival:

| | with `destroy_speed` | column withheld |
|---|---|---|
| stone, wooden pickaxe, 23 ticks | ok, 23 | FAIL, 1 |
| stone, bare hand, 150 ticks | ok, 150 | FAIL, 1 |
| stone, netherite pickaxe, 5 ticks | ok, 5 | FAIL, 1 |
| a stop at 70% is believed | ok, 17 after a 16-tick hold | FAIL, gone before the stop |
| a break let go of does not happen | ok, still stone | FAIL, air |

**5/5 and then 0/5**, by cutting one column out of the operator's
`dust-constants.tsv` and restarting. Two of those five rows only became
falsifiable when the control was run: `netherite pickaxe ≤ 7 ticks` passed at
one tick, and the 70% row passed because the poll started *after* the hold and
so reported the hold's own length. A ceiling cannot tell fast from instant, and
a timer that starts late measures itself. Both are now ranges, and the 70% row
reads the cell the moment before it sends the stop.

`cargo xtask harness break --without-hardness` is the same control on the other
side: **20/20 becomes 0/20, largest disagreement 299 ticks.**

## Why the default game mode is still creative

`[server] game_mode` is a new setting and it defaults to `creative`, which is
the mode Dust served before this. That is not caution, it is priority 1:

- Break timing is exact in **both** modes. A creative client removes the block
  locally and never sends a stop; a server that made it wait would be answering
  a screen that had moved on. Instant *is* the right answer in creative.
- What a survival player gets today is a world they can mine and nothing else.
  No crafting, no furnace, no hunger, no health, no mobs. Shipping survival by
  default would hand every player a hunger bar that never moves.

It is not the harnesses that hold it back, and that was checked rather than
assumed. On a survival Dust server, `check.js` scores **29/29** and
`drops.js --check` scores **11/11**, the same as in creative — Dust accepts
`set_creative_mode_slot` whatever mode it announced, so every survey here keeps
filling its own hand. The day survival has something in it, the default is a
one-word change and the tooling is already there.

## A trap that cost forty minutes: a start with no stop is never broken

The obvious way to time a break from outside is to send `START_DESTROY_BLOCK`,
send nothing else, and watch the cell. It measures nothing, and 42 consecutive
rows timing out at two minutes each is what proves it.

`ServerPlayerGameMode.tick` has two branches and **only one of them takes a
block away**. `hasDelayedDestroy` finishes at progress 1.0; `isDestroyingBlock`
only sends the crack overlay. The block a player holds the button down on is
destroyed by the **stop**, not by the count. So `timeBreak` now sends a start
and a stop back to back: the stop is far below 0.7, which is exactly what arms
the delayed destroy, and the delayed path then reports the server's own full
break time in one round trip.

Two smaller ones from the same run, both about controls:

- **A control that cannot find its own row is not a control.** The timing
  control compared a namespaced block name against an un-namespaced tool column
  and matched nothing, and threw away ten minutes of measurement. Both names
  are now compared un-namespaced, and a failed control prints what it saw to
  stderr — never to stdout, because the rows are still untrustworthy.
- **A survey that writes its output at the end writes nothing when it dies.**
  Every timing row is now echoed to stderr as it lands.

## What it costs

Per player, one `Option<Digging>`: a position, an `Instant`, an `f32` and a
`bool`. The progress is computed **once**, at the click, from one read of the
world — the hardness of a block and the speed of a tool cannot change under a
player holding the button down, and asking the world again every tick would put
a chunk lookup on the tick loop of every mining player for an answer already
known. The delayed-destroy check is one `Option` test on the tick that already
runs for pickups.

## Reading an enchantment, which unlocked three things at once

A stack has carried its data components since the item-component work landed,
and nothing could say what was in them. Three rules waited on the same missing
step: efficiency here, and silk touch and fortune in every loot table, all
compiled and all unreachable. **A player mining ore with a fortune pickaxe got
one drop.**

The step is small and the reason it looked large is worth writing down. The
component carries `(registry id, level)`, and a registry id means nothing
without the registry — and decision record 0009 says Dust does not *serve*
`minecraft:enchantment`, which reads like a blocker. It is not. The **names**
in the order that assigns their ids have been in `dust_registry::synced` since
the registry sync landed: 42 of them, captured off a real server and checked
against that capture by a test. Serving a registry and naming a number against
it are different things, and decision record 0007's line — a name is a fact
about the protocol, a value is Mojang's content — is what separates them. So:
`ComponentPatch::component(name)` walks the patch,
`dust_registry::enchantments::parse` turns the payload into `(name, level)`,
and an unenchanted stack produces an empty `Vec`, which allocates nothing.

### What a real server was measured dropping, with three tools

Nine blocks, a plain netherite pickaxe, the same with silk touch, and the same
with fortune III. `cargo xtask harness drops`:

```text
  27/27 rows agree                             (100.0%)
  15/27 with --without-enchantments             (55.6%)
```

The twelve that break are the point: silk touch stops giving stone, cobblestone
comes back instead, and fortune III's `coal*2`, `redstone*7`, `diamond*2` and
`lapis_lazuli*12` all become answers Dust never produces. **Read the fifteen
too.** Six of them are enchanted rows that agree anyway, because what the
unenchanted branch gives is inside the enchanted distribution's support —
fortune on gravel is still flint, fortune III on glowstone still caps at four.
A control that only reported a rate would have called that a 56% pass; it is
44% of the rows disagreeing and the rest declining to answer.

`node drops.js <port> --check` gained two rows for the same seam, and both
assert a contradiction rather than a presence: silk touch gives `stone` where a
plain pickaxe gives `cobblestone`, and over twelve breaks each a plain pickaxe
gives coal `111111111111` where fortune III gives `421114123114`. A fortune
that quietly did nothing would have to roll a one twelve times running.

### `bot.dig` cannot hold an enchanted tool

prismarine-item's `enchants` getter throws `enchantments is not iterable` on a
1.21.1 stack carrying the component, and mineflayer reaches it while working
out how long a break should take. Every enchanted row of the first survey read
`REFUSED`, which is a library failure wearing a server's clothes. The enchanted
rows now send the raw start and stop instead — the same two packets the timing
survey above uses, which ask prismarine nothing.

## What is still wrong

- **No haste, no mining fatigue, no underwater penalty**, each for the reason
  stated above.
- **Nothing but a break reads a component.** A broken chest still drops a chest
  without its contents.
- **The 20 measured rows are 5 blocks and 4 tools.** They cover both divisors,
  a bare hand, three tool speeds and a block that requires a tool — but they do
  not cover hardness 0, unbreakable, or a block whose properties vary. Those
  are covered by unit tests against published numbers rather than by a
  measurement.
