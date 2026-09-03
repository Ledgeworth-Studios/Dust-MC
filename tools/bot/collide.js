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
  async function claim (what, dx, dy, dz, expectCorrection) {
    corrected = null
    bot._client.write('position', {
      x: here.x + dx, y: here.y + dy, z: here.z + dz, onGround: dy <= 0
    })
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
