// Replays the clicks a player makes to craft, and prints what the server says
// the container became.
//
// The 2x2 grid a player carries is slots 1..4 and the output is slot 0, and
// none of it is an ordinary slot: the output fills by itself, cannot be
// written to, and paying for it moves slots nobody clicked. So this records
// the same way `clicks.js` does — no assertions, one snapshot per step — and
// the measurement is the diff against a real 1.21.1 server.
//
//   node crafting.js <port> --out dust.json
//   node crafting.js <port> --out vanilla.json     (pointed at a vanilla server)
//   node crafting.js --compare vanilla.json dust.json
//
// And one thing a recording cannot reach:
//
//   node crafting.js <port> --refuse
//
// # Why there is a `--refuse` mode at all
//
// Every click below claims "nothing changed and my cursor is empty", which
// makes the server say everything it did and is what makes two recordings
// comparable. But a click the server *refuses* is a click both ends already
// agree about, so neither server sends anything and the diff reports
// agreement — the trap `clicks.js --predict` was built for and the same trap
// here, because the crafting output is the slot a client is most likely to be
// wrong about. `--refuse` tells the lie a real client tells (drawing the item
// it dropped into the output) and requires the contradiction.
//
// # The tracker attaches at construction
//
// A join burst arrives in one TCP read and node runs those handlers
// synchronously, before the microtask a `spawn` listener resolves. A tracker
// attached after `spawn` misses the container it was told at login. Both
// `clicks.js` and `equipment.js` were bitten by this on the same day.

const mineflayer = require('mineflayer')

const VERSION = '1.21.1'
const JOIN_TIMEOUT_MS = 60000
const SPAWN_SETTLE_MS = 3000
const CLICK_SETTLE_MS = 400

const SLOTS = 46
const OUTPUT = 0
const GRID = 1

// The modes, by the number the protocol gives them.
const PICKUP = 0
const QUICK_MOVE = 1
const SWAP = 2
const THROW = 4

// Not a mode: a creative write, so a step can state the grid rather than click
// its way there. Every player Dust serves is in creative today and a real
// server accepts the same packet, so this is the one way both servers can be
// put into the same starting shape.
const SEED_STEP = -1
// Not a mode either: close the window, which is what empties the grid.
const CLOSE_STEP = -2

const wait = ms => new Promise(r => setTimeout(r, ms))

function spawned (port, username) {
  return new Promise((resolve, reject) => {
    // Three characters minimum: a shorter name never spawns and never errors.
    const b = mineflayer.createBot({
      host: '127.0.0.1', port, username, auth: 'offline', version: VERSION
    })
    b.tracked = tracker(b)
    const timer = setTimeout(
      () => reject(new Error(`${username} never reached the world in ${JOIN_TIMEOUT_MS / 1000}s`)),
      JOIN_TIMEOUT_MS
    )
    b.on('error', e => { clearTimeout(timer); reject(new Error(`${username}: ${e.message}`)) })
    b.on('kicked', r => { clearTimeout(timer); reject(new Error(`${username} was kicked: ${JSON.stringify(r)}`)) })
    b.once('spawn', () => { clearTimeout(timer); resolve(b) })
  })
}

// What the server has told us the container holds, out of the packets rather
// than mineflayer's window: `set_slot` for window -1 arrives as 255 and
// mineflayer's own handler drops it, so the cursor is invisible through
// `bot.inventory`.
function tracker (b) {
  const state = {
    slots: new Array(SLOTS).fill(null),
    cursor: null,
    told: new Set(),
    setSlots: 0,
    windowItems: 0
  }
  const name = id => (b.registry.items[id] ? b.registry.items[id].name : `id:${id}`)
  const read = item => {
    if (!item || !item.itemCount || item.itemCount === 0) return null
    return { name: name(item.itemId), count: item.itemCount }
  }
  b._client.on('set_slot', p => {
    state.setSlots++
    if ((p.windowId === -1 || p.windowId === 255) && p.slot === -1) {
      state.cursor = read(p.item)
      return
    }
    if (p.windowId !== 0) return
    if (p.slot >= 0 && p.slot < SLOTS) {
      state.slots[p.slot] = read(p.item)
      state.told.add(p.slot)
    }
  })
  b._client.on('window_items', p => {
    state.windowItems++
    if (p.windowId !== 0) return
    for (let i = 0; i < p.items.length && i < SLOTS; i++) {
      state.slots[i] = read(p.items[i])
      state.told.add(i)
    }
    if ('carriedItem' in p) state.cursor = read(p.carriedItem)
  })
  return state
}

function itemStack (b, itemName, count) {
  return itemName
    ? {
        itemCount: count,
        itemId: b.registry.itemsByName[itemName].id,
        addedComponentCount: 0,
        removedComponentCount: 0,
        components: [],
        removeComponents: []
      }
    : { itemCount: 0 }
}

function creativeSlot (b, slot, itemName, count) {
  b._client.write('set_creative_slot', { slot, item: itemStack(b, itemName, count) })
}

function windowClick (b, slot, mouseButton, mode, changedSlots = [], cursorItem = { itemCount: 0 }) {
  b._client.write('window_click', {
    windowId: 0, stateId: 0, slot, mouseButton, mode, changedSlots, cursorItem
  })
}

function describe (s) {
  return s ? `${s.name} x${s.count}` : null
}

function snapshot (state, step) {
  const slots = {}
  state.slots.forEach((s, i) => { if (s) slots[i] = describe(s) })
  const taken = { slots, cursor: describe(state.cursor) }
  return step === undefined ? taken : { step, ...taken }
}

// What is on the board before the script runs. Three recipes are reachable
// from it and they are three different shapes:
//
//   one oak log            -> four oak planks   (shapeless, one ingredient)
//   four oak planks in 2x2 -> one crafting table (shaped, filling the grid)
//   four honey bottles     -> one honey block   (shaped, and the only vanilla
//                             2x2 recipe whose ingredients leave something
//                             behind — four glass bottles)
//
// A script made only of planks could not tell a server that returns the bottle
// from one that eats it.
const SEED = [
  [9, 'oak_log', 16],
  [10, 'oak_log', 1],
  [11, 'oak_planks', 4],
  [12, 'honey_bottle', 4],
  [13, 'cobblestone', 5],
  [36, 'oak_planks', 60]
]

const SCRIPT = [
  // One log into the grid, by hand, and the output has to fill on its own.
  ['pick a single log up', PICKUP, 10, 0],
  ['put it in the first grid slot', PICKUP, GRID, 0],
  ['left click the output onto the cursor', PICKUP, OUTPUT, 0],
  ['put the planks down', PICKUP, 20, 0],

  // The whole stack, so the output can be taken more than once.
  ['pick the stack of logs up', PICKUP, 9, 0],
  ['put the stack in the grid', PICKUP, GRID, 0],
  ['right click the output', PICKUP, OUTPUT, 1],
  ['put whatever that gave down', PICKUP, 21, 0],
  ['a number key over the output', SWAP, OUTPUT, 8],
  ['Q over the output', THROW, OUTPUT, 0],
  ['control-Q over the output', THROW, OUTPUT, 1],
  ['shift click the output', QUICK_MOVE, OUTPUT, 0],
  ['shift click whatever is left in the grid', QUICK_MOVE, GRID, 0],

  // A shaped recipe that fills the grid, stated rather than clicked into
  // place, and shift-clicked out.
  ['state a grid of four planks', SEED_STEP, 0, 0],
  ['shift click the crafting table out', QUICK_MOVE, OUTPUT, 0],

  // The remainder case. Four honey bottles make a honey block and give four
  // glass bottles back; a server that consumed them destroys four items.
  ['state a grid of four honey bottles', SEED_STEP, 0, 0],
  ['left click the honey block onto the cursor', PICKUP, OUTPUT, 0],
  ['put the honey block down', PICKUP, 22, 0],

  // Nothing may be put into the output.
  ['pick up some cobblestone', PICKUP, 13, 0],
  ['left click it at the output', PICKUP, OUTPUT, 0],
  ['right click it at the output', PICKUP, OUTPUT, 1],
  ['put the cobblestone back', PICKUP, 13, 0],

  // A grid with something in it, and then the window closes.
  ['state a grid with one log in it', SEED_STEP, 0, 0],
  ['close the window', CLOSE_STEP, 0, 0],

  // A shift-click with almost nowhere for the result to go. Sixty planks in
  // the last hotbar slot and everything else full: one craft of four fits
  // exactly and the second has nowhere.
  ['state a nearly full inventory over a full grid', SEED_STEP, 0, 0],
  ['shift click the output with almost no room', QUICK_MOVE, OUTPUT, 0],

  // And the row that separates "does not fit" from "does not fit *whole*".
  //
  // The step above cannot: the result there is oak wood, nothing in the
  // inventory can take any of it, and both servers answer by doing nothing —
  // which the diff calls agreement. Here the result is four planks and the
  // last slot has room for two, so a server that moves what it can and spends
  // the grid anyway destroys two planks, and a server that refuses the craft
  // leaves the log alone. Both are visible in the same snapshot.
  ['state a grid whose result only half fits', SEED_STEP, 0, 0],
  ['shift click a result that only half fits', QUICK_MOVE, OUTPUT, 0]
]

// What each `SEED_STEP` writes. Slot 0 is never writable.
const RESEEDS = [
  // Four planks in the grid, everything else on the board cleared, and room
  // in the inventory for what comes out.
  new Map([
    [1, ['oak_planks', 1]], [2, ['oak_planks', 1]],
    [3, ['oak_planks', 1]], [4, ['oak_planks', 1]],
    [9, [null, 0]], [10, [null, 0]], [11, [null, 0]], [12, [null, 0]],
    [13, [null, 0]], [20, [null, 0]], [21, [null, 0]], [36, [null, 0]]
  ]),
  // Four honey bottles.
  new Map([
    [1, ['honey_bottle', 1]], [2, ['honey_bottle', 1]],
    [3, ['honey_bottle', 1]], [4, ['honey_bottle', 1]],
    [13, ['cobblestone', 5]]
  ]),
  // One log in the grid, to be thrown out by the close.
  new Map([
    [1, ['oak_log', 1]], [2, [null, 0]], [3, [null, 0]], [4, [null, 0]],
    [13, [null, 0]], [20, [null, 0]], [21, [null, 0]], [22, [null, 0]]
  ]),
  // A grid of four logs, and an inventory with room for exactly one craft.
  // Every slot 9..44 holds a full stack of cobblestone except the last, which
  // holds sixty planks — four more fit there and nothing else does.
  (() => {
    const m = new Map([
      [1, ['oak_log', 1]], [2, ['oak_log', 1]], [3, ['oak_log', 1]], [4, ['oak_log', 1]],
      [5, [null, 0]], [6, [null, 0]], [7, [null, 0]], [8, [null, 0]], [45, [null, 0]]
    ])
    for (let slot = 9; slot < 44; slot++) m.set(slot, ['cobblestone', 64])
    m.set(44, ['oak_planks', 60])
    return m
  })(),
  // One log in the grid — four planks — and exactly two plank-shaped spaces
  // left in the whole container.
  (() => {
    const m = new Map([
      [1, ['oak_log', 1]], [2, [null, 0]], [3, [null, 0]], [4, [null, 0]]
    ])
    for (let slot = 9; slot < 44; slot++) m.set(slot, ['cobblestone', 64])
    m.set(44, ['oak_planks', 62])
    return m
  })()
]

async function record (port, out) {
  const bot = await spawned(port, 'Crafter')
  const state = bot.tracked
  await wait(SPAWN_SETTLE_MS)

  // Every writable slot, not just the seeded ones: both servers persist a
  // player's inventory across sessions, so a run that wrote only what it cared
  // about would start on top of the last run's leftovers.
  state.told.clear()
  const seeded = new Map(SEED.map(([slot, name, count]) => [slot, [name, count]]))
  for (let slot = 1; slot < SLOTS; slot++) {
    const what = seeded.get(slot)
    creativeSlot(bot, slot, what ? what[0] : null, what ? what[1] : 0)
  }
  await wait(SPAWN_SETTLE_MS)
  // A server that accepted a creative write says nothing about the slot the
  // client named — it already drew it — so the seed is modelled here. A slot
  // the server *did* speak about keeps what the server said, which is how the
  // crafting output stays the server's answer and not this script's guess.
  for (let slot = 1; slot < SLOTS; slot++) {
    if (state.told.has(slot)) continue
    const what = seeded.get(slot)
    state.slots[slot] = what ? { name: what[0], count: what[1] } : null
  }

  const steps = [{ step: 'seeded', ...snapshot(state) }]
  let reseeds = 0
  for (const [name, mode, slot, button] of SCRIPT) {
    if (mode === SEED_STEP) {
      const reseed = RESEEDS[reseeds++]
      state.told.clear()
      for (const [at, [what, count]] of reseed) creativeSlot(bot, at, what, count)
      await wait(SPAWN_SETTLE_MS)
      for (const [at, [what, count]] of reseed) {
        if (state.told.has(at)) continue
        state.slots[at] = what ? { name: what, count } : null
      }
      steps.push({ step: name, mode, ...snapshot(state) })
      continue
    }
    if (mode === CLOSE_STEP) {
      bot._client.write('close_window', { windowId: 0 })
      await wait(CLICK_SETTLE_MS)
      steps.push({ step: name, mode, ...snapshot(state) })
      continue
    }
    // Every click claims nothing changed and an empty cursor, so the model
    // being corrected has an empty cursor here too.
    state.cursor = null
    windowClick(bot, slot, button, mode)
    await wait(CLICK_SETTLE_MS)
    steps.push({ step: name, mode, slot, button, ...snapshot(state) })
  }

  try { bot.quit() } catch (e) { /* already gone */ }
  require('fs').writeFileSync(out, JSON.stringify(steps, null, 1))
  console.log(`${steps.length - 1} steps recorded to ${out}`)
}

// The output slot's refusals, which a recording cannot see.
//
// A click the server refuses moves nothing, and every click above claims
// nothing moved, so both servers answer a refusal with silence and the diff
// calls that agreement. Here the client claims what a real client draws — the
// cobblestone it just dropped into the output, and an empty hand — and the
// server has to take both back.
async function refuse (port) {
  const bot = await spawned(port, 'Refuser')
  const state = bot.tracked
  await wait(SPAWN_SETTLE_MS)

  for (let slot = 1; slot < SLOTS; slot++) creativeSlot(bot, slot, null, 0)
  creativeSlot(bot, 9, 'cobblestone', 5)
  creativeSlot(bot, 10, 'oak_log', 1)
  await wait(SPAWN_SETTLE_MS)

  // Honestly onto the cursor. Predicted correctly, so the server may say
  // nothing at all about it.
  state.cursor = null
  windowClick(bot, 9, 0, PICKUP)
  await wait(CLICK_SETTLE_MS)

  // The lie: "I put the cobblestone in the crafting output and my hand is
  // empty." Both halves are wrong, because nothing may be put there.
  const cobble = itemStack(bot, 'cobblestone', 5)
  state.slots[OUTPUT] = { name: 'cobblestone', count: 5 }
  state.cursor = null
  windowClick(bot, OUTPUT, 0, PICKUP, [{ location: OUTPUT, number: OUTPUT, item: cobble }], { itemCount: 0 })
  await wait(CLICK_SETTLE_MS)

  const checks = [
    ['the output is put back to empty', state.slots[OUTPUT] === null],
    ['the cursor is put back to the cobblestone', state.cursor !== null && state.cursor.count === 5]
  ]

  // And the other half: a click on an output that has something in it, claimed
  // as having done nothing. The server must say the grid was spent.
  windowClick(bot, 9, 0, PICKUP) // put the cobblestone back
  await wait(CLICK_SETTLE_MS)
  state.cursor = null
  windowClick(bot, 10, 0, PICKUP) // the log onto the cursor
  await wait(CLICK_SETTLE_MS)
  state.cursor = null
  windowClick(bot, GRID, 0, PICKUP) // into the grid
  await wait(CLICK_SETTLE_MS)
  checks.push(['a log in the grid fills the output unasked', state.slots[OUTPUT] !== null])
  state.cursor = null
  windowClick(bot, OUTPUT, 0, PICKUP)
  await wait(CLICK_SETTLE_MS)
  checks.push(['taking the result empties the grid slot', state.slots[GRID] === null])
  checks.push(['and empties the output with it', state.slots[OUTPUT] === null])
  checks.push(['and the result is on the cursor', state.cursor !== null])

  try { bot.quit() } catch (e) { /* already gone */ }
  let failed = 0
  for (const [what, ok] of checks) {
    if (!ok) failed++
    console.log(`  ${ok ? 'ok  ' : 'FAIL'}  ${what}`)
  }
  console.log(`\n${checks.length - failed}/${checks.length} checks passed`)
  process.exit(failed === 0 ? 0 : 1)
}

function compare (left, right) {
  const a = JSON.parse(require('fs').readFileSync(left, 'utf8'))
  const b = JSON.parse(require('fs').readFileSync(right, 'utf8'))
  if (a.length !== b.length) {
    console.log(`different lengths: ${a.length} and ${b.length}`)
    process.exit(1)
  }
  let agree = 0
  for (let i = 0; i < a.length; i++) {
    const differences = []
    for (let slot = 0; slot < SLOTS; slot++) {
      const l = a[i].slots[slot] || null
      const r = b[i].slots[slot] || null
      if (l !== r) differences.push(`slot ${slot}: ${l} / ${r}`)
    }
    if ((a[i].cursor || null) !== (b[i].cursor || null)) {
      differences.push(`cursor: ${a[i].cursor} / ${b[i].cursor}`)
    }
    if (differences.length === 0) { agree++; continue }
    console.log(`\n${i}. ${a[i].step}`)
    for (const d of differences) console.log(`     ${d}`)
  }
  console.log(`\n${agree} of ${a.length} snapshots agree (${left} / ${right})`)
  process.exit(agree === a.length ? 0 : 1)
}

async function main () {
  const args = process.argv.slice(2)
  if (args[0] === '--compare') return compare(args[1], args[2])
  const port = Number(args[0])
  if (!port) {
    console.log('usage: crafting.js <port> [--out file.json | --refuse]')
    console.log('       crafting.js --compare vanilla.json dust.json')
    process.exit(2)
  }
  if (args.includes('--refuse')) return refuse(port)
  const out = args[args.indexOf('--out') + 1]
  if (!args.includes('--out') || !out) {
    console.log('need --out <file.json>')
    process.exit(2)
  }
  await record(port, out)
  process.exit(0)
}

main().catch(e => { console.error(e.message); process.exit(1) })
