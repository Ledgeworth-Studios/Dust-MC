# D23 — What shape an entity has

**Status:** Decided, 2026-09-03. One flat vector under one lock, ticked by one
participant of the tick loop, announced on one broadcast channel. Measured; the
numbers are below.

## Context

Item entities are the first entities Dust has. Before them the only thing with
an entity id was the player body, and `net/mod.rs` said in as many words that
the seam between the network side and the tick loop had deliberately not been
invented yet. That seam has to be invented to drop an item, and **whatever
shape is chosen here is the shape mobs, projectiles, boats and falling blocks
will all be poured into**, so it is worth deciding rather than discovering.

Item entities are also the classic way a Minecraft server dies. A thousand
dropped stacks in a tunnel, each ticking, each checked against every player, is
a documented way to bring vanilla to its knees. So the decision has a hard
second half: what does one cost, and what do a thousand cost.

## Options for where entities live

**1. A task per entity.** Natural in a tokio server and wrong at any scale: a
thousand items is a thousand tasks, a thousand wakeups every tick and a
thousand allocations. Rejected without measuring.

**2. An ECS.** The shape most game engines reach for, and the right one when
there are twenty component types and systems that iterate subsets. There is one
entity kind and six fields. An ECS here would be a dependency and a vocabulary
bought against a future that has not arrived.

**3. A spatial index — a grid or a quadtree — keyed by chunk.** The right
answer if the pairwise questions were over the whole world. They are not: the
only pairwise question is merging, and merging is only asked about entities
near a player. An index rebuilt twenty times a second to answer a question a
linear scan answers in microseconds is work spent to avoid work.

**4. One flat `Vec<ItemEntity>` under one `Mutex`. — TAKEN.** A thousand
entities is 88 kilobytes walked in one pass with no pointer chasing. The whole
tick happens under one lock acquisition, and the fan-out to clients is the
`broadcast` channel `EditedWorld` already established for block edits, filtered
by the same `View::holds` the edit relay uses.

## Options for who ticks them

**1. A tokio interval task.** Where the rest of the world already lives, and
one more clock in a server that has one.

**2. A participant of the tick loop. — TAKEN.** The tick loop exists, runs at
exactly 20 Hz with catch-up and a watchdog, and had nothing real in it. An
entity that moves is the definition of what a tick is for. The cost is that the
participant has to be built in phase 3, where the world and the roster are, and
inserted in phase 4, where ticking is — one `Option` field on `Server`, and it
is the seam `net/mod.rs` was waiting for.

## What it costs, measured

`cargo bench -p dust-server --bench items`, on an otherwise busy ten-core
machine, so these are ceilings rather than floors. Median of five rounds, each
row the one above it plus a single named change. **An item has two costs and
they differ by four times**, so there are two groups: the fifteen ticks between
a block breaking and the item landing, and every tick after that.

```text
  falling
    nothing on the floor                  8 ns/tick
    1 item, somebody near                38 ns/tick     38 ns/item
    100 items, somebody near          5,463 ns/tick     54 ns/item
    1,000 items, somebody near      424,366 ns/tick    424 ns/item

  at rest
    1 item, somebody near                23 ns/tick     23 ns/item
    100 items, somebody near          3,838 ns/tick     38 ns/item
    1,000 items, somebody near      109,563 ns/tick    109 ns/item
    1,000 items, nobody near            616 ns/tick
```

A tick is 50,000,000 ns. **A thousand items lying beside a player is 0.22% of
one**, the same thousand mid-air is 0.85%, and the same thousand with nobody
near is 0.0012% — one hundred and seventy-eight times cheaper than the at-rest
row above it. The per-item cost rising between a hundred and a thousand is the
merge pass, which is quadratic over the items *near a player*; at the ceiling
that is 424 microseconds, and the ceiling is where it stops because of the
third mechanism below.

**Measuring only the second group was a real hazard here rather than a
theoretical one.** An item takes an early return out of `step` once it has
settled, so the first version of this bench popped a thousand items, ran a
thousand ticks, and spent fifteen of them measuring physics and nine hundred
and eighty-five measuring a branch — then printed the average as though it were
the cost of an item. Each falling row now reports how many of its items were
still moving when it finished and says so if none were. W2 found the identical
shape in the movement bench the same day: a bench whose subject stops is
measuring it having stopped.

### And the world underneath is not free, which is the finding

Every row above is over a flat world. Over a world read out of region files,
with `DUST_BENCH_REGION` set:

```text
  1 falling item, flat                     38 ns/tick
  1 falling item, region files        558,308 ns/tick    14,700x
  100 falling items, region files   5,299,738 ns/tick    10.6% of a tick
```

`Ground` is built inside every `ItemWorld::tick`, so its four-column cache
lives for one tick and is thrown away — and on an Anvil world a miss is a
column decompressed and rebuilt, about half a millisecond. One falling item
therefore rebuilds one column, twenty times a second, for the second and a half
it is in the air.

**This is not fixed here, and the reason is that the fix is not in this
module.** A cache that outlives a tick belongs to `collide.rs`, which is where
`Ground` lives and which another track is working in. What is recorded is the
number, so that whoever gives `Ground` a longer-lived cache knows what it buys
and can check it. Until then: a break on a saved world costs about half a
millisecond a tick while the drop is falling, which is fine for the eight
blocks of a vein and is not fine for a hundred at once — and a hundred at once
needs an explosion, which Dust does not have.

## The three mechanisms, and each is load-bearing

1. **Nothing more than 64 blocks from any player is ticked.** No physics, no
   merging, no despawn clock — one bounds check. This is the 178x above, and it
   is why a tunnel of dropped cobblestone nobody is standing in is free.
2. **Two of the same item lying together become one.** What stops a mined-out
   vein being sixty entities, and also what a player expects to see.
3. **Everything despawns after 6,000 ticks, and there are never more than
   4,096.** The lifetime is vanilla's five minutes. The ceiling is not
   vanilla's — vanilla has none — and it is here because a server that dies
   under a dropped-item flood is worse for everyone in it than one that forgets
   the oldest cobblestone.

## And there are no movement packets

An item is spawned with a velocity and the client runs the same arc: the same
gravity, the same drag, the same numbers. So a drop costs **`AddEntity` once,
`SetEntityData` once for the stack it is holding, and `TeleportEntity` once when
it comes to rest** — three packets for the whole life of an item, against the
twenty a second a server streaming positions would send. The one place two
simulations can disagree is the moment it settles, and that is exactly where
the one correction goes.

`tools/bot/drops.js --check` counts them off the wire and asserts the count
**from below as well as above**: "at most one correction per item" passes on a
server that sends none, and a server that sends none is one whose items are in
the wrong place.

## What the shape cost to get right

Two defects, both found by `crates/dust-server/tests/items.rs`, which needs no
socket, and both about copying vanilla's numbers without its context.

**An item bounced for three seconds.** Vanilla's `ItemEntity` really does
multiply its vertical speed by minus a half on landing — but only *after* the
collision that stopped it has set that speed to zero, so the multiply is on
nothing and the item does not bounce. Read out of the source without the
collision, cobblestone behaved like a rubber ball.

**Two drops from one block did not reliably merge**, which was the same defect
seen from the other end. An item pops out anywhere within a quarter of a block
of the centre, so two can land three quarters of a block apart by the diagonal;
a merge reach of half a block is a vein that is sometimes one stack and
sometimes six. Vanilla asks the same question as a box inflated by half a block
around an item a quarter wide, which comes to one block.

Both are the same lesson and it generalises past items: **a constant copied out
of Minecraft's source is only right inside the machinery that surrounds it
there.**

## What is not decided here

- **A column cache that outlives a tick.** The section above measures what its
  absence costs a falling item on a saved world, and the fix is `collide.rs`'s.
- **Sideways collision.** An item is a quarter of a block wide and only ever
  meets the surface under it in ordinary play, so `step` handles landing and
  nothing else. Entities with real boxes want `dust-guard`'s shapes, and that
  is the same work a mob needs.
- **Which players an entity is sent to.** Today every session receives every
  item change and filters by `View::holds`, exactly as it does for block edits.
  That is right for hundreds of entities and wrong for hundreds of thousands;
  the answer when it comes is a tracker, and it should arrive with the first
  entity kind that exists in numbers.
- **Persistence.** Items are not saved, so a restart clears the floor. Vanilla
  saves them; the argument for doing it later is that nothing else in the world
  is saved as an entity yet either.
- **Throwing.** `Q` still destroys a stack rather than throwing it: the
  serverbound drop actions are not wired to `ItemWorld::pop`, and doing it is
  now a small change rather than a missing subsystem.

## Related

- 0022 — what a broken block yields, which is what these entities carry.
- 0015 — what a movement check asks the world; `Ground` is what an item lands
  on.
