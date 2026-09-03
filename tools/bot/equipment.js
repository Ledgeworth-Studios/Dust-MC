// Records what one player can see of another player's gear, and prints it so
// the same recording can be taken from Minecraft's own server and the two
// diffed.
//
//   node equipment.js <port> --out dust.json
//   node equipment.js <port> --out vanilla.json    (pointed at a vanilla server)
//   node equipment.js --compare vanilla.json dust.json
//
// The comparison is the measurement. A recording on its own is not a result.
//
// # Why this needs three bots and not one
//
// `minecraft:set_equipment` is the one thing a server tells everybody *except*
// the player it is about. A wearer's own client draws its armour and its hand
// out of the container it already has, and vanilla's entity tracker does not
// send an entity its own equipment. So a script with one bot can only record
// silence, and silence is what a server that implements none of this also
// produces. There are three: a **wearer** who dresses, a **watcher** who was
// already here, and a **latecomer** who joins after the wearer is fully
// dressed and never sees a single thing change.
//
// The latecomer is the point. A server that sends equipment only on change
// looks completely correct to the watcher and leaves the latecomer staring at
// a naked player forever, which is the defect this whole feature is about.
//
// # Why every step records a count as well as a picture
//
// A differential where both sides legitimately send nothing *agrees*. Half the
// steps here are steps where the right answer is to say nothing — a stack
// dropped into the middle of the inventory changes no equipment slot — and a
// recording that only held the resulting picture would call a server that
// broadcasts all six slots on every click identical to one that broadcasts
// none. So each step records how many packets arrived and how many entries
// were in them, and those numbers are compared too. That is what makes "said
// nothing" a measurement rather than an absence.
//
// The same shape answers the batching question: four pieces of armour in one
// step is one packet of four entries or four packets of one, and the recording
// says which without anybody having to reason about it.
//
// # Why the packets are read raw
//
// mineflayer keeps an `entity.equipment` array, and it keeps it by a slot
// index of its own. Reading it would make this a check on prismarine-entity.
// The raw `entity_equipment` packet is what the server actually said.

const mineflayer = require('mineflayer')

const VERSION = '1.21.1'
const JOIN_TIMEOUT_MS = 60000
const SPAWN_SETTLE_MS = 3000
// How long a step's packets have to arrive. One tick plus a round trip, not a
// guess at how long work takes.
const STEP_SETTLE_MS = 600

// The six slots `minecraft:set_equipment` numbers, in its own order. Boots
// before the helmet, and the hand before either.
const WIRE_SLOTS = ['main_hand', 'off_hand', 'boots', 'leggings', 'chestplate', 'helmet']

// Container slots, in vanilla's numbering.
const HELMET = 5
const CHESTPLATE = 6
const LEGGINGS = 7
const BOOTS = 8
const MAIN = 9
const HOTBAR = 36
const OFFHAND = 45

const wait = ms => new Promise(r => setTimeout(r, ms))

const nbtString = value => ({ type: 'string', name: '', value })

// The packet listener is attached at *construction*, not after `spawn`.
//
// This is not tidiness, it is the difference between the script working and
// the script reporting a defect that is its own. A join burst arrives in one
// TCP read and node runs every packet handler for that read synchronously,
// while the promise a `spawn` listener resolves runs as a microtask *after*
// all of them. A listener attached once the bot has spawned therefore misses
// everything the server said in the same breath as the position packet —
// which is exactly where the equipment of everybody already here is sent. The
// first run of this script recorded a latecomer seeing nothing, and that
// reading was the instrument.
function spawned (port, username) {
  return new Promise((resolve, reject) => {
    // Three characters minimum: a shorter name never spawns and never errors.
    const b = mineflayer.createBot({
      host: '127.0.0.1', port, username, auth: 'offline', version: VERSION
    })
    b.seen = watching(b)
    const timer = setTimeout(
      () => reject(new Error(`${username} never reached the world in ${JOIN_TIMEOUT_MS / 1000}s`)),
      JOIN_TIMEOUT_MS
    )
    b.on('error', e => { clearTimeout(timer); reject(new Error(`${username}: ${e.message}`)) })
    b.on('kicked', r => { clearTimeout(timer); reject(new Error(`${username} was kicked: ${JSON.stringify(r)}`)) })
    b.once('spawn', () => { clearTimeout(timer); resolve(b) })
  })
}

// What one bot has been told about everybody's equipment, assembled from the
// packets. Keyed by entity id, because a name is a tab-list row and equipment
// is addressed to a body.
function watching (b) {
  const state = { worn: new Map(), packets: 0, entries: 0, byEntity: new Map() }
  const name = id => (b.registry.items[id] ? b.registry.items[id].name : `id:${id}`)
  const read = item => {
    if (!item || !item.itemCount || item.itemCount === 0) return null
    const components = (item.components || [])
      .map(c => `${c.type}=${c.data === undefined ? 'present' : stable(c.data)}`)
      .sort()
    return `${name(item.itemId)} x${item.itemCount}${components.length ? ' [' + components.join(' ') + ']' : ''}`
  }
  b._client.on('entity_equipment', p => {
    state.packets++
    state.byEntity.set(p.entityId, (state.byEntity.get(p.entityId) || 0) + 1)
    // 1.21.1 carries a list. The single-slot shape belongs to older versions,
    // and a script that only read the list would silently record nothing on a
    // server that sent the other one — so both are read and the difference is
    // visible in the entry count.
    const list = p.equipments !== undefined ? p.equipments : [{ slot: p.slot, item: p.item }]
    if (!state.worn.has(p.entityId)) state.worn.set(p.entityId, new Array(WIRE_SLOTS.length).fill(null))
    const worn = state.worn.get(p.entityId)
    for (const e of list) {
      state.entries++
      if (e.slot >= 0 && e.slot < WIRE_SLOTS.length) worn[e.slot] = read(e.item)
    }
  })
  return state
}

function stable (value) {
  if (value === undefined) return 'absent'
  if (value === null || typeof value !== 'object') return JSON.stringify(value)
  if (Array.isArray(value)) return `[${value.map(stable).join(',')}]`
  if (Buffer.isBuffer(value)) return value.toString('hex')
  const keys = Object.keys(value).sort()
  return `{${keys.map(k => `${k}:${stable(value[k])}`).join(',')}}`
}

function creativeSlot (b, slot, itemName, count = 1, components = []) {
  const item = itemName
    ? {
        itemCount: count,
        itemId: b.registry.itemsByName[itemName].id,
        addedComponentCount: components.length,
        removedComponentCount: 0,
        components,
        removeComponents: []
      }
    : { itemCount: 0 }
  b._client.write('set_creative_slot', { slot, item })
}

// One line per equipment slot that holds something, so a diff points at what
// is worn rather than at six dashes.
function picture (state, entityId) {
  const worn = state.worn.get(entityId) || new Array(WIRE_SLOTS.length).fill(null)
  const out = {}
  worn.forEach((item, i) => { if (item) out[WIRE_SLOTS[i]] = item })
  return out
}

// The steps, in order. Each one does something to the wearer and then says
// what the watcher was told.
//
// The main-inventory steps are not padding: they are where a server that
// broadcasts the whole set on every container change parts company with one
// that broadcasts the difference, and the only thing that can see that is the
// entry count.
function steps (b) {
  return [
    ['a helmet on the head', () => creativeSlot(b, HELMET, 'diamond_helmet')],
    ['a sword in the selected hotbar slot', () => creativeSlot(b, HOTBAR, 'diamond_sword')],
    ['a shield in the offhand', () => creativeSlot(b, OFFHAND, 'shield')],
    ['a stack in the middle of the inventory, which nobody can see', () => creativeSlot(b, MAIN, 'cobblestone', 64)],
    ['a second stack in the middle of the inventory', () => creativeSlot(b, MAIN + 1, 'dirt', 32)],
    ['the hand moves to an empty hotbar slot', () => b.setQuickBarSlot(1)],
    ['a sword put into that slot while it is the hand', () => creativeSlot(b, HOTBAR + 1, 'iron_sword')],
    ['the hand moves back to the diamond sword', () => b.setQuickBarSlot(0)],
    ['three pieces of armour in one step', () => {
      creativeSlot(b, CHESTPLATE, 'diamond_chestplate')
      creativeSlot(b, LEGGINGS, 'diamond_leggings')
      creativeSlot(b, BOOTS, 'diamond_boots')
    }],
    ['a named sword replaces the plain one', () => creativeSlot(b, HOTBAR, 'diamond_sword', 1, [
      { type: 'custom_name', data: nbtString('Bob') }
    ])],
    ['the helmet comes off', () => creativeSlot(b, HELMET, null)],
    ['the helmet goes back on', () => creativeSlot(b, HELMET, 'netherite_helmet')]
  ]
}

async function record (port, out) {
  const wearer = await spawned(port, 'Wearer')
  // The wearer watches too, and what it is watching for is itself: a server
  // that sent a player their own equipment would show up here as a non-zero
  // count against the wearer's own entity id. That number is only worth
  // anything beside a positive one, which is why the watcher dresses at the
  // end and the wearer has to hear about it.
  const mirror = wearer.seen
  await wait(SPAWN_SETTLE_MS)

  const watcher = await spawned(port, 'Watcher')
  const seen = watcher.seen
  await wait(SPAWN_SETTLE_MS)

  const wearerId = watcher.players.Wearer && watcher.players.Wearer.entity
    ? watcher.players.Wearer.entity.id
    : null
  if (wearerId === null) throw new Error('the watcher never got a body for the wearer')

  const snapshots = []
  for (const [name, act] of steps(wearer)) {
    seen.packets = 0
    seen.entries = 0
    act()
    await wait(STEP_SETTLE_MS)
    snapshots.push({
      step: name,
      worn: picture(seen, wearerId),
      packets: seen.packets,
      entries: seen.entries
    })
  }

  // The whole reason for a third bot. It joins into a world where the wearer
  // is already dressed and where nothing is going to change, and everything it
  // knows it was told on sight.
  const late = await spawned(port, 'Latecomer')
  const arrived = late.seen
  await wait(SPAWN_SETTLE_MS)
  const lateId = late.players.Wearer && late.players.Wearer.entity
    ? late.players.Wearer.entity.id
    : null
  snapshots.push({
    step: 'what a player who has just arrived can see, having watched nothing happen',
    worn: lateId === null ? { '(no body)': 'the latecomer never got a body for the wearer' } : picture(arrived, lateId),
    packets: arrived.packets,
    entries: arrived.entries
  })

  // The positive control for the mirror below: the watcher puts something on,
  // and the wearer has to hear about it. Without this, "the wearer heard
  // nothing about itself" is a sentence a disconnected socket also satisfies.
  const watcherId = wearer.players.Watcher && wearer.players.Watcher.entity
    ? wearer.players.Watcher.entity.id
    : null
  mirror.packets = 0
  mirror.entries = 0
  creativeSlot(watcher, HELMET, 'golden_helmet')
  await wait(STEP_SETTLE_MS)
  snapshots.push({
    step: 'the watcher puts on a helmet, which the wearer must be told about',
    worn: watcherId === null ? { '(no body)': 'the wearer never got a body for the watcher' } : picture(mirror, watcherId),
    packets: mirror.packets,
    entries: mirror.entries
  })
  snapshots.push({
    step: 'how much of its own equipment the wearer was sent, which must be none',
    worn: {},
    packets: mirror.byEntity.get(wearer.entity.id) || 0,
    entries: 0
  })

  require('fs').writeFileSync(out, JSON.stringify(snapshots, null, 2))
  console.log(`${snapshots.length} snapshots written to ${out}`)
  for (const s of snapshots) {
    const worn = Object.entries(s.worn).map(([k, v]) => `${k}=${v}`).join(' ') || '(nothing)'
    console.log(`  ${String(s.packets).padStart(2)}pkt ${String(s.entries).padStart(2)}ent  ${s.step}`)
    console.log(`                 ${worn}`)
  }
  process.exit(0)
}

// Steps where the two servers are *expected* to disagree, and in which field.
//
// Not a way to make red go away: an entry names the one field that may differ
// and every other field on that step still has to agree, and a difference on
// any step not named here fails the comparison.
//
// The one entry is the design difference decision record 0029 measured.
// Minecraft coalesces equipment per tick, so three creative writes inside one
// tick leave as one packet of three entries; Dust broadcasts per container
// change, so they leave as three packets of one entry each. The *entries* are
// the same number and the resulting picture is identical, which is why only
// `packets` is allowed to differ here. A player cannot produce that burst —
// each piece of armour is a separate click — so what this row measures is a
// creative menu clicked by a script.
const EXPECTED = new Map([
  ['three pieces of armour in one step', {
    field: 'packets',
    why: 'Minecraft coalesces equipment per tick; Dust broadcasts per container change'
  }]
])

// Counts, not a rate. A percentage would not say which step.
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
  const told = []
  for (let i = 0; i < a.length; i++) {
    const lines = []
    const keys = new Set([...Object.keys(a[i].worn), ...Object.keys(b[i].worn)])
    for (const k of [...keys].sort()) {
      const x = a[i].worn[k] || 'empty'
      const y = b[i].worn[k] || 'empty'
      if (x !== y) lines.push(`      ${k}: ${aPath} ${x} / ${bPath} ${y}`)
    }
    const allowed = EXPECTED.get(a[i].step)
    for (const field of ['packets', 'entries']) {
      if (a[i][field] === b[i][field]) continue
      const line = `      ${field}: ${aPath} ${a[i][field]} / ${bPath} ${b[i][field]}`
      if (allowed && allowed.field === field) told.push([a[i].step, allowed.why, line])
      else lines.push(line)
    }
    if (lines.length) {
      disagreed++
      console.log(`  DIFF  ${a[i].step}`)
      lines.forEach(l => console.log(l))
    }
  }
  for (const [step, why, line] of told) {
    named++
    console.log(`  told ${step} — ${why}`)
    console.log(line)
  }
  console.log(`\n${a.length - disagreed - named}/${a.length} snapshots agree`)
  console.log(`${named} differ for a named reason, ${disagreed} differ for none`)
  process.exit(disagreed === 0 ? 0 : 1)
}

const args = process.argv.slice(2)
if (args[0] === '--compare') {
  compare(args[1], args[2])
} else {
  const port = Number(args[0] || 25565)
  const outAt = args.indexOf('--out')
  record(port, outAt === -1 ? 'equipment.json' : args[outAt + 1]).catch(e => {
    console.log(`FAIL  ${e.message}`)
    console.log('\nIs a server running on this port, in creative, with online mode off?')
    process.exit(1)
  })
}
