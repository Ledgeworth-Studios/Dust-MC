// The joiners, in a process that is not the settler's.
//
// Forked by `join.js`; not run directly. It waits for one message saying which
// port, how many bots and what to call them, connects them, and reports how
// many columns each received.
//
// It exists because of what the one-process harness could not tell apart. A
// joiner's 289 chunk packets are parsed by prismarine on the node thread that
// receives them, so a settler sharing that thread was timing an event loop
// rather than a server. `join.js each` forks one of these per joiner, which is
// both the most simultaneous arrangement — every joiner has a whole thread to
// receive on, and they finish their streams sooner than in any other mode —
// and the most isolated. Decision record 0042.
const mineflayer = require('mineflayer')

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

// Long enough to cover the parent's window and short enough that the report
// lands before the parent stops listening.
const REPORT_AFTER_MS = 2500

process.on('message', async (m) => {
  const { port, joiners, stagger, expected, first } = m
  const bots = []
  const stats = []
  for (let i = 0; i < joiners; i++) {
    if (i > 0 && stagger > 0) await sleep(stagger)
    const bot = mineflayer.createBot({
      host: '127.0.0.1', port, username: `joiner${first + i}`, auth: 'offline', version: '1.21.1'
    })
    const t0 = Date.now()
    const s = { columns: 0, screen: null, last: null }
    bot._client.on('map_chunk', () => { s.columns += 1; s.last = Date.now() - t0 })
    bot._client.on('game_state_change', (p) => {
      if (p.reason === 13 && s.screen === null) s.screen = Date.now() - t0
    })
    bots.push(bot)
    stats.push(s)
  }
  await sleep(REPORT_AFTER_MS)
  // Printed rather than sent back, because it is evidence the joiners really
  // joined: a run whose settler is untroubled because nobody arrived is the
  // one way this measurement can lie, and this line is what rules it out.
  stats.forEach((s, i) => {
    console.log(`joiner${first + i}: ${s.columns} of ${expected} columns, screen at ${s.screen} ms, last at ${s.last} ms`)
  })
  for (const b of bots) b.quit()
  await sleep(200)
  process.exit(0)
})

process.send({ ready: true })
