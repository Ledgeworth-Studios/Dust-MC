// What does a real server do to a block when the world around it changes?
//
// Dust's four earlier surveys all hold the world still and vary one input: a
// click's face, a neighbour's block, the cell a placement lands in, how long a
// break takes. None of them can see the rule this one is about, because that
// rule does not run until **after** a change: a torch whose wall is mined
// breaks and drops, a rail whose ground is dug out breaks and drops, a column
// of sand whose bottom block goes falls. This builds a block with everything
// around it, takes one neighbour away, and writes down what the server did
// and when.
//
// It asks **vanilla**, and only vanilla. What it produces is a file of
// Minecraft's own answers — the same kind of thing `xtask extract --only
// constants` produces and under the same rule, so it belongs on the operator's
// own disk and no row of it is committed. Scoring Dust against those answers is
// `cargo xtask harness updates`, which is a comparison and lives elsewhere.
//
// Usage: node updates.js <port> --check          the gate, against Dust
//        node updates.js <port> [blocks.txt|block,block,...] > answers.tsv
//        node updates.js <port> [blocks] --shell dirt >> answers.tsv
//        node updates.js <port> [blocks] --fall [height]
//        node updates.js <port> --all > answers.tsv
//
// `--check` is the other half and the only one that runs against **Dust**: it
// puts a torch on a block, mines the block, and says whether the torch fell —
// which is the whole feature, seen the way a player sees it. The scorer
// `cargo xtask harness updates` compares `dust_sim::updates` against the
// answers below and cannot see the queue, the tick loop or the entity at all;
// a rule that is right in a crate nobody calls is a world that does not move.
//
// The default mode is the **support** survey: for each subject block and each
// of the six sides, stand the block in a shell of stone, take that one side
// away, and record what the subject cell holds afterwards. Six rows a block,
// and the six together are the whole of "which neighbour is holding this up".
//
// `--shell` is what the six cells are made of, and running the survey twice
// with two shells is not optional. A dandelion in a shell of **stone** breaks
// when *any* of its six neighbours changes, because a dandelion wants dirt and
// stone is not dirt — so a single-shell run reports every plant as depending
// on all six sides, which is not a support rule, it is the arena. It is the
// same shape as the standing warning that a control which only holds on a
// superflat is not a control. Run it with `stone` and again with `dirt` and
// score both.
//
// `--fall` is the other question, and it is about *time* rather than about
// which side: build a column of the subject on a stone plinth, remove the
// plinth, and record how many polls pass before the top of the column has
// moved and where the column ends up. A falling block that arrives instantly
// and one that arrives in two ticks are the same row in the support survey and
// are not the same thing to look at.
//
// Notes to whoever runs it next, most of them inherited from `placement.js`
// and one that is this tool's own:
//
//   * The arena is built from the **server console**, not by the bot. Start the
//     server with its stdin on a pipe and point DUST_SERVER_CONSOLE at it;
//     `tools/bot/README.md` has the two lines that do it.
//
//   * **Both** block-change packets. A server sends `block_change` when exactly
//     one block in a section changed in a tick and `multi_block_change` when
//     more than one did. A break that takes its neighbour with it arrives the
//     second way, and a tool listening to one of them sees half of what this
//     survey exists to record.
//
//   * The state is read out of the change packets and never out of
//     `bot.blockAt`: the bot's own world lags by an unbounded amount, and a
//     read that way describes the sample before this one.
//
//   * **What is known about a cell is forgotten before the commands that
//     change it go out, not after.** Otherwise "wait until the subject is a
//     torch" matches the torch from the previous sample and returns at once.
//
//   * And the one this tool learned for itself: **a block that was never there
//     produces the same silence as a block that survived.** `/setblock` on a
//     state the server refuses leaves air, and air after the neighbour is
//     removed looks exactly like a torch that broke. So every row carries
//     `stood`, which is what the subject cell held *before* the neighbour was
//     taken away, and a row whose `stood` is not the subject scores as nothing
//     rather than as a break. Nineteen of the first run's rows were that.

const mineflayer = require('mineflayer')
const fs = require('fs')

const PORT = Number(process.argv[2] || 25565)
const VERSION = '1.21.1'

// In the order `Direction` declares and `dust_sim::placement::Face` indexes,
// so a row's `face` column and a column of the constants table are the same
// six names in the same six positions.
const FACES = [
  ['down', [0, -1, 0]],
  ['up', [0, 1, 0]],
  ['north', [0, 0, -1]],
  ['south', [0, 0, 1]],
  ['west', [-1, 0, 0]],
  ['east', [1, 0, 0]]
]

/// The control, run first and every time.
///
/// `minecraft:stone` needs nothing and loses nothing, so all six of its rows
/// have to say the subject survived. It is a test of *this tool* and not of
/// the server: a run whose arena never settles, or whose reads land a sample
/// late, shows up here first as a control that broke.
const CONTROL = 'stone'

/// The blocks worth asking about when nobody names any.
///
/// One per family of support rule rather than a long list, because `--all`
/// exists for the long list. The last four are the controls that make the rest
/// mean something: stone and oak_planks need nothing, sand and gravel need
/// nothing *and* fall, and a survey that could not tell those apart would be
/// reporting one column twice.
const DEFAULT_BLOCKS = [
  'torch',            // stands on the floor
  'wall_torch',       // hangs off one wall, and which one is the question
  'rail',             // floor, and it is not a plant
  'ladder',           // one wall
  'oak_sign',         // floor
  'oak_wall_sign',    // one wall
  'lever',            // whichever face it was put on
  'stone_button',
  'oak_pressure_plate',
  'redstone_wire',
  'oak_sapling',      // wants dirt, and stone is not dirt
  'dandelion',
  'snow',
  'lantern',          // hangs from above or stands below
  'vine',             // any of five, which is the `or` in the rule
  'cactus',
  'sugar_cane',
  'oak_door',         // two cells, and the top one stands on the bottom
  'scaffolding',
  'pointed_dripstone',
  'sand',             // falls
  'gravel',           // falls
  'stone',            // needs nothing: the control
  'oak_planks'        // needs nothing either
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
function describe (stateId, registry) {
  const { name, props } = properties(stateId, registry)
  const qualified = name.includes(':') ? name : `minecraft:${name}`
  const kv = Object.entries(props).map(([k, v]) => `${k}=${v}`).sort().join(',')
  return kv ? `${qualified}[${kv}]` : qualified
}

function names (argument) {
  if (fs.existsSync(argument)) {
    return fs.readFileSync(argument, 'utf8').split('\n').map(s => s.trim()).filter(Boolean)
  }
  return argument.split(',').map(s => s.trim()).filter(Boolean)
}

function main () {
  const flags = process.argv.slice(3).filter(a => a.startsWith('--'))
  const fall = flags.includes('--fall')
  const check = flags.includes('--check')
  const all = flags.includes('--all')
  const shellAt = process.argv.indexOf('--shell')
  const shell = shellAt > 0 ? process.argv[shellAt + 1] : 'stone' 
  const positional = process.argv.slice(3)
    .filter((a, i) => !a.startsWith('--') && process.argv[i + 2] !== '--shell')
  const argument = positional[0]
  const height = Number(positional[1] || 8)

  if (check) {
    return gate()
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
    host: '127.0.0.1', port: PORT, username: 'Updater', auth: 'offline', version: VERSION
  })

  const changes = new Map()
  // How many change *events* have arrived, which is not `changes.size`: a cell
  // the arena already knows about changing again is news and does not make the
  // map any bigger. Counting the map reported zero cascade for every row of
  // the first run.
  let events = 0
  const at = p => `${p.x},${p.y},${p.z}`
  const record = (position, state) => {
    events++
    const key = at(position)
    const seen = changes.get(key)
    if (seen) seen.push(state)
    else changes.set(key, [state])
  }
  bot._client.on('block_change', p => record(p.location, p.type))
  bot._client.on('multi_block_change', p => {
    const section = p.chunkCoordinates
    for (const entry of p.records) {
      const state = Math.floor(entry / 4096)
      const packed = entry % 4096
      record({
        x: section.x * 16 + ((packed >> 8) & 0xf),
        y: section.y * 16 + (packed & 0xf),
        z: section.z * 16 + ((packed >> 4) & 0xf)
      }, state)
    }
  })

  /// What a cell holds, out of the change log. `null` for a cell nothing has
  /// ever been said about, which the caller reads as "not known" and never as
  /// air — the two are different and conflating them is how a refused
  /// `/setblock` scores as a break.
  const held = (cell, registry) => {
    const seen = changes.get(at(cell))
    return seen && seen.length ? describe(seen[seen.length - 1], registry) : null
  }

  /// Wait until the cell reads a block of this name.
  async function settles (cell, want, registry, tries = 120) {
    for (let i = 0; i < tries; i++) {
      const there = held(cell, registry)
      if (there && there.split('[')[0] === want) return true
      await wait(25)
    }
    return false
  }

  bot.once('spawn', async () => {
    await wait(2500)
    const registry = bot.registry
    const base = bot.entity.position.floored()
    const floor = base.y - 1
    const ax = base.x + 8
    const az = base.z

    say(`fill ${ax - 5} ${floor} ${az - 5} ${ax + 5} ${floor + 20} ${az + 5} air`)
    say(`fill ${ax - 5} ${floor} ${az - 5} ${ax + 5} ${floor} ${az + 5} stone`)
    say(`tp Updater ${ax + 0.5} ${floor + 1} ${az + 3.5}`)
    await wait(1500)

    const target = { x: ax, y: floor + 3, z: az }
    const around = {}
    for (const [name, [dx, dy, dz]] of FACES) {
      around[name] = { x: target.x + dx, y: target.y + dy, z: target.z + dz }
    }

    let blocks = DEFAULT_BLOCKS
    if (argument) blocks = names(argument)
    if (all) {
      blocks = Object.values(registry.blocksByName)
        .map(b => b.name)
        .filter(name => name !== 'air')
    }
    blocks = [CONTROL, ...blocks.filter(b => b !== CONTROL)]

    /// Clear the arena and forget everything known about it.
    ///
    /// Forgotten **before** the commands go out and never after: the barrier
    /// this run waits on is "the subject appeared", and a log that still holds
    /// the previous sample's subject satisfies that instantly.
    const clear = () => {
      changes.clear()
      events = 0
      say(`fill ${ax - 3} ${floor + 1} ${az - 3} ${ax + 3} ${floor + 18} ${az + 3} air`)
    }

    if (fall) {
      process.stdout.write('# block\theight\tstood\tpolls\ttop\tbottom\tmoved\n')
      for (const block of blocks) {
        clear()
        // A plinth one cell above the floor, the column on top of it, and air
        // under the plinth — so removing the plinth leaves the whole column
        // over a two-cell drop and the fall is visible rather than a nudge.
        const plinth = { x: target.x, y: floor + 2, z: target.z }
        say(`setblock ${plinth.x} ${plinth.y} ${plinth.z} stone`)
        for (let i = 0; i < height; i++) {
          say(`setblock ${plinth.x} ${plinth.y + 1 + i} ${plinth.z} ${block}`)
        }
        const top = { x: plinth.x, y: plinth.y + height, z: plinth.z }
        if (!await settles(top, `minecraft:${block}`, registry, 200)) {
          process.stdout.write(`${block}\t${height}\tNOT_BUILT\t-\t-\t-\t-\n`)
          continue
        }
        const stood = held(top, registry)
        say(`setblock ${plinth.x} ${plinth.y} ${plinth.z} air`)
        // Poll at a tick rather than at a wall-clock guess, and stop when
        // nothing has moved for a while: a column of sand is still arriving
        // several ticks after the first block does.
        let polls = 0
        let quiet = 0
        let last = -1
        while (polls < 120 && quiet < 20) {
          await wait(50)
          polls++
          const size = changes.size
          if (size === last) quiet++
          else { quiet = 0; last = size }
        }
        const settled = held(top, registry)
        const bottom = held({ x: plinth.x, y: plinth.y, z: plinth.z }, registry)
        process.stdout.write(
          `${block}\t${height}\t${stood}\t${polls}\t${settled || 'minecraft:air'}` +
          `\t${bottom || 'minecraft:air'}\t${settled !== stood}\n`)
      }
      bot.quit()
      await wait(500)
      process.exit(0)
    }

    // The support survey.
    process.stdout.write('# block\tface\tshell\tstood\tstate_before\tafter\toutcome\tchanged\n')
    for (const block of blocks) {
      for (const [face] of FACES) {
        clear()
        await wait(120)
        for (const [name, ] of FACES) {
          const cell = around[name]
          say(`setblock ${cell.x} ${cell.y} ${cell.z} ${shell}`)
        }
        say(`setblock ${target.x} ${target.y} ${target.z} ${block}`)
        // The barrier: the subject really is in the cell. A `/setblock` the
        // server refused leaves air here, and air after the removal is
        // indistinguishable from a block that broke — which is this tool's own
        // version of the standing warning that silence reads as agreement.
        if (!await settles(target, `minecraft:${block}`, registry, 24)) {
          process.stdout.write(
            `${block}\t${face}\t${shell}\tNOT_PLACED\t-\t-\tnot_placed\t-\n`)
          continue
        }
        const stood = held(target, registry)
        const built = FACES
          .map(([name]) => `${name}=${held(around[name], registry) || 'minecraft:air'}`)
          .join(';')
        const before = events
        say(`setblock ${around[face].x} ${around[face].y} ${around[face].z} air`)
        await wait(400)
        const after = held(target, registry)
        // **Three outcomes and not two.** A cell that came back holding a
        // different *state* of the same block did not break — a pressure
        // plate's `powered`, a rail's `shape`, a vine's connections — and the
        // first run of this scored four of those as breaks, because the test
        // was `after === stood`. What a support rule is about is the block
        // going away.
        const gone = (after || 'minecraft:air') === 'minecraft:air'
        const outcome = gone ? 'broke' : (after === stood ? 'stayed' : 'changed')
        // One change is the neighbour that was taken away, so `changed` is
        // everything the server did *because* of it — the subject breaking,
        // and anything that broke with it.
        process.stdout.write(
          `${block}\t${face}\t${shell}\t${stood}\t${built}\t${after || 'minecraft:air'}` +
          `\t${outcome}\t${Math.max(0, events - before - 1)}\n`)
      }
    }
    // The server is deliberately left running: a survey is run several times
    // over while a rule is argued about, and a tool that stopped the server it
    // measured would cost a two-minute boot each time.
    bot.quit()
    await wait(500)
    process.exit(0)
  })

  let done = false
  bot.on('kicked', reason => {
    if (done) return
    process.stderr.write(`kicked: ${JSON.stringify(reason)}\n`)
    process.exit(1)
  })
  bot.on('error', error => {
    process.stderr.write(`${error}\n`)
    process.exit(1)
  })
}

/// The gate, against a running Dust server.
///
/// No console and no `/setblock`: everything here is done the way a player
/// does it, with a creative inventory write, a right-click and a dig. That is
/// the point — the harness scorer asks `dust_sim::updates` and cannot see the
/// queue, the tick loop, the entity or the packets, so a rule that is perfect
/// in a crate with no caller passes it and leaves a world that never moves.
///
/// Five checks and one of them is the control. The control is not decoration:
/// cobblestone on a block whose support is mined **must stay**, and a server
/// that broke everything on every update would pass all four of the others.
function gate () {
  const bot = mineflayer.createBot({
    host: '127.0.0.1', port: PORT, username: 'Updater', auth: 'offline', version: VERSION
  })

  let failures = 0
  const results = []
  const report = (name, ok, detail) => {
    results.push(`${ok ? 'ok  ' : 'FAIL'}  ${name}${detail ? `  — ${detail}` : ''}`)
    if (!ok) failures++
  }

  // What the server says is really there, out of the block-change packets and
  // never out of `bot.blockAt`: the bot's own world lags an edit by an
  // unbounded amount, and every earlier tool in this directory learned that
  // the same way.
  const changes = new Map()
  const at = p => `${p.x},${p.y},${p.z}`
  bot._client.on('block_change', p => changes.set(at(p.location), p.type))
  bot._client.on('multi_block_change', p => {
    const section = p.chunkCoordinates
    for (const entry of p.records) {
      const state = Math.floor(entry / 4096)
      const packed = entry % 4096
      changes.set(at({
        x: section.x * 16 + ((packed >> 8) & 0xf),
        y: section.y * 16 + (packed & 0xf),
        z: section.z * 16 + ((packed >> 4) & 0xf)
      }), state)
    }
  })
  let spawnedFalling = 0
  bot._client.on('spawn_entity', p => {
    const type = bot.registry.entitiesByName.falling_block
    if (type && p.type === type.id) spawnedFalling++
  })

  bot.once('spawn', async () => {
    await wait(2500)
    const registry = bot.registry
    const name = cell => {
      const state = changes.get(at(cell))
      if (state === undefined) {
        const block = bot.blockAt(cell)
        return block ? (block.name.includes(':') ? block.name : `minecraft:${block.name}`) : null
      }
      return properties(state, registry).name.replace(/^(?!.*:)/, 'minecraft:')
    }

    const arm = (slot, item) => {
      const entry = registry.itemsByName[item]
      bot._client.write('set_creative_slot', {
        slot: 36 + slot,
        item: {
          itemCount: 1,
          itemId: entry.id,
          addedComponentCount: 0,
          removedComponentCount: 0,
          components: [],
          removeComponents: []
        }
      })
    }
    let sequence = 1
    const dig = cell => {
      // A creative break is one packet, and this server honours both halves;
      // sending the start alone is what a creative client actually does.
      bot._client.write('block_dig', {
        status: 0, location: cell, face: 1, sequence: sequence++
      })
      bot._client.write('block_dig', {
        status: 2, location: cell, face: 1, sequence: sequence++
      })
    }
    const place = (on, slot) => {
      bot._client.write('held_item_slot', { slotId: slot })
      bot._client.write('block_place', {
        hand: 0,
        location: on,
        direction: 1,
        cursorX: 0.5,
        cursorY: 1.0,
        cursorZ: 0.5,
        insideBlock: false,
        sequence: sequence++
      })
    }

    arm(0, 'torch')
    arm(1, 'sand')
    arm(2, 'cobblestone')
    arm(3, 'stone')
    await wait(600)

    const stood = bot.entity.position.floored()
    const ground = { x: stood.x + 3, y: stood.y - 1, z: stood.z }
    if (name(ground) === 'minecraft:air') {
      report('the arena has a floor', false, 'the cell beside the player is air')
    }

    // 1 and 2. A torch on a block, and the block mined.
    const pillar = { x: ground.x, y: ground.y + 1, z: ground.z }
    place(ground, 3)
    await wait(400)
    const torchAt = { x: pillar.x, y: pillar.y + 1, z: pillar.z }
    place(pillar, 0)
    await wait(400)
    report('a torch goes down on a block', name(torchAt) === 'minecraft:torch', name(torchAt))
    dig(pillar)
    await wait(700)
    report(
      'the torch breaks when the block under it is mined',
      name(torchAt) === 'minecraft:air',
      `the cell holds ${name(torchAt)}`
    )

    // 3. The control. Cobblestone is not held up by anything and must not move.
    place(ground, 3)
    await wait(400)
    place(pillar, 2)
    await wait(400)
    const kept = { x: pillar.x, y: pillar.y + 1, z: pillar.z }
    report('a cobblestone goes down on a block', name(kept) === 'minecraft:cobblestone', name(kept))
    dig(pillar)
    await wait(700)
    report(
      'the cobblestone stays when the block under it is mined',
      name(kept) === 'minecraft:cobblestone',
      `the cell holds ${name(kept)}`
    )

    // 4 and 5. Sand, two cells up, and the plinth mined out from under it.
    dig(kept)
    await wait(400)
    const plinth = { x: ground.x, y: ground.y + 1, z: ground.z }
    place(ground, 3)
    await wait(300)
    place(plinth, 3)
    await wait(300)
    const sandAt = { x: ground.x, y: ground.y + 3, z: ground.z }
    place({ x: ground.x, y: ground.y + 2, z: ground.z }, 1)
    await wait(400)
    report('sand goes down two cells up', name(sandAt) === 'minecraft:sand', name(sandAt))
    spawnedFalling = 0
    dig({ x: ground.x, y: ground.y + 2, z: ground.z })
    dig(plinth)
    dig({ x: ground.x, y: ground.y + 1, z: ground.z })
    await wait(1500)
    report(
      'the sand became a falling entity',
      spawnedFalling > 0,
      `${spawnedFalling} falling block(s) spawned`
    )
    report(
      'the sand landed on the floor',
      name({ x: ground.x, y: ground.y + 1, z: ground.z }) === 'minecraft:sand',
      `the cell above the floor holds ${name({ x: ground.x, y: ground.y + 1, z: ground.z })}`
    )

    for (const line of results) process.stdout.write(line + '\n')
    process.stdout.write(
      `${results.length - failures}/${results.length} checks passed\n`)
    bot.quit()
    await wait(400)
    process.exit(failures === 0 ? 0 : 1)
  })

  bot.on('kicked', reason => {
    process.stderr.write(`kicked: ${JSON.stringify(reason)}\n`)
    process.exit(1)
  })
  bot.on('error', error => {
    process.stderr.write(`${error}\n`)
    process.exit(1)
  })
}

main()
