// Points mineflayer at a running Dust server and reports what it can see.
//
// mineflayer implements the client protocol independently and shares no code
// with this project, which is the whole value: a check written against Dust's
// own framing would agree with Dust under any convention including a wrong
// one. See README.md for what it has found.
//
// Usage: node check.js [port]        (default 25565)
// Exits 0 if every check passed, 1 with a named failure otherwise.

const mineflayer = require('mineflayer')

const PORT = Number(process.argv[2] || 25565)
const VERSION = '1.21.1'
const JOIN_TIMEOUT_MS = 30000
const SETTLE_MS = 3000

const results = []
function check (name, ok, detail) {
  results.push({ name, ok, detail })
  console.log(`${ok ? 'ok  ' : 'FAIL'}  ${name}${detail ? ' — ' + detail : ''}`)
}

function bot (username) {
  return mineflayer.createBot({
    host: '127.0.0.1', port: PORT, username, auth: 'offline', version: VERSION
  })
}

// A bot that has reached the world, or a rejection naming why it did not.
// Rejecting rather than throwing keeps a refused connection — which is what a
// server with no `[data] path` does, on purpose — a reported failure instead
// of an unhandled error.
function spawned (username) {
  return new Promise((resolve, reject) => {
    const b = bot(username)
    const timer = setTimeout(
      () => reject(new Error(`${username} never reached the world in ${JOIN_TIMEOUT_MS / 1000}s`)),
      JOIN_TIMEOUT_MS
    )
    b.on('error', e => { clearTimeout(timer); reject(new Error(`${username}: ${e.message}`)) })
    b.on('kicked', r => { clearTimeout(timer); reject(new Error(`${username} was kicked: ${JSON.stringify(r)}`)) })
    b.once('spawn', () => { clearTimeout(timer); resolve(b) })
  })
}

const wait = ms => new Promise(r => setTimeout(r, ms))

async function main () {
  const watcher = await spawned('Watcher')
  check('a third-party client joins', true)

  // The dimension came from the registry contents the server sent. A client
  // that had fallen back on defaults of its own would still have *a* height;
  // what says these arrived is that they are the overworld's and not a guess.
  const { minY, height, dimension } = watcher.game
  check(
    'it was told the dimension it is in',
    dimension === 'overworld' && minY === -64 && height === 384,
    `dimension=${dimension} minY=${minY} height=${height}`
  )

  const biomes = Object.keys(watcher.registry.biomes || {}).length
  check('it has the biome registry', biomes === 64, `${biomes} biomes`)

  // Reading a block means the chunk packet decoded and the palette resolved.
  const under = watcher.blockAt(watcher.entity.position.offset(0, -1, 0))
  check('it can read the block under its feet', Boolean(under && under.name), under && under.name)

  // Three things another player does that a server can drop without anything
  // else noticing: two that change no block and no position at all, and one
  // that changes a block but whose *effect* is a separate packet.
  let sawSwing = false
  let sawCrouch = false
  let sawBreakEffect = null
  watcher._client.on('animation', () => { sawSwing = true })
  watcher._client.on('entity_metadata', p => {
    const flags = (p.metadata || []).find(m => m.key === 0)
    const pose = (p.metadata || []).find(m => m.key === 6)
    if (flags && pose && (flags.value & 0x02) && pose.value === 5) sawCrouch = true
  })
  watcher._client.on('world_event', p => {
    if (p.effectId === 2001) sawBreakEffect = p
  })
  // A placement has no particles and no level event: the sound is the only
  // packet, and it has to name the sound itself. Kept as the raw packet
  // because the position on it is the thing being checked, and mineflayer's
  // own view of a sound would have applied its own idea of the units.
  let heardPlace = null
  watcher._client.on('sound_effect', p => { heardPlace = p })

  // Chat, which nothing else here covers and which is a whole packet path:
  // the message goes up as `chat`, is rendered server-side with the sender's
  // name kept apart from their words, and comes back down to everybody.
  let heard = null
  watcher.on('messagestr', message => {
    if (message.includes('soup')) heard = message
  })

  const actor = await spawned('Actor')
  await wait(500)
  // Where it stands *before* it digs: it is about to break the block under its
  // own feet and fall, and a neighbour taken from the position afterwards is
  // taken from somewhere else.
  const stood = actor.entity.position.clone()
  actor.swingArm('right')
  await wait(500)
  actor.setControlState('sneak', true)
  await wait(500)

  // Breaking the block underfoot. `dig` waits for the server to confirm; the
  // server answers a start-digging as a finished break, which is what a
  // creative client sends and what this server honours.
  const target = actor.blockAt(actor.entity.position.offset(0, -1, 0))
  if (target) {
    try { await actor.dig(target) } catch (e) { /* the effect is the check */ }
  }
  await wait(SETTLE_MS)

  // Placing. Everything below is written by hand rather than through
  // `bot.placeBlock`, which wants a held item mineflayer can only get from an
  // inventory this server does not keep. The packets are the same packets, from
  // a library that has never seen Dust's encoder.
  //
  // Two items, chosen for what each one can fail at. Cobblestone is the
  // ordinary case: a player holds a thing and that thing goes down. Wheat
  // seeds are the case a server that matched item names to block names gets
  // wrong — they place `minecraft:wheat`, and `minecraft:wheat` the *item* is
  // what bread is made of and places nothing at all.
  const held = [
    { item: 'cobblestone', block: 'cobblestone', sound: 'block.stone.place', at: 2 },
    { item: 'wheat_seeds', block: 'wheat', sound: 'item.crop.plant', at: 4 }
  ]
  for (const [slot, what] of held.entries()) {
    const id = actor.registry.itemsByName[what.item]
    // 36 is hotbar slot 0. The offset is vanilla's own container numbering.
    actor._client.write('set_creative_slot', {
      slot: 36 + slot,
      item: {
        itemCount: 1,
        itemId: id.id,
        addedComponentCount: 0,
        removedComponentCount: 0,
        components: [],
        removeComponents: []
      }
    })
  }
  await wait(500)

  // Each on its own block, two apart, so neither placement is on top of the
  // other and neither is the block that was dug.
  //
  // Each cell is broken before it is placed into. Without that the check
  // depends on what the last run left there — this server keeps its edits
  // across a restart — and a placement into a cell that already holds that
  // block is correctly silent, so a stale world would fail the sound check for
  // the right reason at the wrong time.
  let sequence = 1
  for (const [slot, what] of held.entries()) {
    const on = actor.blockAt(stood.offset(what.at, -1, 0))
    if (!on) continue
    what.on = on.position
    what.placedAt = on.position.offset(0, 1, 0)
    actor._client.write('held_item_slot', { slotId: slot })
    actor._client.write('block_dig', {
      status: 0,
      location: { x: what.placedAt.x, y: what.placedAt.y, z: what.placedAt.z },
      face: 1,
      sequence: sequence++
    })
    await wait(500)
    heardPlace = null
    // Face 1 is up, so the block lands above the one clicked — the server puts
    // it on the face that was clicked and not in the cell that was.
    actor._client.write('block_place', {
      hand: 0,
      location: { x: on.position.x, y: on.position.y, z: on.position.z },
      direction: 1,
      cursorX: 0.5,
      cursorY: 1.0,
      cursorZ: 0.5,
      insideBlock: false,
      sequence: sequence++
    })
    await wait(SETTLE_MS)
    what.heard = heardPlace
  }
  const cobble = held[0]
  const seeds = held[1]

  actor.chat('there is soup')
  await wait(1000)

  check('one player hears another talk', Boolean(heard), heard || 'nothing arrived')
  // The sender's name is the server's to add, not the client's to send — a
  // server that relayed the raw line would let anybody speak as anybody.
  check(
    'and is told who said it',
    Boolean(heard) && heard.includes('Actor'),
    heard || ''
  )

  check('one player sees another swing', sawSwing)
  check('one player sees another crouch', sawCrouch, 'entity flag and pose together')
  // The data is the *broken* block's state, not the air left behind — the
  // client makes the particles and the sound out of it, and the air's id gives
  // a silent puff of nothing.
  check(
    'one player sees another break a block',
    Boolean(sawBreakEffect) && sawBreakEffect.data > 0,
    sawBreakEffect ? `effect 2001, state ${sawBreakEffect.data}` : 'no world_event arrived'
  )
  check(
    'one player hears another place a block',
    Boolean(cobble.heard),
    cobble.heard
      ? `sound at ${cobble.heard.x}/${cobble.heard.y}/${cobble.heard.z}, ` +
        `volume ${cobble.heard.volume} pitch ${cobble.heard.pitch}`
      : 'no sound_effect arrived — is there a dust-constants.tsv beside [data] path?'
  )
  // And it is the right sound, which is the half of this that the arithmetic
  // below cannot check: the id came out of Dust's own generated sound-event
  // table, and this resolves it through a table that has never seen it.
  //
  // minecraft-data numbers sound events from one, which is the wire's own
  // convention: the packet carries `id + 1` so that zero can mark an inline
  // sound, and prismarine subtracts it back off.
  const named = sound => watcher.registry.soundsByName[sound]
  const rang = (what) =>
    Boolean(what.heard) && Boolean(named(what.sound)) &&
    what.heard.sound.soundId + 1 === named(what.sound).id
  check(
    'and it is the sound that block makes',
    rang(cobble),
    cobble.heard && cobble.heard.sound
      ? `sent ${cobble.heard.sound.soundId}, ${cobble.sound} is ` +
        `${named(cobble.sound) && named(cobble.sound).id - 1}`
      : 'nothing to compare'
  )
  // The one field on that packet whose unit is not in its name: vanilla writes
  // eighths of a block, and a server that wrote the block coordinate would put
  // the sound an eighth of the way to the origin. Nothing on either side of the
  // wire can see that; a second implementation reading the number can.
  check(
    'and hears it where the block went',
    Boolean(cobble.heard) && Boolean(cobble.placedAt) &&
      cobble.heard.x === Math.trunc((cobble.placedAt.x + 0.5) * 8) &&
      cobble.heard.y === Math.trunc((cobble.placedAt.y + 0.5) * 8) &&
      cobble.heard.z === Math.trunc((cobble.placedAt.z + 0.5) * 8),
    cobble.placedAt && cobble.heard
      ? `placed at ${cobble.placedAt.x}/${cobble.placedAt.y}/${cobble.placedAt.z}, ` +
        `sound at ${cobble.heard.x / 8}/${cobble.heard.y / 8}/${cobble.heard.z / 8}`
      : 'nothing to compare'
  )

  // What actually went into the world, read by the *other* player: the block
  // update reached them and it is the block the actor was holding rather than
  // the one block a server with no item table can place.
  const landed = what => what.placedAt && watcher.blockAt(what.placedAt)
  check(
    'a player places what they are holding',
    Boolean(landed(cobble)) && landed(cobble).name === cobble.block,
    landed(cobble) ? landed(cobble).name : 'the watcher has no block there'
  )
  // The row that says this is a table and not a rule. `minecraft:wheat_seeds`
  // places `minecraft:wheat`, and a server that matched item names to block
  // names would put down `minecraft:wheat_seeds`, which is not a block — or,
  // reading the other way, would let `minecraft:wheat` the item place a crop
  // it does not place. Sixteen items on 1.21.1 are like this and every one of
  // them is a thing a player finds before a test does.
  check(
    'and an item whose block has another name still places the right block',
    Boolean(landed(seeds)) && landed(seeds).name === seeds.block,
    landed(seeds)
      ? `wheat_seeds placed ${landed(seeds).name}`
      : 'the watcher has no block there'
  )
  check(
    'and that block makes its own sound',
    rang(seeds),
    seeds.heard && seeds.heard.sound
      ? `sent ${seeds.heard.sound.soundId}, ${seeds.sound} is ` +
        `${named(seeds.sound) && named(seeds.sound).id - 1}`
      : 'no sound_effect arrived'
  )

  try { actor.quit() } catch (e) { /* already gone */ }
  try { watcher.quit() } catch (e) { /* already gone */ }
}

main().then(
  () => {
    const failed = results.filter(r => !r.ok)
    console.log(`\n${results.length - failed.length}/${results.length} checks passed`)
    process.exit(failed.length === 0 ? 0 : 1)
  },
  e => {
    console.log(`FAIL  ${e.message}`)
    console.log('\nIs a dust server running on this port, with online_mode = false')
    console.log('and [data] path set? A client that acknowledges no data packs')
    console.log('cannot be served without it, and is refused at configuration.')
    process.exit(1)
  }
)
