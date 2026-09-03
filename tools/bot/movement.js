// What a genuine client's movement packets actually look like, as counts.
//
// The server has to decide which claimed positions are impossible, and a
// threshold reasoned about in the abstract is a threshold that rubber-bands
// somebody on bad wifi. So this measures instead: it drives mineflayer through
// the motions a player actually makes, records every position packet it sends
// on its way out, and prints the distribution of per-packet displacement.
//
// mineflayer's physics is `prismarine-physics`, an independent reimplementation
// of Minecraft's player movement that shares no code with this project. It is
// the same reason `check.js` uses this client rather than a hand-written one:
// numbers taken from a client that agreed with Dust by construction would be
// numbers about Dust.
//
// Usage: node movement.js [port] [--check]
//   without --check   record and print the distribution, exit 0
//   with --check      also assert the server corrects an impossible move,
//                     exit 1 if it does not
//
// The correction check is deliberately in the same file as the measurement:
// the threshold and the evidence that it never fires on honest play are one
// claim, and splitting them is how they drift apart.

const mineflayer = require('mineflayer')

const args = process.argv.slice(2)
const CHECK = args.includes('--check')
const PORT = Number(args.find(a => !a.startsWith('--')) || 25565)
const VERSION = '1.21.1'
const JOIN_TIMEOUT_MS = 30000

// Blocks per packet. A client sends one movement packet per client tick, so a
// bucket here is a per-tick displacement. The top three exist to be empty:
// they are where a validator's threshold will sit, and a run that puts an
// honest packet in them is the run that says the threshold is wrong.
const EDGES = [0.05, 0.1, 0.2, 0.3, 0.4, 0.6, 0.8, 1.0, 1.5, 2.0, 3.0, 5.0, 10.0, Infinity]

function label (i) {
  const lo = i === 0 ? 0 : EDGES[i - 1]
  const hi = EDGES[i]
  return hi === Infinity ? `>= ${lo}` : `${lo} - ${hi}`
}

class Phase {
  constructor (name) {
    this.name = name
    this.counts = new Array(EDGES.length).fill(0)
    this.n = 0
    this.max = 0
    this.maxY = 0
    this.gapMax = 0
    this.gapSum = 0
  }

  add (d, dy, gap) {
    this.n += 1
    if (d > this.max) this.max = d
    if (Math.abs(dy) > Math.abs(this.maxY)) this.maxY = dy
    if (gap !== null) {
      if (gap > this.gapMax) this.gapMax = gap
      this.gapSum += gap
    }
    for (let i = 0; i < EDGES.length; i += 1) {
      if (d < EDGES[i]) { this.counts[i] += 1; return }
    }
  }
}

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

// Every outbound movement packet, as the server would see it: the position the
// client claimed and the wall-clock moment it left. Hooked on `write` rather
// than read off `bot.entity.position` because the packet is what the server
// validates, and a client's internal position is a tick ahead of it.
function record (bot, state) {
  const write = bot._client.write.bind(bot._client)
  bot._client.write = (name, params) => {
    if (name === 'position' || name === 'position_look') {
      const now = Date.now()
      const gap = state.last === null ? null : now - state.lastAt
      if (state.phase && state.last !== null) {
        const dx = params.x - state.last.x
        const dy = params.y - state.last.y
        const dz = params.z - state.last.z
        state.phase.add(Math.sqrt(dx * dx + dy * dy + dz * dz), dy, gap)
      }
      state.last = { x: params.x, y: params.y, z: params.z }
      state.lastAt = now
    }
    return write(name, params)
  }
}

async function main () {
  const bot = await spawned('Mover')
  const state = { phase: null, last: null, lastAt: 0 }
  record(bot, state)

  // The correction, if one arrives. A clientbound player_position is the only
  // thing that moves a client that believes it is somewhere else; a log line
  // is not a correction.
  let corrected = null
  bot._client.on('position', p => {
    corrected = p
    bot._client.write('teleport_confirm', { teleportId: p.teleportId })
  })

  await wait(2000)
  const phases = []
  async function during (name, body) {
    const p = new Phase(name)
    phases.push(p)
    state.phase = p
    await body()
    state.phase = null
    await wait(400)
  }
  async function phase (name, ms, before, after) {
    const p = new Phase(name)
    phases.push(p)
    if (before) before()
    state.phase = p
    await wait(ms)
    state.phase = null
    if (after) after()
    await wait(400)
  }

  await phase('standing still', 3000)
  await phase('walking', 5000,
    () => bot.setControlState('forward', true),
    () => bot.setControlState('forward', false))
  await phase('sprinting', 5000,
    () => { bot.setControlState('forward', true); bot.setControlState('sprint', true) },
    () => { bot.setControlState('forward', false); bot.setControlState('sprint', false) })
  await phase('sprint-jumping', 6000,
    () => {
      bot.setControlState('forward', true)
      bot.setControlState('sprint', true)
      bot.setControlState('jump', true)
    },
    () => {
      bot.setControlState('forward', false)
      bot.setControlState('sprint', false)
      bot.setControlState('jump', false)
    })

  // Creative flight, which the server grants in its abilities packet. This is
  // the fastest thing an honest client does under its own power on this
  // server, and it is not physics: the client simply moves.
  await during('flying up 300 blocks', async () => {
    bot.creative.startFlying()
    try {
      await bot.creative.flyTo(bot.entity.position.offset(0, 300, 0))
    } catch (e) { /* whatever height it reached is the height it fell from */ }
  })
  await phase('flying forward', 4000,
    () => { bot.setControlState('forward', true); bot.setControlState('sprint', true) },
    () => { bot.setControlState('forward', false); bot.setControlState('sprint', false) })

  // A real fall from that height: stop flying and let prismarine-physics drop
  // it. Free fall is where the largest honest per-tick displacement comes
  // from — a player accelerates until drag balances gravity — and it is the
  // number a threshold has to clear with room to spare.
  await phase('falling', 20000, () => bot.creative.stopFlying())

  // A stalled connection. The physics keeps ticking and the packets queue,
  // then arrive together — which changes the *gap* between packets and not the
  // displacement in each. That distinction is the whole reason the server's
  // budget is floored at one tick rather than scaled by elapsed time alone.
  const queue = []
  const socket = bot._client.socket
  const realWrite = socket.write.bind(socket)
  await phase('walking through a 700 ms stall', 4000, () => {
    bot.setControlState('forward', true)
    socket.write = (...a) => { queue.push(a); return true }
    setTimeout(() => {
      socket.write = realWrite
      for (const a of queue) realWrite(...a)
    }, 700)
  }, () => bot.setControlState('forward', false))

  report(phases)

  if (CHECK) {
    // Physics off, so nothing argues with the position written by hand. Then
    // one packet claiming a spot five hundred blocks away, which no honest
    // client can produce and which the numbers above say is nowhere near any
    // bucket an honest client reaches.
    bot.physicsEnabled = false
    await wait(500)
    corrected = null
    const here = bot.entity.position
    const far = { x: here.x + 500, y: here.y, z: here.z + 500, onGround: false }
    bot._client.write('position', far)
    await wait(2000)
    const back = corrected
    const ok = Boolean(back) &&
      Math.abs(back.x - far.x) > 100 && Math.abs(back.z - far.z) > 100
    console.log(
      `${ok ? 'ok  ' : 'FAIL'}  a player who claims to be 707 blocks away is put back` +
      (back ? ` — teleported to ${back.x.toFixed(1)}, ${back.y.toFixed(1)}, ${back.z.toFixed(1)}` : ' — no correction arrived')
    )

    // And the other half: a step an honest client makes is not corrected. The
    // same code path, one tick of sprinting apart.
    corrected = null
    const near = { x: here.x + 0.3, y: here.y, z: here.z, onGround: true }
    bot._client.write('position', near)
    await wait(1500)
    const quiet = corrected === null
    console.log(
      `${quiet ? 'ok  ' : 'FAIL'}  a 0.3 block step is left alone` +
      (quiet ? '' : ' — the server corrected an honest move')
    )

    try { bot.quit() } catch (e) { /* already gone */ }
    process.exitCode = ok && quiet ? 0 : 1
    return
  }
  try { bot.quit() } catch (e) { /* already gone */ }
}

function report (phases) {
  const width = Math.max(...phases.map(p => p.name.length))
  console.log('\nPer-packet displacement, in blocks. One packet is one client tick.\n')
  const head = 'phase'.padEnd(width) + '  ' + EDGES.map((_, i) => label(i).padStart(9)).join('') +
    '      n       max     max dy   max gap'
  console.log(head)
  for (const p of phases) {
    console.log(
      p.name.padEnd(width) + '  ' +
      p.counts.map(c => String(c).padStart(9)).join('') +
      String(p.n).padStart(7) +
      p.max.toFixed(3).padStart(10) +
      p.maxY.toFixed(3).padStart(11) +
      (String(p.gapMax) + ' ms').padStart(10)
    )
  }
  const all = phases.reduce((a, p) => a + p.n, 0)
  const worst = Math.max(...phases.map(p => p.max))
  console.log(`\n${all} packets, largest honest step ${worst.toFixed(3)} blocks in one tick.`)
}

main().catch(e => {
  console.log(`FAIL  ${e.message}`)
  console.log('\nIs a dust server running on this port, with online_mode = false')
  console.log('and [data] path set?')
  process.exit(1)
})
