// What state does Minecraft put down when a player right-clicks?
//
// Dust places every block in its default state, because it has no placement
// context to compute another one from. Minecraft computes one per block, in
// Java — `Block.getStateForPlacement` — from the face that was clicked, where
// on it the cursor was, and which way the player was looking. That method needs
// a `Level` to run, and `Level` is an abstract class rather than an interface,
// so the reflection the block oracle uses cannot reach it. **A running server
// can be asked instead**, one placement at a time, and this writes the answers
// down.
//
// It asks **vanilla**, and only vanilla. What it produces is a file of
// Minecraft's own answers — the same kind of thing `xtask extract --only
// constants` produces and under the same rule, so it lands in the harness cache
// on the operator's own disk and no row of it is committed. Comparing Dust
// against those answers is a separate job in a separate place, because it is a
// comparison and not a measurement.
//
// Usage: node placement.js <port> [items.txt|item,item,...] [--survey] > answers.tsv
//        node placement.js <port> [items] --neighbours [--against all|blocks.txt]
//        node placement.js <port> [items] --into [all|blocks.txt|block,block,...]
//
// `--survey` trades the full 144-situation grid for eight chosen to answer only
// "does this block's placement depend on anything at all?", which is the
// question worth asking of every placeable item rather than of a handful.
//
// `--neighbours` asks the other question entirely. It holds the click still and
// varies what is **beside** the target instead, because the grid cannot see a
// rule that reads a neighbour and sixty-one of the items it calls wrong are
// exactly that: a fence connects to what it touches, a wall and a pane do, a
// rail bends toward the rail next to it, and a stair becomes an inner or an
// outer corner. Its rows carry two more columns — the neighbourhood the
// placement went out into, and which of those cells the placement *changed* —
// and both are read back off the wire rather than copied from the commands that
// built them.
//
// `--against` replaces the built-in scenes with one per block in a list, each
// putting that block to the north of the target. `--against all` is every block
// this build knows, which is how "does a fence connect to X" is answered for
// every X at once rather than guessed at from a rule.
//
// `--into` asks the third question, which neither of the others can reach: what
// was **already in the cell** the block goes into. The grid clears that cell and
// the neighbour scenes clear it too, so every row either of them has ever
// written was a placement into air — and three of Minecraft's rules read that
// cell. A block put into water comes out waterlogged, a second layer of snow
// stacks on the first, and a slab into its own other half becomes a double slab.
// The target is walled on four sides with stone so that a fluid put there stays
// there: an unwalled source spreads across the arena within a tick or two and
// the next sample is measured in a puddle.
//
// Its rows carry one more column than `--neighbours` — `into`, the state that
// really was in the target when the placement went out — and like `before` it is
// read back off the wire rather than copied from the `/setblock` that asked for
// it. That matters more here than anywhere else: flowing water put in a sealed
// pocket is gone by the next tick, and a row claiming the placement landed in
// it would be a row about a cell that held air.
//
// Notes to whoever runs it next:
//
//   * The arena is built from the **server console**, not by the bot: the bot
//     is not opped and does not need to be. Start the server with its stdin on
//     a pipe and point DUST_SERVER_CONSOLE at it; `tools/bot/README.md` has the
//     two lines that do it.
//
//   * **What is known about the arena is forgotten before the commands that
//     change it go out, not after.** The barrier is "wait until the support
//     turns to stone", and console changes arrive in the order the console ran
//     them, so seeing that means the clearing fill before it has landed too.
//     Waiting without forgetting first matches the support's stone from the
//     *previous* sample and returns immediately — which put `air` in
//     twenty-two rows of a run, every one of them a down-face placement,
//     because that is the cell the fill happened to reach last.
//
//   * **A refusal arrives as air.** The client predicted a block; the server
//     answers by telling it what is really there. No item places air, so there
//     is nothing to confuse it with — and the reader on the other side applies
//     the same rule, because a comparison of `minecraft:air` against anything
//     is a comparison against a placement that did not happen.
//
//   * The **first** change after a placement is the placement, and the last one
//     may not be: a door with nothing under it is put down and breaks on the
//     next tick. Reading the last change recorded `air` for forty-seven of a
//     door's situations. Whether it survived is a support rule rather than a
//     placement rule, so it is a column of its own.
//
//   * The state is read out of the block-change packets and never out of
//     `bot.blockAt`. The bot's own world lags a placement by an unbounded
//     amount; read that way, this reported the *previous* sample's block.
//
//   * **Both** block-change packets. A server sends `block_change` when exactly
//     one block in a section changed in a tick and `multi_block_change` when
//     more than one did — so the arena's `/fill` arrives the second way, and so
//     does a *door*, which puts down two blocks. Listening to one of them makes
//     every door read as REFUSED and every arena read as never settling.
//
//   * **The rotation written down is the one taken off the wire**, not the one
//     `bot.look` was given: mineflayer's convention and the protocol's differ by
//     a sign and a half turn. Recorded as the request, the file cannot tell "a
//     furnace faces where you look" from "a furnace faces back at you" — both
//     fit the same rows under different guesses about the convention.
//
//   * Yaw is set with `bot.look()` and not by writing `position_look`.
//     mineflayer's physics loop sends a position every tick and overwrites a
//     hand-written one, which makes every stair come out facing the same way
//     whatever was asked for.
//
//   * **The look is the outer loop and the face is the inner one**, which is
//     not a tidiness choice. A look reaches the server on mineflayer's next
//     physics tick, and a placement sent before it arrives is measured against
//     the *previous* sample's rotation. Turning twelve times an item instead of
//     a hundred and forty-four leaves one settling point per turn rather than
//     one per sample — and the first run of this tool, which turned every time,
//     produced exactly one poisoned row in 2,448: a piston at yaw 270 pitch 90
//     that came out facing west, which is where the sample before it had been
//     looking. One row in two thousand is the rate at which a measurement stops
//     being worth quoting.

const mineflayer = require('mineflayer')
const fs = require('fs')

const PORT = Number(process.argv[2] || 25565)
const VERSION = '1.21.1'

// The four horizontal directions, in Minecraft's own yaw convention: 0 is
// south (+z) and it turns clockwise seen from above.
const YAWS = [0, 90, 180, 270]
// Straight up, level, straight down. Enough to separate a block that reads the
// horizontal direction from one that reads the nearest looking direction.
const PITCHES = [-90, 0, 90]
// Low and high on the clicked face, which is what a slab and a trapdoor read.
const CURSORS = [0.25, 0.75]
const FACES = [0, 1, 2, 3, 4, 5]

// The reduced grid, for the question "does this block's placement depend on
// anything at all?" — which is the one worth asking of all 925 placeable items,
// where the full grid is worth asking of a handful.
//
// One baseline and one change from it per input, plus the two faces whose
// answers differ for reasons other than the inputs.
//
// **It undercounts, and by how much is knowable rather than mysterious.** What
// it varies is the four numbers a right-click carries; what it cannot vary is
// the block's *surroundings*, because the arena is one stone block in a cleared
// volume with nothing beside it. A stair's `shape` is computed from the stairs
// next to it, a chest becomes half of a double chest beside another, a fence
// connects to what it touches, and redstone wire reads all four neighbours.
// None of that shows up here. So a block this survey calls context-free is one
// whose *placement* reads nothing, and it may still owe a neighbour rule —
// which is a different problem, in a different place, and worth measuring
// separately rather than folding in and losing.
const SURVEY = [
  { yaw: 0, pitch: 0, face: 1, cursorY: 0.25 },   // the baseline: on the top
  { yaw: 0, pitch: 0, face: 0, cursorY: 0.25 },   // and on the bottom
  { yaw: 0, pitch: 0, face: 2, cursorY: 0.25 },   // and on a side
  { yaw: 0, pitch: 0, face: 2, cursorY: 0.75 },   // the cursor, where it is read
  { yaw: 0, pitch: 0, face: 5, cursorY: 0.25 },   // the opposite side
  { yaw: 90, pitch: 0, face: 1, cursorY: 0.25 },  // the yaw
  { yaw: 0, pitch: -90, face: 1, cursorY: 0.25 }, // the pitch, looking up
  { yaw: 0, pitch: 90, face: 1, cursorY: 0.25 }   // and looking down
]

/// The control, run first and every time.
///
/// `minecraft:stone` has exactly one state, so every situation in the grid has
/// to produce the same answer. It is not a test of the server — it is a test of
/// *this tool*, and it has earned its place twice: a run that read the bot's
/// cached world reported the previous item's block, and a run whose `/fill`
/// raced the read put `air` in twenty-two rows. Both showed up here first, as a
/// control disagreeing with itself, before anything downstream could be
/// believed.
const CONTROL = 'stone'

const DEFAULT_ITEMS = [
  // One per behaviour this is trying to tell apart. The control is prepended
  // to whatever list this is, so it is not in here.
  'oak_stairs',       // horizontal facing, half, shape
  'oak_slab',         // type from the cursor
  'oak_log',          // axis from the face
  'furnace',          // horizontal facing, opposite the look
  'observer',         // nearest looking direction, including vertical
  'piston',           // likewise
  'torch',            // wall variant chosen by the face
  'ladder',           // facing from the face, and refused on most of them
  'lever',            // face and facing together
  'oak_trapdoor',     // half from the cursor, facing from the look
  'oak_door',         // two blocks, hinge from where on the face
  'oak_sign',         // rotation, sixteen of them
  'chest',            // horizontal facing and a neighbour rule
  'repeater',         // facing, and it needs support
  'end_rod',          // facing straight off the face
  'hopper'            // facing off the face, but never up
]

// What to put in the target cell, for `--into` with no list of its own.
//
// One per rule the cell is read by, plus the controls that say the rule is
// keyed on the right thing. `air` is the baseline and is what every other
// survey's target cell holds; `lava` is water's twin and must *not* waterlog
// anything; seagrass and a bubble column are blocks that stand *in* water and
// report it, which is the clause a rule written as "is this block water" gets
// wrong.
//
// The snow depths are 1, 7 and 8 rather than all eight: one is where stacking
// starts, seven is the last that stacks, and eight is where it stops.
const DEFAULT_INTO = [
  'air',
  'water',
  'lava',
  'seagrass',
  'kelp_plant',
  'bubble_column',
  'short_grass',
  'snow[layers=1]',
  'snow[layers=7]',
  'snow[layers=8]',
  'oak_slab[type=bottom]',
  'oak_slab[type=top]',
  'oak_slab[type=double]',
  'spruce_slab[type=bottom]'
]

const wait = ms => new Promise(r => setTimeout(r, ms))

/// A rotation as a whole number of degrees.
///
/// The wire carries a float and the four the grid asks for come back as
/// -0.00001 and 179.99998; a reader grouping rows by rotation wants the four
/// and not eight thousand.
const round = degrees => Math.round(degrees * 100) / 100

function properties (stateId, registry) {
  const block = registry.blocksByStateId[stateId]
  if (!block) return { name: `state:${stateId}`, props: {} }
  const props = {}
  let rest = stateId - block.minStateId
  for (const state of (block.states || []).slice().reverse()) {
    const n = state.num_values
    const v = rest % n
    rest = Math.floor(rest / n)
    // prismarine orders a bool's values true-then-false, which is the opposite
    // of the reading order and is worth the extra line rather than the bug.
    //
    // **An int is looked up in `values` like an enum, and printing `v` is
    // wrong.** `v` is the index and the values need not start at zero:
    // `snow[layers]` runs 1..8, `candle[candles]` 1..4, a leaf's `distance`
    // 1..7. Printing the index reported `snow[layers=0]`, which is not a state
    // Minecraft has, and the run then disagreed with a server that was right.
    // Nineteen blocks were scored against Dust that way before it was found,
    // and the control could not catch it: stone has no properties.
    props[state.name] = state.type === 'bool'
      ? (v === 0 ? 'true' : 'false')
      : state.values
        ? state.values[v]
        : String(v)
  }
  return { name: block.name, props }
}

/// A state in the spelling everything on the Rust side uses: the namespaced
/// name, then the properties in name order.
///
/// Namespaced because that is what `dust_registry::Block::name` returns and
/// what the reader of this file will compare against — prismarine drops the
/// namespace and a comparison that had to put it back would be one more place
/// for the two vocabularies to disagree.
function describe (stateId, registry) {
  const { name, props } = properties(stateId, registry)
  const qualified = name.includes(':') ? name : `minecraft:${name}`
  const kv = Object.entries(props).map(([k, v]) => `${k}=${v}`).sort().join(',')
  return kv ? `${qualified}[${kv}]` : qualified
}

/// Every situation to try, grouped so that the look changes as rarely as
/// possible — see the note about the outer loop at the top of this file.
function situations (survey) {
  const list = survey
    ? SURVEY.slice()
    : YAWS.flatMap(yaw =>
      PITCHES.flatMap(pitch =>
        FACES.flatMap(face => CURSORS.map(cursorY => ({ yaw, pitch, face, cursorY })))))
  // A stable sort by rotation. `sort` is stable in every engine this runs on,
  // so the order within one rotation is the order written above.
  return list.sort((a, b) => (a.yaw - b.yaw) || (a.pitch - b.pitch))
}

/// What to put around the target, one scene per sample.
///
/// The neighbourhood is the variable here and the click is the constant, which
/// is the opposite of what `situations` does. Every scene is a list of
/// `[direction, block]` pairs written the way `/setblock` takes them, and the
/// row records what actually landed rather than what was asked for.
///
/// The common seven ask the questions that are the same for every family:
/// nothing beside it, a full block, a **full block that does not occlude**
/// (glass, which a fence connects to and a rule keyed on opacity would miss), a
/// **block with no full side** (a bottom slab, which a fence does not connect
/// to and a rule keyed on "is it solid" would get wrong), something above, and
/// the straight run that decides whether a wall raises its post.
///
/// The four after them put the block's *own* kind beside it, which is the case
/// a player builds all day: a fence next to a fence, a pane next to a pane, a
/// rail next to a rail. And where the block has a four-valued `facing` there
/// are four more with the neighbour turned, because a stair only becomes a
/// corner next to a stair facing across it — a scene using the neighbour's
/// default facing would report `straight` and call the rule correct.
function scenesFor (item, registry, against) {
  if (against) return against.map(block => [['north', block]])
  const list = [
    [],
    [['north', 'stone']],
    [['north', 'glass']],
    [['north', 'oak_slab[type=bottom]']],
    [['up', 'stone']],
    [['north', 'stone'], ['up', 'stone']],
    [['north', 'stone'], ['south', 'stone']],
    [['north', 'stone'], ['south', 'stone'], ['up', 'stone']]
  ]
  const self = registry.blocksByName[item] ? item : null
  if (!self) return list
  list.push(
    [['north', self]],
    [['north', self], ['south', self]],
    [['north', self], ['east', self]],
    [['north', self], ['south', self], ['east', self], ['west', self]]
  )
  const properties = registry.blocksByName[self].states || []
  const facing = properties.find(state => state.name === 'facing')
  if (facing && facing.num_values === 4) {
    list.push(
      [['north', `${self}[facing=east]`]],
      [['north', `${self}[facing=west]`]],
      [['south', `${self}[facing=east]`]],
      [['south', `${self}[facing=west]`], ['north', `${self}[facing=east]`]]
    )
    // The same corner with the neighbour in the *other* half. A stair only
    // takes a corner from a stair in its own half, and a rule that read the
    // facing and forgot the half would agree with every scene above and be
    // wrong about this one — which is the only reason it is worth a sample.
    const half = properties.find(state => state.name === 'half')
    if (half && half.num_values === 2) {
      list.push([['north', `${self}[facing=east,half=top]`]])
    }
  }
  return list
}

function main () {
  const survey = process.argv.includes('--survey')
  const neighbours = process.argv.includes('--neighbours')
  const againstAt = process.argv.indexOf('--against')
  const againstArgument = againstAt === -1 ? null : process.argv[againstAt + 1]
  const intoAt = process.argv.indexOf('--into')
  const intoArgument = intoAt === -1 ? null : process.argv[intoAt + 1]
  const names = argument => fs.existsSync(argument)
    ? fs.readFileSync(argument, 'utf8').split(/\s+/).filter(Boolean)
    : argument.split(',')
  const taken = [againstAt + 1, intoAt + 1]
  const argument = process.argv
    .slice(3)
    .find((a, i) => !a.startsWith('--') && !taken.includes(i + 3))
  let items = DEFAULT_ITEMS
  if (argument) items = names(argument)
  if (againstArgument && !neighbours) {
    process.stderr.write('--against only means anything with --neighbours.\n')
    process.exit(2)
  }

  const console_ = process.env.DUST_SERVER_CONSOLE
  if (!console_) {
    process.stderr.write(
      'DUST_SERVER_CONSOLE must name a pipe the server reads its console from.\n' +
      'The arena is built with /fill and /setblock, which a bot cannot run.\n'
    )
    process.exit(2)
  }
  const say = line => fs.appendFileSync(console_, line + '\n')

  const bot = mineflayer.createBot({
    host: '127.0.0.1', port: PORT, username: 'Placer', auth: 'offline', version: VERSION
  })

  // Every state a cell has held since it was last forgotten, in order. A list
  // rather than a value because the *first* change after a placement is the
  // placement and the last one may not be: a door with nothing under it is put
  // down and breaks on the next tick, and a tool that read the last change
  // recorded `air` for forty-seven of a door's situations.
  // The rotation the server was actually told, taken off the wire.
  //
  // **Not the number `bot.look` was given.** mineflayer's yaw and pitch are its
  // own convention and the protocol's are Minecraft's, and the two differ by a
  // sign and a half turn. Recording the request left the reader unable to tell
  // "a furnace faces where you look" from "a furnace faces back at you": both
  // fit the same rows under different guesses about the convention, which is a
  // measurement that cannot answer the question it was taken for. What a rule
  // in Dust has to work from is what arrives on the wire, so that is what this
  // writes down.
  let sent = { yaw: 0, pitch: 0 }
  const write = bot._client.write.bind(bot._client)
  bot._client.write = (name, params) => {
    if (params && typeof params.yaw === 'number' && typeof params.pitch === 'number') {
      sent = { yaw: params.yaw, pitch: params.pitch }
    }
    return write(name, params)
  }

  const changes = new Map()
  const at = p => `${p.x},${p.y},${p.z}`
  const record = (position, state) => {
    const key = at(position)
    const seen = changes.get(key)
    if (seen) seen.push(state)
    else changes.set(key, [state])
  }
  bot._client.on('block_change', p => {
    record(p.location, p.type)
  })
  // **Both** packets, and forgetting the second one cost an afternoon. A
  // server sends `block_change` when exactly one block in a section changed in
  // a tick and `multi_block_change` when more than one did, so a `/fill` and a
  // *door* — which puts down two blocks — both arrive the other way. A tool
  // that listens to one of them reads a door as REFUSED and never sees its
  // arena settle.
  bot._client.on('multi_block_change', p => {
    const section = p.chunkCoordinates
    for (const entry of p.records) {
      // stateId in the high bits, the position within the section in the low
      // twelve. Divided rather than shifted: a state id of 26,000 shifted left
      // by twelve is still inside a 32-bit int today, and `>>>` is a promise
      // about a width this has no reason to make.
      const state = Math.floor(entry / 4096)
      const packed = entry % 4096
      record({
        x: section.x * 16 + ((packed >> 8) & 0xf),
        y: section.y * 16 + (packed & 0xf),
        z: section.z * 16 + ((packed >> 4) & 0xf)
      }, state)
    }
  })

  /// Wait until the cell at `pos` reads `want`, or give up.
  ///
  /// The confirmation the whole measurement rests on: a console command and a
  /// placement are two independent things and nothing else orders them.
  async function settles (pos, want, registry, tries = 120) {
    for (let i = 0; i < tries; i++) {
      const seen = changes.get(at(pos))
      if (seen && properties(seen[seen.length - 1], registry).name === want) return true
      await wait(25)
    }
    return false
  }


  /// What each of the six cells around the target holds, right now.
  ///
  /// Read out of the change log and never out of `bot.blockAt`, for the reason
  /// the note at the top of this file gives about the placement itself: the
  /// bot's own world lags by an unbounded amount, and a neighbourhood read that
  /// way describes the sample before this one.
  const read = (around, registry) => {
    const out = {}
    for (const [direction, cell] of Object.entries(around)) {
      const seen = changes.get(at(cell))
      out[direction] = seen && seen.length
        ? describe(seen[seen.length - 1], registry)
        : 'minecraft:air'
    }
    return out
  }

  /// A neighbourhood as one field: `north=minecraft:stone;up=…`, air left out.
  ///
  /// Air is left out because a reader that assumes it for anything unnamed is
  /// right, and writing five `=minecraft:air` per row would treble the file to
  /// say nothing.
  const spell = states => Object.entries(states)
    .filter(([, state]) => state !== 'minecraft:air')
    .map(([direction, state]) => `${direction}=${state}`)
    .join(';')

  bot.once('spawn', async () => {
    await wait(2500)
    const registry = bot.registry
    const base = bot.entity.position.floored()
    const floor = base.y - 1

    // The arena: a floor to stand on, air above it, and one support block the
    // whole run reuses. Reusing it rather than walking a row keeps the bot in
    // one place, which keeps every sample the same distance from the eye and
    // therefore inside any reach limit either server applies.
    const ax = base.x + 8
    const az = base.z
    say(`fill ${ax - 4} ${floor} ${az - 4} ${ax + 4} ${floor + 8} ${az + 4} air`)
    say(`fill ${ax - 4} ${floor} ${az - 4} ${ax + 4} ${floor} ${az + 4} stone`)
    say(`tp Placer ${ax + 0.5} ${floor + 1} ${az + 2.5}`)
    await wait(1500)

    const support = { x: ax, y: floor + 2, z: az }
    const offsets = [[0, -1, 0], [0, 1, 0], [0, 0, -1], [0, 0, 1], [-1, 0, 0], [1, 0, 0]]

    // The support, once, before the first sample. Without this the first row
    // of every run is `ARENA DID NOT SETTLE`: the loop below waits for the
    // block_change that *changes* the support, and the first time round there
    // is nothing there to change.
    say(`setblock ${support.x} ${support.y} ${support.z} stone`)
    if (!await settles(support, 'stone', registry, 200)) {
      process.stderr.write('the arena never appeared; is the console pipe connected?\n')
      process.exit(1)
    }

    // -----------------------------------------------------------------------
    // The neighbour survey.
    //
    // The grid above varies the four numbers a right-click carries and nothing
    // else, so it cannot see a rule that reads the block *beside* the one going
    // down — and sixty-one of the items it reports wrong are exactly that. This
    // varies the surroundings instead and holds the click still.
    //
    // The target is always the cell on top of the support, clicked on the
    // support's up face at yaw 0. That leaves five cells free — north, south,
    // east, west and up — and each scene sets some of them from the console
    // before the placement goes out.
    //
    // **Two columns are added and both are measurements rather than
    // intentions.** `before` is what the six cells around the target actually
    // held when the placement went out, read back out of the change log rather
    // than copied from the command that was sent: a `/setblock` naming a
    // property the block does not have is refused by the server and leaves air,
    // and a scene written down as what was *asked for* would score a rule
    // against a neighbourhood that was never there. `after` is which of those
    // six changed *because* of the placement, which is the other half of the
    // rule: a fence has to connect when the block beside it arrives later, and
    // a survey that only recorded the placed cell could not tell whether it
    // did.
    async function neighbourSurvey (against) {
      const target = { x: support.x, y: support.y + 1, z: support.z }
      const around = {
        down: support,
        up: { x: target.x, y: target.y + 1, z: target.z },
        north: { x: target.x, y: target.y, z: target.z - 1 },
        south: { x: target.x, y: target.y, z: target.z + 1 },
        west: { x: target.x - 1, y: target.y, z: target.z },
        east: { x: target.x + 1, y: target.y, z: target.z }
      }
      // Face 1 is the support's top, so the placement lands in `target`. The
      // look is set once for the whole run and never again, which is the same
      // rule the grid loop follows for a different reason: here the click is
      // the constant and the surroundings are the variable.
      await bot.look(0, 0, true)
      await wait(300)

      let sequence = 1
      const seen = []
      process.stdout.write(
        '# item\tface\tyaw\tpitch\tcursor_y\tresult\tsurvived\tbefore\tafter\n'
      )
      for (const item of [CONTROL, ...items.filter(i => i !== CONTROL)]) {
        const entry = registry.itemsByName[item]
        if (!entry) {
          process.stdout.write(`${item}\t-\t-\t-\t-\tNO SUCH ITEM\t-\t-\t-\n`)
          continue
        }
        bot._client.write('set_creative_slot', {
          slot: 36,
          item: {
            itemCount: 1,
            itemId: entry.id,
            addedComponentCount: 0,
            removedComponentCount: 0,
            components: [],
            removeComponents: []
          }
        })
        bot._client.write('held_item_slot', { slotId: 0 })
        await wait(120)

        for (const scene of scenesFor(item, registry, against)) {
          // Forget the whole neighbourhood before the commands that change it
          // go out, for the reason the grid loop's comment gives: waiting on a
          // barrier without forgetting first matches the *previous* sample.
          for (const cell of Object.values(around)) changes.delete(at(cell))
          changes.delete(at(target))
          say(`fill ${support.x - 2} ${support.y - 2} ${support.z - 2} ` +
              `${support.x + 2} ${support.y + 3} ${support.z + 2} air`)
          // A floor under the four cells beside the target, and not only under
          // the target itself. Without it every neighbour that needs something
          // to stand on falls the tick after it is set — which recorded a rail
          // beside a rail as `north=minecraft:air` and scored the shape rule
          // against a neighbourhood that had emptied itself. The support is
          // *not* laid here: it is the barrier below and has to be the last
          // change of the batch.
          for (const [dx, dz] of [[-1, 0], [1, 0], [0, -1], [0, 1]]) {
            say(`setblock ${support.x + dx} ${support.y} ${support.z + dz} stone`)
          }
          for (const [direction, block] of scene) {
            const cell = around[direction]
            say(`setblock ${cell.x} ${cell.y} ${cell.z} ${block}`)
          }
          // The support goes down **last**, so that seeing it turn to stone
          // means the scene before it has landed too. The grid loop can put it
          // first because nothing follows it there; here something does, and a
          // barrier that is not last is not a barrier.
          say(`setblock ${support.x} ${support.y} ${support.z} stone`)
          if (!await settles(support, 'stone', registry)) {
            process.stdout.write(
              `minecraft:${item}\t1\t${round(sent.yaw)}\t${round(sent.pitch)}\t` +
              `0.25\tARENA DID NOT SETTLE\t-\t-\t-\n`
            )
            continue
          }
          // A tick or two for the world to react to the scene before it is
          // written down. **`before` has to be true at the moment the
          // placement goes out, and the barrier only says the commands were
          // delivered.** A `/setblock` puts a ladder or a torch wherever it is
          // told and the block falls on the next tick, so a scene read the
          // instant the barrier passed recorded a ladder that was already
          // gone by the time the click arrived — and the placement was then
          // scored against a neighbourhood that had emptied itself. One row in
          // 799 of a fence-against-every-block run, and it looked exactly like
          // a wrong connection rule.
          await wait(100)
          const before = read(around, registry)
          changes.delete(at(target))

          bot._client.write('block_place', {
            hand: 0,
            location: support,
            direction: 1,
            cursorX: 0.5,
            cursorY: 0.25,
            cursorZ: 0.5,
            insideBlock: false,
            sequence: sequence++
          })
          await wait(200)
          const got = changes.get(at(target))
          const first = got ? describe(got[0], registry) : null
          const result = first === null || first === 'minecraft:air'
            ? 'REFUSED\t-'
            : first + (got.length > 1 && got[got.length - 1] !== got[0] ? '\tbroke' : '\tstood')
          const after = read(around, registry)
          const moved = Object.keys(after)
            .filter(direction => after[direction] !== before[direction])
            .map(direction => `${direction}=${after[direction]}`)
          if (item === CONTROL) seen.push(`${spell(before)} -> ${result}`)
          process.stdout.write(
            `minecraft:${item}\t1\t${round(sent.yaw)}\t${round(sent.pitch)}\t` +
            `0.25\t${result}\t${spell(before) || '-'}\t${moved.join(';') || '-'}\n`
          )
        }

        if (item === CONTROL) {
          // The same control as the grid run and for the same reason, asking
          // the same question of a different variable: stone has one state, so
          // no arrangement of neighbours may change it. It catches a scene that
          // leaked into the next sample, which is this loop's own way to be
          // wrong.
          const states = new Set(
            seen.map(s => s.split(' -> ')[1].split('\t')[0]).filter(r => r !== 'REFUSED')
          )
          if (states.size !== 1 || !states.has(`minecraft:${CONTROL}`)) {
            process.stderr.write(
              `the control disagreed with itself, so nothing below it is worth reading.\n` +
              `${CONTROL} has one state and this run saw ${states.size}:\n  ` +
              seen.join('\n  ') + '\n'
            )
            process.exit(1)
          }
          process.stderr.write(
            `control: ${CONTROL} agrees with itself over ${seen.length} neighbourhoods\n`
          )
        }
      }
    }

    // -----------------------------------------------------------------------
    // The `into` survey.
    //
    // The third variable, and the one neither of the others can vary: what was
    // already in the cell the block goes into. Both of them clear it, so every
    // row either has ever written is a placement into air — and Minecraft has
    // three rules that read it.
    //
    // The click is held still, at the support's top face, so the placement
    // always lands in the cell above the support and that cell is the one this
    // fills. **It is walled on four sides with stone**, which is not tidiness:
    // an unwalled water source spreads across the arena inside two ticks and
    // every sample after it is measured in a puddle. The cell above is left
    // open so that a block taller than one is not refused for the wrong reason.
    //
    // A refusal here does not look like a refusal anywhere else. In the other
    // surveys the target is air and a refused placement leaves air; here it
    // leaves **whatever was already there**, which is a state and reads like a
    // successful placement of it. So the test is not "is it air", it is "is it
    // what was there a tick ago" — and a slab put into a double slab, or a
    // ninth layer of snow, is exactly that.
    async function intoSurvey (blocks) {
      const target = { x: support.x, y: support.y + 1, z: support.z }
      const around = {
        down: support,
        up: { x: target.x, y: target.y + 1, z: target.z },
        north: { x: target.x, y: target.y, z: target.z - 1 },
        south: { x: target.x, y: target.y, z: target.z + 1 },
        west: { x: target.x - 1, y: target.y, z: target.z },
        east: { x: target.x + 1, y: target.y, z: target.z }
      }
      const sides = [[-1, 0], [1, 0], [0, -1], [0, 1]]
      await bot.look(0, 0, true)
      await wait(300)

      let sequence = 1
      const seen = []
      process.stdout.write(
        '# item\tface\tyaw\tpitch\tcursor_y\tresult\tsurvived\tbefore\tafter\tinto\n'
      )
      for (const item of [CONTROL, ...items.filter(i => i !== CONTROL)]) {
        const entry = registry.itemsByName[item]
        if (!entry) {
          process.stdout.write(`${item}\t-\t-\t-\t-\tNO SUCH ITEM\t-\t-\t-\t-\n`)
          continue
        }
        bot._client.write('set_creative_slot', {
          slot: 36,
          item: {
            itemCount: 1,
            itemId: entry.id,
            addedComponentCount: 0,
            removedComponentCount: 0,
            components: [],
            removeComponents: []
          }
        })
        bot._client.write('held_item_slot', { slotId: 0 })
        await wait(120)

        for (const block of blocks) {
          for (const cell of Object.values(around)) changes.delete(at(cell))
          changes.delete(at(target))
          say(`fill ${support.x - 2} ${support.y - 2} ${support.z - 2} ` +
              `${support.x + 2} ${support.y + 3} ${support.z + 2} air`)
          for (const [dx, dz] of sides) {
            say(`setblock ${support.x + dx} ${support.y} ${support.z + dz} stone`)
            say(`setblock ${target.x + dx} ${target.y} ${target.z + dz} stone`)
          }
          say(`setblock ${target.x} ${target.y} ${target.z} ${block}`)
          // The support last, so that seeing it turn to stone means the pocket
          // and what is in it have landed too.
          say(`setblock ${support.x} ${support.y} ${support.z} stone`)
          if (!await settles(support, 'stone', registry)) {
            process.stdout.write(
              `minecraft:${item}\t1\t${round(sent.yaw)}\t${round(sent.pitch)}\t` +
              `0.25\tARENA DID NOT SETTLE\t-\t-\t-\t-\n`
            )
            continue
          }
          // A tick for the world to react before anything is written down, for
          // the reason the neighbour survey gives: a block that cannot hold
          // itself up falls between the barrier and the click, and flowing
          // water in a sealed pocket is gone by the next tick. What is
          // recorded is what was there when the placement went out.
          await wait(100)
          const before = read(around, registry)
          const held = changes.get(at(target))
          const inside = held && held.length
            ? describe(held[held.length - 1], registry)
            : 'minecraft:air'
          changes.delete(at(target))

          bot._client.write('block_place', {
            hand: 0,
            location: support,
            direction: 1,
            cursorX: 0.5,
            cursorY: 0.25,
            cursorZ: 0.5,
            insideBlock: false,
            sequence: sequence++
          })
          await wait(200)
          const got = changes.get(at(target))
          const first = got ? describe(got[0], registry) : null
          // Unchanged is the refusal, and `minecraft:air` is only one spelling
          // of unchanged — the one every other survey happens to see because
          // its target is always empty.
          const result = first === null || first === inside || first === 'minecraft:air'
            ? 'REFUSED\t-'
            : first + (got.length > 1 && got[got.length - 1] !== got[0] ? '\tbroke' : '\tstood')
          const after = read(around, registry)
          const moved = Object.keys(after)
            .filter(direction => after[direction] !== before[direction])
            .map(direction => `${direction}=${after[direction]}`)
          if (item === CONTROL) seen.push(`${inside} -> ${result}`)
          process.stdout.write(
            `minecraft:${item}\t1\t${round(sent.yaw)}\t${round(sent.pitch)}\t` +
            `0.25\t${result}\t${spell(before) || '-'}\t${moved.join(';') || '-'}\t${inside}\n`
          )
        }

        if (item === CONTROL) {
          // Stone has one state, so no cell it is placed into may change what
          // it comes out as. This one has a second job the others do not: it
          // says the *pocket* is being built, because a scene whose block never
          // landed would show up as a control that agreed with itself over a
          // run of empty cells. The stderr line prints how many distinct cells
          // it was placed into for exactly that reason.
          const states = new Set(
            seen.map(s => s.split(' -> ')[1].split('\t')[0]).filter(r => r !== 'REFUSED')
          )
          const cells = new Set(seen.map(s => s.split(' -> ')[0]))
          if (states.size !== 1 || !states.has(`minecraft:${CONTROL}`)) {
            process.stderr.write(
              `the control disagreed with itself, so nothing below it is worth reading.\n` +
              `${CONTROL} has one state and this run saw ${states.size}:\n  ` +
              seen.join('\n  ') + '\n'
            )
            process.exit(1)
          }
          process.stderr.write(
            `control: ${CONTROL} agrees with itself over ${seen.length} cells, ` +
            `${cells.size} of them distinct\n`
          )
        }
      }
    }

    if (intoAt !== -1) {
      const spec = intoArgument && !intoArgument.startsWith('--') ? intoArgument : null
      const blocks = spec === null
        ? DEFAULT_INTO
        : spec === 'all'
          ? Object.keys(registry.blocksByName).sort()
          : names(spec)
      await intoSurvey(blocks)
      process.exit(0)
    }

    if (neighbours) {
      const against = againstArgument === null
        ? null
        : againstArgument === 'all'
          ? Object.keys(registry.blocksByName).sort()
          : names(againstArgument)
      await neighbourSurvey(against)
      process.exit(0)
    }

    const grid = situations(survey)
    let sequence = 1
    // The control first, whatever was asked for, and its answers are checked
    // before any of the rest are printed.
    const seen = []
    process.stdout.write('# item\tface\tyaw\tpitch\tcursor_y\tresult\tsurvived\n')
    for (const item of [CONTROL, ...items.filter(i => i !== CONTROL)]) {
      const entry = registry.itemsByName[item]
      if (!entry) {
        process.stdout.write(`${item}\t-\t-\t-\t-\tNO SUCH ITEM\n`)
        continue
      }
      bot._client.write('set_creative_slot', {
        slot: 36,
        item: {
          itemCount: 1,
          itemId: entry.id,
          addedComponentCount: 0,
          removedComponentCount: 0,
          components: [],
          removeComponents: []
        }
      })
      bot._client.write('held_item_slot', { slotId: 0 })
      await wait(120)

      let looking = null
      for (const { yaw, pitch, face, cursorY } of grid) {
        if (looking !== `${yaw},${pitch}`) {
          // Radians, and mineflayer's own physics loop is what actually sends
          // it. The settling wait is here and not beside the placement because
          // this is the only place the rotation changes — see the note about
          // the outer loop at the top of this file.
          await bot.look((yaw * Math.PI) / 180, (pitch * Math.PI) / 180, true)
          await wait(250)
          looking = `${yaw},${pitch}`
        }

        const off = offsets[face]
        const target = {
          x: support.x + off[0],
          y: support.y + off[1],
          z: support.z + off[2]
        }
        // Forget everything known about these two cells *before* the commands
        // that change them go out. This is the whole barrier and the line that
        // was missing: without it, `settles` below matched the support's stone
        // from the *previous* sample and returned before the fill had been
        // delivered, which put `air` in twenty-two rows of a run — every one of
        // them a down-face placement, because that is the cell the fill
        // happened to reach last.
        changes.delete(at(support))
        changes.delete(at(target))
        // A clean slate: the volume rather than the two cells, because a door
        // is two blocks tall and leaves its top half behind and a bed would be
        // worse. The fill empties the support too, so the setblock after it
        // always changes something and always emits.
        say(`fill ${support.x - 2} ${support.y - 2} ${support.z - 2} ` +
            `${support.x + 2} ${support.y + 3} ${support.z + 2} air`)
        say(`setblock ${support.x} ${support.y} ${support.z} stone`)
        // Seeing the support turn to stone means everything the console ran
        // before it has been delivered: the changes go down one connection in
        // the order the commands ran.
        if (!await settles(support, 'stone', registry)) {
          process.stdout.write(
            `minecraft:${item}\t${face}\t${round(sent.yaw)}\t${round(sent.pitch)}\t` +
            `${cursorY}\tARENA DID NOT SETTLE\t-\n`
          )
          continue
        }
        changes.delete(at(target))

        bot._client.write('block_place', {
          hand: 0,
          location: support,
          direction: face,
          cursorX: 0.5,
          cursorY,
          cursorZ: 0.5,
          insideBlock: false,
          sequence: sequence++
        })
        await wait(200)
        const got = changes.get(at(target))
        // The first change is the placement. A later one is the world reacting
        // to it — a door with nothing under it, a torch on a face that cannot
        // hold one — and that is a support rule rather than a placement rule,
        // so it is reported beside the answer instead of replacing it.
        // Air is how a refusal arrives. The client predicted a block and the
        // server answers by telling it what is really there, which for a cell
        // the arena just cleared is air — and no item places air, so there is
        // nothing for this to be confused with. A run with no `air` in it and
        // no `REFUSED` either would be a run where nothing was ever refused,
        // which `torch` alone disproves: it cannot hang from a ceiling.
        const first = got ? describe(got[0], registry) : null
        const result = first === null || first === 'minecraft:air'
          ? 'REFUSED\t-'
          : first + (got.length > 1 && got[got.length - 1] !== got[0] ? '\tbroke' : '\tstood')
        if (item === CONTROL) seen.push(`${face}/${yaw}/${pitch}/${cursorY} -> ${result}`)
        process.stdout.write(
          `minecraft:${item}\t${face}\t${round(sent.yaw)}\t${round(sent.pitch)}\t` +
          `${cursorY}\t${result}\n`
        )
      }

      if (item === CONTROL) {
        // Everything after this row depends on the tool being sound, so this
        // is where the run stops if it is not. Refusals are allowed — a
        // control that could not be placed somewhere is still a control — and
        // two different *states* are not.
        const states = new Set(
          seen.map(s => s.split(' -> ')[1].split('\t')[0]).filter(r => r !== 'REFUSED')
        )
        if (states.size !== 1 || !states.has(`minecraft:${CONTROL}`)) {
          process.stderr.write(
            `the control disagreed with itself, so nothing below it is worth reading.\n` +
            `${CONTROL} has one state and this run saw ${states.size}:\n  ` +
            seen.join('\n  ') + '\n'
          )
          process.exit(1)
        }
        process.stderr.write(`control: ${CONTROL} agrees with itself over ${seen.length} situations\n`)
      }
    }
    process.exit(0)
  })

  bot.on('error', e => {
    process.stderr.write(`FAIL  ${e.message}\n`)
    process.exit(1)
  })
  bot.on('kicked', r => {
    process.stderr.write(`FAIL  kicked: ${JSON.stringify(r)}\n`)
    process.exit(1)
  })
}

main()
