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

  // Placing, by hand rather than through `bot.placeBlock`: that helper wants a
  // held item, and this server has no inventory to hold one in. The packet is
  // the same packet either way, written by a library that has never seen
  // Dust's encoder.
  //
  // On the block *beside* the one just dug, whose top face is still there.
  // Face 1 is up, so the block lands above it — the server puts it on the
  // face that was clicked and not in the cell that was.
  // Two blocks along, so the dig above did not touch it. Read now rather than
  // at spawn: `blockAt` answers null until the column has arrived, and half a
  // second after joining it has not.
  const beside = actor.blockAt(stood.offset(2, -1, 0))
  const placedAt = beside && beside.position.offset(0, 1, 0)
  if (beside) {
    actor._client.write('block_place', {
      hand: 0,
      location: { x: beside.position.x, y: beside.position.y, z: beside.position.z },
      direction: 1,
      cursorX: 0.5,
      cursorY: 1.0,
      cursorZ: 0.5,
      insideBlock: false,
      sequence: 1
    })
  }
  await wait(SETTLE_MS)

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
    Boolean(heardPlace),
    heardPlace
      ? `sound at ${heardPlace.x}/${heardPlace.y}/${heardPlace.z}, ` +
        `volume ${heardPlace.volume} pitch ${heardPlace.pitch}`
      : 'no sound_effect arrived — is there a dust-constants.tsv beside [data] path?'
  )
  // And it is the right sound, which is the half of this that the arithmetic
  // above cannot check: the id came out of Dust's own generated sound-event
  // table, and this resolves it through a table that has never seen it.
  //
  // There is no inventory, so the block that goes down is the world's own
  // surface block whatever the client is holding — grass, and grass sounds
  // like `block.grass.place`. minecraft-data numbers sound events from one,
  // which is the wire's own convention: the packet carries `id + 1` so that
  // zero can mark an inline sound, and prismarine subtracts it back off.
  const expectedSound = watcher.registry.soundsByName['block.grass.place']
  check(
    'and it is the sound that block makes',
    Boolean(heardPlace) && Boolean(expectedSound) &&
      heardPlace.sound.soundId + 1 === expectedSound.id,
    heardPlace && heardPlace.sound
      ? `sent ${heardPlace.sound.soundId}, block.grass.place is ` +
        `${expectedSound && expectedSound.id - 1}`
      : 'nothing to compare'
  )
  // The one field on that packet whose unit is not in its name: vanilla writes
  // eighths of a block, and a server that wrote the block coordinate would put
  // the sound an eighth of the way to the origin. Nothing on either side of the
  // wire can see that; a second implementation reading the number can.
  check(
    'and hears it where the block went',
    Boolean(heardPlace) && Boolean(placedAt) &&
      heardPlace.x === Math.trunc((placedAt.x + 0.5) * 8) &&
      heardPlace.y === Math.trunc((placedAt.y + 0.5) * 8) &&
      heardPlace.z === Math.trunc((placedAt.z + 0.5) * 8),
    placedAt && heardPlace
      ? `placed at ${placedAt.x}/${placedAt.y}/${placedAt.z}, ` +
        `sound at ${heardPlace.x / 8}/${heardPlace.y / 8}/${heardPlace.z / 8}`
      : 'nothing to compare'
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
