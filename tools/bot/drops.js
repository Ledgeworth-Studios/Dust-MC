// Breaks blocks on a running server and writes down what came out.
//
// The fourth survey in this directory and the first about *entities*. It reads
// the item entities off the raw packet stream — `spawn_entity` for the entity
// and `entity_metadata` for which item it is holding — because that is what a
// client actually receives, and because `bot.entities` is prismarine's reading
// of the same bytes plus its own opinions.
//
// Usage, against Dust (creative, the bot puts its own block down):
//   node drops.js 25601 stone,dirt,oak_leaves
//
// Usage, as a gate rather than a survey — what a player feels, checked:
//   node drops.js 25601 --check
//
// Usage, against a real vanilla server, which is the measurement:
//   mkfifo /tmp/mc-console
//   ( tail -f /tmp/mc-console | java -jar server.jar nogui > /tmp/mc.log 2>&1 & )
//   DUST_SERVER_CONSOLE=/tmp/mc-console node drops.js 25701 blocks.txt --survival
//
// `--tool` takes a comma-separated list and every block is broken with every
// one of them, because what a block drops is a question about the pair and not
// about the block. `-` in that list is a **bare hand**, which is the row the
// whole tool requirement is about: `stone` bare-handed yields nothing, and a
// survey that only ever held a netherite pickaxe cannot see it.
//
//   DUST_SERVER_CONSOLE=/tmp/mc-console node drops.js 25701 blocks.txt \
//     --survival --tool -,wooden_pickaxe,iron_pickaxe,shears
//
// Output is TSV on stdout and belongs in the operator's own scratch directory,
// never in the repository: what a block drops is Minecraft's data.
//
// # A break that yielded nothing and a break that never happened
//
// These look identical from outside — both leave air where a block was, and
// most of the interesting blocks (leaves, grass) legitimately yield nothing.
// Every earlier survey in this directory was bitten by the same shape from the
// other end, and the fix is the same: the target cell is **read before and
// after**, and a run only says NOTHING when the block was there first and is
// air afterwards. A cell that never held the block is NO SUCH BLOCK; one that
// still holds it is NOT BROKEN. Three outcomes, not one.

const mineflayer = require('mineflayer')
const fs = require('fs')

const VERSION = '1.21.1'
const JOIN_TIMEOUT_MS = 30000
const SETTLE_MS = 900
const DIG_TIMEOUT_MS = 45000

/// Where a survival arena is built. High enough to be above any terrain the
/// world generated and below the build limit by a margin.
const ARENA_Y = 200

// One state, one drop, one count: if breaking this does not yield exactly one
// cobblestone the run is not measuring what it thinks it is and stops before
// printing a single answer. Every survey here has one and every one of them
// has caught something.
const CONTROL = 'stone'
const CONTROL_YIELDS = 'minecraft:cobblestone*1'

const wait = ms => new Promise(r => setTimeout(r, ms))

function usage (why) {
  console.error(why)
  console.error('usage: [DUST_SERVER_CONSOLE=<fifo>] node drops.js <port> <blocks|file> [--survival] [--tool <item>]')
  process.exit(2)
}

const argv = process.argv.slice(2)
const port = Number(argv[0])
if (!port) usage('a port is the first argument')
let survival = false
let gate = false
let times = false
let tools = ['netherite_pickaxe']
const rest = []
for (let i = 1; i < argv.length; i++) {
  if (argv[i] === '--survival') survival = true
  else if (argv[i] === '--times') times = true
  else if (argv[i] === '--check') gate = true
  else if (argv[i] === '--tool') tools = argv[++i].split(',').filter(Boolean)
  else rest.push(argv[i])
}
// A bare hand is spelled `-` on the command line and in the output, because an
// empty field in a TSV is invisible and a row whose tool column vanished reads
// as one that was never written.
const BARE = '-' 
const console_path = process.env.DUST_SERVER_CONSOLE
if (survival && !console_path) {
  usage('--survival builds its arena from the server console, so DUST_SERVER_CONSOLE must be set')
}

let blocks = []
if (rest.length === 0 && !gate) usage('name the blocks to break, or a file of them')
if (rest.length === 0) {
  blocks = []
} else if (fs.existsSync(rest[0])) {
  blocks = fs.readFileSync(rest[0], 'utf8').split(/\s+/).filter(Boolean)
} else {
  blocks = rest.join(',').split(',').filter(Boolean)
}
blocks = blocks.map(name => (name.includes(':') ? name : 'minecraft:' + name))
if (!gate && !blocks.includes('minecraft:' + CONTROL)) blocks.unshift('minecraft:' + CONTROL)

function say (command) {
  fs.appendFileSync(console_path, command + '\n')
}

function spawned (username) {
  return new Promise((resolve, reject) => {
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

// Every item entity the server has told us about, by entity id. Two packets
// make one answer: the spawn says an item entity exists and the metadata says
// which item, and a server that sent only the first has dropped something the
// player cannot see.
function watchItems (b) {
  const itemType = b.registry.entitiesByName.item.id
  const seen = new Map()
  b._client.on('spawn_entity', p => {
    if (p.type !== itemType) return
    seen.set(p.entityId, { id: p.entityId, item: null, count: 0, x: p.x, y: p.y, z: p.z })
  })
  b._client.on('entity_metadata', p => {
    const entry = seen.get(p.entityId)
    if (!entry) return
    for (const field of p.metadata || []) {
      // Index 8 on an item entity is the stack it is holding.
      if (field.key !== 8) continue
      const slot = field.value
      if (!slot || slot.itemId === undefined) continue
      entry.item = b.registry.items[slot.itemId]
        ? 'minecraft:' + b.registry.items[slot.itemId].name
        : 'item#' + slot.itemId
      entry.count = slot.itemCount
    }
  })
  const collected = []
  b._client.on('collect', p => collected.push(p))
  // How many times the server corrected an item's position. The claim this
  // counts is the one in `net/items.rs`: an item costs a spawn, a settle and
  // nothing else, because the client runs the same arc the server does. A
  // server streaming positions would put twenty of these a second here.
  let teleports = 0
  const destroyed = []
  // By packet id rather than by name: prismarine's name for a packet is its
  // own reading of the protocol, and this counts what the *server* sent about
  // an entity it knows is an item. Any packet carrying a known item entity's
  // id and a position is a correction, whatever it is called.
  b._client.on('packet', (data, meta) => {
    if (data && seen.has(data.entityId) && data.x !== undefined) teleports += 1
    if (process.env.DUST_BOT_PACKETS) tally[meta.name] = (tally[meta.name] || 0) + 1
  })
  const tally = {}
  b._client.on('entity_destroy', p => {
    for (const id of p.entityIds || []) destroyed.push(id)
  })
  return {
    teleports: () => teleports,
    tally: () => tally,
    destroyed,
    all: () => [...seen.values()],
    since (mark) {
      return [...seen.values()].filter(entry => entry.id > mark)
    },
    mark () {
      let top = 0
      for (const id of seen.keys()) top = Math.max(top, id)
      return top
    },
    collected
  }
}

function creativeSlot (b, slot, name) {
  const known = b.registry.itemsByName[name.replace('minecraft:', '')]
  if (!known) return false
  b._client.write('set_creative_slot', {
    slot,
    item: {
      itemCount: 1,
      itemId: known.id,
      addedComponentCount: 0,
      removedComponentCount: 0,
      components: [],
      removeComponents: []
    }
  })
  return true
}

const results = []
function check (name, ok, detail) {
  results.push({ name, ok })
  console.log(`${ok ? 'ok  ' : 'FAIL'}  ${name}${detail ? ' — ' + detail : ''}`)
}

// Walk the player onto a point, one small step at a time.
//
// By hand rather than through mineflayer's pathfinder, for the reason every
// other script here writes its packets by hand: the movement this is exercising
// is the server's, and a library that decided to teleport instead would be
// testing itself. Small steps because the server has a speed limit and a
// player who claims to have crossed the room in one packet is put back.
async function walkTo (b, x, y, z) {
  const steps = 24
  const from = b.entity.position.clone()
  for (let i = 1; i <= steps; i++) {
    const t = i / steps
    b._client.write('position', {
      x: from.x + (x - from.x) * t,
      y: from.y + (y - from.y) * t,
      z: from.z + (z - from.z) * t,
      onGround: true
    })
    await wait(50)
  }
}

/// How long the **server** takes to break a block that is already there.
///
/// Not `bot.dig`, and the difference is the whole measurement: mineflayer
/// computes its own break time from its own copy of Minecraft's numbers, waits
/// that long and then says it is done, so timing it measures prismarine. This
/// sends `START_DESTROY_BLOCK` once, never sends a stop, and waits for the cell
/// to become air — because a vanilla server that is never told to stop keeps
/// counting on its own and destroys the block when *its* progress reaches one.
/// What comes back is the server's own answer in milliseconds.
///
/// A poll rather than a packet listener, at half a tick, because the answer
/// wanted is "which tick did it go" and any read finer than that is measuring
/// the poll.
async function timeBreak (b, at) {
  const started = Date.now()
  b._client.write('block_dig', {
    status: 0,
    location: { x: at.x, y: at.y, z: at.z },
    face: 1,
    sequence: 1
  })
  while (Date.now() - started < TIME_LIMIT_MS) {
    const now = b.blockAt(at)
    if (now && now.name === 'air') return Date.now() - started
    await wait(25)
  }
  return null
}

/// How long a timing run waits before calling a break impossible.
const TIME_LIMIT_MS = 120000

/// What a player feels, asked of a running server.
async function gateRun () {
  const b = await spawned('Digger')
  const items = watchItems(b)
  await wait(SETTLE_MS)
  const stood = b.entity.position.floored()
  const at = stood.offset(4, 0, 0)

  async function put (block) {
    const target = b.blockAt(at)
    if (target && target.name !== 'air') {
      try { await b.dig(target) } catch (e) {}
      await wait(200)
    }
    creativeSlot(b, 36, block)
    b._client.write('held_item_slot', { slotId: 0 })
    await wait(150)
    const under = b.blockAt(at.offset(0, -1, 0))
    b._client.write('block_place', {
      hand: 0,
      location: { x: under.position.x, y: under.position.y, z: under.position.z },
      direction: 1,
      cursorX: 0.5,
      cursorY: 1.0,
      cursorZ: 0.5,
      insideBlock: false,
      sequence: 1
    })
    await wait(400)
    return b.blockAt(at)
  }

  // What is in the hand when the block is broken. Hotbar slot 7 holds a tool
  // and slot 8 is left empty, so neither of them is the slot `put` fills with
  // the block being placed — a gate that reused one slot for both would be
  // holding a stone while it tried to prove what a pickaxe does.
  async function hand (tool) {
    if (tool) creativeSlot(b, 43, tool)
    b._client.write('held_item_slot', { slotId: tool ? 7 : 8 })
    await wait(200)
  }

  // The cell is four blocks away, which is inside the reach a player breaks
  // at and outside the reach they pick up at. That gap is what makes the
  // pickup check have a negative half: a drop that is collected before
  // anybody walks to it would pass a check that only ever looked afterwards.
  //
  // 1. A block that is broken drops.
  let placed = await put('minecraft:stone')
  await hand('minecraft:netherite_pickaxe')
  check('a stone can be put down to break', placed && placed.name === 'stone',
    placed ? placed.name : 'nothing there')
  let mark = items.mark()
  const before = items.collected.length
  await b.dig(b.blockAt(at))
  await wait(SETTLE_MS)
  let fresh = items.since(mark).filter(e => e.item)
  check('breaking stone drops one cobblestone',
    fresh.length === 1 && fresh[0].item === 'minecraft:cobblestone' && fresh[0].count === 1,
    fresh.map(e => `${e.item}*${e.count}`).join(',') || 'nothing')
  const drop = fresh[0]

  // 2. It came out of the block, not out of the player.
  const centre = drop
    ? Math.hypot(drop.x - (at.x + 0.5), drop.z - (at.z + 0.5))
    : 99
  check('the item is at the centre of the block that broke',
    centre < 0.5,
    drop ? `${centre.toFixed(2)} blocks from the centre, and ` +
      `${Math.hypot(drop.x - b.entity.position.x, drop.z - b.entity.position.z).toFixed(2)} from the player`
      : 'no item')

  // 3. It is *not* taken from where the player is standing. The negative half.
  check('an item out of reach is left where it fell',
    items.collected.length === before,
    `${items.collected.length - before} collected without anybody walking to it`)

  // 4. Walking over it collects it, with no key pressed.
  if (drop) await walkTo(b, drop.x, at.y, drop.z)
  await wait(SETTLE_MS)
  const taken = items.collected.slice(before)
  check('walking over it picks it up',
    taken.length === 1 && taken[0].collectedEntityId === (drop && drop.id),
    taken.length ? `entity ${taken[0].collectedEntityId} to ${taken[0].collectorEntityId}` : 'nothing collected')
  check('and it is in the inventory afterwards',
    b.inventory.items().some(i => i.name === 'cobblestone'),
    b.inventory.items().map(i => `${i.name}*${i.count}`).join(',') || 'empty')

  // 5. Two of the same item lying together become one, back out of reach.
  await walkTo(b, stood.x + 0.5, stood.y, stood.z + 0.5)
  await wait(SETTLE_MS)
  mark = items.mark()
  for (let i = 0; i < 2; i++) {
    const again = await put('minecraft:stone')
    // `put` selects the slot the block came out of, so the tool has to go
    // back before the break — a stone broken while holding a stone is a
    // break with the wrong tool, and now that the wrong tool means something
    // it yields nothing and this check measures a rule it is not about.
    await hand('minecraft:netherite_pickaxe')
    if (again && again.name === 'stone') await b.dig(b.blockAt(at))
    await wait(600)
  }
  await wait(2500)
  fresh = items.since(mark).filter(e => e.item)
  const gone = fresh.filter(e => items.destroyed.includes(e.id))
  check('two of the same item lying together become one',
    fresh.length === 2 && gone.length === 1,
    `${fresh.length} spawned, ${gone.length} removed`)

  // 6. The wire cost of an item, which is the claim `net/items.rs` makes.
  // A number that can only be too high: every item that landed sent exactly
  // one correction, so the count is at least one and at most one per item. A
  // check written as "at most" alone would pass on a server that sent none,
  // and a server that sent none is one whose items are in the wrong place.
  const corrections = items.teleports()
  check('an item costs a spawn and one correction, not a position stream',
    corrections >= 1 && corrections <= items.all().length,
    `${items.all().length} item(s), ${corrections} correction(s)`)
  if (process.env.DUST_BOT_PACKETS) console.error(JSON.stringify(items.tally()))

  // 7. The same block with an empty hand, which is the rule a player feels
  // first in a survival world: stone breaks and yields nothing. **Both halves
  // are asserted** — a server that refused the break would also produce no
  // item, and the two are entirely different things to stand in front of.
  const bare = await put('minecraft:stone')
  await hand(null)
  mark = items.mark()
  await b.dig(b.blockAt(at))
  await wait(SETTLE_MS)
  const nothing = items.since(mark).filter(e => e.item)
  const after = b.blockAt(at)
  check('a bare hand breaks stone and gets nothing for it',
    Boolean(bare) && bare.name === 'stone' && after && after.name === 'air' &&
      nothing.length === 0,
    `${after ? after.name : 'unloaded'} afterwards, ` +
      (nothing.map(e => `${e.item}*${e.count}`).join(',') || 'nothing dropped'))
  await hand('minecraft:netherite_pickaxe')

  // 8. A wall block yields the block its loot table is named after. Sixty
  // blocks drop nothing without `dust-blocks.tsv`, and this is one of them:
  // `oak_wall_sign` draws from `blocks/oak_sign.json`, which no rule about
  // names arrives at. The sign is placed against the *side* of a stone, so
  // the server's own placement decides it is the wall form.
  const anchor = await put('minecraft:stone')
  const signAt = at.offset(-1, 0, 0)
  creativeSlot(b, 36, 'minecraft:oak_sign')
  b._client.write('held_item_slot', { slotId: 0 })
  await wait(200)
  b._client.write('block_place', {
    hand: 0,
    location: { x: at.x, y: at.y, z: at.z },
    direction: 4,
    cursorX: 0.0,
    cursorY: 0.5,
    cursorZ: 0.5,
    insideBlock: false,
    sequence: 2
  })
  await wait(500)
  const sign = b.blockAt(signAt)
  check('an oak sign put on the side of a block is a wall sign',
    Boolean(anchor) && anchor.name === 'stone' && sign && sign.name === 'oak_wall_sign',
    sign ? sign.name : 'nothing there')
  await hand('minecraft:netherite_pickaxe')
  mark = items.mark()
  if (sign && sign.name === 'oak_wall_sign') await b.dig(sign)
  await wait(SETTLE_MS)
  const fromWall = items.since(mark).filter(e => e.item)
  check('breaking it yields the sign its loot table is named after',
    fromWall.length === 1 && fromWall[0].item === 'minecraft:oak_sign',
    fromWall.map(e => `${e.item}*${e.count}`).join(',') || 'nothing')


  b.quit()
  const failed = results.filter(r => !r.ok).length
  console.log(`\n${results.length - failed}/${results.length} checks passed`)
  process.exit(failed ? 1 : 0)
}

async function main () {
  if (gate) return gateRun()
  const b = await spawned('Digger')
  const items = watchItems(b)
  await wait(SETTLE_MS)

  let stood = b.entity.position.floored()
  // Two blocks away and one down: far enough that the bot is not standing on
  // the cell it is breaking, near enough to be in reach at any yaw.
  let at = stood.offset(2, 0, 0)

  if (survival) {
    // **The arena is built at a stated height and the player is put on it**,
    // rather than built around wherever the bot landed. A world that has been
    // surveyed before has holes in it: the first run of this against a world
    // an earlier survey had dug through put the bot at y=-60, built its floor
    // inside deepslate, and hung on the first block it could not reach. Where
    // the arena is has to be a decision and not an observation.
    stood = stood.offset(0, 0, 0)
    stood.y = ARENA_Y
    at = stood.offset(4, 0, 0)
    // Survival, because **a creative player's break drops nothing** and a
    // survey run in creative would record an empty answer for all 982 blocks
    // and call it a measurement. Peaceful and haste because what is being
    // measured is what a break yields, not whether a bot can survive a
    // skeleton or swing fast enough; no random ticks because a crop that grew
    // between the setblock and the dig is a row about the wrong state.
    say('difficulty peaceful')
    say('gamerule randomTickSpeed 0')
    say('gamerule doTileDrops true')
    say(`gamemode survival Digger`)
    // Haste at the top of its range and not at 5. A bare hand on obsidian is
    // 5,000 ticks; at amplifier 5 that is still 113 seconds and every such row
    // times out and reads as NOT BROKEN, which is a tool failure wearing a
    // measurement's clothes. Haste changes how long a break takes and nothing
    // about what comes out of it, so this survey can have as much of it as the
    // game will give.
    // No haste at all in a timing run: haste is the largest single term in
    // Minecraft's own break-time formula and a run that has it is measuring
    // the effect rather than the block.
    if (!times) say(`effect give Digger minecraft:haste 99999 255 true`)
    say(`effect give Digger minecraft:saturation 99999 5 true`)
    // A floor to stand the blocks on, and air above it so nothing falls in.
    say(`fill ${stood.x - 8} ${ARENA_Y} ${stood.z - 8} ${stood.x + 8} ${ARENA_Y + 4} ${stood.z + 8} minecraft:air`)
    say(`fill ${stood.x - 8} ${ARENA_Y - 1} ${stood.z - 8} ${stood.x + 8} ${ARENA_Y - 1} ${stood.z + 8} minecraft:stone`)
    await wait(SETTLE_MS)
    say(`tp Digger ${stood.x + 0.5} ${ARENA_Y} ${stood.z + 0.5}`)
    await wait(SETTLE_MS)
    // Believe the server's own answer about where the player ended up rather
    // than the number that was asked for: a teleport into a cell the server
    // will not accept is a survey aimed at somewhere else.
    stood = b.entity.position.floored()
    at = stood.offset(4, 0, 0)
  }

  // Put one tool in the player's hand, or nothing at all.
  //
  // The inventory is emptied first every time, because `/give` fills the next
  // free slot rather than slot zero: a second `/give` without a clear leaves
  // the first tool in hand and every row after it is about the wrong tool.
  async function hold (which) {
    if (!survival) return true
    say(`clear Digger`)
    await wait(250)
    if (which !== BARE) {
      say(`give Digger ${which}`)
      await wait(300)
    }
    b._client.write('held_item_slot', { slotId: 0 })
    await wait(250)
    if (which === BARE) return true
    const held = b.inventory.slots[36]
    return Boolean(held) && held.name === which.replace('minecraft:', '')
  }

  const rows = []
  const pairs = []
  for (const which of tools) {
    for (const block of blocks) pairs.push([block, which])
  }
  let holding = null
  for (const [block, tool] of pairs) {
    if (tool !== holding) {
      if (!(await hold(tool))) {
        rows.push([block, tool, 'NO SUCH ITEM', 'not in hand'])
        continue
      }
      holding = tool
    }
    const name = block.replace('minecraft:', '')
    // Clear the cell first, so what is measured is this run's block and not
    // whatever the last one left.
    if (survival) {
      // **Sweep the floor first.** An item entity that is still lying there
      // merges with the next row's, and the row after reads a count of two
      // for a block that drops one. Eight rows of 245 said `sand*2`,
      // `white_wool*2` and `oak_planks*2` before this line existed, and every
      // one of them was this survey measuring its own litter.
      say(`kill @e[type=minecraft:item]`)
      await wait(200)
      say(`setblock ${at.x} ${at.y} ${at.z} minecraft:air replace`)
      await wait(250)
      say(`setblock ${at.x} ${at.y} ${at.z} ${block} replace`)
      await wait(400)
    } else {
      const target = b.blockAt(at)
      if (target && target.name !== 'air') {
        try { await b.dig(target) } catch (e) { /* reported by the read below */ }
        await wait(250)
      }
      if (!creativeSlot(b, 36, block)) {
        rows.push([block, tool, 'NO SUCH ITEM', '-'])
        continue
      }
      b._client.write('held_item_slot', { slotId: 0 })
      await wait(200)
      const under = b.blockAt(at.offset(0, -1, 0))
      if (under) {
        b._client.write('block_place', {
          hand: 0,
          location: { x: under.position.x, y: under.position.y, z: under.position.z },
          direction: 1,
          cursorX: 0.5,
          cursorY: 1.0,
          cursorZ: 0.5,
          insideBlock: false,
          sequence: 1
        })
      }
      await wait(400)
    }

    const before = b.blockAt(at)
    if (!before || before.name === 'air' || (before.name !== name && 'minecraft:' + before.name !== block)) {
      // The block never got there. Not a fact about drops, and kept apart from
      // one: this is the row that stops a tool failure being read as a block
      // that yields nothing.
      rows.push([block, tool, 'NO SUCH BLOCK', before ? before.name : 'unloaded'])
      continue
    }

    if (times) {
      const took = await timeBreak(b, at)
      rows.push([block, tool, took === null ? 'TIMED OUT' : String(took),
        took === null ? '-' : String(Math.round(took / 50))])
      continue
    }
    const mark = items.mark()
    let dug = true
    try {
      await Promise.race([
        b.dig(before),
        wait(DIG_TIMEOUT_MS).then(() => { throw new Error('dig timed out') })
      ])
    } catch (e) {
      dug = false
    }
    await wait(SETTLE_MS)

    const after = b.blockAt(at)
    // A waterlogged block leaves **water** where it was, not air, and a run
    // that only accepted air called every dead coral wall fan NOT BROKEN.
    // Same shape as the `ice` row that reads back as `water` because it
    // melted: what says the block went is that it is no longer there, and
    // what it left behind is a second question.
    const wet = Boolean(before.getProperties && before.getProperties().waterlogged)
    const gone = after && (after.name === 'air' || (wet && after.name === 'water'))
    if (!gone) {
      rows.push([block, tool, dug ? 'NOT BROKEN' : 'REFUSED', after ? after.name : 'unloaded'])
      continue
    }
    const fresh = items.since(mark).filter(entry => entry.item)
    const spelled = fresh.length
      ? fresh.map(entry => `${entry.item}*${entry.count}`).sort().join(',')
      : '-'
    rows.push([block, tool, fresh.length ? 'BROKE' : 'NOTHING', spelled])
  }

  // Two controls and not one, and the second is the new half.
  //
  // A survey where every row says NOTHING and a survey where every row says
  // NOTHING *because the tool was wrong* are the same file. So the positive
  // control asks that a pickaxe on stone gives exactly one cobblestone, and
  // where the run held a bare hand at all, the negative control asks that the
  // same block with an empty hand gives nothing — the rule this survey exists
  // to measure, checked in the direction that a broken harness cannot fake.
  const control = rows.find(row => row[0] === 'minecraft:' + CONTROL && row[1] !== BARE)
  if (!control || control[2] !== 'BROKE' || control[3] !== CONTROL_YIELDS) {
    console.error(
      `control failed: breaking ${CONTROL} with ${control ? control[1] : 'a tool'} gave ` +
      `${control ? control[2] + ' ' + control[3] : 'no row at all'}, ` +
      `and it has to give ${CONTROL_YIELDS}. Nothing else this run saw is trustworthy.`
    )
    b.quit()
    process.exit(1)
  }
  if (survival && tools.includes(BARE)) {
    const bare = rows.find(row => row[0] === 'minecraft:' + CONTROL && row[1] === BARE)
    if (!bare || bare[2] !== 'NOTHING') {
      console.error(
        `negative control failed: breaking ${CONTROL} bare-handed gave ` +
        `${bare ? bare[2] + ' ' + bare[3] : 'no row at all'}, and it has to break ` +
        `and yield nothing. A run where the hand was never empty measures nothing ` +
        `about the tool requirement.`
      )
      b.quit()
      process.exit(1)
    }
  }

  if (times) {
    console.log('# block\ttool\tms\tticks')
    for (const row of rows) console.log(row.join('\t'))
    console.error(`${rows.length} timing(s)`)
    b.quit()
    process.exit(0)
  }
  console.log('# block\ttool\toutcome\tdrops')
  for (const row of rows) console.log(row.join('\t'))
  console.error(`${rows.length} block(s), ${items.collected.length} picked up`)
  b.quit()
  process.exit(0)
}

main().catch(e => {
  console.error(e.message)
  process.exit(1)
})
