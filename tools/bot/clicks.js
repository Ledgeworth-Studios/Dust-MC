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
// The two slots `--predict` uses, named because the numbers are vanilla's.
const HELMET = 5
const MAIN = 9
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
    return { name: name(item.itemId), count: item.itemCount, components: componentsOf(item) }
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

function windowClick (b, slot, mouseButton, mode, changedSlots = [], cursorItem = { itemCount: 0 }) {
  b._client.write('window_click', {
    windowId: 0,
    stateId: 0,
    slot,
    mouseButton,
    mode,
    changedSlots,
    cursorItem
  })
}

// The container as one comparable line per slot. Empty slots are omitted so a
// diff points at what moved rather than at forty-six dashes.
function describe (s) {
  return s ? `${s.name} x${s.count}${s.components ? ' ' + s.components : ''}` : null
}

function snapshot (state, step) {
  const slots = {}
  state.slots.forEach((s, i) => { if (s) slots[i] = describe(s) })
  const taken = { slots, cursor: describe(state.cursor) }
  return step === undefined ? taken : { step, ...taken }
}

// A component patch as one comparable string: every component named, its value
// rendered with its keys in a fixed order, and the whole list sorted. Two
// servers that hold the same patch in different orders read the same here,
// which is deliberate — the order a patch arrives in is not part of what it
// means, and a diff that reported it would report noise on every line.
function componentsOf (item) {
  // A component whose whole value is that it is there parses to no data at
  // all, which is the one place `undefined` really does mean present.
  const added = (item.components || [])
    .map(c => `${c.type}=${c.data === undefined ? 'present' : stable(c.data)}`)
    .sort()
  const removed = (item.removeComponents || []).map(c => c.type).sort()
  if (added.length === 0 && removed.length === 0) return ''
  return `[${added.join(' ')}${removed.length ? ' -' + removed.join(' -') : ''}]`
}

function stable (value) {
  // `undefined` inside a parsed component is an *absent* optional, not a
  // present one. A renderer that called it 'present' read a server clearing a
  // lodestone's position as a server keeping it.
  if (value === undefined) return 'absent'
  if (value === null || typeof value !== 'object') return JSON.stringify(value)
  if (Array.isArray(value)) return `[${value.map(stable).join(',')}]`
  if (Buffer.isBuffer(value)) return value.toString('hex')
  const keys = Object.keys(value).sort()
  return `{${keys.map(k => `${k}:${stable(value[k])}`).join(',')}}`
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
  ['put the remainder down', PICKUP, 18, 0],

  // A second reseed, for the rules the section above still cannot reach. Every
  // wearable in it above is worn one to a slot *and* stacks to one, so a
  // container that never asked the slot for its limit and only ever asked the
  // item would agree with the game on all of them. `minecraft:player_head` is
  // the case that separates the two: worn on the head and stacks to 64. The
  // elytra and the carved pumpkin are here because they are worn and are in no
  // tag that says so.
  ['clear the board for the stacking wearables', SEED_STEP, 0, 0],
  ['shift click a stack of nine heads', QUICK_MOVE, 9, 0],
  ['shift click an elytra', QUICK_MOVE, 10, 0],
  ['shift click a shield into an empty offhand', QUICK_MOVE, 12, 0],
  ['shift click the head back off', QUICK_MOVE, 5, 0],
  ['pick up a carved pumpkin', PICKUP, 11, 0],
  ['left click it onto the head', PICKUP, 5, 0],
  ['pick up the stack of heads', PICKUP, 9, 0],
  ['left click nine heads at a head wearing a pumpkin', PICKUP, 5, 0],
  ['swap them for the helmet in the inventory', PICKUP, 13, 0],
  ['right click that helmet onto the head', PICKUP, 5, 1],
  ['pick up three heads with the pumpkin in hand', PICKUP, 14, 0],
  ['shift click the boots out of the hotbar', QUICK_MOVE, 37, 0],
  ['right click three heads at a head wearing a helmet', PICKUP, 5, 1],
  ['shift the helmet off with a full hand', QUICK_MOVE, 5, 0],
  ['right click one head onto the bare head', PICKUP, 5, 1],
  ['put the rest down', PICKUP, 16, 0],
  ['Q the elytra off the chest', THROW, 6, 1]
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

// The second reseed. Nothing here stacks the same way twice: nine heads at 64,
// an elytra and a carved pumpkin at 1, and a helmet to be swapped for.
const RESEED2 = new Map([
  [1, [null, 0]],
  [5, [null, 0]],
  [6, [null, 0]],
  [7, [null, 0]],
  [8, [null, 0]],
  [9, ['player_head', 9]],
  [10, ['elytra', 1]],
  [11, ['carved_pumpkin', 1]],
  [12, ['shield', 1]],
  [13, ['diamond_helmet', 1]],
  [14, ['player_head', 3]],
  [15, [null, 0]],
  [16, [null, 0]],
  [17, [null, 0]],
  [18, [null, 0]],
  [36, [null, 0]],
  [37, ['iron_boots', 1]],
  [45, [null, 0]]
])

// The reseeds in the order the script reaches them.
const RESEEDS = [RESEED, RESEED2]

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

// The one thing the recording above cannot reach.
//
// Every click it sends claims "nothing changed", so a click the server refuses
// is a click where both ends already agree and no packet needs to be sent. A
// real client does not claim that. It *predicts*: it draws the cobblestone in
// the helmet slot on the frame the player clicked, tells the server that is
// what it did, and waits to be contradicted. A server that refuses the click
// and says nothing leaves that prediction standing — the player sees a block on
// their head until they relog, and the differential above reports 101/101 the
// whole time, because a recording of two servers that both send nothing is two
// recordings that agree.
//
// So this sends the prediction and requires the contradiction. Run it against a
// real server first: what it asserts is what a real server does, not what looks
// correct.
//
//   node clicks.js <port> --predict
async function predict (port) {
  const bot = await spawned(port, 'Predictor')
  const state = tracker(bot)
  await wait(SPAWN_SETTLE_MS)

  const stone = 'cobblestone'
  for (let slot = 1; slot < SLOTS; slot++) creativeSlot(bot, slot, null, 0)
  creativeSlot(bot, MAIN, stone, 9)
  await wait(SPAWN_SETTLE_MS)

  // Onto the cursor, honestly. This click is predicted correctly, so a server
  // may answer it with nothing at all.
  state.cursor = null
  windowClick(bot, MAIN, 0, PICKUP)
  await wait(CLICK_SETTLE_MS)

  // And now the lie a real client tells: "I put the cobblestone on my head and
  // my hand is empty." The server refused the click, so both halves are wrong.
  const cobble = {
    itemCount: 9,
    itemId: bot.registry.itemsByName[stone].id,
    addedComponentCount: 0,
    removedComponentCount: 0,
    components: [],
    removeComponents: []
  }
  state.slots[HELMET] = { name: stone, count: 9 }
  state.cursor = null
  windowClick(bot, HELMET, 0, PICKUP, [{ location: HELMET, number: HELMET, item: cobble }], { itemCount: 0 })
  await wait(CLICK_SETTLE_MS)

  const checks = [
    ['the helmet slot is put back to empty', state.slots[HELMET] === null],
    ['the cursor is put back to the cobblestone', state.cursor && state.cursor.count === 9],
    ['the inventory slot it came from is still empty', state.slots[MAIN] === null]
  ]
  try { bot.quit() } catch (e) { /* already gone */ }
  let failed = 0
  for (const [what, ok] of checks) {
    if (!ok) failed++
    console.log(`  ${ok ? 'ok  ' : 'FAIL'}  ${what}`)
  }
  console.log(`\n${checks.length - failed}/${checks.length} checks passed`)
  process.exit(failed === 0 ? 0 : 1)
}


// ---------------------------------------------------------------------------
// Components
// ---------------------------------------------------------------------------
//
// A component is a VarInt type id followed by that type's own layout, with no
// length, so a server that cannot walk one loses the position of every field
// after it. Dust walks all fifty-seven layouts and keeps the bytes; whether it
// walks them *correctly* is not something Dust's own tests can answer, because
// the layouts and the tests would share one wrong assumption.
//
// So this sends a component-bearing item and asks the server to say what it
// holds. Two clicks per component: one that takes the stack onto the cursor and
// one that puts it back. **Both are predicted wrongly on purpose** — the click
// claims nothing changed — so a server that stayed silent would be reporting an
// empty inventory rather than agreeing. A recording where both servers say
// nothing is the failure this shape exists to prevent.
//
//   node clicks.js <port> --components --out dust-components.json
//   node clicks.js --compare vanilla-components.json dust-components.json

// A text component as its *simplest* NBT: a bare string, not a compound with a
// `text` key. Both mean the same thing and Minecraft accepts either, but it
// re-serialises the compound form back as a bare string, and Dust echoes the
// bytes it was given without understanding them. Sending the simplified form
// keeps that normalisation out of the diff, which is the right place for it:
// re-encoding a text component means modelling one, and this server carries
// components precisely so that it does not have to.
const nbtString = value => ({ type: 'string', name: '', value })
const nbtEmpty = { type: 'compound', name: '', value: {} }
const nbtWithId = id => ({ type: 'compound', name: '', value: { id: { type: 'string', value: id } } })

// One entry per component type, so a diff names the component rather than the
// item. The values are only as real as they have to be: what is under test is
// where each component *ends*, not what it means.
function componentCorpus (registry) {
  const item = name => registry.itemsByName[name].id
  const named = (s, name) => ({
    ...s,
    addedComponentCount: 1,
    components: [{ type: 'custom_name', data: nbtString(name) }]
  })
  const stack = (name, count) => ({
    itemCount: count,
    itemId: item(name),
    addedComponentCount: 0,
    removedComponentCount: 0,
    components: [],
    removeComponents: []
  })
  return [
    // The scalar shapes.
    ['damage', 'diamond_pickaxe', { type: 'damage', data: 431 }],
    ['max_damage', 'diamond_pickaxe', { type: 'max_damage', data: 900 }],
    ['max_stack_size', 'stone', { type: 'max_stack_size', data: 5 }],
    ['repair_cost', 'diamond_pickaxe', { type: 'repair_cost', data: 3 }],
    ['custom_model_data', 'stone', { type: 'custom_model_data', data: 7 }],
    ['map_id', 'filled_map', { type: 'map_id', data: 4 }],
    ['map_post_processing', 'filled_map', { type: 'map_post_processing', data: 1 }],
    ['ominous_bottle_amplifier', 'ominous_bottle', { type: 'ominous_bottle_amplifier', data: 2 }],
    ['base_color', 'shield', { type: 'base_color', data: 3 }],
    ['rarity', 'stone', { type: 'rarity', data: 'rare' }],
    ['map_color', 'filled_map', { type: 'map_color', data: 5636095 }],
    ['unbreakable', 'diamond_pickaxe', { type: 'unbreakable', data: true }],
    ['enchantment_glint_override', 'stone', { type: 'enchantment_glint_override', data: true }],
    ['note_block_sound', 'player_head', { type: 'note_block_sound', data: 'minecraft:block.note_block.harp' }],

    // The four whose whole value is that they are present. A walker that read
    // even one byte for these puts every later component out by one.
    ['hide_tooltip', 'stone', { type: 'hide_tooltip', data: undefined }],
    ['hide_additional_tooltip', 'stone', { type: 'hide_additional_tooltip', data: undefined }],
    ['creative_slot_lock', 'stone', { type: 'creative_slot_lock', data: undefined }],
    ['fire_resistant', 'stone', { type: 'fire_resistant', data: undefined }],

    // Whole-NBT shapes, including the one that reads like a unit and is not.
    ['custom_name', 'diamond_sword', { type: 'custom_name', data: nbtString('Bob') }],
    ['item_name', 'stone', { type: 'item_name', data: nbtString('Rock') }],
    ['custom_data', 'stone', { type: 'custom_data', data: nbtEmpty }],
    ['intangible_projectile', 'arrow', { type: 'intangible_projectile', data: nbtEmpty }],
    ['entity_data', 'armor_stand', { type: 'entity_data', data: nbtWithId('minecraft:armor_stand') }],
    ['block_entity_data', 'furnace', { type: 'block_entity_data', data: nbtWithId('minecraft:furnace') }],
    ['debug_stick_state', 'debug_stick', { type: 'debug_stick_state', data: nbtEmpty }],
    ['map_decorations', 'filled_map', { type: 'map_decorations', data: nbtEmpty }],
    ['lore', 'stone', { type: 'lore', data: [nbtString('one'), nbtString('two')] }],

    // Lists and flags.
    ['enchantments', 'diamond_sword', { type: 'enchantments', data: { enchantments: [{ id: 2, level: 3 }, { id: 5, level: 1 }], showTooltip: true } }],
    ['stored_enchantments', 'enchanted_book', { type: 'stored_enchantments', data: { enchantments: [{ id: 2, level: 3 }], showInTooltip: false } }],
    ['dyed_color', 'leather_helmet', { type: 'dyed_color', data: { color: 3368635, showTooltip: true } }],
    ['suspicious_stew_effects', 'suspicious_stew', { type: 'suspicious_stew_effects', data: { effects: [{ effect: 1, duration: 100 }] } }],
    ['pot_decorations', 'decorated_pot', { type: 'pot_decorations', data: { decorations: [1, 2, 3, 4] } }],
    ['block_state', 'furnace', { type: 'block_state', data: { properties: [{ property: 'facing', value: 'north' }] } }],
    ['attribute_modifiers', 'diamond_sword', { type: 'attribute_modifiers', data: { attributes: [{ typeId: 1, name: 'minecraft:probe', value: 1.5, operation: 'add', slot: 'main_hand' }], showTooltip: true } }],
    ['profile', 'player_head', { type: 'profile', data: { name: 'Bob', uuid: undefined, properties: [] } }],
    ['writable_book_content', 'writable_book', { type: 'writable_book_content', data: { pages: [{ content: 'hello', filteredContent: undefined }] } }],
    ['written_book_content', 'written_book', { type: 'written_book_content', data: { rawTitle: 'T', filteredTitle: undefined, author: 'A', generation: 0, pages: [{ content: nbtString('page'), filteredContent: undefined }], resolved: false } }],
    ['bees', 'beehive', { type: 'bees', data: { bees: [{ nbtData: nbtEmpty, ticksInHive: 10, minTicksInHive: 20 }] } }],

    // Options, including one whose option is a packed position.
    ['lodestone_tracker', 'compass', { type: 'lodestone_tracker', data: { globalPosition: { dimension: 'minecraft:overworld', position: { x: 1, y: 2, z: 3 } }, tracked: true } }],
    ['firework_explosion', 'firework_star', { type: 'firework_explosion', data: { shape: 'star', colors: [1, 2], fadeColors: [], hasTrail: true, hasTwinkle: false } }],
    ['fireworks', 'firework_rocket', { type: 'fireworks', data: { flightDuration: 2, explosions: [{ shape: 'burst', colors: [7], fadeColors: [8, 9], hasTrail: false, hasTwinkle: true }] } }],

    // Holders: an id, and the same thing written out inline.
    ['banner_patterns by id', 'white_banner', { type: 'banner_patterns', data: { layers: [{ pattern: { patternId: 5 }, colorId: 3 }] } }],
    ['trim', 'diamond_chestplate', { type: 'trim', data: { material: { materialId: 1 }, pattern: { patternId: 1 }, showInTooltip: true } }],
    ['instrument', 'goat_horn', { type: 'instrument', data: { instrumentId: 2 } }],
    ['jukebox_playable by name', 'music_disc_cat', { type: 'jukebox_playable', data: { hasHolder: false, song: 'minecraft:pigstep', showInTooltip: true } }],

    // Holder sets: a tag name, and a list of ids.
    ['tool by tag', 'diamond_pickaxe', { type: 'tool', data: { rules: [{ blocks: { name: 'minecraft:mineable/pickaxe' }, speed: 1.5, correctDropForBlocks: true }], defaultMiningSpeed: 1, damagePerBlock: 1 } }],
    ['tool by ids', 'diamond_pickaxe', { type: 'tool', data: { rules: [{ blocks: { ids: [1, 2, 3] }, speed: undefined, correctDropForBlocks: undefined }], defaultMiningSpeed: 1, damagePerBlock: 1 } }],
    ['can_place_on', 'stone', { type: 'can_place_on', data: { predicates: [{ blockSet: { blockIds: [1, 2] }, properties: undefined, nbt: undefined }], showTooltip: true } }],
    ['can_break by tag', 'stone', { type: 'can_break', data: { predicates: [{ blockSet: { name: 'minecraft:logs' }, properties: undefined, nbt: undefined }], showTooltip: false } }],
    ['can_break by property', 'stone', { type: 'can_break', data: { predicates: [{ blockSet: undefined, properties: [{ name: 'facing', isExactMatch: true, value: { exactValue: 'north' } }], nbt: undefined }], showTooltip: false } }],

    // A food with anything in it is not sent from here, and both halves of
    // why are findings rather than omissions. Its `usingConvertsTo` is an
    // *optional* stack — a flag, then a stack — where minecraft-data 3.115 has
    // a bare stack; the two agree when there is nothing to leave behind, and a
    // real server refuses the bare form the moment there is. This client can
    // only send the bare form, so what settled it is the server's complaint.
    //
    // A food's *effects* are the same story. minecraft-data 3.115 says a food effect is a VarInt and a
    // float; Dust says it is a whole effect instance and a float. Sending
    // minecraft-data's shape made a real 1.21.1 server answer "Failed to
    // decode", which is the server reading *past* the end rather than
    // stopping short — so it wanted more bytes than five, and Dust is right.
    // What this client cannot then do is send the longer form, so that one
    // branch is settled by the server's complaint rather than by its echo.

    // Two component types are missing from this list and their absence is the
    // measurement. `potion_contents` cannot be sent: minecraft-data 3.115
    // gives 1.21.1 the *1.21.2* layout, with a fourth optional custom-name
    // field, and a real 1.21.1 server answers a stack carrying it with "was
    // larger than I expected, found 1 bytes extra". Dust had copied the same
    // extra field and that sentence is what found it. `banner_patterns` with
    // the pattern written out inline is refused by a real server too, so the
    // inline branch of a holder is implemented and *not* verified here — the
    // by-id branch beside it is.

    // Stacks inside stacks, which is the recursion a wrong length breaks.
    ['charged_projectiles', 'crossbow', { type: 'charged_projectiles', data: { projectiles: [stack('arrow', 1)] } }],
    ['bundle_contents', 'bundle', { type: 'bundle_contents', data: { contents: [stack('stone', 2), stack('dirt', 3)] } }],
    ['container', 'shulker_box', { type: 'container', data: { contents: [stack('stone', 1)] } }],
    ['container of a named stack', 'shulker_box', { type: 'container', data: { contents: [named(stack('stone', 1), 'Bob')] } }],
    ['food, empty', 'apple', { type: 'food', data: { nutrition: 4, saturationModifier: 0.5, canAlwaysEat: false, secondsToEat: 1.6, usingConvertsTo: { itemCount: 0 }, effects: [] } }],

    // Two at once, so the *end* of the first is what finds the second.
    ['two components on one stack', 'diamond_sword', [
      { type: 'custom_name', data: nbtString('Bob') },
      { type: 'damage', data: 12 }
    ]],
    ['a removal beside an addition', 'diamond_sword', [
      { type: 'damage', data: 12 }
    ], ['max_stack_size']]
  ]
}

function creativeStack (b, slot, itemName, count, components, removals) {
  b._client.write('set_creative_slot', {
    slot,
    item: {
      itemCount: count,
      itemId: b.registry.itemsByName[itemName].id,
      addedComponentCount: components.length,
      removedComponentCount: removals.length,
      components,
      removeComponents: removals.map(type => ({ type }))
    }
  })
}

// The client's own model of a slot it just wrote. A server says nothing about
// a creative write it accepted, so a snapshot that still reads this is a
// snapshot where the server never spoke — which is the failure this whole
// shape exists to make visible, rather than an agreement.
const UNANSWERED = '(the server never said)'

async function recordComponents (port, out) {
  let bot = await spawned(port, 'Componenter')
  let state = tracker(bot)
  await wait(SPAWN_SETTLE_MS)

  // A server that cannot decode a component kicks the connection, so the run
  // has to be able to lose one and carry on. Reconnecting is not tidying up:
  // an entry that costs a connection is recorded as one, and a run where the
  // two servers refuse different entries is exactly the disagreement worth
  // reporting.
  const reconnect = async () => {
    try { bot.quit() } catch (e) { /* already gone */ }
    bot = await spawned(port, 'Componenter')
    state = tracker(bot)
    await wait(SPAWN_SETTLE_MS)
  }
  const alive = () => bot._client && !bot._client.ended
  const clear = () => { for (let slot = 1; slot < SLOTS; slot++) creativeSlot(bot, slot, null, 0) }

  clear()
  await wait(SPAWN_SETTLE_MS)

  const snapshots = []
  for (const [label, itemName, spec, removals] of componentCorpus(bot.registry)) {
    if (!alive()) await reconnect()
    const components = Array.isArray(spec) ? spec : [spec]
    try {
      creativeStack(bot, MAIN, itemName, 1, components, removals || [])
    } catch (e) {
      // This client could not even encode it. Recorded, not skipped: a step
      // that vanished from one recording and not the other would make the two
      // files different lengths and the comparison would refuse to run.
      snapshots.push({ step: `${label}: onto the cursor`, slots: { 9: `unencodable: ${e.message}` }, cursor: null })
      snapshots.push({ step: `${label}: back into the slot`, slots: { 9: `unencodable: ${e.message}` }, cursor: null })
      continue
    }
    state.slots[MAIN] = { name: itemName, count: 1, components: UNANSWERED }
    state.cursor = null
    await wait(CLICK_SETTLE_MS)

    // Take it, claiming nothing moved. The claim is false, so the server has
    // to send back both halves — and what it sends is *its* encoding of the
    // components, which is the number this script exists to produce.
    if (alive()) windowClick(bot, MAIN, 0, PICKUP)
    await wait(CLICK_SETTLE_MS)
    snapshots.push(snapshot(state, `${label}: onto the cursor`))

    // And put it back, claiming nothing moved again. The model is reset to what
    // the *claim* says first, for the reason `record` gives: the remote model a
    // server corrects against is the one the click asserted, so tracking
    // anything else would compare two servers against two different baselines.
    state.cursor = null
    if (alive()) windowClick(bot, MAIN, 0, PICKUP)
    await wait(CLICK_SETTLE_MS)
    snapshots.push(snapshot(state, `${label}: back into the slot`))

    if (alive()) {
      creativeSlot(bot, MAIN, null, 0)
      state.slots[MAIN] = null
      state.cursor = null
      await wait(CLICK_SETTLE_MS)
    }
  }

  // The merge rule, which is the part a player loses items to. Two stacks of
  // one item that differ only in their components must stay two stacks.
  for (const [label, a, b] of [
    ['two stacks named the same merge', [{ type: 'custom_name', data: nbtString('Bob') }], [{ type: 'custom_name', data: nbtString('Bob') }]],
    ['a named stack and a plain one do not', [{ type: 'custom_name', data: nbtString('Bob') }], []],
    ['two differently named stacks do not', [{ type: 'custom_name', data: nbtString('Bob') }], [{ type: 'custom_name', data: nbtString('Sue') }]]
  ]) {
    if (!alive()) await reconnect()
    // Whatever the previous case left on the cursor goes on the floor first.
    // A cursor carried into the next case is a click starting from somewhere
    // nobody stated, and the answer it produces is about that rather than
    // about the rule under test — which is how the third case of this loop
    // read as a swap of two stacks it had never been given.
    if (alive()) windowClick(bot, OUTSIDE, 0, PICKUP)
    await wait(CLICK_SETTLE_MS)
    clear()
    creativeStack(bot, MAIN, 'stone', 16, a, [])
    creativeStack(bot, MAIN + 1, 'stone', 16, b, [])
    await wait(CLICK_SETTLE_MS)
    for (let slot = 1; slot < SLOTS; slot++) state.slots[slot] = null
    state.slots[MAIN] = { name: 'stone', count: 16, components: UNANSWERED }
    state.slots[MAIN + 1] = { name: 'stone', count: 16, components: UNANSWERED }
    state.cursor = null
    // Pick the first up and put it down on the second, claiming nothing moved
    // both times. A merge makes one stack of thirty-two and empties the other;
    // a refusal swaps them. Either way both servers must speak.
    if (alive()) windowClick(bot, MAIN, 0, PICKUP)
    await wait(CLICK_SETTLE_MS)
    state.cursor = null
    if (alive()) windowClick(bot, MAIN + 1, 0, PICKUP)
    await wait(CLICK_SETTLE_MS)
    snapshots.push(snapshot(state, `merge: ${label}`))
  }

  try { bot.quit() } catch (e) { /* already gone */ }
  const silent = snapshots.filter(s => JSON.stringify(s).includes(UNANSWERED)).length
  require('fs').writeFileSync(out, JSON.stringify(snapshots, null, 1))
  console.log(`${snapshots.length} snapshots written to ${out}`)
  console.log(`${silent} of them are slots this server never spoke about`)
  process.exit(0)
}

// Snapshots where the two servers are *expected* to disagree, and why.
//
// Not a list of things that are wrong. Every one of these is Minecraft
// rewriting a value it understands, and Dust echoing bytes it deliberately
// does not: that is the whole design, so a difference here is the design
// working. A difference that is **not** named here fails the comparison, which
// is what keeps this from being a way to make red go away.
const REWRITTEN_BY_MINECRAFT = new Map([
  ['debug_stick_state', 'an empty compound is not a state, and Minecraft drops the component'],
  ['map_decorations', 'the same: Minecraft drops a decoration list it cannot read'],
  ['enchantments', 'Minecraft holds enchantments in a map and writes them back in its own order'],
  ['profile', 'Minecraft resolves a profile by name and fills in the uuid and the textures'],
  ['lodestone_tracker', 'Minecraft clears a position that is not a lodestone']
])

function namedRewrite (step) {
  for (const [prefix, why] of REWRITTEN_BY_MINECRAFT) {
    if (step.startsWith(prefix)) return why
  }
  return null
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
  let named = 0
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
      const why = namedRewrite(a[i].step || '')
      if (why) {
        named++
        console.log(`  told ${a[i].step} — ${why}`)
      } else {
        disagreed++
        console.log(`  DIFF  ${a[i].step}`)
        lines.forEach(l => console.log(l))
      }
    }
  }
  console.log(`\n${a.length - disagreed - named}/${a.length} snapshots agree`)
  console.log(`${named} differ for a named reason, ${disagreed} differ for none`)
  process.exit(disagreed === 0 ? 0 : 1)
}

const args = process.argv.slice(2)
if (args[0] === '--compare') {
  compare(args[1], args[2])
} else if (args.includes('--components')) {
  const outAt = args.indexOf('--out')
  recordComponents(Number(args[0]), outAt === -1 ? 'components.json' : args[outAt + 1]).catch(e => {
    console.log(`FAIL  ${e.message}`)
    process.exit(1)
  })
} else if (args.includes('--predict')) {
  predict(Number(args[0])).catch(e => {
    console.log(`FAIL  ${e.message}`)
    process.exit(1)
  })
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
