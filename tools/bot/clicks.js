// Replays a survival client's clicks against a server and prints what the
// server says the container became, one snapshot per click.
//
// This exists because `check.js` sends exactly one click — mode 0, button 0 —
// and every other mode is covered only by Rust tests written by whoever wrote
// the modes. A test suite that agrees with itself cannot report that all seven
// modes share one wrong assumption. So this script does not assert anything:
// it *records*, so the same recording can be taken from Minecraft's own server
// and the two diffed.
//
//   node clicks.js <port> --out dust.json
//   node clicks.js <port> --out vanilla.json     (pointed at a vanilla server)
//   node clicks.js --compare vanilla.json dust.json
//
// The comparison is the measurement. The recording is not a result on its own.
//
// # Why the client claims nothing changed
//
// `window_click` carries the client's *prediction* — the slots it thinks moved
// and what it thinks is on the cursor. A server only corrects what the
// prediction got wrong. Claiming nothing therefore makes the server tell us
// everything it changed, on both servers, which is what makes the two
// recordings comparable. It also means the remote model the server is
// correcting against is exactly the one tracked below: slots persist, and the
// cursor is empty at the start of every click because that is what was
// claimed.
//
// # Why the slots are read out of the raw packets
//
// mineflayer drops `set_slot` for window -1 — its handler resolves a window by
// id, there is no window -1, and the packet returns having done nothing. The
// cursor is therefore invisible to `bot.inventory`, and a drag or a
// double-click read through mineflayer's model would look like it moved
// nothing. Decision record 0013 hit the same wall from the other side with
// window -2. Read the packets.

const mineflayer = require('mineflayer')

const VERSION = '1.21.1'
const JOIN_TIMEOUT_MS = 60000
const SPAWN_SETTLE_MS = 3000

// How long to wait for a click's corrections before snapshotting. A click is
// answered on the tick it arrives, so this is one network round trip plus a
// tick, not a guess at how long work takes.
const CLICK_SETTLE_MS = 400

const OUTSIDE = -999
const SWAP_OFFHAND_BUTTON = 40
const SLOTS = 46

// The seven modes, by the number the protocol gives them.
const PICKUP = 0
const QUICK_MOVE = 1
const SWAP = 2
const CLONE = 3
const THROW = 4
const DRAG = 5
const PICKUP_ALL = 6

// Not a mode. A step that writes a slot from the creative menu, so the second
// half of the script can start from a stated hand rather than from whatever
// the first half left behind.
const SEED_STEP = -1

const wait = ms => new Promise(r => setTimeout(r, ms))

function spawned (port, username) {
  return new Promise((resolve, reject) => {
    // Three characters minimum: a shorter name never spawns and never errors.
    const b = mineflayer.createBot({
      host: '127.0.0.1', port, username, auth: 'offline', version: VERSION
    })
    const timer = setTimeout(
      () => reject(new Error(`${username} never reached the world in ${JOIN_TIMEOUT_MS / 1000}s`)),
      JOIN_TIMEOUT_MS
    )
    b.on('error', e => { clearTimeout(timer); reject(new Error(`${username}: ${e.message}`)) })
    b.on('kicked', r => { clearTimeout(timer); reject(new Error(`${username} was kicked: ${JSON.stringify(r)}`)) })
    b.once('spawn', () => { clearTimeout(timer); resolve(b) })
  })
}

// What the server has told us the container holds, assembled from the packets
// rather than from mineflayer's window. `null` is an empty slot.
function tracker (b) {
  const state = { slots: new Array(SLOTS).fill(null), cursor: null, told: new Set() }
  const name = id => (b.registry.items[id] ? b.registry.items[id].name : `id:${id}`)
  const read = item => {
    if (!item || !item.itemCount || item.itemCount === 0) return null
    return { name: name(item.itemId), count: item.itemCount }
  }
  b._client.on('set_slot', p => {
    // The cursor's window id is -1, and it arrives here as 255: minecraft-data
    // types `container_set_slot`'s window id as an unsigned byte on 1.21.1, so
    // prismarine hands back the two's complement rather than the number the
    // protocol names. mineflayer's own handler resolves 255 to no window and
    // drops the packet, which is why the cursor is invisible through
    // `bot.inventory` and why a differential that trusted it would report a
    // server holding nothing on every click that picks something up.
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
    if (p.windowId !== 0) return
    for (let i = 0; i < p.items.length && i < SLOTS; i++) {
      state.slots[i] = read(p.items[i])
      state.told.add(i)
    }
    if ('carriedItem' in p) state.cursor = read(p.carriedItem)
  })
  return state
}

function creativeSlot (b, slot, itemName, count) {
  const item = itemName
    ? {
        itemCount: count,
        itemId: b.registry.itemsByName[itemName].id,
        addedComponentCount: 0,
        removedComponentCount: 0,
        components: [],
        removeComponents: []
      }
    : { itemCount: 0 }
  b._client.write('set_creative_slot', { slot, item })
}

function windowClick (b, slot, mouseButton, mode) {
  b._client.write('window_click', {
    windowId: 0,
    stateId: 0,
    slot,
    mouseButton,
    mode,
    changedSlots: [],
    cursorItem: { itemCount: 0 }
  })
}

// The container as one comparable line per slot. Empty slots are omitted so a
// diff points at what moved rather than at forty-six dashes.
function snapshot (state) {
  const slots = {}
  state.slots.forEach((s, i) => { if (s) slots[i] = `${s.name} x${s.count}` })
  return { slots, cursor: state.cursor ? `${state.cursor.name} x${state.cursor.count}` : null }
}

// What is seeded before the clicks run. Three max stack sizes on purpose: a
// stack of 64, a stack of 16 and a stack of 1 are the same code path with a
// different number, and a script made only of cobblestone cannot tell a server
// that hardcoded 64 from one that read the item.
const SEED = [
  [9, 'cobblestone', 64],
  [10, 'cobblestone', 20],
  [11, 'cobblestone', 60],
  [12, 'egg', 10], // stacks to 16
  [13, 'egg', 16],
  [14, 'cobblestone', 3],
  [20, 'water_bucket', 1], // stacks to 1
  [21, 'diamond_sword', 1], // stacks to 1
  [22, 'bucket', 5], // stacks to 16
  [36, 'cobblestone', 5],
  [37, 'egg', 3]
]

// Each step is a click. The names are what a player would call the gesture,
// because the report is read by somebody deciding whether the game is right.
const SCRIPT = [
  ['left click a full stack onto the cursor', PICKUP, 9, 0],
  ['left click it into an empty slot', PICKUP, 30, 0],
  ['left click it back onto the cursor', PICKUP, 30, 0],
  ['left click onto a partial stack of the same item', PICKUP, 10, 0],
  ['left click onto a different item, swapping', PICKUP, 21, 0],
  ['left click the swapped item down', PICKUP, 31, 0],

  ['right click takes half of an odd stack', PICKUP, 11, 1],
  ['right click puts one down in an empty slot', PICKUP, 32, 1],
  ['right click puts a second one on it', PICKUP, 32, 1],
  ['right click onto a different item swaps instead', PICKUP, 20, 1],
  ['left click puts that down', PICKUP, 33, 0],

  ['right click a single-item stack takes it whole', PICKUP, 33, 1],
  ['drop one of the cursor outside the window', PICKUP, OUTSIDE, 1],
  ['drop the rest of the cursor outside the window', PICKUP, OUTSIDE, 0],

  ['shift click the hotbar into the inventory', QUICK_MOVE, 36, 0],
  ['shift click the inventory into the hotbar', QUICK_MOVE, 12, 0],
  ['shift click merges before it takes an empty slot', QUICK_MOVE, 14, 0],
  ['shift click a stack that stacks to one', QUICK_MOVE, 22, 0],
  ['shift click an empty slot', QUICK_MOVE, 40, 0],

  ['a number key swaps with that hotbar slot', SWAP, 13, 2],
  ['the same number key swaps it back', SWAP, 13, 2],
  ['a number key onto an empty hotbar slot', SWAP, 11, 7],
  ['F swaps with the offhand', SWAP, 9, SWAP_OFFHAND_BUTTON],
  ['F swaps back', SWAP, 9, SWAP_OFFHAND_BUTTON],

  ['middle click clones a stack of 64', CLONE, 9, 2],
  ['put the clone down', PICKUP, 34, 0],
  ['middle click clones an item that stacks to one', CLONE, 20, 2],
  ['put that clone down', PICKUP, 35, 0],

  ['Q drops one', THROW, 9, 0],
  ['control-Q drops the stack', THROW, 34, 1],
  ['Q on an empty slot', THROW, 34, 0],

  ['pick a stack up for the drag', PICKUP, 9, 0],
  ['left drag starts', DRAG, OUTSIDE, 0],
  ['left drag takes a slot', DRAG, 16, 1],
  ['left drag takes another', DRAG, 17, 1],
  ['left drag takes a third', DRAG, 18, 1],
  ['left drag ends, splitting evenly', DRAG, OUTSIDE, 2],
  ['put the remainder down', PICKUP, 19, 0],

  ['pick a stack up for the right drag', PICKUP, 19, 0],
  ['right drag starts', DRAG, OUTSIDE, 4],
  ['right drag takes a slot', DRAG, 23, 5],
  ['right drag takes another', DRAG, 24, 5],
  ['right drag ends, one in each', DRAG, OUTSIDE, 6],
  ['put what is left down', PICKUP, 25, 0],

  ['pick a stack up for the interrupted drag', PICKUP, 25, 0],
  ['a drag starts', DRAG, OUTSIDE, 0],
  ['it takes a slot', DRAG, 26, 1],
  ['and an ordinary click arrives instead of the end', PICKUP, 27, 0],
  ['the drag end that follows must do nothing', DRAG, OUTSIDE, 2],

  ['pick the same item up again', PICKUP, 27, 0],
  ['double click gathers the loose ones', PICKUP_ALL, 27, 0],
  ['put the gathered stack down', PICKUP, 28, 0],

  ['a middle drag starts', DRAG, OUTSIDE, 8],
  ['it takes a slot', DRAG, 29, 9],
  ['and ends', DRAG, OUTSIDE, 10],

  ['a click on the crafting output', PICKUP, 0, 0],
  ['a swap that names the slot it is already in', SWAP, 38, 2],

  // The armour slots, the offhand and the crafting grid. Everything above this
  // line is the ordinary inventory, where a slot takes any item; these are the
  // slots that have an opinion about what goes in them, which is where a server
  // that treats all forty-six alike stops matching the game.
  ['clear the board for the armour cases', SEED_STEP, 0, 0],
  ['shift click a helmet out of the inventory', QUICK_MOVE, 9, 0],
  ['shift click a chestplate out of the inventory', QUICK_MOVE, 10, 0],
  ['shift click a shield out of the inventory', QUICK_MOVE, 11, 0],
  ['shift click a helmet already on the head', QUICK_MOVE, 5, 0],
  ['pick up a block', PICKUP, 12, 0],
  ['left click a block into the helmet slot', PICKUP, 5, 0],
  ['left click a block into the boots slot', PICKUP, 8, 0],
  ['left click a block into the offhand', PICKUP, 45, 0],
  ['put whatever is left down', PICKUP, 13, 0],
  ['pick up a helmet', PICKUP, 14, 0],
  ['left click the helmet into the helmet slot', PICKUP, 5, 0],
  ['left click that helmet into the boots slot', PICKUP, 8, 0],
  ['put whatever is left down', PICKUP, 15, 0],
  ['a number key swaps a block into the helmet slot', SWAP, 5, 0],
  ['a number key swaps a helmet into the helmet slot', SWAP, 5, 1],
  ['shift click out of the offhand', QUICK_MOVE, 45, 0],
  ['shift click out of the crafting grid', QUICK_MOVE, 1, 0],
  ['Q out of an armour slot', THROW, 5, 1],
  ['pick a block up for the armour drag', PICKUP, 16, 0],
  ['a drag starts', DRAG, OUTSIDE, 0],
  ['it takes an armour slot', DRAG, 6, 1],
  ['it takes an ordinary slot', DRAG, 17, 1],
  ['and ends', DRAG, OUTSIDE, 2],
  ['put the remainder down', PICKUP, 18, 0]
]

// What the mid-script reseed writes. Slot 0 is the crafting output and is
// never writable.
const RESEED = new Map([
  [1, ['cobblestone', 4]],
  [5, ['iron_helmet', 1]],
  [6, [null, 0]],
  [7, [null, 0]],
  [8, [null, 0]],
  [9, ['golden_helmet', 1]],
  [10, ['iron_chestplate', 1]],
  [11, ['shield', 1]],
  [12, ['cobblestone', 9]],
  [13, [null, 0]],
  [14, ['diamond_helmet', 1]],
  [15, [null, 0]],
  [16, ['cobblestone', 8]],
  [17, [null, 0]],
  [18, [null, 0]],
  [36, ['cobblestone', 6]],
  [37, ['netherite_helmet', 1]],
  [45, ['egg', 2]]
])

async function record (port, out) {
  const bot = await spawned(port, 'Clicker')
  const state = tracker(bot)
  await wait(SPAWN_SETTLE_MS)

  // Every slot, not just the seeded ones. Both servers persist a player's
  // inventory across sessions — that is what PR #42 was — so a run that only
  // wrote the slots it cared about would start on top of the previous run's
  // leftovers, differently on each server. Clearing all forty-five writable
  // slots is what makes two recordings comparable.
  state.told.clear()
  const seeded = new Map(SEED.map(([slot, name, count]) => [slot, [name, count]]))
  for (let slot = 1; slot < SLOTS; slot++) {
    const what = seeded.get(slot)
    creativeSlot(bot, slot, what ? what[0] : null, what ? what[1] : 0)
  }
  await wait(SPAWN_SETTLE_MS)

  // The client knows what it asked for. A server that accepted a creative
  // write says nothing back — Dust does not echo one, on purpose, because that
  // would be a packet per creative-menu click — so the seed has to be modelled
  // here or every later snapshot would report empty slots that are not empty.
  // A server that *refused* a write corrects it, and that correction lands on
  // top of this.
  for (let slot = 1; slot < SLOTS; slot++) {
    if (state.told.has(slot)) continue
    const what = seeded.get(slot)
    state.slots[slot] = what ? { name: what[0], count: what[1] } : null
  }

  const steps = [{ step: 'seeded', ...snapshot(state) }]
  for (const [name, mode, slot, button] of SCRIPT) {
    if (mode === SEED_STEP) {
      state.told.clear()
      for (const [at, [what, count]] of RESEED) creativeSlot(bot, at, what, count)
      await wait(SPAWN_SETTLE_MS)
      for (const [at, [what, count]] of RESEED) {
        if (state.told.has(at)) continue
        state.slots[at] = what ? { name: what, count } : null
      }
      steps.push({ step: name, mode, slot, button, ...snapshot(state) })
      continue
    }
    // The claim in every packet is "nothing changed and my cursor is empty",
    // so the model being corrected has an empty cursor at this point too.
    state.cursor = null
    windowClick(bot, slot, button, mode)
    await wait(CLICK_SETTLE_MS)
    steps.push({ step: name, mode, slot, button, ...snapshot(state) })
  }

  try { bot.quit() } catch (e) { /* already gone */ }
  require('fs').writeFileSync(out, JSON.stringify(steps, null, 1))
  console.log(`${steps.length - 1} clicks recorded to ${out}`)
}

// Counts, not a rate. A percentage would not say which click.
function compare (aPath, bPath) {
  const fs = require('fs')
  const a = JSON.parse(fs.readFileSync(aPath))
  const b = JSON.parse(fs.readFileSync(bPath))
  if (a.length !== b.length) {
    console.log(`FAIL  ${aPath} has ${a.length} steps and ${bPath} has ${b.length}`)
    process.exit(1)
  }
  let disagreed = 0
  for (let i = 0; i < a.length; i++) {
    const lines = []
    const keys = new Set([...Object.keys(a[i].slots), ...Object.keys(b[i].slots)])
    for (const k of [...keys].sort((x, y) => x - y)) {
      const x = a[i].slots[k] || 'empty'
      const y = b[i].slots[k] || 'empty'
      if (x !== y) lines.push(`      slot ${k}: ${aPath} ${x} / ${bPath} ${y}`)
    }
    if (a[i].cursor !== b[i].cursor) {
      lines.push(`      cursor: ${aPath} ${a[i].cursor || 'empty'} / ${bPath} ${b[i].cursor || 'empty'}`)
    }
    if (lines.length) {
      disagreed++
      console.log(`  DIFF  ${a[i].step}`)
      lines.forEach(l => console.log(l))
    }
  }
  console.log(`\n${a.length - disagreed}/${a.length} snapshots agree, ${disagreed} differ`)
  process.exit(disagreed === 0 ? 0 : 1)
}

const args = process.argv.slice(2)
if (args[0] === '--compare') {
  compare(args[1], args[2])
} else {
  const port = Number(args[0] || 25565)
  const outAt = args.indexOf('--out')
  const out = outAt === -1 ? 'clicks.json' : args[outAt + 1]
  record(port, out).catch(e => {
    console.log(`FAIL  ${e.message}`)
    console.log('\nIs a server running on this port, in creative, with online mode off?')
    process.exit(1)
  })
}
