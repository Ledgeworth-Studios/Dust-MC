# D8 — Block opacity and light emission

**Status:** Open, with the options costed and the cost of the gap measured.
Decided when somebody chooses a source; nothing is blocked on it that is not
already stated as a known gap.

## Context

Dust's light engine asks one question of every block: how much light does
entering this cell cost? Today the answer is **zero for air and fifteen for
everything else**. Minecraft's answer is not binary — glass costs nothing,
water and leaves cost one, ice costs three — and neither is its light
*emission*, which Dust has no model for at all. There is no block light in
Dust, and every block but air is a wall to sky light.

Both numbers are code constants in Minecraft. They are in no `--reports`
output, in no data pack, and in nothing `xtask extract` can reach. That is the
whole of the problem: this is not a piece of work nobody has done, it is a
piece of data nobody can currently get.

## What the gap actually costs, measured

`cargo xtask harness light --version 1.21.1 --seed 0 --radius 4` reads the
light Minecraft computed into its own region files, lights the same chunks with
Dust's engine, and compares cell by cell.

**99.41% of cells agree, at radius 2, 4 and 6 alike, and every single one of
the disagreements is Dust being darker.** What the shortfalls are standing in is
the diagnosis rather than a guess:

```text
minecraft:oak_leaves      the largest share
minecraft:water           the next
minecraft:birch_leaves
minecraft:air             the cells those shadows fall into
minecraft:short_grass, seagrass, flowers
```

Every one of them a block Minecraft gives an opacity of one or two and Dust
treats as a wall. So the cost of this decision is known to three decimal places
and its cause is named block by block, which is an unusually good position from
which to not have decided something.

## Options

**1. Extract it from the server jar.** A small Java program on the jar's
classpath, using the ProGuard mappings already downloaded beside it
(`server-mappings-1.21.1.txt`), reflecting over the block registry and printing
`getLightBlock` and `getLightEmission` per state.

The right answer in principle — it produces Minecraft's own numbers, at the
operator's machine, from the operator's jar, which is exactly D6 and D7's rule.
The cost is that Minecraft's static initialisation has to run, which means
`Bootstrap.bootStrap()` through obfuscated names, which is what mod-loader-based
extractors exist to avoid doing by hand. Not attempted; not costed beyond
"hours, with a real chance of not working".

**2. Derive it from tags.** Dust now holds all thirteen tag registries. A rough
opacity model could be built from `minecraft:leaves`, `minecraft:impermeable`
(glass), the flower and replaceable groups, and the fluid registry — which is
very nearly the list the measurement above names.

Rejected as a *silent* approximation by D6 and D7, both of which refuse derived
values on the grounds that a wrong number is invisible. **That objection is
weaker than it was**, because `harness light` makes exactly this kind of error
visible: a derived table can be adopted and its accuracy reported as a
percentage against Minecraft's own answer. What it cannot do is stop being an
invention. A tag is not an opacity, and mapping one to the other is choosing
numbers.

**3. Take them from the operator, like the registry contents.** A file in the
`[data]` directory. Honest, and it moves the problem to whoever has to write the
file, which is nobody.

**4. Leave it.** What is happening now. Sky light is 99.41% right, wrong in one
direction, and wrong in a way that is written down where the code lives.

## Why this is not decided here

Option 2 is an afternoon's work and would visibly improve the world a player
sees. It is also the one that puts numbers in this repository that no
measurement produced and no extraction justified, against two decision records
that say values come from the operator's own copy of the game. That is a
judgement about the project's line and not about lighting, and it wants a
decision rather than a commit.

The measurement is the input. This record consumes it, exactly as D4 waits on
Phase 10's.

## Consequences of leaving it

- **No block light at all.** Torches, lava and glowstone light nothing, and
  there is no engine work outstanding for it — `dust_world::propagation` runs
  the same walks vanilla does. It is waiting on emission values and nothing
  else.
- **Sky light stops at the surface of an ocean and under a tree**, which is
  visible to a player and is the largest share of the 0.59%.
- **`opacity_of` is the one place this is decided**, and it says so. Whichever
  option is taken changes that function and nothing else.

## Related

* D6 — the ore baseline, which established that vanilla's numbers arrive from
  the operator's jar rather than from here.
* D7 — registry contents, which extended the same rule to the wire.
