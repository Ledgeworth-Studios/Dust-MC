# D30 — What a stale container click gets back

**Status:** Decided, 2026-09-03. A click quoting a sequence number that is not
the server's current one is **performed**, and then answered with the whole
container rather than with per-slot corrections. Measured against a real 1.21.1
server, which does the same. Dust's existing per-packet sequence numbering was
checked against the same server and left alone.

## Context

`Click Container` carries a `stateId`: the sequence number of the last
container update the client applied. It is the client saying *which* window it
clicked on. Dust read every other field of that packet and ignored this one.

The protocol's whole design for containers is that the client predicts, the
server replays, and only the disagreement goes back — which is right, and which
[D13](0013-where-a-players-inventory-lives.md) and
[D16](0016-which-slot-an-item-is-worn-in.md) built. A difference is only
meaningful against a picture both ends agree on, though, and `stateId` is
exactly the client saying it may no longer hold that picture. Correcting a
handful of slots against a container the client has lost track of leaves every
*other* slot it is wrong about wrong until something happens to move it — which
is a player looking at an inventory that is not theirs, for as long as they
leave it alone.

## What was measured

`tools/bot/clicks.js --stateid`, new, and an extension of the instrument that
was already there rather than a second one. Seven clicks, each claiming nothing
changed so the server has to state everything it believes, quoting in turn the
number the server last sent and a number five behind it. It records **how many
packets came back and how many distinct sequence numbers were on them**, not
just the resulting container — because two servers can put a container into the
same shape and say a very different amount doing it, and the amount is the
whole finding. `clicks.js` at 101 of 101 could not see any of this.

| recording | agreement with a real 1.21.1 server |
|---|---|
| Dust before | **3 of 7** — every stale row |
| Dust after | **7 of 7** |
| after, with the staleness comparison forced to `false` | 4 of 7 |

The sharpest row is the last one: *a click that changes nothing, quoting a
stale number.* Fresh, both servers answer it with silence. Stale, Minecraft
answers it with the entire forty-seven-stack container. That is not a
correction — nothing was wrong with the click — it is a **repair**, and it is
the row that says what the field is for.

The existing checks were re-run against the change, because it alters what
every click gets back: `clicks.js` is still **101 of 101** and `--predict` is
still 3 of 3.

## The decision: perform the click, then state the container

The click still happens. A stale number says the client is out of date, not
that it clicked on nothing, and a server that discarded the click would make a
player's inventory stop responding whenever their connection hiccuped.

Then the whole container. Forty-seven stacks is the expensive answer and it is
the one that ends the disagreement in a single packet.

Per-slot corrections on a stale click were the alternative and are what Dust
did. Rejected on priority 1: it is cheaper on the wire and it leaves a player
looking at an inventory that is not theirs, which is the worst failure a
container can have — a stack that is not where the screen says it is, until
they happen to touch that slot.

## What was checked and left alone: one sequence number per packet

The first draft of this change also made a *batch* of corrections share one
sequence number, on the reasoning that a client which quoted an intermediate
number would be told it was stale and be sent a container it did not need.

That reasoning was wrong, and the measurement is what said so. Minecraft
stamps a **fresh number on every packet**: a click that came back as two
`set_slot`s came back carrying two different numbers, which is the
`stateIds` column in the table above and the reason that column exists. Dust
already did the same, so the batching change was reverted and only the
staleness comparison kept. **A differential run against the source of your own
hypothesis is not a differential** — and neither is one run against your
recollection of the source.

The same run settled the other half: a click a server answers with **silence**
spends no sequence number, on both servers, so a client's next click is not
accidentally stale.

## What is not guarded by `just verify`

The staleness comparison itself. It is one line in the session loop and the
check that covers it is the mineflayer differential, which needs two running
servers and is outside `verify` for the reason every bot check is. What `verify`
does hold is the contract the comparison rests on: `Inventory::state_id()`
returns the last number *sent*, not the next one. A version of that returning
the next number would make every honest click look stale and answer all of them
with the whole container — no crash, no wrong item, and invisible to any test
that only looks at slots.

## Traps found

`clicks.js` attached its packet tracker in a `spawn` handler, which is too
late. A join burst arrives in one TCP read and node runs every packet handler
for that read synchronously, while the microtask a `spawn` listener resolves
runs after all of them — so the tracker missed the container the server sent on
join, along with its sequence number. Every "fresh" click in the first run was
quoting the 0 it had never been told otherwise, and the run reported Dust
failing a row it passes. `tools/bot/equipment.js` was bitten by the identical
thing on the same day from the other end, and both now attach at construction.
