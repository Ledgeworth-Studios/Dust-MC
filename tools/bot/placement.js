// What state does Minecraft put down when a player right-clicks?
//
// Dust places every block in its default state, because it has no placement
// context to compute another one from. Minecraft computes one per block, in
// Java — `Block.getStateForPlacement` — from the face that was clicked, where
// on it the cursor was, and which way the player was looking. That method needs
// a `Level` to run, and `Level` is an abstract class rather than an interface,
// so the reflection the block oracle uses cannot reach it. **A running server
// can be asked instead**, one placement at a time, and this writes the answers
// down.
//
// It asks **vanilla**, and only vanilla. What it produces is a file of
// Minecraft's own answers — the same kind of thing `xtask extract --only
// constants` produces and under the same rule, so it lands in the harness cache
// on the operator's own disk and no row of it is committed. Comparing Dust
// against those answers is a separate job in a separate place, because it is a
// comparison and not a measurement.
//
// Usage: node placement.js <port> [items.txt|item,item,...] [--survey] > answers.tsv
//
// `--survey` trades the full 144-situation grid for eight chosen to answer only
// "does this block's placement depend on anything at all?", which is the
// question worth asking of every placeable item rather than of a handful.
//
// Notes to whoever runs it next:
//
//   * The arena is built from the **server console**, not by the bot: the bot
//     is not opped and does not need to be. Start the server with its stdin on
//     a pipe and point DUST_SERVER_CONSOLE at it; `tools/bot/README.md` has the
//     two lines that do it.
//
//   * **What is known about the arena is forgotten before the commands that
//     change it go out, not after.** The barrier is "wait until the support
//     turns to stone", and console changes arrive in the order the console ran
//     them, so seeing that means the clearing fill before it has landed too.
//     Waiting without forgetting first matches the support's stone from the
//     *previous* sample and returns immediately — which put `air` in
//     twenty-two rows of a run, every one of them a down-face placement,
//     because that is the cell the fill happened to reach last.
//
//   * **A refusal arrives as air.** The client predicted a block; the server
//     answers by telling it what is really there. No item places air, so there
//     is nothing to confuse it with — and the reader on the other side applies
//     the same rule, because a comparison of `minecraft:air` against anything
//     is a comparison against a placement that did not happen.
//
//   * The **first** change after a placement is the placement, and the last one
//     may not be: a door with nothing under it is put down and breaks on the
//     next tick. Reading the last change recorded `air` for forty-seven of a
//     door's situations. Whether it survived is a support rule rather than a
//     placement rule, so it is a column of its own.
//
//   * The state is read out of the block-change packets and never out of
//     `bot.blockAt`. The bot's own world lags a placement by an unbounded
//     amount; read that way, this reported the *previous* sample's block.
//
//   * **Both** block-change packets. A server sends `block_change` when exactly
//     one block in a section changed in a tick and `multi_block_change` when
//     more than one did — so the arena's `/fill` arrives the second way, and so
//     does a *door*, which puts down two blocks. Listening to one of them makes
//     every door read as REFUSED and every arena read as never settling.
//
//   * Yaw is set with `bot.look()` and not by writing `position_look`.
//     mineflayer's physics loop sends a position every tick and overwrites a
//     hand-written one, which makes every stair come out facing the same way
//     whatever was asked for.
//
//   * **The look is the outer loop and the face is the inner one**, which is
//     not a tidiness choice. A look reaches the server on mineflayer's next
//     physics tick, and a placement sent before it arrives is measured against
//     the *previous* sample's rotation. Turning twelve times an item instead of
//     a hundred and forty-four leaves one settling point per turn rather than
//     one per sample — and the first run of this tool, which turned every time,
//     produced exactly one poisoned row in 2,448: a piston at yaw 270 pitch 90
//     that came out facing west, which is where the sample before it had been
//     looking. One row in two thousand is the rate at which a measurement stops
//     being worth quoting.

const mineflayer = require('mineflayer')
const fs = require('fs')

const PORT = Number(process.argv[2] || 25565)
const VERSION = '1.21.1'

// The four horizontal directions, in Minecraft's own yaw convention: 0 is
// south (+z) and it turns clockwise seen from above.
const YAWS = [0, 90, 180, 270]
// Straight up, level, straight down. Enough to separate a block that reads the
// horizontal direction from one that reads the nearest looking direction.
const PITCHES = [-90, 0, 90]
// Low and high on the clicked face, which is what a slab and a trapdoor read.
const CURSORS = [0.25, 0.75]
const FACES = [0, 1, 2, 3, 4, 5]

// The reduced grid, for the question "does this block's placement depend on
// anything at all?" — which is the one worth asking of all 925 placeable items,
// where the full grid is worth asking of a handful.
//
// One baseline and one change from it per input, plus the two faces whose
// answers differ for reasons other than the inputs.
//
// **It undercounts, and by how much is knowable rather than mysterious.** What
// it varies is the four numbers a right-click carries; what it cannot vary is
// the block's *surroundings*, because the arena is one stone block in a cleared
// volume with nothing beside it. A stair's `shape` is computed from the stairs
// next to it, a chest becomes half of a double chest beside another, a fence
// connects to what it touches, and redstone wire reads all four neighbours.
// None of that shows up here. So a block this survey calls context-free is one
// whose *placement* reads nothing, and it may still owe a neighbour rule —
// which is a different problem, in a different place, and worth measuring
// separately rather than folding in and losing.
const SURVEY = [
  { yaw: 0, pitch: 0, face: 1, cursorY: 0.25 },   // the baseline: on the top
  { yaw: 0, pitch: 0, face: 0, cursorY: 0.25 },   // and on the bottom
  { yaw: 0, pitch: 0, face: 2, cursorY: 0.25 },   // and on a side
  { yaw: 0, pitch: 0, face: 2, cursorY: 0.75 },   // the cursor, where it is read
  { yaw: 0, pitch: 0, face: 5, cursorY: 0.25 },   // the opposite side
  { yaw: 90, pitch: 0, face: 1, cursorY: 0.25 },  // the yaw
  { yaw: 0, pitch: -90, face: 1, cursorY: 0.25 }, // the pitch, looking up
  { yaw: 0, pitch: 90, face: 1, cursorY: 0.25 }   // and looking down
]

/// The control, run first and every time.
///
/// `minecraft:stone` has exactly one state, so every situation in the grid has
/// to produce the same answer. It is not a test of the server — it is a test of
/// *this tool*, and it has earned its place twice: a run that read the bot's
/// cached world reported the previous item's block, and a run whose `/fill`
/// raced the read put `air` in twenty-two rows. Both showed up here first, as a
/// control disagreeing with itself, before anything downstream could be
/// believed.
const CONTROL = 'stone'

const DEFAULT_ITEMS = [
  // One per behaviour this is trying to tell apart. The control is prepended
  // to whatever list this is, so it is not in here.
  'oak_stairs',       // horizontal facing, half, shape
  'oak_slab',         // type from the cursor
  'oak_log',          // axis from the face
  'furnace',          // horizontal facing, opposite the look
  'observer',         // nearest looking direction, including vertical
  'piston',           // likewise
  'torch',            // wall variant chosen by the face
  'ladder',           // facing from the face, and refused on most of them
  'lever',            // face and facing together
  'oak_trapdoor',     // half from the cursor, facing from the look
  'oak_door',         // two blocks, hinge from where on the face
  'oak_sign',         // rotation, sixteen of them
  'chest',            // horizontal facing and a neighbour rule
  'repeater',         // facing, and it needs support
  'end_rod',          // facing straight off the face
  'hopper'            // facing off the face, but never up
]

const wait = ms => new Promise(r => setTimeout(r, ms))

function properties (stateId, registry) {
  const block = registry.blocksByStateId[stateId]
  if (!block) return { name: `state:${stateId}`, props: {} }
  const props = {}
  let rest = stateId - block.minStateId
  for (const state of (block.states || []).slice().reverse()) {
    const n = state.num_values
    const v = rest % n
    rest = Math.floor(rest / n)
    // prismarine orders a bool's values true-then-false, which is the opposite
    // of the reading order and is worth the extra line rather than the bug.
    //
    // **An int is looked up in `values` like an enum, and printing `v` is
    // wrong.** `v` is the index and the values need not start at zero:
    // `snow[layers]` runs 1..8, `candle[candles]` 1..4, a leaf's `distance`
    // 1..7. Printing the index reported `snow[layers=0]`, which is not a state
    // Minecraft has, and the run then disagreed with a server that was right.
    // Nineteen blocks were scored against Dust that way before it was found,
    // and the control could not catch it: stone has no properties.
    props[state.name] = state.type === 'bool'
      ? (v === 0 ? 'true' : 'false')
      : state.values
        ? state.values[v]
        : String(v)
  }
  return { name: block.name, props }
}

/// A state in the spelling everything on the Rust side uses: the namespaced
/// name, then the properties in name order.
///
/// Namespaced because that is what `dust_registry::Block::name` returns and
/// what the reader of this file will compare against — prismarine drops the
/// namespace and a comparison that had to put it back would be one more place
/// for the two vocabularies to disagree.
function describe (stateId, registry) {
  const { name, props } = properties(stateId, registry)
  const qualified = name.includes(':') ? name : `minecraft:${name}`
  const kv = Object.entries(props).map(([k, v]) => `${k}=${v}`).sort().join(',')
  return kv ? `${qualified}[${kv}]` : qualified
}

/// Every situation to try, grouped so that the look changes as rarely as
/// possible — see the note about the outer loop at the top of this file.
function situations (survey) {
  const list = survey
    ? SURVEY.slice()
    : YAWS.flatMap(yaw =>
      PITCHES.flatMap(pitch =>
        FACES.flatMap(face => CURSORS.map(cursorY => ({ yaw, pitch, face, cursorY })))))
  // A stable sort by rotation. `sort` is stable in every engine this runs on,
  // so the order within one rotation is the order written above.
  return list.sort((a, b) => (a.yaw - b.yaw) || (a.pitch - b.pitch))
}

function main () {
  const survey = process.argv.includes('--survey')
  const argument = process.argv.slice(3).find(a => !a.startsWith('--'))
  let items = DEFAULT_ITEMS
  if (argument) {
    items = fs.existsSync(argument)
      ? fs.readFileSync(argument, 'utf8').split(/\s+/).filter(Boolean)
      : argument.split(',')
  }

  const console_ = process.env.DUST_SERVER_CONSOLE
  if (!console_) {
    process.stderr.write(
      'DUST_SERVER_CONSOLE must name a pipe the server reads its console from.\n' +
      'The arena is built with /fill and /setblock, which a bot cannot run.\n'
    )
    process.exit(2)
  }
  const say = line => fs.appendFileSync(console_, line + '\n')

  const bot = mineflayer.createBot({
    host: '127.0.0.1', port: PORT, username: 'Placer', auth: 'offline', version: VERSION
  })

  // Every state a cell has held since it was last forgotten, in order. A list
  // rather than a value because the *first* change after a placement is the
  // placement and the last one may not be: a door with nothing under it is put
  // down and breaks on the next tick, and a tool that read the last change
  // recorded `air` for forty-seven of a door's situations.
  const changes = new Map()
  const at = p => `${p.x},${p.y},${p.z}`
  const record = (position, state) => {
    const key = at(position)
    const seen = changes.get(key)
    if (seen) seen.push(state)
    else changes.set(key, [state])
  }
  bot._client.on('block_change', p => {
    record(p.location, p.type)
  })
  // **Both** packets, and forgetting the second one cost an afternoon. A
  // server sends `block_change` when exactly one block in a section changed in
  // a tick and `multi_block_change` when more than one did, so a `/fill` and a
  // *door* — which puts down two blocks — both arrive the other way. A tool
  // that listens to one of them reads a door as REFUSED and never sees its
  // arena settle.
  bot._client.on('multi_block_change', p => {
    const section = p.chunkCoordinates
    for (const entry of p.records) {
      // stateId in the high bits, the position within the section in the low
      // twelve. Divided rather than shifted: a state id of 26,000 shifted left
      // by twelve is still inside a 32-bit int today, and `>>>` is a promise
      // about a width this has no reason to make.
      const state = Math.floor(entry / 4096)
      const packed = entry % 4096
      record({
        x: section.x * 16 + ((packed >> 8) & 0xf),
        y: section.y * 16 + (packed & 0xf),
        z: section.z * 16 + ((packed >> 4) & 0xf)
      }, state)
    }
  })

  /// Wait until the cell at `pos` reads `want`, or give up.
  ///
  /// The confirmation the whole measurement rests on: a console command and a
  /// placement are two independent things and nothing else orders them.
  async function settles (pos, want, registry, tries = 120) {
    for (let i = 0; i < tries; i++) {
      const seen = changes.get(at(pos))
      if (seen && properties(seen[seen.length - 1], registry).name === want) return true
      await wait(25)
    }
    return false
  }


  bot.once('spawn', async () => {
    await wait(2500)
    const registry = bot.registry
    const base = bot.entity.position.floored()
    const floor = base.y - 1

    // The arena: a floor to stand on, air above it, and one support block the
    // whole run reuses. Reusing it rather than walking a row keeps the bot in
    // one place, which keeps every sample the same distance from the eye and
    // therefore inside any reach limit either server applies.
    const ax = base.x + 8
    const az = base.z
    say(`fill ${ax - 4} ${floor} ${az - 4} ${ax + 4} ${floor + 8} ${az + 4} air`)
    say(`fill ${ax - 4} ${floor} ${az - 4} ${ax + 4} ${floor} ${az + 4} stone`)
    say(`tp Placer ${ax + 0.5} ${floor + 1} ${az + 2.5}`)
    await wait(1500)

    const support = { x: ax, y: floor + 2, z: az }
    const offsets = [[0, -1, 0], [0, 1, 0], [0, 0, -1], [0, 0, 1], [-1, 0, 0], [1, 0, 0]]

    // The support, once, before the first sample. Without this the first row
    // of every run is `ARENA DID NOT SETTLE`: the loop below waits for the
    // block_change that *changes* the support, and the first time round there
    // is nothing there to change.
    say(`setblock ${support.x} ${support.y} ${support.z} stone`)
    if (!await settles(support, 'stone', registry, 200)) {
      process.stderr.write('the arena never appeared; is the console pipe connected?\n')
      process.exit(1)
    }

    const grid = situations(survey)
    let sequence = 1
    // The control first, whatever was asked for, and its answers are checked
    // before any of the rest are printed.
    const seen = []
    process.stdout.write('# item\tface\tyaw\tpitch\tcursor_y\tresult\tsurvived\n')
    for (const item of [CONTROL, ...items.filter(i => i !== CONTROL)]) {
      const entry = registry.itemsByName[item]
      if (!entry) {
        process.stdout.write(`${item}\t-\t-\t-\t-\tNO SUCH ITEM\n`)
        continue
      }
      bot._client.write('set_creative_slot', {
        slot: 36,
        item: {
          itemCount: 1,
          itemId: entry.id,
          addedComponentCount: 0,
          removedComponentCount: 0,
          components: [],
          removeComponents: []
        }
      })
      bot._client.write('held_item_slot', { slotId: 0 })
      await wait(120)

      let looking = null
      for (const { yaw, pitch, face, cursorY } of grid) {
        if (looking !== `${yaw},${pitch}`) {
          // Radians, and mineflayer's own physics loop is what actually sends
          // it. The settling wait is here and not beside the placement because
          // this is the only place the rotation changes — see the note about
          // the outer loop at the top of this file.
          await bot.look((yaw * Math.PI) / 180, (pitch * Math.PI) / 180, true)
          await wait(250)
          looking = `${yaw},${pitch}`
        }

        const off = offsets[face]
        const target = {
          x: support.x + off[0],
          y: support.y + off[1],
          z: support.z + off[2]
        }
        // Forget everything known about these two cells *before* the commands
        // that change them go out. This is the whole barrier and the line that
        // was missing: without it, `settles` below matched the support's stone
        // from the *previous* sample and returned before the fill had been
        // delivered, which put `air` in twenty-two rows of a run — every one of
        // them a down-face placement, because that is the cell the fill
        // happened to reach last.
        changes.delete(at(support))
        changes.delete(at(target))
        // A clean slate: the volume rather than the two cells, because a door
        // is two blocks tall and leaves its top half behind and a bed would be
        // worse. The fill empties the support too, so the setblock after it
        // always changes something and always emits.
        say(`fill ${support.x - 2} ${support.y - 2} ${support.z - 2} ` +
            `${support.x + 2} ${support.y + 3} ${support.z + 2} air`)
        say(`setblock ${support.x} ${support.y} ${support.z} stone`)
        // Seeing the support turn to stone means everything the console ran
        // before it has been delivered: the changes go down one connection in
        // the order the commands ran.
        if (!await settles(support, 'stone', registry)) {
          process.stdout.write(
            `minecraft:${item}\t${face}\t${yaw}\t${pitch}\t${cursorY}\tARENA DID NOT SETTLE\t-\n`
          )
          continue
        }
        changes.delete(at(target))

        bot._client.write('block_place', {
          hand: 0,
          location: support,
          direction: face,
          cursorX: 0.5,
          cursorY,
          cursorZ: 0.5,
          insideBlock: false,
          sequence: sequence++
        })
        await wait(200)
        const got = changes.get(at(target))
        // The first change is the placement. A later one is the world reacting
        // to it — a door with nothing under it, a torch on a face that cannot
        // hold one — and that is a support rule rather than a placement rule,
        // so it is reported beside the answer instead of replacing it.
        // Air is how a refusal arrives. The client predicted a block and the
        // server answers by telling it what is really there, which for a cell
        // the arena just cleared is air — and no item places air, so there is
        // nothing for this to be confused with. A run with no `air` in it and
        // no `REFUSED` either would be a run where nothing was ever refused,
        // which `torch` alone disproves: it cannot hang from a ceiling.
        const first = got ? describe(got[0], registry) : null
        const result = first === null || first === 'minecraft:air'
          ? 'REFUSED\t-'
          : first + (got.length > 1 && got[got.length - 1] !== got[0] ? '\tbroke' : '\tstood')
        if (item === CONTROL) seen.push(`${face}/${yaw}/${pitch}/${cursorY} -> ${result}`)
        process.stdout.write(
          `minecraft:${item}\t${face}\t${yaw}\t${pitch}\t${cursorY}\t${result}\n`
        )
      }

      if (item === CONTROL) {
        // Everything after this row depends on the tool being sound, so this
        // is where the run stops if it is not. Refusals are allowed — a
        // control that could not be placed somewhere is still a control — and
        // two different *states* are not.
        const states = new Set(
          seen.map(s => s.split(' -> ')[1].split('\t')[0]).filter(r => r !== 'REFUSED')
        )
        if (states.size !== 1 || !states.has(`minecraft:${CONTROL}`)) {
          process.stderr.write(
            `the control disagreed with itself, so nothing below it is worth reading.\n` +
            `${CONTROL} has one state and this run saw ${states.size}:\n  ` +
            seen.join('\n  ') + '\n'
          )
          process.exit(1)
        }
        process.stderr.write(`control: ${CONTROL} agrees with itself over ${seen.length} situations\n`)
      }
    }
    process.exit(0)
  })

  bot.on('error', e => {
    process.stderr.write(`FAIL  ${e.message}\n`)
    process.exit(1)
  })
  bot.on('kicked', r => {
    process.stderr.write(`FAIL  kicked: ${JSON.stringify(r)}\n`)
    process.exit(1)
  })
}

main()
