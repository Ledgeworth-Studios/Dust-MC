// How much of the ground around spawn is open, read out of a real client.
//
// The worldgen harness scores Dust against the world Minecraft generated for
// the same seed, chunk by chunk, and every number it prints comes out of the
// generator. This asks the other question: what a third-party client, given
// nothing but the socket, finds under a player who has actually joined. It is
// the end-to-end half — a generator that carves caves and a server that does
// not send them would score perfectly and be unplayable.
//
// It prints one JSON line and nothing else, so two runs of two builds diff.
// Decision record 0039 is where its numbers are: eight cells of air below a
// player at seed 0's spawn became 1,738, of which 1,451 are below y 0.
//
//   node openness.js 25565
//
// mineflayer's `bot.blockAt` wants a real `Vec3`; a plain `{x, y, z}` throws
// inside prismarine-chunk, some distance from the call that did it.
const mineflayer = require('mineflayer')
const Vec3 = require('vec3').Vec3

const port = Number(process.argv[2] || 25565)
const RADIUS = 16
const LOW = -60
const HIGH = 60

const bot = mineflayer.createBot({
  host: '127.0.0.1',
  port,
  username: 'Prober',
  auth: 'offline',
  version: '1.21.1'
})

bot.once('spawn', () => setTimeout(() => {
  const origin = bot.entity.position.floored()
  const count = { air: 0, airBelowZero: 0, airBelowSea: 0, water: 0, lava: 0, solid: 0, unknown: 0 }
  for (let dx = -RADIUS; dx <= RADIUS; dx++) {
    for (let dz = -RADIUS; dz <= RADIUS; dz++) {
      for (let y = LOW; y < HIGH; y++) {
        const block = bot.blockAt(new Vec3(origin.x + dx, y, origin.z + dz))
        if (!block) { count.unknown++; continue }
        const name = block.name
        // A carver fills a cave with `cave_air`, not `air`, so a probe that
        // asked only about `air` would report that nothing had been carved.
        if (name === 'air' || name === 'cave_air' || name === 'void_air') {
          count.air++
          if (y < 0) count.airBelowZero++
          if (y < 63) count.airBelowSea++
        } else if (name === 'water') count.water++
        else if (name === 'lava') count.lava++
        else count.solid++
      }
    }
  }
  console.log(JSON.stringify({ at: [origin.x, origin.y, origin.z], ...count }))
  bot.quit()
}, 6000))

bot.on('error', e => { console.log('ERR ' + e.message); process.exit(1) })
