# D12 — How fast a player may say they moved

**Status:** Decided, 2026-09-02. Ten blocks per tick, checked on every movement
packet, corrected with a teleport. Collision against the world is **not** part
of it, and the last section says why that is a separate decision rather than an
omission.

## Context

Until this, a movement packet was believed. The README said so in its "Not yet"
paragraph and `dust_guard::Reach` said so in its own documentation: the position
a reach was measured from was whatever the client last claimed, so the reach
check refused acting far from where a player *said* they were and had nothing to
say about the saying. A client that lied about its position walked around it.

The check had to go somewhere that could be tested without a socket, which is
the precedent `Reach` set and the reason it is a crate rather than a method on a
session. So this is `dust_guard::Movement`: plain numbers in, a verdict out.

## What was measured

A threshold argued about in the abstract is a threshold that rubber-bands
somebody on bad wifi, so the first thing built was not the check. It was
`tools/bot/movement.js`: it drives mineflayer — whose physics is
`prismarine-physics`, an independent reimplementation that shares no code with
this project — through the motions a player actually makes, hooks the client's
own packet writer, and counts the displacement in every position packet that
leaves it.

**1,217 packets.** Blocks moved per packet, and a packet is a client tick:

```text
phase                            <0.05  0.05-0.1  0.1-0.2  0.2-0.3  0.3-0.4  0.4-0.6  0.6-0.8   0.8-1    1-1.5    1.5-2      2-3      3-5      n     max
standing still                       3         0        0        0        0        0        0       0        0        0        0        0       3   0.000
walking                              0         1        3       96        0        0        0       0        0        0        0        0     100   0.216
sprinting                            0         0        2       98        0        0        0       0        0        0        0        0     100   0.281
sprint-jumping                       0         0        0        6       67       38        9       0        0        0        0        0     120   0.742
flying up 300 blocks                 0         0        0        0        0      596        0       0        2        0        0        0     598   1.000
flying forward                       2         2        8       68        0        0        0       0        0        0        0        0      80   0.283
falling                             13         0        3        1        2        3        3       3       10       12       36       50     136   3.580
walking through a 700 ms stall       0         1        3       76        0        0        0       0        0        0        0        0      80   0.216
```

Three things in that table decided the design.

**The largest honest step is a free fall, and it was still accelerating.** 3.580
blocks in one tick after a 300-block drop; a longer fall converges on 3.92,
where Minecraft's drag balances its gravity. Everything that is not a fall is
under 1.0, and ordinary walking is 0.216.

**A stall does not produce a big step.** The 700 ms row is a walk through a
connection that stopped and then delivered everything it had queued, and its
displacement column is *identical* to the ordinary walk: 0.216 at the top. A
client that stalls keeps ticking and keeps writing one packet per tick — the
packets bunch, the steps do not. So a budget charged by the clock refuses an
honest client for arriving *early*, and the budget is floored at one full tick
per packet whatever the elapsed time says. This is the single most important
line in the whole record: it is the failure mode that would have been invisible
in a test and unmissable to somebody standing in the world.

**Creative flight is not physics.** 1.000 blocks per tick, in a flat band, with
no acceleration — the client simply moves. It is inside the limit, but it is a
reminder that "what a player can do" is not the same question as "what the
physics produces", and the next answer to it (elytra, riptide) will be faster.

## The decision

**Ten blocks per tick**, `[server] movement_speed_limit`, default on.

That is vanilla's own constant for a player not flying an elytra — its check
compares a squared distance against 100 — and it is 2.8 times the fastest thing
measured above. The headroom is the setting's real content rather than slack:
elytra, riptide, knockback and TNT boosts all move a player faster than walking
and **none of them exist in this server yet**. A limit tuned to what Dust can do
this month is a limit that starts correcting players the month after, and by the
project's first priority a correction that fires on honest play is a defect and
not a tradeoff. When elytra land, vanilla's answer is to widen the same number
to 300 (17.3 blocks a tick) while flying one; that is the change to make, and
making it is a decision rather than an emergency because the check will not have
been firing in the meantime.

The configuration floor is **4**, checked at boot: below 3.92 the server would
teleport people back for falling off a cliff. `inf` is a legal value and turns
the speed bound off, which is how an operator with movement mods says so without
a magic zero.

Three further choices, each in the player's favour:

- **The budget grows as the square of the elapsed ticks**, clamped to five.
  Distance is speed times time and this is a squared quantity, so two ticks is
  four times the budget. Vanilla multiplies its squared constant linearly, which
  is tighter and only never fires because the constant is eight times what
  honest play produces; the correct relation costs one multiply.
- **A correction is never answered with another correction.** The packets
  already in flight when the teleport went out describe a player who no longer
  exists; refusing each of them sends another teleport, which is how a
  rubber-band becomes a loop. Until the client acknowledges the teleport id, its
  movement packets are dropped in silence.
- **A lost acknowledgement does not freeze a player.** If a client sends a
  position that passes the check on its own merits while a correction is
  outstanding, it is believed and the wait ends. It cannot be used to skip
  anything: it accepts only what the check already allows.

The correction itself is a `player_position` with the two rotation bits marked
**relative and sent as zero**, so the player moves and their view does not. An
absolute rotation would snap a corrected player's head to whatever yaw the
server last heard, which is a second jolt on top of the one they are already
having.

## What was declined

**Collision against the world.** A player may still walk through a wall at a
walking pace. Doing it properly means the player's bounding box against every
block state's shape, twenty times a second per player, on a chunk that may not
be loaded — it is a decision about the world and the hot path together, and it
is worth its own record and its own measurement rather than being smuggled into
this one. What it would add is real; what this already closes is the shape the
README named, the client that claims to be somewhere it could not have got to.

**A tighter bound.** A steady nine-blocks-a-tick speed hack is not caught. That
is 180 blocks a second and it is plainly cheating, and this will not say so —
the same hole vanilla has, for the same reason. Closing it means arguing with
falls, elytra and knockback, and the first priority says which way that goes.

**An off-by-default setting.** Considered, because a validator that is on and
wrong is worse than one that is off. It is on because the number is measured
rather than assumed and every honest packet in the table above clears it by a
factor of three, and because a check nobody enables protects nobody.

## How it is known to work

`tools/bot/movement.js --check` walks the whole table above against a running
server and then claims to be 707 blocks away. Against a server built from this
branch the correction arrives and the position packet puts the player back;
against a server built without it, the same run prints `FAIL ... no correction
arrived`. `a_player_who_claims_to_be_across_the_map_is_put_back` is the same
thing over a socket in CI, and it goes red when `Movement::claimed` is stubbed
to accept — which is how it was written.
