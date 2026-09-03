# D16 — Which slot an item is worn in

**Status:** Decided, 2026-09-03. Read from Mojang's own item tags, with the two
items no tag places named here and guarded by a third tag. Armour slots refuse
what is not worn in them; shift-click equips; a shift-click runs until nothing
more moves.

## Context

[D13](0013-where-a-players-inventory-lives.md) built the forty-six-slot
container and replayed the seven `Click Container` modes over it. It said in its
own module header that the one thing it did not do was equip armour, "because
which slot a helmet goes in is `Item.getEquipmentSlot()` in Java and is in no
report".

That was true and it was also the whole of the gap. Every slot took every item.
A player could put cobblestone on their head, could not shift-click a chestplate
on, and had a shield that went to the hotbar instead of the offhand. Inventory
handling is the interaction a player performs most often, and each of those is
visible on the first attempt.

## What was measured

`tools/bot/clicks.js` records rather than asserts: a hundred clicks over a
seeded container, one snapshot of every slot per click, taken the same way from
Dust and from a real 1.21.1 server on the same script, and then diffed. The
comparison is the measurement; a recording on its own is not a result. Counts,
because a rate would not say which click:

| script | Dust vs. a real 1.21.1 server |
|---|---|
| the ordinary inventory, 58 clicks | **58 of 58 snapshots agree** |
| plus armour, offhand and crafting grid, 82 clicks | **60 of 83** |
| after the slot rules, 82 clicks | **83 of 83** |
| plus the stacking wearables, 100 clicks | **84 of 101** |
| after the shift-click loop, 100 clicks | **101 of 101** |

The first row is the reason this record exists. Fifty-eight of fifty-eight is
not a result about a container; it is a result about the fifty-eight situations
the script reached, all of which were the ordinary inventory, where every slot
does take every item. **A stand-in only exposes the defects its own range
reaches.** Widening the same script by twenty-five clicks took it from 58/58 to
60/83, and every one of the twenty-three disagreements was a rule that had never
been asked about.

The fourth row is that lesson a second time, one layer down. Every wearable in
the armour section stacks to one, so a container that asked the *item* for its
limit rather than the *slot* agreed with the game on all of them. Eighteen more
clicks seeded with `minecraft:player_head` — worn on the head and stacks to 64 —
took 83/83 back to 84/101.

## The decision: which slot an item is worn in

Java answers this with `Mob.getEquipmentSlotForItem`, which walks a class
hierarchy: `ArmorItem` knows its own type, `ElytraItem` is the chest,
`ShieldItem` is the offhand, a skull block's item is the head. A class hierarchy
is in no report, and the item report is no help either — on 1.21.1 every armour
piece's `minecraft:attribute_modifiers` is an **empty list**, so the report that
knows a helmet's durability does not know it is worn. The
`minecraft:equippable` component that would answer this outright is 1.21.2.

**Mojang's own item tags do answer it**, and they arrive through the extraction
that is already there:

| tag | slot | members |
|---|---|---|
| `minecraft:head_armor` | head | 7 |
| `minecraft:chest_armor` | chest | 6 |
| `minecraft:leg_armor` | legs | 6 |
| `minecraft:foot_armor` | feet | 6 |
| `minecraft:skulls` | head | 7 |

Thirty-two of the thirty-four items a player wears. The last two —
`minecraft:elytra` on the chest and `minecraft:carved_pumpkin` on the head — are
in no tag that names a slot, and `minecraft:shield` is in none of them either
because a shield is held rather than worn. Those three are named in the source.

**Three names written down is three names that can go stale, so they are
guarded rather than trusted.** `minecraft:enchantable/equippable` is vanilla's
own list of everything worn — the four armour tags, the skulls, the elytra and
the carved pumpkin, thirty-four items — and a test walks it and fails on any
member this table places nowhere. A version that adds a wearable stops on the
row where it happened rather than shipping an item a player cannot put on.

### What was declined

**A new column in `dust-items.tsv`.** It was the obvious answer and D13 declined
its own version of it for the same reason: the value was already extracted.
Adding a column would mean the extractor deciding, out of the same tags, what
this decides out of the same tags — one question with two answers, and the
column would go stale independently.

**Writing the twenty-five armour items out by name.** Faster to type and wrong
the first time a version adds a material. The tags are Mojang's list of exactly
this, kept by Mojang.

## The decision: what a slot accepts, and what it holds

Forty-four of the forty-six slots take any item. The crafting output takes none.
Each armour slot takes only what is worn in *that* slot — a helmet is refused by
the boots slot as firmly as cobblestone is.

**The offhand takes anything**, which is measured rather than assumed: a real
server accepts nine cobblestone into slot 45 without complaint. It looks like a
slot that should have an opinion and does not.

**An armour slot holds one item**, not one stack — `ArmorSlot.getMaxStackSize`
is 1. That is only visible for a wearable that stacks above one, and there is
exactly one class of those: a stack of nine player heads left-clicked onto the
helmet slot leaves eight on the cursor.

The rule reaches five of the seven modes. A left click, a right click, a
shift-click, a number key and a drag all consult it. **The drag consults it
where a slot joins the drag rather than at the end**, which is load-bearing: the
share each slot receives is divided by how many slots joined, so a slot filtered
on the way in makes the others' share larger. Twenty-one cobblestone dragged
across the chest slot and one ordinary slot puts all twenty-one in the ordinary
slot on a real server, not ten.

## The decision: a shift-click runs until nothing more moves

`AbstractContainerMenu.clicked` does not call `quickMoveStack` once. It calls it
in a loop, until a call moves nothing or the slot stops holding the same item.

That is not an optimisation, it is the rule. The first pass sees an empty head
slot and moves one player head there, because an armour slot holds one; the
second pass sees it occupied, takes a different arm of the same method, and
sends the other eight to the hotbar. A single pass leaves eight heads in the
slot the player shift-clicked. All seventeen disagreements in the fourth row of
the table above were this one cause.

## What is still not right, stated plainly

- **A helmet is worn and does nothing.** There is no armour value, no damage
  and no entity equipment packet, so a helmet on the head is not visible to
  another player and does not protect the wearer. The slot rules are the
  container half of the feature; the world half is not written.
- **Q still destroys rather than throws.** D13 said so and it is still true:
  there are no item entities.
- **The crafting grid is storage.** Nothing crafts, so the output slot never
  fills. It is refused as a destination for exactly that reason.
- **Components are dropped**, as D13 records. A named or enchanted item comes
  back plain, and two stacks that differ only in components stack together here
  and would not in the game.

## Cost

One byte per item, 1,333 of them, built once on the first click of a server's
life and read as an array index afterwards. The alternative — a tag lookup per
click — is five binary searches over a 514-row table on a path a player hits
several times a second. Priority 2: the table is 1.3 KB once, per server, not
per player.

Nothing was added to the wire. D13's `push_back` already sends one
`container_set_slot` per slot the client is wrong about and the whole container
only on a join or a close, and the new refusals ride the path that was already
there for a click the server declines. `clicks.js --predict` is what says that
path is real: it sends the prediction a client makes on a click the server
refuses and requires the contradiction, because a recording where both servers
send nothing is a recording that agrees.
