# D24 — How a stack carries its components

**Status:** Decided, 2026-09-03. A component is **walked, not modelled**: the
fifty-seven layouts say where each component ends, and the bytes themselves are
kept, compared and returned exactly as they arrived. Two stacks merge only when
their canonical component bytes are equal. The type *ids* come from the
operator's own jar, never from a table written here.

## Context

[D13](0013-where-a-players-inventory-lives.md) built the forty-six-slot
container and [D16](0016-which-slot-an-item-is-worn-in.md) taught it which slot
an item is worn in. Both said, in the same words, that the thing neither of them
did was components — and `dust_protocol::types::Slot` had a long doc comment
explaining why partial credit was not on offer:

> A component carries no length. It is a VarInt type id followed by that type's
> own layout, and there are around a hundred types, each different. So a reader
> that meets a component it does not know cannot skip it — it does not know
> where the component ends, and therefore does not know where the next one
> begins, or where the slot ends, or where the *packet* ends.

That paragraph is correct and it is still true. What it does not say is that
**walking a component is much less work than modelling one.** To keep a name, an
enchantment or a shulker box's contents, this server never has to know what any
of those mean; it has to know where each of them ends.

Which mattered, because the gap was not cosmetic. An enchanted pickaxe came back
plain. A named sword lost its name. Worst of all, two stacks that differed only
in their components **merged**, which is not just loss — it is loss that looks
like the server tidying up.

## What was decided

**Layouts here, numbers from the jar.** The fifty-seven layouts are protocol
knowledge of exactly the kind the packet bodies beside them are, they are in
none of Mojang's reports, and they are written by hand in
`dust-protocol/src/components.rs` — **keyed by name**. The names' protocol ids
are Minecraft's: they are positions in `minecraft:data_component_type`, that
registry is already extracted from the operator's own jar, and `dust-server`
hands the id-to-name lookup down at boot. D16 declined a `dust-items.tsv` column
because the tags already answered the question; this declines a hard-coded
`custom_data = 0` for the same reason and it is the same reason: a second answer
to one question goes stale on its own.

**Byte equality, and which direction it can fail in.** Two stacks merge when
their component patches are equal, and equality is byte equality of a canonical
form — entries sorted by type id, duplicates refused. Vanilla compares parsed
values, so the two can disagree: a patch spelled differently (a different order,
a different key order inside an NBT compound) compares unequal here. **That is
the safe direction and it is the only direction available.** Every component
codec is injective, so distinct values cannot collide; byte equality can only
fail to merge two stacks vanilla would join, which a player sees as two stacks
of thirty-two rather than as an item destroyed or duplicated. Canonicalising on
the way in removes the ordinary case of even that.

**One `Option<Arc<[u8]>>` per stack** holding the canonical wire tail. `None`
for the overwhelming majority of stacks, which allocate nothing and compare in
one branch; sending a slot is a `memcpy` rather than a re-encode. `Stack` lost
`Copy` and pays clones on the click path, which is the cheaper half of that
trade. (Priority 2.)

**The save writes the version beside the bytes.** A component type id is a
position in a table Minecraft regenerates, so the same eleven bytes are an
enchantment in one version and a food value in the next. A file whose components
another version wrote loads with its items, without their components, and logs
how many it dropped.

## What was measured

`tools/bot/clicks.js --components` sends a component-bearing stack from a
third-party client and then asks the server to **state what it holds**: two
clicks per component, each one claiming that nothing moved. The claim is false
both times, so a server that stayed silent would be reporting an empty
inventory rather than agreeing — which is the trap
[D16](0016-which-slot-an-item-is-worn-in.md) recorded, applied here from the
start. Counts, against a real 1.21.1 server on the same script:

| | |
|---|---|
| corpus snapshots | 117 |
| slots either server never spoke about | **0** |
| snapshots where the two agree | **107** |
| differ for a reason named in the script | 10 |
| differ for no named reason | **0** |

The ten are Minecraft rewriting a value it understands and Dust echoing bytes it
deliberately does not: an enchantment list reordered by the map it lives in, a
profile resolved against Mojang, a lodestone position cleared, and two
components dropped outright because an empty compound is not a valid value for
them. Each is named in the script with its reason, and a difference that is
*not* named still fails — otherwise the list would be a way to make red go away.

`clicks.js`'s existing hundred-click survey still agrees 101 of 101 and
`--predict` still passes 3 of 3.

**Watched to fail.** Making `hide_tooltip` cost one byte — one byte, in one of
the four components whose whole value is that they are present — took the
components survey from 0 unnamed disagreements to **8**, and left the server
silent about 8 slots it could no longer decode. Making `Stack::stacks_with`
compare the item and not the components took it to **2**, and the diff is the
harm in one line: `stone x32` where a real server has sixteen named Bob and
sixteen plain.

## What the differential found, which is the point of running one

Dust's layouts were drafted from minecraft-data 3.115, a third-party dataset,
and then checked against Minecraft's own decoder. **Two of them were wrong, and
both were wrong in the dataset first:**

- **`potion_contents` had a fourth field.** minecraft-data gives 1.21.1 an
  optional custom name that arrived in 1.21.2. A real 1.21.1 server answered a
  stack carrying it with "was larger than I expected, found 1 bytes extra".
- **`food`'s `usingConvertsTo` is an *optional* stack, not a bare one.** The two
  agree when there is nothing to leave behind — a count of zero and a flag of
  false are both one zero byte — and disagree the moment there is a bowl. Given
  the bare form a real server answered "Failed to decode", which is reading
  *past* the end rather than stopping short; a bare-stack reader consumes those
  bytes exactly, so the complaint can only be the flag.

A third disagreement went the other way and is worth the same space:
minecraft-data says a food effect is a VarInt and a float, Dust says it is a
whole effect instance and a float, and the server refused minecraft-data's
shape by reading past the end. **Dust was right and the dataset was wrong.**

The transferable part is not any of the three. It is that **a hypothesis source
and a differential are different things**, and the project's own rule — a
differential cannot catch a rule that is wrong on both sides — bites hardest
when the reference *is* where the rule came from. The rule came from
minecraft-data; the check was Minecraft.

## What was declined

**Modelling the components.** Fifty-seven typed Rust structures would let this
server render a name, sort by enchantment and count a shulker box's contents. It
would also be a large surface with no caller: nothing here crafts, enchants,
renames or drops. Walking is what carrying requires and it is what was built.

**Re-encoding what arrives.** Minecraft normalises a text component and Dust
does not, so `{"text":"Bob"}` comes back as it was sent rather than as `"Bob"`.
Matching that would mean modelling text components, which is the thing above.
Both render "Bob"; neither loses anything.

**Refusing an unknown component type.** A *removal* is a bare type id and needs
no layout, so one is accepted whatever it names — refusing it would be this
crate's ignorance costing a player a click. An *addition* whose type has no
layout is refused by name, because there is nothing else honest available.

**Bumping the save version to 3.** A version 2 file has no component keys and
reads as exactly what it is. The encoding version is written *in* the file
instead, because it answers a different question — not "can this reader parse
these fields" but "do these bytes still mean what they meant".

## What is still wrong

A component-bearing stack still cannot be *made* here: nothing crafts, enchants,
renames or drops, so every one of them arrives from a creative client. And a
click carrying a stale state id gets per-slot corrections from Dust where a real
server answers with the whole container, so a client that desynchronises stays
that way until something else corrects it. Neither is a component defect; both
are the next things a player would meet.
