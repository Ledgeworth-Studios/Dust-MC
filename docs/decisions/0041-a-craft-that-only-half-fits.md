# D41 — A craft that only half fits

**Status:** Decided, 2026-09-04. **Dust refuses the craft. Vanilla spends the
grid and destroys the remainder.** This is a deliberate divergence and the only
one the crafting work made.

## Context

Shift-clicking a crafting output crafts repeatedly until something stops it —
the grid runs out, or the player's inventory does. The interesting case is the
one in the middle: the player has room for some of what a pass would make and
not all of it.

`AbstractContainerMenu.quickMoveStack` on a result slot calls `moveItemStackTo`
and then acts on `ItemStack.isEmpty()` of what came back. `moveItemStackTo`
moves what it can and **shrinks the stack in place**; the remainder is a live
`ItemStack` on the stack frame of a method that is about to return. Nothing
puts it anywhere. The grid has already been paid — `onTake` ran, the ingredients
are gone — so the arithmetic is: eight planks in, one stick out, one stick in
the void.

It is reachable without trying. A full inventory with one slot holding 63 of
something, and a recipe whose pass makes four of it, loses three. On a stack of
torches or arrows it is a routine amount to lose.

This was recorded in a pull request body and nowhere else for two weeks, which
is how it comes to be written down now: the next reader of `quick_move_result`
meets an intentional divergence with no sign that anybody chose it.

## Options

**1. Match vanilla exactly, remainder and all.** The strongest argument for it
is the one that outranks the rest of this record everywhere else: a server that
does not behave like Minecraft is a server that surprises people. And it is a
real argument — a player who has learned the behaviour, or a tutorial written
against it, or a machine that depends on the item vanishing, all get something
different here.

It is rejected because of what the surprise *is* in each direction. A player
who expects the loss and does not get it has more items than they planned for
and no way to notice. A player who does not expect it and gets it has silently
lost work, and the client shows no message, plays no sound, and leaves no
particle. Priority 1 does not say "be familiar", it says be better to play, and
the failure mode of matching vanilla here is the one a player cannot see.

**2. Craft the partial pass and drop the remainder as an entity.** Nothing is
destroyed and vanilla's grid arithmetic is preserved. Rejected: it is neither
behaviour. A player at a crafting table in a lava-floored base, or one whose
screen is open above a hopper, has a new way to lose the same items, and the
item on the floor is *also* something no message announces.

**3. Refuse the pass. — TAKEN.** A pass that cannot deliver everything it makes
does not run. The grid is untouched, the output slot still holds what it held,
and the player's next action — closing a slot, dropping something — makes the
same shift-click work. Nothing is created and nothing is destroyed.

The cost is that a shift-click sometimes appears to do nothing, and "appears to
do nothing" is the shape of a bug. It is accepted because the state that
produced it is visible on the screen the player is already looking at: their
inventory is full. A refusal a player can explain by looking is better than a
loss they cannot see at all.

## What it does not change

The **partial pass is the only divergence**. A shift-click that can deliver
everything runs, and it runs in a loop until it cannot — matching vanilla, and
matching it for the reason recorded in the brief's traps: `clicked` calls
`quickMoveStack` repeatedly, and one pass is wrong wherever the destination can
change between passes. That loop owned all 17 disagreements in the inventory
widening and is not what this record is about.

A furnace's output slot is not covered by any of this. It is not a recipe
result — the fire filled it and it is already paid for — and it takes the
ordinary quick-move path. The two were briefly the same code: routing a
furnace's output down the crafting path turns one iron ingot into 2,304, which
`tools/bot/furnace.js --states` now reports as `0 -> 2304` against a build with
that rule collapsed and `0 -> 1` against this one.

## Measured

`tools/bot/crafting.js` is 28 of 29 against a real 1.21.1 server. **The one
disagreement is this record**, and it is the only row in that run where Dust
and Minecraft do different things on purpose.
