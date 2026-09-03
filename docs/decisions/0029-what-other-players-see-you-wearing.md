# D29 — What other players see you wearing

**Status:** Decided, 2026-09-03. Equipment is state on the roster and a
difference on the wire: **only the slots that changed**, batched into one
packet per container change, sent to every viewer but the wearer, and the whole
set sent unprompted to anybody who has just come into view. Per-tick
coalescing was measured against Minecraft and **declined**.

## Context

[D16](0016-which-slot-an-item-is-worn-in.md) built the container half. An
armour slot took armour, a shield went to the offhand, a shift-click equipped,
and `tools/bot/clicks.js` agreed with a real 1.21.1 server 101 times out of
101. What it did not build was any way for a *second* player to know. There is
no entity-equipment packet in that work, so to everybody else in the world
every player was bare-headed and empty-handed — including the slot people look
at most, where a swung diamond sword rendered as a bare fist.

This is a purely observational feature. Nothing about it changes what a player
can do, which means the only three things it can get wrong are being absent,
being late, and being wrong about somebody. That is the whole of priority 1
here, and it is why the shape below prefers sending correct state slightly more
often to being clever about suppressing it.

## What was measured

`tools/bot/equipment.js`, new, and the same idea as `clicks.js`: it records
rather than asserts, so the same recording can be taken from Minecraft's own
server and the two diffed. Three bots, because equipment is the one packet a
server sends to every viewer **except** the player it is about — a wearer who
dresses, a watcher who was already here, and a **latecomer** who joins after
everything has already happened and then watches nothing change.

Every step records how many packets arrived and how many entries were in them,
not just the resulting picture. That is deliberate and it is the fix for a trap
this project has paid for twice: **a differential where both sides legitimately
send nothing agrees.** Two of the fifteen steps drop a stack into the middle of
the inventory, where saying nothing is the right answer; a recording that held
only the picture would call a server that broadcasts all six slots on every
click identical to one that broadcasts none.

Counts, because a rate would not say which step:

| recording | agreement with a real 1.21.1 server |
|---|---|
| Dust as built | **14 of 15**, the fifteenth named below |
| with the on-sight send removed | 13 of 15 |
| broadcasting the whole set on every container change | **1 of 15** |

The second and third rows are the check being watched fail. The first control
turns exactly one step red — the latecomer's — and turns it red in eight fields
at once, which is what a player who sees a naked stranger is looking at. The
second turns thirteen red, **including both of the steps whose right answer is
silence**, which is the whole reason the counts are recorded.

The wire cost was measured in `dust-server`'s own tests rather than estimated,
since the question is which form the protocol makes cheaper and an estimate
would have been a restatement of the hypothesis:

| body | bytes |
|---|---|
| one changed slot | **7** |
| the same change sent as all six slots | **17** |
| a player in full diamond, on sight, empty slots omitted | **37** |
| a player carrying nothing, on sight | **0 — no packet at all** |

## The decision: the difference, not the set

Entries in `minecraft:set_equipment` are self-delimiting — a high bit on the
slot byte says another follows — so there is no bitmask to fill in and no fixed
payload to pay for. An entry nobody needs is an entry nobody pays for *only if
it is not sent*. The protocol therefore makes the difference the cheaper wire
form, at 7 bytes against 17, and the roster sends the difference.

The comparison that produces that difference lives in `Roster::equipped`, which
takes the whole set and works out what changed. It is there and not at the call
sites because there are five places a player's container changes — a pickup, a
hotbar key, a creative write, a click and a close — and a rule spelled at five
call sites is a rule that is wrong at one of them. It also means a container
change that moved nothing visible sends nothing at all, which is every click in
the main inventory and most clicks a player makes.

The same packet carries a list, so however many slots one container change
moved leave together. A swap of hand and offhand is one packet of two entries;
a close that empties the crafting grid and the cursor is one packet of however
many that moved.

## The decision: the whole set, unprompted, on sight

The roster holds each player's six slots beside their posture, and for exactly
the reason it holds posture: a player who joins has to be told what everybody
is already wearing, and a broadcast carries what happens next rather than what
already happened. Without it the watcher's view is perfect and the latecomer
stares at a naked player until that player happens to change a slot — which is
the failure mode this feature exists to prevent, and which no check watching
only the actor can see.

Empty slots are left out of that send, because a client that has just been told
an entity exists already has all six empty. A player carrying nothing costs a
viewer no packet.

The equipment is published after the container is *restored*, not when the
player joins the roster: the roster takes a player before their inventory is
loaded, and a player who logged out in full armour would otherwise be naked to
everybody until their first click.

## What was declined: per-tick coalescing

The one difference from Minecraft, and it is the fifteenth row. Minecraft
coalesces equipment in its entity tracker, once a tick, so three creative
writes that arrive inside one tick leave as **one packet of three entries**.
Dust broadcasts per container change, so the same three leave as **three
packets of one entry each**. The entries are the same number and the resulting
picture is identical.

Declined for two reasons and neither is effort. The first is priority 1: a
tick-scheduled equipment broadcast is an equipment change that arrives up to a
tick late, and lateness is one of the three things this feature can get wrong.
The second is that no player can produce that burst — each piece of armour is a
separate click, seconds apart, and the only thing that writes three container
slots inside 50 ms is a script clicking a creative menu. Buying a packet back
from a case that does not occur, by making every case that does occur later, is
the trade backwards.

It is named in `equipment.js`'s comparison with its reason, in the same shape
`clicks.js` names Minecraft's rewrites: only the `packets` field of that one
step may differ, and a difference in any other field on any other step still
fails.

## What it costs with ten players, and with a hundred

Per-viewer and per-wearer, so the fan-out is quadratic in the players who can
see each other. Dust has no per-viewer visibility filtering yet — every roster
change goes to every session — so these are worst-case numbers for a region
where everybody can see everybody:

- **10 players.** A join costs the joiner 9 packets and at most 333 bytes of
  body, once. One player changing one slot costs 9 × 7 = 63 bytes.
- **100 players.** A join costs the joiner 99 packets and at most 3,663 bytes,
  once. One player changing one slot costs 99 × 7 = 693 bytes. All hundred
  changing a slot in the same second costs 69,300 bytes.

The comparison worth making is with movement, which shares the same quadratic
and is already on the wire: 20 position packets a second per player, to 99
viewers, is four to five orders of magnitude more traffic than equipment at any
plausible rate of players changing their armour. The n² term is not new and
equipment is not what will make it hurt; what will is the absence of a
visibility filter, which is a separate piece of work and is where the ceiling
should be raised.

## What this record does not settle

Whether a viewer should be told about a player they cannot see. Dust sends
every roster change to every session today, so equipment follows movement and
the join rather than inventing a rule of its own — a filter that applied to
equipment alone would hide a player's gear and then have nothing to re-send it
on when they came back into range, which is a worse failure than the cost it
saves. When view-scoped entity tracking lands, equipment on entering range is
already the packet this record specifies.
