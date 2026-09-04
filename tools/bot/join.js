// What four people joining at once does to somebody already standing there.
//
// Usage: node join.js [port] [joiners] [where] [stagger ms] [expected columns]
//   where: each  — one process per joiner. The default, and the only one whose
//                  number is about this server.
//          apart — every joiner in one process, the settler in this one.
//          same  — every bot in this process. What this harness used to do,
//                  kept because it is the only way to reproduce the numbers
//                  decision records 0031 and 0038 published. See below.
//
// One settler joins, streams its whole view, then sends a chat line twenty
// times a second and times the round trip. Then N joiners connect at the same
// moment, and the settler keeps timing across a fixed three-second window.
// Its worst round trip is the number: whether somebody standing in the world,
// doing nothing, noticed that other people arrived.
//
// # Why `where` exists, and why it is the whole point of this file
//
// A joiner receives 289 chunk packets at the default view distance, and
// prismarine parses every one of them on the node thread that receives it.
// When all five bots lived in one process — which is what this harness did for
// its whole life — the settler's chat round trip was timed by an event loop
// that four joins had just filled with work. The stall that decision record
// 0031 called a regression and 0038 called a floor is **that event loop**.
//
// The two arrangements disagree by two orders of magnitude on the same server,
// the same commit and the same run: eight interleaved runs each, four joiners
// on a world read from region files, six busy threads on the machine —
//
//   same  median worst round trip 1,486 ms, 8 of 8 runs with one over 300 ms
//   each  median worst round trip     7 ms, 0 round trips over 50 ms at all
//
// and `same` scores the *current* server worse than the one before the region
// lock was narrowed, which is not a statement any server-side story can make.
// Keep the settler out of the joiners' process. See decision record 0042.
const mineflayer = require('mineflayer')

const port = Number(process.argv[2] || 25565)
const joiners = Number(process.argv[3] || 4)
const where = process.argv[4] || 'each'
const stagger = Number(process.argv[5] || 0) // ms between joiners; 0 is all at once
const expected = Number(process.argv[6] || 289) // (2*d+1)^2 at view distance d

// The window the settler is scored over, from the moment the first joiner
// connects. Fixed, so that a row is never scored on more samples than the row
// it is compared with — a measurement whose sample size is the variable it is
// measuring says nothing.
const WINDOW_MS = 3000
// Long enough for the settler to have its whole view before anybody else
// arrives, so that its own stream is not what is being measured.
const SETTLE_MS = 2000
const PING_MS = 20

function connect (name) {
  return mineflayer.createBot({ host: '127.0.0.1', port, username: name, auth: 'offline', version: '1.21.1' })
}

function watch (bot) {
  const t0 = Date.now()
  const s = { columns: 0, screen: null, last: null }
  bot._client.on('map_chunk', () => { s.columns += 1; s.last = Date.now() - t0 })
  bot._client.on('game_state_change', (p) => {
    if (p.reason === 13 && s.screen === null) s.screen = Date.now() - t0
  })
  return s
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

async function main () {
  // Forked before anything is timed, so that node's own start-up is not inside
  // the window and the joiners connect the moment they are told to.
  let children = null
  if (where !== 'same') {
    const n = where === 'each' ? joiners : 1
    children = []
    for (let i = 0; i < n; i++) {
      const c = require('child_process').fork(`${__dirname}/joiners.js`, { stdio: 'inherit' })
      await new Promise((r) => c.once('message', r))
      children.push(c)
    }
  }

  const settler = connect('settler')
  const settled = watch(settler)
  const up = await new Promise((r) => {
    const t = setTimeout(() => r(false), 20000)
    settler.once('spawn', () => { clearTimeout(t); r(true) })
  })
  if (!up) { console.error('the settler never spawned'); process.exit(1) }
  for (let i = 0; i < 200 && settled.columns < expected; i++) await sleep(50)
  console.log(`settler: ${settled.columns} of ${expected} columns, screen at ${settled.screen} ms, last at ${settled.last} ms`)

  const trips = []
  let pending = null
  settler.on('messagestr', (m) => {
    if (pending && m.includes(pending.tag)) { trips.push(Date.now() - pending.at); pending = null }
  })
  let n = 0
  const pinging = setInterval(() => {
    if (pending) return
    const tag = `p${n++}`
    pending = { tag, at: Date.now() }
    settler.chat(tag)
  }, PING_MS)

  await sleep(SETTLE_MS)
  const quiet = trips.length
  const bots = []
  const stats = []
  const spawns = []
  const opened = Date.now()
  if (children) {
    children.forEach((c, i) => c.send({
      port, joiners: children.length === 1 ? joiners : 1, stagger, expected, first: i
    }))
  } else {
    for (let i = 0; i < joiners; i++) {
      if (i > 0 && stagger > 0) await sleep(stagger)
      const b = connect(`joiner${i}`)
      // The spawn listener is attached here, at connect, and not after the
      // loop: with a stagger the first joiner has already spawned by the time
      // the last one connects, and `once` on an event that has already fired
      // waits forever. It waited ten minutes before it was bounded.
      spawns.push(new Promise((r) => {
        const t = setTimeout(() => r(false), 20000)
        b.once('spawn', () => { clearTimeout(t); r(true) })
      }))
      bots.push(b)
      stats.push(watch(b))
    }
  }
  await sleep(Math.max(0, WINDOW_MS - (Date.now() - opened)))
  clearInterval(pinging)

  const spawned = await Promise.all(spawns)
  if (spawned.some((ok) => !ok)) {
    console.log(`WARNING: ${spawned.filter((x) => !x).length} of ${joiners} joiners never spawned`)
  }
  stats.forEach((s, i) => {
    console.log(`joiner${i}: ${s.columns} of ${expected} columns, screen at ${s.screen} ms, last at ${s.last} ms`)
  })
  const report = (label, xs) => {
    const s = [...xs].sort((a, b) => a - b)
    const at = (q) => s[Math.min(s.length - 1, Math.max(0, Math.ceil(q * s.length) - 1))]
    // Counts, not rates: how many round trips a player would have felt.
    const over = (ms) => s.filter((x) => x > ms).length
    console.log(`${label}: n=${s.length} p50=${at(0.5)} p90=${at(0.9)} p99=${at(0.99)} ` +
      `max=${s[s.length - 1]} over50=${over(50)} over100=${over(100)} over300=${over(300)}`)
  }
  report(`during ${joiners} joining (${where})`, trips.slice(quiet))
  report('quiet control          ', trips.slice(0, quiet))
  if (children) for (const c of children) c.kill()
  for (const b of bots) b.quit()
  settler.quit()
  await sleep(300)
  process.exit(0)
}

main().catch((e) => { console.error(e); process.exit(1) })
