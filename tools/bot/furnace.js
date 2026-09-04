// What a furnace does over time, recorded against a real 1.21.1 server.
//
//   node furnace.js <port> --out dust.json
//   node furnace.js <port> --out vanilla.json      (pointed at a vanilla server)
//   node furnace.js --compare vanilla.json dust.json
//   node furnace.js <port> --states
//   node furnace.js <port> --restart-arm | --restart-check
//
// # Why this one is different from `clicks.js` and `crafting.js`
//
// Every check before this one was a *sequence*: click, snapshot, click,
// snapshot. Nothing happened between the snapshots. A furnace happens between
// the snapshots — it is the first thing in Dust that moves while nobody is
// touching it — so this samples on a timer and the measurement is a
// trajectory rather than a list.
//
// # The trap that shape walks into, and what is done about it
//
// **Two servers do not tick in step with a wall clock.** A recording keyed on
// elapsed milliseconds would disagree by a tick or two everywhere and agree
// nowhere, and loosening it until it passed would loosen it past the errors
// worth catching. So the comparison is over quantities that are *integers the
// server itself states*, not over times this script measured:
//
//   * `litTotal` and `cookTotal` — properties 1 and 3, straight out of the
//     fuel table and the recipe file. A furnace burning at the wrong rate has
//     the wrong number here, exactly, and no tolerance is involved.
//   * the **sequence of output counts** — 0,1,2,…,8. Quantised, so sampling
//     jitter cannot move it.
//   * how many ticks of fuel each new ingot appeared at, from the server's own
//     `lit` counter rather than from a stopwatch: `litTotal - lit` is a tick
//     count both servers agree on the meaning of. Compared to within a few
//     ticks, which is one sampling gap and is two orders of magnitude tighter
//     than the errors it is there to catch.
//   * the final slot contents, exactly.
//
// # And the trap `crafting.js` paid for, which applies here twice over
//
// A differential where both sides legitimately do nothing *agrees*. A furnace
// has more of those states than anything before it — no fuel, no input, output
// full, mid-burn, just gone out — and from outside, at one moment, four of
// them look like "nothing is happening". `--states` is the mode that asks each
// one a question whose answers differ, and every check in it requires a
// positive answer rather than the absence of a change.
//
// # The tracker attaches at construction
//
// A join burst arrives in one TCP read and node runs those handlers
// synchronously, before the microtask a `spawn` listener resolves. A tracker
// attached after `spawn` misses the container it was told at login.

const mineflayer = require('mineflayer')
const { Vec3 } = require('vec3')

const VERSION = '1.21.1'
const JOIN_TIMEOUT_MS = 60000
const SPAWN_SETTLE_MS = 3000
const CLICK_SETTLE_MS = 400
const SAMPLE_MS = 250

// A furnace window: 0 input, 1 fuel, 2 result, 3..29 inventory, 30..38 hotbar.
const IN = 0
const FUEL = 1
const OUT = 2
const FURNACE_SLOTS = 39

// The player's own numbering, which is what a creative write always uses.
const PLAYER_SLOTS = 46

const PICKUP = 0
const QUICK_MOVE = 1

// The four properties a furnace screen draws, in the protocol's own order.
const P_LIT = 0
const P_LIT_TOTAL = 1
const P_COOK = 2
const P_COOK_TOTAL = 3

const wait = ms => new Promise(r => setTimeout(r, ms))

function spawned (port, username) {
  return new Promise((resolve, reject) => {
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

// What the server has told us, out of the packets. `bot.window` is not used
// for the same reason `clicks.js` does not use it, and the properties are not
// in it at all.
function tracker (b) {
  const state = {
    slots: new Array(FURNACE_SLOTS).fill(null),
    properties: [null, null, null, null],
    window: 0,
    menu: null,
    opened: 0,
    experience: null,
    level: null
  }
  const name = id => (b.registry.items[id] ? b.registry.items[id].name : `id:${id}`)
  const read = item => {
    if (!item || !item.itemCount || item.itemCount === 0) return null
    return { name: name(item.itemId), count: item.itemCount }
  }
  b._client.on('open_window', p => {
    state.opened++
    state.window = p.windowId
    state.menu = p.inventoryType
    state.slots = new Array(FURNACE_SLOTS).fill(null)
    state.properties = [null, null, null, null]
  })
  b._client.on('set_slot', p => {
    if (p.windowId !== state.window) return
    if (p.slot >= 0 && p.slot < FURNACE_SLOTS) state.slots[p.slot] = read(p.item)
  })
  b._client.on('window_items', p => {
    if (p.windowId !== state.window) return
    for (let i = 0; i < p.items.length && i < FURNACE_SLOTS; i++) {
      state.slots[i] = read(p.items[i])
    }
  })
  // The packet nothing before this needed. Without it a furnace screen draws a
  // cold fire and an empty arrow however far through a smelt it is.
  b._client.on('craft_progress_bar', p => {
    if (p.windowId !== state.window) return
    if (p.property >= 0 && p.property < 4) state.properties[p.property] = p.value
  })
  b._client.on('experience', p => {
    state.experience = p.totalExperience
    state.level = p.level
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

function windowClick (b, windowId, slot, mouseButton, mode) {
  b._client.write('window_click', {
    windowId, stateId: 0, slot, mouseButton, mode, changedSlots: [], cursorItem: { itemCount: 0 }
  })
}

function describe (s) {
  return s ? `${s.name} x${s.count}` : null
}

// Somewhere to put a block, out of the client's own view of the world.
//
// The obvious spot — the block the player is standing on, placed against its
// top face — is the one that cannot work: it would put the furnace inside the
// player's own hitbox, and a real server refuses that placement while Dust does
// not, so the diff would read as "vanilla never opens a furnace". That happened
// to `crafting.js` on its first run.
function support (bot) {
  const feet = bot.entity.position
  for (const [dx, dz] of [[2, 0], [-2, 0], [0, 2], [0, -2], [3, 0], [0, 3], [2, 2], [-2, -2]]) {
    for (let dy = 0; dy >= -3; dy--) {
      const at = {
        x: Math.floor(feet.x) + dx,
        y: Math.floor(feet.y) + dy - 1,
        z: Math.floor(feet.z) + dz
      }
      const here = bot.blockAt(new Vec3(at.x, at.y, at.z))
      const above = bot.blockAt(new Vec3(at.x, at.y + 1, at.z))
      if (here && above && here.name !== 'air' && above.name === 'air') return at
    }
  }
  throw new Error('no solid block with air above it within reach')
}

function place (bot, at, sequence) {
  bot._client.write('block_place', {
    hand: 0,
    location: at,
    direction: 1,
    cursorX: 0.5,
    cursorY: 1.0,
    cursorZ: 0.5,
    insideBlock: false,
    sequence
  })
}

// Put a furnace down and open it. Returns { placed, block, window }.
async function openFurnace (bot, block = 'furnace') {
  const state = bot.tracked
  for (let slot = 1; slot < PLAYER_SLOTS; slot++) creativeSlot(bot, slot, null, 0)
  creativeSlot(bot, 36, block, 1)
  bot._client.write('held_item_slot', { slotId: 0 })
  await wait(SPAWN_SETTLE_MS)

  const below = support(bot)
  const placed = { x: below.x, y: below.y + 1, z: below.z }
  place(bot, below, 1)
  await wait(SPAWN_SETTLE_MS)
  const there = bot.blockAt(new Vec3(placed.x, placed.y, placed.z))
  // Crouch-free: `useItemOn` asks the block first, so a right-click on a
  // furnace while holding a furnace opens it rather than stacking a second.
  place(bot, placed, 2)
  await wait(SPAWN_SETTLE_MS)
  return { placed, block: there ? there.name : 'nothing', window: state.window }
}

function sample (state, elapsed) {
  return {
    t: elapsed,
    lit: state.properties[P_LIT],
    litTotal: state.properties[P_LIT_TOTAL],
    cook: state.properties[P_COOK],
    cookTotal: state.properties[P_COOK_TOTAL],
    in: describe(state.slots[IN]),
    fuel: describe(state.slots[FUEL]),
    out: describe(state.slots[OUT])
  }
}

function outCount (row) {
  if (!row.out) return 0
  const m = /x(\d+)$/.exec(row.out)
  return m ? Number(m[1]) : 0
}

// One coal, sixteen raw iron, and long enough for the coal to be gone.
//
// The numbers are chosen so the answer is an integer rather than a rate:
// 1,600 ticks of coal at 200 ticks a smelt is **eight ingots, exactly**, and
// the last one completes on the tick the fuel runs out. A furnace that took
// the fuel one tick early makes seven and one that held the fire a tick late
// makes nine, and neither is visible in "about ten seconds an ingot".
const BURN_SECONDS = 88

async function record (port, out) {
  const bot = await spawned(port, 'Smelter')
  const state = bot.tracked
  await wait(SPAWN_SETTLE_MS)

  const opened = await openFurnace(bot)
  const window = state.window

  // Seeded through the *player's* inventory and then shift-clicked in, rather
  // than written straight into the furnace: a creative write cannot address a
  // block's slots, and the shift-click is itself a thing the two servers must
  // agree about — a log both smelts and burns, so which slot a stack lands in
  // is a real disagreement waiting to happen.
  creativeSlot(bot, 9, 'coal', 1)
  creativeSlot(bot, 10, 'raw_iron', 16)
  await wait(SPAWN_SETTLE_MS)

  // Furnace-window slot 3 is the first main-inventory slot, which is the
  // player's own slot 9.
  windowClick(bot, window, 3, 0, QUICK_MOVE)
  await wait(CLICK_SETTLE_MS)
  windowClick(bot, window, 4, 0, QUICK_MOVE)
  await wait(CLICK_SETTLE_MS)

  const rows = [{ step: 'loaded', placed: opened.block, menu: state.menu, ...sample(state, 0) }]
  const started = Date.now()
  while (Date.now() - started < BURN_SECONDS * 1000) {
    await wait(SAMPLE_MS)
    rows.push(sample(state, Date.now() - started))
  }
  // And what the player is paid for taking it out, which is the other half a
  // recording of the slots alone cannot see.
  const before = state.experience
  windowClick(bot, window, OUT, 0, QUICK_MOVE)
  await wait(CLICK_SETTLE_MS)
  rows.push({
    step: 'taken',
    ...sample(state, Date.now() - started),
    experienceBefore: before,
    experience: state.experience,
    level: state.level
  })

  try { bot.quit() } catch (e) { /* already gone */ }
  require('fs').writeFileSync(out, JSON.stringify(rows, null, 1))
  console.log(`${rows.length} samples over ${BURN_SECONDS}s to ${out} (menu ${state.menu})`)
}

function compare (left, right) {
  const a = JSON.parse(require('fs').readFileSync(left, 'utf8'))
  const b = JSON.parse(require('fs').readFileSync(right, 'utf8'))
  const problems = []
  let checks = 0
  let agree = 0
  const check = (what, l, r) => {
    checks++
    if (JSON.stringify(l) === JSON.stringify(r)) { agree++; return }
    problems.push(`${what}: ${JSON.stringify(l)} / ${JSON.stringify(r)}`)
  }

  check('the furnace is placed', a[0].placed, b[0].placed)
  check('the menu it opens', a[0].menu, b[0].menu)
  check('the shift-click loaded the input', a[0].in, b[0].in)
  check('the shift-click loaded the fuel', a[0].fuel, b[0].fuel)

  // The two numbers that are the rate, stated by the server and not timed.
  const stated = rows => {
    const lit = rows.map(r => r.litTotal).filter(v => v !== null && v > 0)
    const cook = rows.map(r => r.cookTotal).filter(v => v !== null && v > 0)
    return { litTotal: lit[0] ?? null, cookTotal: cook[0] ?? null }
  }
  const sa = stated(a)
  const sb = stated(b)
  check('the fuel is worth this many ticks', sa.litTotal, sb.litTotal)
  check('one smelt takes this many ticks', sa.cookTotal, sb.cookTotal)

  // The sequence of output counts. Quantised, so jitter cannot move it.
  const sequence = rows => {
    const out = []
    for (const row of rows) {
      const n = outCount(row)
      if (out.length === 0 || out[out.length - 1] !== n) out.push(n)
    }
    return out
  }
  check('the output count goes', sequence(a), sequence(b))

  // **How many different arrow positions the client was ever told about.**
  // The check that catches a server whose two bars are right at the moment
  // they are read and never move in between: a furnace that announced only
  // when an ingot appeared would report eight or nine distinct values here
  // where one that animates reports hundreds. Compared as "both saw plenty"
  // rather than exactly, because it is a sampling count and not a fact about
  // the furnace.
  const moved = rows => new Set(rows.map(r => r.cook).filter(v => v !== null)).size
  checks++
  const [ma, mb] = [moved(a), moved(b)]
  if (ma >= 100 && mb >= 100) {
    agree++
    console.log(`  distinct arrow positions seen: ${ma} / ${mb}`)
  } else {
    problems.push(`the arrow was seen at ${ma} / ${mb} positions; a bar a player watches moves`)
  }

  // And **when** each one appeared, measured in the server's own ticks of
  // fuel rather than in milliseconds this script counted. A few ticks of slack
  // is one sampling gap; the errors this is here to catch are hundreds.
  const arrivals = rows => {
    const at = []
    let seen = 0
    for (const row of rows) {
      const n = outCount(row)
      if (n > seen && row.lit !== null && row.litTotal) {
        at.push(row.litTotal - row.lit)
        seen = n
      }
    }
    return at
  }
  const aa = arrivals(a)
  const ba = arrivals(b)
  checks++
  if (aa.length !== ba.length) {
    problems.push(`ingots arrived ${aa.length} times / ${ba.length} times`)
  } else {
    const SLACK = 12
    const off = aa.map((t, i) => Math.abs(t - ba[i]))
    const worst = Math.max(0, ...off)
    if (worst > SLACK) {
      problems.push(`ingot arrival ticks differ by up to ${worst}: ${aa} / ${ba}`)
    } else {
      agree++
      console.log(`  ingots arrived at fuel ticks ${aa.join(', ')} / ${ba.join(', ')} (worst gap ${worst})`)
    }
  }

  const last = rows => rows[rows.length - 1]
  check('the input left over', last(a).in, last(b).in)
  check('the fuel left over', last(a).fuel, last(b).fuel)
  check('the output taken out', last(a).out, last(b).out)
  // Experience is a whole number of points and the fraction is a coin toss, so
  // this compares the *floor* rather than the exact value: 8 iron ingots are
  // 5.6 points and both 5 and 6 are correct answers.
  checks++
  const xp = rows => (last(rows).experience ?? 0) - (last(rows).experienceBefore ?? 0)
  const [xa, xb] = [xp(a), xp(b)]
  if (Math.abs(xa - xb) <= 1 && xa >= 5) {
    agree++
    console.log(`  experience for eight ingots: ${xa} / ${xb} (5.6 rounds to 5 or 6)`)
  } else {
    problems.push(`experience ${xa} / ${xb}`)
  }

  for (const p of problems) console.log(`  ${p}`)
  console.log(`\n${agree}/${checks} agree (${left} / ${right})`)
  process.exit(agree === checks ? 0 : 1)
}

// The states that all look like "nothing is happening" from outside.
//
// Each row here asks a question whose two answers are different *things that
// happened*, never "did anything change". A check that can pass by both
// servers staying silent is not a check — `clicks.js --predict` is the same
// argument and `crafting.js --refuse` is the same argument again.
async function states (port) {
  const bot = await spawned(port, 'Stater')
  const state = bot.tracked
  await wait(SPAWN_SETTLE_MS)

  const opened = await openFurnace(bot)
  // **Read at click time, never captured.** Every click below went to the
  // window id the *first* open handed out, and the close-and-reopen halfway
  // through this run hands out a different one — so the last two rows clicked
  // into a window that no longer existed, the server correctly ignored them,
  // and the two rows went red against a server that was right. It looked like
  // a furnace that would not give its ingot back. A stale window id is silence
  // on both sides, which is the shape this suite exists to refuse.
  const window = () => state.window
  const checks = []
  const load = async (slot, item, count) => {
    creativeSlot(bot, slot, item, count)
    await wait(CLICK_SETTLE_MS)
  }
  const shift = async slot => {
    windowClick(bot, window(), slot, 0, QUICK_MOVE)
    await wait(CLICK_SETTLE_MS)
  }
  const clear = async () => {
    // Take everything back out of the furnace, so one row cannot leave a state
    // the next one reads as its own.
    for (const slot of [IN, FUEL, OUT]) await shift(slot)
    for (let slot = 9; slot < 45; slot++) creativeSlot(bot, slot, null, 0)
    await wait(CLICK_SETTLE_MS)
  }

  checks.push(['a furnace opens on a right-click', state.opened === 1 && opened.block === 'furnace'])
  checks.push(['and the screen is told its cook time', state.properties[P_COOK_TOTAL] !== null])

  // Fuel and no input: the fire must not light and the coal must not go.
  await clear()
  await load(9, 'coal', 4)
  await shift(3)
  checks.push(['fuel alone lands in the fuel slot', state.slots[FUEL] !== null && state.slots[FUEL].count === 4])
  await wait(2000)
  checks.push(['fuel alone does not light the fire', state.properties[P_LIT] === 0])
  checks.push(['and no coal is spent', state.slots[FUEL] !== null && state.slots[FUEL].count === 4])

  // Now add the input and the same furnace must start.
  await load(10, 'raw_iron', 4)
  await shift(4)
  checks.push(['the input lands in the input slot', state.slots[IN] !== null])
  await wait(2000)
  checks.push(['adding an input lights the fire', state.properties[P_LIT] > 0])
  // **Moving, not merely non-zero.** `P_COOK > 0` was what this asked first,
  // and a mutation says that is not the same question: with the per-tick
  // announce for a watched furnace deleted, so that the arrow only ever
  // updates when a slot changes, all eighteen rows here still passed — a
  // window click resends the properties, so a single reading is always a
  // fresh one. Two readings a second apart with no click between them is the
  // question the animation actually answers; under that same mutation this
  // row goes red.
  const arrowWas = state.properties[P_COOK]
  await wait(1000)
  checks.push([
    `the arrow moves on its own (${arrowWas} -> ${state.properties[P_COOK]})`,
    state.properties[P_COOK] > arrowWas
  ])
  checks.push(['and one coal is gone', state.slots[FUEL] !== null && state.slots[FUEL].count === 3])

  // Taking the input out mid-cook throws the progress away, which is the
  // state a check that only asks "is there an ingot" cannot see at all.
  await shift(IN)
  checks.push(['taking the input back resets the arrow', state.properties[P_COOK] === 0])
  checks.push(['and the fire keeps burning', state.properties[P_LIT] > 0])

  // A block that does not burn is refused by the fuel slot, and a shift-click
  // of something the fire cooks goes to the input rather than under it.
  await clear()
  await load(9, 'cobblestone', 8)
  await shift(3)
  checks.push([
    'cobblestone shift-clicks to the input, not the fuel',
    state.slots[IN] !== null && state.slots[FUEL] === null
  ])
  const cobbleAt = state.slots[IN] && state.slots[IN].count
  // And put there by hand, the fuel slot refuses it: pick the input up and
  // drop it on the fuel slot.
  windowClick(bot, window(), IN, 0, PICKUP)
  await wait(CLICK_SETTLE_MS)
  windowClick(bot, window(), FUEL, 0, PICKUP)
  await wait(CLICK_SETTLE_MS)
  checks.push(['and the fuel slot refuses it by hand', state.slots[FUEL] === null])
  checks.push(['and the cobblestone is not destroyed', cobbleAt === 8])
  // Put it back down somewhere it is allowed.
  windowClick(bot, window(), IN, 0, PICKUP)
  await wait(CLICK_SETTLE_MS)

  // Nothing may be put into the output.
  await clear()
  await load(9, 'iron_ingot', 4)
  await shift(3)
  windowClick(bot, window(), IN, 0, PICKUP)
  await wait(CLICK_SETTLE_MS)
  windowClick(bot, window(), OUT, 0, PICKUP)
  await wait(CLICK_SETTLE_MS)
  checks.push(['nothing may be put in the output', state.slots[OUT] === null])
  windowClick(bot, window(), IN, 0, PICKUP)
  await wait(CLICK_SETTLE_MS)

  // The window closes and the furnace keeps what is in it. This is the whole
  // difference between a furnace and every container before it.
  await clear()
  await load(9, 'coal', 2)
  await load(10, 'raw_iron', 2)
  await shift(3)
  await shift(4)
  await wait(1500)
  bot._client.write('close_window', { windowId: window() })
  await wait(CLICK_SETTLE_MS)
  await wait(3000)
  place(bot, opened.placed, 3)
  await wait(SPAWN_SETTLE_MS)
  checks.push(['reopening finds the same fuel still there', state.slots[FUEL] !== null])
  checks.push(['and the fire still lit', state.properties[P_LIT] > 0])
  // And it did not stand still while the screen was shut: the arrow is
  // further on than it was when the window closed, which no amount of
  // "nothing changed" can produce.
  const advanced = state.properties[P_LIT_TOTAL] - state.properties[P_LIT]
  checks.push([`it burned ${advanced} ticks with the screen shut`, advanced > 60])

  // And the ingot shift-clicks out of the block and into the player, which is
  // how a furnace is emptied and is the one rule this suite was blind to: with
  // the furnace's output routed down the crafting path — the loop that pays
  // out of a grid a furnace has not got — every row above still passed and the
  // ingots stayed in the block. `--compare` caught it three ways and this
  // caught it none, which is why the row is here.
  for (let i = 0; i < 60 && state.slots[OUT] === null; i++) await wait(500)
  const made = state.slots[OUT] && state.slots[OUT].count
  checks.push([`the fire filled the output (${describe(state.slots[OUT])})`, made > 0])
  // **Exactly what the furnace held, not merely more than before.** "More"
  // was the first wording and the mutation says it is the wrong question:
  // with the furnace's output routed down the crafting path, one ingot came
  // out as thirty-six — the loop crafted again into every free slot — and a
  // row that asks `after > before` calls that a pass. Counting items rather
  // than stacks, and demanding the exact number, is what separates "it moved"
  // from "it was copied".
  const ingots = () => state.slots.slice(3, FURNACE_SLOTS)
    .filter(s => s && s.name === 'iron_ingot')
    .reduce((n, s) => n + s.count, 0)
  const heldBefore = ingots()
  await shift(OUT)
  checks.push(['the output shift-clicks out', state.slots[OUT] === null])
  const heldAfter = ingots()
  checks.push([`and exactly the ${made} it held reach the player (${heldBefore} -> ${heldAfter})`,
    heldAfter === heldBefore + made])

  try { bot.quit() } catch (e) { /* already gone */ }
  let failed = 0
  for (const [what, ok] of checks) {
    if (!ok) failed++
    console.log(`  ${ok ? 'ok  ' : 'FAIL'}  ${what}`)
  }
  console.log(`\n${checks.length - failed}/${checks.length} checks passed`)
  process.exit(failed === 0 ? 0 : 1)
}

// A furnace across a restart, in two halves so a server can be stopped between
// them.
//
//   --restart-arm    light one and write down where it got to
//   --restart-check  reopen it and say whether it is where it was
//
// The control is in the second half: it also opens a furnace nobody ever lit,
// and requires that one to be cold. A check that only asserts "the furnace is
// burning" would pass on a server that lit every furnace it ever saw.
// Keyed on the port because four agents share `/tmp` on this machine and one
// of them has already written another's config out from under it.
const restartFile = port => process.env.DUST_RESTART_FILE ||
  `/tmp/dust-furnace-restart-${port}.json`

async function restartArm (port) {
  const bot = await spawned(port, 'Restarter')
  const state = bot.tracked
  await wait(SPAWN_SETTLE_MS)
  const opened = await openFurnace(bot)
  creativeSlot(bot, 9, 'coal', 2)
  creativeSlot(bot, 10, 'raw_iron', 8)
  await wait(SPAWN_SETTLE_MS)
  windowClick(bot, opened.window, 3, 0, QUICK_MOVE)
  await wait(CLICK_SETTLE_MS)
  windowClick(bot, opened.window, 4, 0, QUICK_MOVE)
  await wait(CLICK_SETTLE_MS)
  await wait(4000)
  const armed = {
    placed: opened.placed,
    lit: state.properties[P_LIT],
    litTotal: state.properties[P_LIT_TOTAL],
    cook: state.properties[P_COOK],
    fuel: describe(state.slots[FUEL]),
    in: describe(state.slots[IN])
  }
  bot._client.write('close_window', { windowId: opened.window })
  await wait(CLICK_SETTLE_MS)
  try { bot.quit() } catch (e) { /* already gone */ }
  require('fs').writeFileSync(restartFile(port), JSON.stringify(armed, null, 1))
  console.log(`armed: lit ${armed.lit}/${armed.litTotal}, arrow ${armed.cook}, ${armed.fuel}, ${armed.in}`)
  if (!(armed.lit > 0)) {
    console.log('FAIL  the furnace was never lit, so the restart proves nothing')
    process.exit(1)
  }
  process.exit(0)
}

async function restartCheck (port) {
  const armed = JSON.parse(require('fs').readFileSync(restartFile(port), 'utf8'))
  const bot = await spawned(port, 'Restarter')
  const state = bot.tracked
  await wait(SPAWN_SETTLE_MS)
  place(bot, armed.placed, 1)
  await wait(SPAWN_SETTLE_MS)
  const back = {
    lit: state.properties[P_LIT],
    litTotal: state.properties[P_LIT_TOTAL],
    cook: state.properties[P_COOK],
    fuel: describe(state.slots[FUEL]),
    in: describe(state.slots[IN])
  }
  const checks = [
    ['the furnace is still there and opens', state.opened === 1],
    ['its fuel came back', back.fuel === armed.fuel],
    // The *item*, not the count. It is still smelting while this runs, so a
    // furnace that came back correctly has one fewer raw iron in it a few
    // seconds later — and a check that demanded the exact number would be
    // demanding that the furnace stopped, which is the opposite of the thing
    // being measured.
    ['its input came back', back.in !== null && armed.in !== null &&
      back.in.split(' ')[0] === armed.in.split(' ')[0]],
    ['it is still alight', back.lit > 0],
    ['the fuel it was worth came back', back.litTotal === armed.litTotal],
    // Not "the same tick": the server ran for a moment before the bot arrived,
    // so the fire has come down a little. Forwards only, and by less than the
    // fuel it had: a furnace restored to zero and relit would read as full.
    ['it resumed where it was rather than restarting', back.lit <= armed.lit && back.lit > armed.lit - 400]
  ]

  // The control. A furnace nobody ever lit must be cold when it is opened, or
  // "it is burning" says nothing about the save at all.
  const fresh = await openFurnace(bot, 'blast_furnace')
  checks.push(['a furnace nobody lit opens cold', state.properties[P_LIT] === 0])
  checks.push(['and holds nothing', state.slots[FUEL] === null && state.slots[IN] === null])
  checks.push(['and it really is a second block', fresh.block === 'blast_furnace'])

  try { bot.quit() } catch (e) { /* already gone */ }
  let failed = 0
  for (const [what, ok] of checks) {
    if (!ok) failed++
    console.log(`  ${ok ? 'ok  ' : 'FAIL'}  ${what}`)
  }
  console.log(`\n${checks.length - failed}/${checks.length} checks passed`)
  console.log(`  armed at lit ${armed.lit}, came back at ${back.lit}`)
  process.exit(failed === 0 ? 0 : 1)
}

async function main () {
  const args = process.argv.slice(2)
  if (args[0] === '--compare') return compare(args[1], args[2])
  const port = Number(args[0])
  if (!port) {
    console.log('usage: furnace.js <port> [--out file.json | --states | --restart-arm | --restart-check]')
    console.log('       furnace.js --compare vanilla.json dust.json')
    process.exit(2)
  }
  if (args.includes('--states')) return states(port)
  if (args.includes('--restart-arm')) return restartArm(port)
  if (args.includes('--restart-check')) return restartCheck(port)
  const out = args[args.indexOf('--out') + 1]
  if (!args.includes('--out') || !out) {
    console.log('need --out <file.json>')
    process.exit(2)
  }
  await record(port, out)
  process.exit(0)
}

main().catch(e => { console.error(e.message); process.exit(1) })
