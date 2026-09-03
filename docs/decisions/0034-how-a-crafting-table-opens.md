# D34 — How a crafting table opens

**Status:** Decided, 2026-09-03. A window is a **numbering**, not a container.
The player's forty-six slots and the ten a crafting table adds live in one
array, and one implementation of the seven click modes serves both.

## Context

Decision record 0033 gave the 2x2 a player carries a working output slot. It
also said what was missing, and it is the larger half: every three-wide recipe
— a pickaxe, a bed, a chest, bread, a furnace — is made in a 3x3 that only a
crafting table opens. A player who can craft a crafting table and then cannot
use it is a worse experience than one who could never craft at all, because the
first one looks like a bug and the second looks like a missing feature.

## Options

**1. A second menu with its own click handling.** A `CraftingMenu` type holding
its own ten slots and its own implementations of pickup, shift-click, swap,
throw, drag and double-click. It is the obvious shape and the diff would be
purely additive, which is exactly what makes it attractive and exactly the
problem: **two implementations of one set of rules drift**. This container is
at 101 of 101 against a real 1.21.1 server on `tools/bot/clicks.js`, and every
one of those hundred clicks is a rule somebody had to get right once. A second
copy would be a hundred rules that are right today and one bug-fix apart from
being wrong. Rejected — and rejected on the first priority, not on tidiness: a
right click that takes half a stack in one window and all of it in another is a
player learning two inventories.

**2. Generalise over a slot map. — TAKEN.** `Inventory` stores 56 slots: the
player's `0..=45` as before, then `46..=54` for a table's 3x3 and `55` for its
output. `inventory::Window` is the map from a wire slot number to a storage
index. The seven modes work in **storage** indices and did not change. What a
window changes is exactly two things:

- which storage index a wire number reaches, and
- where a shift-click sends a stack.

```text
  Window::Player   0..=45 -> 0..=45, the identity
  Window::Table    0      -> 55   the result
                   1..=9  -> 46..=54  the grid
                   10..=36 -> 9..=35   the player's main inventory
                   37..=45 -> 36..=44  their hotbar
                   (the armour and the offhand have no number at all)
```

**3. A separate container per open table, keyed by block position.** What a
chest will need, and wrong for a crafting table: a table's grid is the
*player's*, not the block's, which is why closing one hands the contents back
rather than leaving them in the world. Deferred to whenever a chest arrives.

## What the window model forced into the open

**A correction naming a slot the open window cannot see is not sent.** With a
table open, slot 5 of its numbering is a grid slot and not the helmet.
`send_slot` asks the window for the wire number of a storage index and sends
nothing when there is none, which is the only reading that cannot put a helmet
in a crafting grid.

**A click naming a window that is not the open one changes nothing.** It is not
replayed against whatever *is* open: the numbering would not even mean the same
thing, and a click on a container the player has already left would land in
slots they never aimed at.

**A player dropped mid-craft owns what is in the table's grid.** There is no
close packet on a disconnect, and the nine slots of a table's grid are not
saved under a player's name. `Inventory::saved` folds them into the
forty-six as a **projection** — it clones, closes the clone, and reads that;
nothing moves in the real container. So a player who *does* close the window
has the same items moved into the same slots and recorded again, with nothing
doubled. The fold only runs when the grid holds something. Priority 1: a
disconnect must not be a way to lose what you were holding.

**Shift-clicking from the player's half fills the grid first.**
`CraftingMenu.quickMoveStack` tries `1..10` before it tries anything else, and
falls through **only when the grid took nothing** — vanilla writes that as
`if (!moveItemStackTo(...))` and the `!` is the rule, not a detail. A stack
that half fits into the grid does not spill its other half into the hotbar. A
shift-click destination is therefore up to two ranges, tried in order, held in
a fixed two-element array so the per-click path allocates nothing.

**A right-click opens the table unless the player is crouching.** Vanilla's
`useItemOn` asks the *block* first, and only secondary use skips that and
places what is in the hand. Getting this backwards would make a crafting table
unusable for anyone carrying blocks, which is everyone.

## What says this is right

`tools/bot/crafting.js --table` places a crafting table, right-clicks it, lays
eight planks around an empty middle, takes the chest, shift-clicks wheat into
the grid, takes the bread and closes the window — recording the container after
each step, in the table's own numbering, plus **which window is open** and
**what block is actually there**.

```text
  node crafting.js 25703 --table --out vanilla.json   (a real 1.21.1 server)
  node crafting.js 25603 --table --out dust.json
  node crafting.js --compare vanilla.json dust.json

  17 of 17 snapshots agree
```

Rebuilt with the open-on-right-click branch removed it reports **1 of 17**, so
the check can fail.

**Recording which window is open is not decoration and neither is recording the
block.** A click naming a window that does not exist changes nothing on either
server, so a Dust that never opened a table would agree with vanilla on every
slot — the same "both sides send nothing" trap `--refuse` exists for. The first
run of this script hit the block half of it: it placed the table on the block
the player was standing on, which puts it inside the player's own hitbox, and a
real server refuses that placement while Dust does not. Vanilla opened nothing,
Dust opened nothing to click in, and the honest reading would have been
"vanilla does not open crafting tables". The support block is now **probed out
of the client's own view of the world** — a solid block a step away with air
above it — because a hard-coded offset is a superflat-only control on a server
that generates terrain, which is the lesson `tools/bot/README.md` already
carries from the open-air control that walked into a hillside.

Nothing regressed, on the same pair of servers in the same run:

```text
  clicks.js --compare       101 of 101   (unchanged)
  clicks.js --predict         3 of 3     (unchanged)
  crafting.js --compare      28 of 29    (unchanged; the 29th is D33's)
  crafting.js --refuse        6 of 6     (unchanged)
  cargo test -p dust-server --lib   273 passed
```

Six new unit tests, including that every number either window has resolves back
to itself, that a table pays out of its own grid and leaves the player's 2x2
untouched, and that a double-click inside a table cannot reach the armour.

## What is still wrong, with numbers

**389 of the 1,290 recipe files still want a block that does not open**: a
furnace, a blast furnace, a smoker, a campfire, a stonecutter or a smithing
table. They are a different shape from this one — a furnace has a fuel slot, a
burn timer and a tick — and none of it is a numbering problem.

**14 recipes are still Java classes** and stay refused.

**A table's grid is not shown to anybody else and the block holds nothing.**
Two players at one crafting table each get their own grid, which is what
vanilla does. A chest will not be, and the separate-container option above is
what it will need.

**Nothing checks that the player is still near the table.** A player who opens
one and walks away keeps the window; vanilla closes it at eight blocks. The
grid comes back to them either way, so this costs nobody an item — it is a
window that stays open, and it wants the same distance check
`within_reach` already performs on the open.
