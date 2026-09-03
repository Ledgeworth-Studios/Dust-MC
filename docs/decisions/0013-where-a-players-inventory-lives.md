# D13 — Where a player's inventory lives

**Status:** Decided, 2026-09-02. Forty-six slots in memory, written by name into
the save file beside the world, corrected on window **0** and not on `-2`.

## Context

Until this record a player's inventory was nine slots, no counts, and nothing
that survived a disconnect. `net::hotbar` said so in its own first paragraph and
was right to: it existed so that a right-click could place what a creative
client had picked out of the menu, and it did that. What it could not do is
anything a player would call carrying something. Leaving a Dust server emptied
your pockets.

Three questions had to be answered together, because the answer to each
constrains the others: what shape the container is, how many of a thing a slot
may hold, and where it is written down.

## How many a slot holds — the column that turned out to already exist

The obvious plan was a new column in `dust-items.tsv`: `max_stack_size` is
per-item, Minecraft's, and a server that wrote `64` would be wrong about every
bucket and every ender pearl. Decision records 0006, 0007 and 0008 all say a
value like that arrives at run time from the operator's own jar.

**It does not need to.** `max_stack_size` is a *data component*, it is in the
item report Minecraft's own data generators emit, and `cargo xtask extract`
already turns that report into `dust-registry`'s component table — with the
extractor refusing to emit a table where any of the 1,333 items' value is not an
integer in `1..=99`. `Item::max_stack_size()` has been sitting there since the
item table landed. So nothing was extracted for this cycle and nothing new is
committed, and the rule that a stack size is Minecraft's rather than Dust's is
kept by using a table that already exists.

Recorded because the wrong version of this is invisible: a server that hardcoded
64 passes every test written against stone, dirt and cobblestone, and is wrong
about the first bucket a player picks up. The unit test that says so uses a
water bucket (1) and an empty bucket (16) — **an empty bucket stacks to sixteen
on 1.21.1**, which is exactly the sort of thing a hand-written table gets wrong.

## The shape: forty-six slots, including the five nobody crafts with

Vanilla's own numbering, `0..=45`: crafting output, 2x2 grid, four armour, 27
main, 9 hotbar, offhand. All of it is stored.

The five crafting slots are the arguable ones — nothing in Dust crafts, so the
output never fills. They are stored anyway, because a player can *put* something
into the grid, and a container that dropped those four slots would swallow items
into a hole with vanilla's own numbering painted on it. Twenty bytes buys the
absence of a bug that only ever shows up as a lost item.

A stack is an item and a `u8`: four bytes, `Copy`, and the container is a fixed
array. Nothing on the read path allocates, which is the second decision-rule
priority applied where it actually bites — `held()` is read on every right-click,
and the "what changed" value a click returns is a `u64` bitmask rather than a
`Vec`, so a click reporting that one slot moved does not allocate to say it.

## Where it is written: the edit file, at version 2

The running server already saves its own edits beside a world rather than back
into it (`dust-edits.json`), and already keeps each player's position there. An
inventory is the same question with the same answer, so it is the same file:
`SavedPlayer` gains a list of the slots that hold something and which hotbar slot
was in hand, and `SAVE_VERSION` goes to 2. A version 1 file still loads — the new
fields default to an empty inventory, which is precisely what a save written
before players carried anything means.

**Items are written by name**, for the reason blocks already are: an item's
protocol id is a position in a generated table, so a saved id would survive a
version bump by silently becoming a different item. A name this build has no
entry for is dropped and *named* in a warning, never counted — an operator who
changed Minecraft version needs to know which item their players just lost.

**What the record promises:** the forty-six slots, the item in each, the count,
and the selected hotbar slot. Those come back exactly.

**What it does not promise:** components. A stack's name is written and its data
components are not, so a renamed block, an enchanted tool and a full shulker box
all reload as the plain item. That is not a saving decision — it is
`dust_protocol::types::Slot`'s wall, where a component carries no length and an
unknown one cannot be stepped over, and this format cannot record what nothing
upstream of it can read. It is written down here rather than found later,
because a record that quietly means less than it looks like it means is worse
than one that refuses to be written.

## Two clientbound packets came off the blocked list, and the wall did not move

`container_set_content` and `container_set_slot` sat on `packets::play`'s blocked
list under "the Slot wall" for as long as that list has existed. They came off,
and the distinction is worth keeping because the same mistake is available for
every other Slot-carrying packet: **`Slot` refuses added components on *decode*.**
Encoding has no such problem — it writes a zero for the additions and knows
exactly where every byte goes. Both of these are clientbound, so the server only
ever encodes them. The refusal still bites anything that decodes one back, which
is the round-trip tests and any future capture replay, and that is the same
refusal rather than a new one.

## The measured decision: correct on window 0, not on window -2

`container_set_slot` has a **signed** window id so that `-1` can address the
cursor and `-2` can mean "the player's own inventory, and do not check the state
id". `-2` reads like exactly the right id for a server correcting a client that
guessed wrong, and Mojang's client honours it.

It is still the wrong choice, and only a second implementation could say so.
Pointed at a build that corrected on `-2`, **mineflayer dropped every correction
on the floor** — its handler resolves a window by id, there is no window `-2`,
and the packet returns without doing anything. No error, no log line. Four
checks failed and nothing anywhere said why.

```text
  tools/bot/check.js, one running server, same commit apart from this id

  correcting on window -2      24 of 29 checks
  correcting on window  0      25 of 29 checks   (the four remaining were the
                                                  relog checks, deliberately
                                                  broken at the time)
  and with persistence back    29 of 29 checks
```

Window 0 is what vanilla's own `ContainerSynchronizer` sends for a player's own
menu, both clients honour it, and it is what this server has already told the
client the window is. **A correction one client cannot see is not a correction,
and the fact that another one can see it is not a defence.** The cursor stays on
`-1` because there is no second spelling of the cursor to prefer; mineflayer
ignores that too and keeps its own, which is why the bot check reads slots.

## What was declined

**Auto-equipping armour on shift-click.** Vanilla's `quickMoveStack` routes a
helmet to the head slot before it considers the ordinary destination, and which
slot an item equips into is `Item.getEquipmentSlot()` — Java, in no report, the
same wall as the light constants. It *is* extractable the way 0008's constants
are, as one more column of `dust-items.tsv`, and it is the next column that file
needs. Declined this cycle rather than done badly: every other way of filling an
armour slot works, including dragging one there, so what is missing is a
shortcut and not a capability. Shift-clicking a helmet moves it to the other
half of the inventory instead of onto the player's head.

**Item entities for drops.** Q, control-Q and a click outside the window all
work and all *destroy* what they throw, because there is nothing in the world
for a dropped stack to become. Dropping is the click mode a player is most
likely to use by accident, so this is stated in the module, in the README and
here rather than left to be discovered.

**Sending the whole container on every change.** The join sends all forty-seven
stacks because a join has nothing to compare against. Everything after it is one
`container_set_slot`, and only for the slots the client's own prediction got
wrong — which for an ordinary left click that the client predicted correctly is
zero packets.
