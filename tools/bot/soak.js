// A bot that stays, moves and works for a while, and says whether anything
// went wrong. Phase 3's exit criterion asks for a ten-minute session; this is
// that session, with a knob for how long.
//
// Usage: node soak.js [port] [minutes]     (defaults: 25565, 10)
// Exits 0 if it survived and kept receiving, 1 with a named failure otherwise.
//
// What a soak can find that `check.js` cannot is anything that only goes wrong
// after a while: a keep-alive that stops being answered, a streaming set that
// leaks, a chunk boundary crossed a thousand times, a broadcast channel that
// falls behind. So the failures it watches for are *ending* and *stopping*,
// not a wrong value in one packet.

const mineflayer = require('mineflayer')

const PORT = Number(process.argv[2] || 25565)
const MINUTES = Number(process.argv[3] || 10)
const VERSION = '1.21.1'

// How far the bot wanders. Far enough to cross chunk boundaries constantly,
// which is what exercises the streaming: a bot standing still soaks nothing
// but the keep-alive.
const LEG_BLOCKS = 40

const started = Date.now()
const deadline = started + MINUTES * 60_000
let ended = null
const counts = { chunks: 0, forgets: 0, keepAlives: 0, packets: 0 }
let lastPacketAt = Date.now()

function fail (why) {
  console.log(`FAIL  ${why}`)
  console.log(summary())
  process.exit(1)
}

function summary () {
  const secs = Math.round((Date.now() - started) / 1000)
  return `ran ${secs}s: ${counts.packets} packets, ${counts.chunks} columns sent, ` +
    `${counts.forgets} forgotten, ${counts.keepAlives} keep-alives answered`
}

const bot = mineflayer.createBot({
  host: '127.0.0.1', port: PORT, username: 'Soak', auth: 'offline', version: VERSION
})

bot.on('error', e => fail(`the connection errored: ${e.message}`))
bot.on('kicked', r => fail(`kicked: ${JSON.stringify(r)}`))
bot.on('end', r => { ended = r || 'end' })

bot._client.on('packet', (d, meta) => {
  if (meta.state !== 'play') return
  counts.packets++
  lastPacketAt = Date.now()
  if (meta.name === 'map_chunk') counts.chunks++
  if (meta.name === 'unload_chunk') counts.forgets++
  if (meta.name === 'keep_alive') counts.keepAlives++
})

// Cancelled on spawn. `unref` stops a timer holding the process open; it does
// not stop it firing, which is how a soak that had already joined reported that
// it never had.
const joinTimer = setTimeout(() => fail('never reached the world'), 30_000)

bot.once('spawn', async () => {
  clearTimeout(joinTimer)
  console.log(`joined at ${JSON.stringify(bot.entity.position)}; soaking for ${MINUTES} minute(s)`)
  const origin = bot.entity.position.clone()
  let leg = 0

  // A square, flown rather than walked. The server grants creative flight, and
  // flying is what makes this a *streaming* soak: a walking bot at an ocean
  // spawn drifts a few blocks a minute and crosses almost no chunk boundaries,
  // which soaks the keep-alive and nothing else. Measured — the walking version
  // moved ten blocks in a minute and had the server forget zero columns.
  bot.creative.startFlying()
  const corners = [
    [LEG_BLOCKS, 0],
    [LEG_BLOCKS, LEG_BLOCKS],
    [0, LEG_BLOCKS],
    [0, 0]
  ]

  while (Date.now() < deadline) {
    if (ended) fail(`the connection ended after ${Math.round((Date.now() - started) / 1000)}s: ${ended}`)

    const [dx, dz] = corners[leg % corners.length]
    leg++
    const to = origin.offset(dx, 8, dz)
    try {
      await bot.creative.flyTo(to)
    } catch (e) {
      // Flight refused or interrupted is not itself a failure — the checks
      // below are about the connection, not about arriving.
    }

    // Something that changes the world, so the edit path is soaked too.
    const under = bot.blockAt(bot.entity.position.offset(0, -1, 0))
    if (under && under.name !== 'air') {
      try { await bot.dig(under) } catch (e) { /* out of reach is not a failure */ }
    }
    bot.chat(`leg ${leg}, ${counts.chunks} columns so far`)

    // The liveness check that matters: a server that has stopped talking has
    // failed, whatever its socket thinks. Thirty seconds is twice the
    // keep-alive period with room for a slow machine.
    const quietFor = Date.now() - lastPacketAt
    if (quietFor > 30_000) fail(`nothing has arrived for ${Math.round(quietFor / 1000)}s`)
  }

  if (ended) fail(`the connection ended before the soak did: ${ended}`)
  if (counts.chunks === 0) fail('no columns were ever sent')
  if (counts.keepAlives === 0) fail('no keep-alive ever arrived, so nothing proved the link alive')

  // A soak that never made the server stream anything soaked the keep-alive
  // and nothing else, so this is a check and not a note.
  if (counts.forgets === 0) {
    fail('no column was ever forgotten, so the bot never left the square it joined in')
  }
  console.log(`ok    survived ${MINUTES} minute(s)`)
  console.log(`ok    ${summary()}`)
  console.log(`ok    flew ${leg} leg(s) of ${LEG_BLOCKS} blocks`)
  try { bot.quit() } catch (e) { /* already gone */ }
  process.exit(0)
})
