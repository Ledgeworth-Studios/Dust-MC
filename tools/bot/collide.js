// Whether a player who claims to be standing inside a block is put back.
//
// `movement.js` asks whether the server catches a position a player could not
// have *reached*. This asks the other question: a position a player could have
// reached at a walking pace and is not allowed to *be* in. The two are
// different rules and only one of them needs the world.
//
// mineflayer's physics is `prismarine-physics`, an independent reimplementation
// of Minecraft's player movement that shares no code with this project. A
// client that agreed with Dust by construction could not find anything.
//
// Every case here moves the same distance in the same number of packets and
// differs only in *where* it ends up, so a pass cannot be the speed limit
// wearing this check's name: the moves that go up are the control for the
// moves that go down.
//
// Usage: node collide.js [port]
// Exit 0 if every case behaved, 1 otherwise.

const mineflayer = require('mineflayer')

const PORT = Number(process.argv.slice(2).find(a => !a.startsWith('--')) || 25565)
const VERSION = '1.21.1'
const JOIN_TIMEOUT_MS = 30000

// How long to wait for a correction before deciding none is coming. A
// correction is written by the session that read the packet, so this is a
// round trip on loopback and not a tick budget; it is generous because the
// failure it guards against — calling a slow pass a fail — is the one that
// wastes an afternoon.
const CORRECTION_MS = 1500

const wait = ms => new Promise(r => setTimeout(r, ms))

function spawned (username) {
  return new Promise((resolve, reject) => {
    const b = mineflayer.createBot({
      host: '127.0.0.1', port: PORT, username, auth: 'offline', version: VERSION
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

const results = []
function say (ok, what, detail) {
  results.push(ok)
  console.log(`${ok ? 'ok  ' : 'FAIL'}  ${what}${detail ? ` — ${detail}` : ''}`)
}

async function main () {
  const bot = await spawned('Collider')

  let corrected = null
  bot._client.on('position', p => {
    corrected = p
    bot._client.write('teleport_confirm', { teleportId: p.teleportId })
  })

  await wait(2000)

  // Where the ground is, asked of the client rather than assumed: this file
  // has to work on a superflat and on a world read from region files, and the
  // one thing both have is a player standing on something.
  const feet = Math.round(bot.entity.position.y)
  const x = bot.entity.position.x
  const z = bot.entity.position.z
  console.log(`standing at ${x.toFixed(2)}, ${feet}, ${z.toFixed(2)}\n`)

  // The false-positive control comes first, and on purpose: if honest walking
  // is being corrected then every refusal below is meaningless, and the run
  // should say so before it says anything else.
  corrected = null
  bot.setControlState('forward', true)
  await wait(3000)
  bot.setControlState('forward', false)
  await wait(CORRECTION_MS)
  say(corrected === null, 'three seconds of ordinary walking is left alone',
    corrected ? 'the server corrected an honest walk' : '')

  // Physics off, so nothing argues with a position written by hand. From here
  // on the bot is the cheat client it is standing in for.
  bot.physicsEnabled = false
  await wait(500)
  const here = bot.entity.position.clone()

  // One packet, one block down, which is a step no faster than walking: the
  // speed rule has nothing to say about it and the only thing that can refuse
  // it is the block the feet would be inside.
  async function claimAt (what, x, y, z, onGround, expectCorrection) {
    corrected = null
    bot._client.write('position', { x, y, z, onGround })
    await wait(CORRECTION_MS)
    const back = corrected
    say(Boolean(back) === expectCorrection, what,
      back
        ? `put back to ${back.x.toFixed(1)}, ${back.y.toFixed(1)}, ${back.z.toFixed(1)}`
        : 'no correction arrived')
    // Whatever happened, put the bot back where this function assumes it is.
    if (!back) {
      bot._client.write('position', { x: here.x, y: here.y, z: here.z, onGround: true })
      await wait(300)
    }
    corrected = null
  }

  async function claim (what, dx, dy, dz, expectCorrection) {
    await claimAt(what, here.x + dx, here.y + dy, here.z + dz, dy <= 0, expectCorrection)
  }

  // Where the open air is, asked of the client's own copy of the world rather
  // than assumed. A superflat has nothing but air above the grass and a world
  // read from region files has a hillside in most directions, and a control
  // that walked into one would fail for being *right*. The first run of this
  // file did exactly that on a real world, which is the reason it probes.
  function clear (dx, dy, dz) {
    for (const up of [0, 1]) {
      const at = here.offset(dx, dy + up, dz).floored()
      const block = bot.blockAt(at)
      if (!block || block.boundingBox !== 'empty') return false
    }
    return true
  }
  function open (what, candidates) {
    const found = candidates.find(c => clear(c[0], c[1], c[2]))
    if (!found) console.log(`--    ${what} — nowhere open to move to; not checked`)
    return found
  }

  await claim('a step down into the ground is refused', 0, -1, 0, true)
  const step = open('the same step, upwards', [[0, 1, 0], [0, 2, 0], [0, 3, 0]])
  if (step) await claim('the same step upwards is allowed', ...step, false)
  await claim('a 5-block dash into the ground is refused', 5, -1, 0, true)
  const dash = open('the same dash, through open air',
    [[5, 1, 0], [-5, 1, 0], [0, 1, 5], [0, 1, -5], [0, 5, 0]])
  if (dash) await claim('the same dash through open air is allowed', ...dash, false)
  // Half a block down is still inside the block below, and half a block is
  // what a client sends when it steps off a slab. The check is about which
  // cell the feet are in and not about whole numbers, so this has to be
  // refused for the same reason the first case is.
  await claim('half a block down into the ground is refused', 0, -0.5, 0, true)

  // ---- The head, which is the half a 0.6-high box could not see ----
  //
  // Everything above is about a player's feet, and a check that only watched
  // feet let a client walk with its head inside a wall. These cases build the
  // one shape a superflat does not contain — solid with air under it — and ask
  // about a position whose foot cell is open and whose head cell is not.

  // A column two blocks away with something to stand on and three cells of air
  // over it. Probed rather than assumed, for the same reason the controls above
  // are: this file has to work on a superflat and on a hillside.
  function empty (dx, dy, dz) {
    const block = bot.blockAt(here.offset(dx, dy, dz).floored())
    return Boolean(block) && block.boundingBox === 'empty'
  }
  function footing (dx, dz) {
    const under = bot.blockAt(here.offset(dx, -1, dz).floored())
    if (!under || under.boundingBox === 'empty') return false
    if (!empty(dx, 0, dz) || !empty(dx, 1, dz) || !empty(dx, 2, dz)) return false
    // And the way there has to be open too, a block either side of the line,
    // because the claim below is one packet across several blocks and the
    // server samples the points between. On a superflat that is free; on a
    // hillside a straight line at a constant height walks into the hill, and
    // a case that was refused for *that* would be a pass this file had not
    // earned. Same lesson as the open-air control above.
    const lo = [Math.min(0, dx), Math.min(0, dz)]
    const hi = [Math.max(0, dx), Math.max(0, dz)]
    for (let sx = lo[0]; sx <= hi[0]; sx++) {
      for (let sz = lo[1]; sz <= hi[1]; sz++) {
        if (!empty(sx, 0, sz) || !empty(sx, 1, sz)) return false
      }
    }
    return true
  }
  // Every cell within three of the player, nearest first — nearest because a
  // claim one block across is one sample and cannot be refused by something
  // on the way, and three at the furthest because the block has to be
  // *placed* as well as claimed and a placement beyond the interaction range
  // is refused by the other check in this crate. On a superflat the first
  // candidate wins; on a hillside most of them are inside the hill, which is
  // why there are forty-eight of them.
  const ring = []
  for (let dx = -3; dx <= 3; dx++) {
    for (let dz = -3; dz <= 3; dz++) {
      if (dx !== 0 || dz !== 0) ring.push([dx, dz])
    }
  }
  ring.sort((a, b) => (a[0] * a[0] + a[1] * a[1]) - (b[0] * b[0] + b[1] * b[1]))
  const site = ring.find(c => footing(c[0], c[1]))

  if (!site) {
    console.log('--    the head cases — nowhere with a footing and headroom; not checked')
  } else {
    const air = here.offset(site[0], 0, site[1]).floored()
    const support = air.offset(0, -1, 0)
    const overhead = air.offset(0, 1, 0)

    // Creative, so the block comes from the client's own slot. `check.js` does
    // the same thing for the same reason: mineflayer will not place a block it
    // cannot see in a hand.
    bot._client.write('set_creative_slot', {
      slot: 36,
      item: {
        itemCount: 4,
        itemId: bot.registry.itemsByName.cobblestone.id,
        addedComponentCount: 0,
        removedComponentCount: 0,
        components: [],
        removeComponents: []
      }
    })
    bot._client.write('held_item_slot', { slotId: 0 })
    await wait(500)

    // A two-block pillar, then the bottom of it taken away: solid at head
    // height with open air underneath, which is the shape a wall makes for a
    // player standing in a doorway and which a superflat has nowhere.
    for (const on of [support, air]) {
      bot._client.write('block_place', {
        hand: 0,
        location: { x: on.x, y: on.y, z: on.z },
        direction: 1,
        cursorX: 0.5,
        cursorY: 1.0,
        cursorZ: 0.5,
        insideBlock: false,
        sequence: 1
      })
      await wait(400)
    }
    bot._client.write('block_dig', {
      status: 0,
      location: { x: air.x, y: air.y, z: air.z },
      face: 1,
      sequence: 2
    })
    await wait(600)

    const under = bot.blockAt(air)
    const above = bot.blockAt(overhead)
    const built = under && under.boundingBox === 'empty' && above && above.boundingBox !== 'empty'
    say(built, 'an overhang was built: air at foot height, solid at head height',
      built ? `${above.name} over ${under.name}` : 'the world did not end up that shape')

    if (built) {
      const x = air.x + 0.5
      const z = air.z + 0.5
      const y = air.y
      // The cheat. The cell the feet are in is open air and a check that
      // watched only the bottom 0.6 of a player accepted this.
      await claimAt('a claim with the feet in air and the head in a block is refused',
        x, y, z, true, true)
      // Same claim, one bit different: a client that says it is sprinting and
      // not on the ground may be swimming, and a swimmer is 0.6 tall. This is
      // the permission the server takes deliberately because it cannot see
      // water, and it is checked rather than left to be discovered.
      bot._client.write('entity_action', {
        entityId: bot.entity.id, actionId: 3, jumpBoost: 0
      })
      await wait(200)
      await claimAt('the same claim from a client that says it is swimming is allowed',
        x, y, z, false, false)
      bot._client.write('entity_action', {
        entityId: bot.entity.id, actionId: 4, jumpBoost: 0
      })
      await wait(200)

      // And the differential: take the block away and nothing about the claim
      // changes but the world, and it is allowed. Without this the case above
      // could be passing because of the distance or the on-ground flag.
      bot._client.write('block_dig', {
        status: 0,
        location: { x: overhead.x, y: overhead.y, z: overhead.z },
        face: 1,
        sequence: 3
      })
      await wait(600)
      const gone = bot.blockAt(overhead)
      if (gone && gone.boundingBox === 'empty') {
        await claimAt('the same claim with the head block gone is allowed',
          x, y, z, true, false)
      } else {
        console.log('--    the same claim with the head block gone — it did not break; not checked')
      }
    }
  }

  try { bot.quit() } catch (e) { /* already gone */ }
  const passed = results.filter(Boolean).length
  console.log(`\n${passed} of ${results.length} checks passed.`)
  process.exitCode = passed === results.length ? 0 : 1
}

main().catch(e => {
  console.log(`FAIL  ${e.message}`)
  console.log('\nIs a dust server running on this port, with online_mode = false')
  console.log('and [data] path set?')
  process.exit(1)
})
