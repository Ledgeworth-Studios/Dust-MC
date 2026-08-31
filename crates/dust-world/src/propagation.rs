//! Light propagation: the increase and decrease passes over a [`LightGraph`].
//!
//! [`LightArray`](crate::light::LightArray) stores a section's light; this
//! module changes it. Placing a torch must raise the cells around it, each
//! neighbour of a raised cell one below that, fading across section borders;
//! breaking the torch must take it all back without extinguishing the
//! neighbouring torch's share. Both directions are breadth-first walks, and
//! both are written here against a trait rather than against chunks, for the
//! reason the rest of this crate keeps hitting: who owns which block, and
//! therefore what attenuates light, is registry knowledge this crate does
//! not have yet.
//!
//! # The seam: what a `LightGraph` owes the engine
//!
//! A [`LightGraph`] is *neighbour light arrays plus an opacity provider*, in
//! exactly the shape the walks consume. The engine asks a cell's stored
//! level, writes a new one, asks how much light entering a cell loses, and
//! asks whether a cell is inside the lit volume at all. It never learns that
//! sections exist: stepping from one section into the next is an ordinary
//! `y + 1` step, and whether that lands in another array, another chunk, or
//! a wall is entirely the implementation's business. A future wiring
//! implements the trait over a chunk column's
//! [`LightArray`](crate::light::LightArray)s and its block states' opacity,
//! and every walk here works over it unchanged.
//!
//! Neighbours are the six face-adjacent cells, examined in the fixed order
//! `+x, -x, +y, -y, +z, -z`, and queues run first-in-first-out. That is the
//! canonical order, and it is load-bearing: the walks visit and rewrite
//! cells in an order fixed by the seeds alone, so two runs over one input
//! produce one trace — replay logs and lockstep tests stand on it.
//!
//! # The rules the walks obey
//!
//! * Entering a cell costs one plus that cell's opacity, saturating at zero.
//!   Uniform opacity therefore attenuates monotonically — the property
//!   `tests/light_propagation.rs` checks exactly against a naive reference.
//! * A cell's level is only ever rewritten upwards by [`raise`] and
//!   downwards by [`darken`]. Every rewrite strictly moves toward its
//!   target, so no cell can be rewritten more times than there are levels —
//!   fifteen — and even a graph full of cycles (a torus, say) terminates.
//!   Termination is a theorem about the queue discipline, and the tests
//!   exercise it rather than trust it.
//! * Work is bounded by [`Budget`], counted in edge examinations; running
//!   past the cap stops the walk with
//!   [`PropagationError::BudgetExhausted`] rather than pinning a server
//!   thread. The partial result is left consistent: only completed rewrites
//!   were made.
//!
//! # Sky light, stated plainly
//!
//! [`seed_skylight`] fills the cells a heightmap says are open to the sky
//! and lets them spill by the ordinary rules. Vanilla does one thing more:
//! sky light falling *straight down* through transparent air does not fade.
//! That refinement deliberately waits — it is a statement about what sky
//! *means*, wired alongside the registry, and bolting it on now would fork
//! this walker from every other user of the same rules. Until then seeded
//! columns behave like any other brightness-fifteen source, which is
//! monotone, terminating, and slightly darker under overhangs than vanilla.
//!
//! **What this does not catch:** a graph that lies. An implementation whose
//! `level` disagrees with its own arrays, or whose opacity answers differ
//! between the raise and the pass that reads them, propagates confidently
//! from bad data; the walks see exactly what the trait shows them.

use std::collections::VecDeque;

/// The six face-adjacent offsets, in the order neighbours are always
/// examined. One list, used by every walk, is what makes traces comparable.
const NEIGHBOURS: [(i32, i32, i32); 6] = [
    (1, 0, 0),
    (-1, 0, 0),
    (0, 1, 0),
    (0, -1, 0),
    (0, 0, 1),
    (0, 0, -1),
];

/// A world seen as the light walks see it: readable and writable levels,
/// an opacity for every cell, and a definite inside.
///
/// See the module documentation for why this is a trait and what an
/// implementation owes it. Levels run 0 to 15 as everywhere else in the
/// format; opacity is the extra loss for *entering* a cell, 0 for clear air
/// through 15 for something fully opaque.
pub trait LightGraph {
    /// The stored level at a cell.
    ///
    /// # Panics
    ///
    /// Implementations are expected to treat an out-of-range query as their
    /// bug; the walks only ask [`LightGraph::contains`]-positive cells.
    #[must_use]
    fn level(&self, x: i32, y: i32, z: i32) -> u8;

    /// Overwrite the level at a cell.
    ///
    /// # Panics
    ///
    /// As [`LightGraph::level`], for cells the walks were never told about.
    fn set_level(&mut self, x: i32, y: i32, z: i32, level: u8);

    /// How much light is lost entering this cell, `0..=15`.
    #[must_use]
    fn opacity(&self, x: i32, y: i32, z: i32) -> u8;

    /// Whether a cell is part of this volume at all. Steps leave the queue
    /// at the boundary; they are never taken into the void.
    #[must_use]
    fn contains(&self, x: i32, y: i32, z: i32) -> bool;
}

/// How much work one walk may do before it stops and says so.
///
/// Counted in edge examinations — neighbour lookups, the unit every walk is
/// made of. A section holds four thousand cells and each has six edges, so
/// a budget in the low hundreds of thousands covers a section's worth of
/// rewriting several times over; a stuck walk burns the cap and reports
/// instead of burning the tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    limit: u64,
}

impl Budget {
    /// A budget of `limit` edge examinations.
    #[must_use]
    pub const fn new(limit: u64) -> Self {
        Self { limit }
    }

    /// The cap this budget names.
    #[must_use]
    pub const fn limit(&self) -> u64 {
        self.limit
    }
}

impl Default for Budget {
    /// Enough for a full section rewrite, six times over. Deliberate head
    /// room: hitting this cap means something is wrong, and the error says
    /// how much work it took to be sure.
    fn default() -> Self {
        Self { limit: 147_456 }
    }
}

/// Why a walk stopped short of finishing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropagationError {
    /// The walk ran past its [`Budget`] with work still outstanding.
    /// Whatever it rewrote before stopping was rewritten correctly, but
    /// nothing after the cap happened. Retry with more room, or investigate
    /// a graph that will not settle.
    BudgetExhausted {
        /// The examinations the walk was allowed before it stopped.
        spent: u64,
        /// The cap that tripped, as given.
        budget: u64,
    },
    /// A seed brighter than the format can store. Nothing was written.
    SeedTooBright {
        /// The level that was refused.
        level: u8,
    },
}

impl std::fmt::Display for PropagationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BudgetExhausted { spent, budget } => write!(
                f,
                "the light walk spent its whole budget of {budget} edge examinations \
                 ({spent} charged) and stopped with work outstanding"
            ),
            Self::SeedTooBright { level } => {
                write!(f, "{level} does not fit in the four bits a light level has")
            }
        }
    }
}

impl std::error::Error for PropagationError {}

/// Where a walk remembers the cells still to visit, with the level each was
/// last rewritten to — or, darkening, the level it used to hold.
struct Queue {
    pending: VecDeque<((i32, i32, i32), u8)>,
    spent: u64,
    budget: Budget,
}

impl Queue {
    fn new(budget: Budget) -> Self {
        Self {
            pending: VecDeque::new(),
            spent: 0,
            budget,
        }
    }

    /// One edge examination, charged against the budget before it happens.
    fn charge(&mut self) -> Result<(), PropagationError> {
        if self.spent >= self.budget.limit {
            return Err(PropagationError::BudgetExhausted {
                spent: self.spent,
                budget: self.budget.limit,
            });
        }
        self.spent += 1;
        Ok(())
    }
}

/// Offer a queued cell's level to its six neighbours, and their neighbours
/// in turn, until nothing improves.
///
/// Shared by [`raise`] and [`darken`]; the difference between them is what
/// goes into the queue, not how it drains. An entry whose level no longer
/// matches the cell's stored one is dropped on arrival: either something
/// rewrote the cell brighter since (the classic superseded case) or, coming
/// out of a darkening, the cell's light was taken back after the entry was
/// made. Either way the entry describes a past the field no longer agrees
/// with, and spreading from it would resurrect light nobody holds.
///
/// What one step into a cell of this opacity costs.
///
/// **`max(1, opacity)`, and not `1 + opacity`.** Minecraft charges the move
/// itself *or* the block, whichever is larger, so entering water — opacity one
/// — costs one level and not two, and a column of water reads 15, 14, 13
/// rather than 15, 13, 11.
///
/// This was `1 + opacity` for the whole of the light engine's life and nothing
/// could see it, because the only opacity model that existed answered 0 or 15
/// and the two rules agree at both ends: at 0 they are both one, and at 15
/// they both take everything. It became visible the day
/// [`OpacityModel::per_state`] carried Minecraft's own numbers into the same
/// walk and `cargo xtask harness light` reported six thousand cells short by
/// exactly the amount this doubled. **A wrong constant hidden by another wrong
/// constant** — which is the argument for measuring against the real thing
/// rather than against a stand-in, made once more.
///
/// Saturating at fifteen is not a third case: a cell that costs everything
/// takes everything, and a level is a nibble.
#[must_use]
pub const fn step_cost(opacity: u8) -> u8 {
    if opacity > 1 {
        opacity
    } else {
        1
    }
}

/// Each neighbour is charged to the budget before it is examined.
fn spread<G: LightGraph + ?Sized>(
    graph: &mut G,
    queue: &mut Queue,
) -> Result<(), PropagationError> {
    while let Some(((x, y, z), level)) = queue.pending.pop_front() {
        if level != graph.level(x, y, z) {
            continue;
        }
        for &(dx, dy, dz) in &NEIGHBOURS {
            let next = (x + dx, y + dy, z + dz);
            if !graph.contains(next.0, next.1, next.2) {
                continue;
            }
            queue.charge()?;
            let offered = level.saturating_sub(step_cost(graph.opacity(next.0, next.1, next.2)));
            if offered > graph.level(next.0, next.1, next.2) {
                graph.set_level(next.0, next.1, next.2, offered);
                queue.pending.push_back((next, offered));
            }
        }
    }
    Ok(())
}

/// Raise light from seeds outward until nothing improves.
///
/// Seeds arrive as `(x, y, z, level)` and are processed in the order given —
/// the first half of canonical ordering; the second half is the fixed
/// neighbour order beside the trait. A seed brighter than the cell already
/// holds is written and queued; from there each queued cell offers its
/// level to its six neighbours minus one plus their opacity, and every
/// neighbour that would end up strictly brighter is rewritten and queued in
/// turn. No cell is rewritten more than fifteen times, once per level above
/// the one it started at, so even a toroidal graph drains.
///
/// Returns the edge examinations spent, which is also how a caller tells a
/// quiet pass from a busy one.
///
/// # Errors
///
/// [`PropagationError::SeedTooBright`] before anything is written, or
/// [`PropagationError::BudgetExhausted`] partway through, leaving the
/// completed portion applied.
pub fn raise<G: LightGraph + ?Sized>(
    graph: &mut G,
    seeds: &[(i32, i32, i32, u8)],
    budget: Budget,
) -> Result<u64, PropagationError> {
    for (_, _, _, level) in seeds {
        if *level > 15 {
            return Err(PropagationError::SeedTooBright { level: *level });
        }
    }
    let mut queue = Queue::new(budget);
    // Seeds go in in the order given, each written before the next is looked
    // at: the trace of writes is a function of the seed list alone.
    for &(x, y, z, level) in seeds {
        if !graph.contains(x, y, z) || level <= graph.level(x, y, z) {
            continue;
        }
        graph.set_level(x, y, z, level);
        queue.pending.push_back(((x, y, z), level));
    }
    spread(graph, &mut queue)?;
    Ok(queue.spent)
}

/// Take light away around cells whose source went dark, and re-light what
/// survives.
///
/// `darkened` names the cells whose stored level is now meaningless — a
/// torch was broken there, a glowing block replaced. Each is read, zeroed
/// and queued with the level it had; the walk then takes back every cell
/// that was lit *through* one of them — strictly dimmer than the level that
/// fed it.
///
/// `relight` names the emitters that still stand: every source within reach
/// of the change, as `(x, y, z, emission)`. The walks cannot discover these
/// for themselves — a stored level and an own emission read identically
/// through [`LightGraph::level`] — which is why the parameter exists and why
/// vanilla runs the very same pair of queues: one taking light away, one
/// giving the survivors' share back. Each named cell is rewritten up to its
/// emission if the removal ate into it, and all of them spread together
/// with whatever the removal spared, so the region settles to exactly the
/// field the surviving sources produce — the definition of correct here,
/// and what the differential tests check against a from-scratch
/// recomputation.
///
/// Cells holding no light are skipped: darkness spreading from darkness is
/// work for nothing.
///
/// Returns the examinations spent across both phases.
///
/// # Errors
///
/// As [`raise`]; the budget is shared across both phases and the error
/// reports how far it got when it tripped.
pub fn darken<G: LightGraph + ?Sized>(
    graph: &mut G,
    darkened: &[(i32, i32, i32)],
    relight: &[(i32, i32, i32, u8)],
    budget: Budget,
) -> Result<u64, PropagationError> {
    for (_, _, _, level) in relight {
        if *level > 15 {
            return Err(PropagationError::SeedTooBright { level: *level });
        }
    }
    let mut queue = Queue::new(budget);
    for &(x, y, z) in darkened {
        let had = graph.level(x, y, z);
        if had == 0 {
            continue;
        }
        graph.set_level(x, y, z, 0);
        queue.pending.push_back(((x, y, z), had));
    }

    // Cells spared because their own light stands: they feed the refill.
    let mut survivors: Vec<((i32, i32, i32), u8)> = Vec::new();
    while let Some((cell, had)) = queue.pending.pop_front() {
        for &(dx, dy, dz) in &NEIGHBOURS {
            let next = (cell.0 + dx, cell.1 + dy, cell.2 + dz);
            if !graph.contains(next.0, next.1, next.2) {
                continue;
            }
            queue.charge()?;
            let held = graph.level(next.0, next.1, next.2);
            if held == 0 {
                // Already taken back, or dark to begin with.
                continue;
            }
            if held < had {
                // Lit only through the cell that went dark: its light goes
                // too, and whatever depended on it is asked the same
                // question in turn.
                graph.set_level(next.0, next.1, next.2, 0);
                queue.pending.push_back((next, held));
            } else {
                // Lit at least as brightly from elsewhere: it survives this
                // removal and lends that light back afterwards.
                survivors.push((next, held));
            }
        }
    }

    let remaining = queue.budget.limit - queue.spent;
    let mut refill = Queue::new(Budget::new(remaining));
    // Survivors spread as they stand; standing emitters are restored to
    // their emission first, in the order given, exactly as raise would
    // seed them. One drain covers both.
    refill.pending.extend(survivors);
    for &(x, y, z, emission) in relight {
        if !graph.contains(x, y, z) || emission <= graph.level(x, y, z) {
            continue;
        }
        graph.set_level(x, y, z, emission);
        refill.pending.push_back(((x, y, z), emission));
    }
    spread(graph, &mut refill)?;
    Ok(queue.spent + refill.spent)
}

/// Fill the open sky above a heightmap and let it spill.
///
/// `columns` yields `(x, z, open_to_sky)` per column, where the range names
/// the cells between the surface and the volume's ceiling — exactly what a
/// [`Heightmap`](crate::heightmap::Heightmap)'s first-available reading
/// implies for that column. Every named cell is seeded at fifteen, in
/// column order, and one [`raise`] spreads the light under overhangs and
/// into shadowed ground by the ordinary rules. See the module documentation
/// for the one vanilla refinement this deliberately leaves to the wiring
/// that owns the finished skylight policy.
///
/// Seeding is idempotent over unchanged columns: fifteen over fifteen
/// rewrites nothing, and the spread finds nothing to do.
///
/// # Errors
///
/// As [`raise`], across the seeding and the spread together.
pub fn seed_skylight<G, I>(
    graph: &mut G,
    columns: I,
    budget: Budget,
) -> Result<u64, PropagationError>
where
    G: LightGraph + ?Sized,
    I: IntoIterator<Item = (i32, i32, std::ops::Range<i32>)>,
{
    let mut seeds: Vec<(i32, i32, i32, u8)> = Vec::new();
    for (x, z, open_to_sky) in columns {
        for y in open_to_sky {
            seeds.push((x, y, z, 15));
        }
    }
    raise(graph, &seeds, budget)
}

/// How much light is lost entering a block state.
///
/// Two shapes, and which one is in force is the difference between Dust's
/// lighting being approximate and being Minecraft's own.
///
/// * [`OpacityModel::transparent_only`] — the stand-in. The named states pass
///   light untouched and *everything else* costs the full fifteen, which
///   swallows light in one step. It is conservative by construction: a state
///   nobody named darkens rather than leaks. It is also wrong about water,
///   glass, leaves and ice, and `cargo xtask harness light` says by how much.
/// * [`OpacityModel::per_state`] — Minecraft's own number for every state,
///   read out of the operator's own jar by the light oracle. Opacity and
///   emission are Java code in Minecraft, in no report and no data pack, which
///   is decision record 0008; `dust_registry::light::LightTable` is the reader
///   and this is where its answer arrives.
///
/// Both answer 15 for a state they have never heard of, for the same reason:
/// every known gap in Dust's lighting under-lights, so an unknown block that
/// stops light is one more of the same, while one that passes it is a new kind
/// of wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpacityModel {
    shape: Shape,
}

/// The two shapes an [`OpacityModel`] takes. Private: which one a model is
/// carrying is not a question any caller has needed to ask, and the day one
/// does it wants a named method rather than a match.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Shape {
    /// Sorted state ids that pass light; everything else is a wall.
    TransparentOnly(Vec<u32>),
    /// One level per state id, indexed directly.
    PerState(Box<[u8]>),
}

impl OpacityModel {
    /// A model where exactly these block states pass light and every other
    /// one is a wall.
    #[must_use]
    pub fn transparent_only(states: impl IntoIterator<Item = u32>) -> Self {
        let mut transparent: Vec<u32> = states.into_iter().collect();
        transparent.sort_unstable();
        transparent.dedup();
        Self {
            shape: Shape::TransparentOnly(transparent),
        }
    }

    /// A model holding one level per block state, indexed by state id.
    ///
    /// Levels above fifteen are clamped rather than refused. This crate has no
    /// idea what a block state is and cannot tell a bad table from a version
    /// it has not met; the reader that *can* —
    /// `dust_registry::light::LightTable` — refuses one, and by the time a
    /// slice arrives here the question has been asked by somebody who could
    /// answer it. Clamping keeps the invariant the walks rely on, which is
    /// that a step costs at most everything.
    #[must_use]
    pub fn per_state(levels: impl IntoIterator<Item = u8>) -> Self {
        Self {
            shape: Shape::PerState(levels.into_iter().map(|l| l.min(15)).collect()),
        }
    }

    /// The opacity a block state carries under this model.
    #[must_use]
    pub fn opacity(&self, state: u32) -> u8 {
        match &self.shape {
            Shape::TransparentOnly(transparent) => {
                if transparent.binary_search(&state).is_ok() {
                    0
                } else {
                    15
                }
            }
            Shape::PerState(levels) => levels.get(state as usize).copied().unwrap_or(15),
        }
    }
}

/// How much light a block state gives off.
///
/// The other half of what a light engine needs from the registry, and the
/// mirror of [`OpacityModel`]: opacity is what a cell takes and this is what it
/// gives. Both are Java code in Minecraft, both arrive from the operator's own
/// jar, and both answer conservatively for a state they do not know — an
/// unknown block that emits nothing is one dark cell, where one that emits
/// would be light coming out of a block nobody can point at.
///
/// [`EmissionModel::nothing`] is not a placeholder in the way the opacity
/// stand-in was. A world of stone and water really does emit nothing, and a
/// server with no constants table is not approximating anything by saying so —
/// it is declining to invent the one number it has no source for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EmissionModel {
    levels: Box<[u8]>,
}

impl EmissionModel {
    /// A model where nothing emits.
    #[must_use]
    pub fn nothing() -> Self {
        Self::default()
    }

    /// A model holding one emission per block state, indexed by state id.
    ///
    /// Levels above fifteen are clamped, for the same reason
    /// [`OpacityModel::per_state`] clamps: the reader that could tell a bad
    /// table from an unmet version has already refused one, and a level is a
    /// nibble.
    #[must_use]
    pub fn per_state(levels: impl IntoIterator<Item = u8>) -> Self {
        Self {
            levels: levels.into_iter().map(|l| l.min(15)).collect(),
        }
    }

    /// What `state` gives off.
    #[must_use]
    pub fn emission(&self, state: u32) -> u8 {
        self.levels.get(state as usize).copied().unwrap_or(0)
    }

    /// Whether nothing in this model emits at all.
    ///
    /// The fast path a whole pass can be skipped on, and it is the common case
    /// twice over: a server with no constants table, and — with one — a column
    /// of a world that has no torch, no lava and no glowstone in it.
    #[must_use]
    pub fn is_dark(&self) -> bool {
        self.levels.iter().all(|level| *level == 0)
    }

    /// Whether any of these states emits.
    ///
    /// Asked of a section's palette before its 4,096 cells are read. A palette
    /// is the shortlist of what a section can possibly hold, so a section whose
    /// palette holds no emitter has no emitter, and nearly every section of a
    /// real world is one.
    #[must_use]
    pub fn any_emits(&self, states: impl IntoIterator<Item = u32>) -> bool {
        states.into_iter().any(|state| self.emission(state) > 0)
    }
}

impl Default for OpacityModel {
    /// Air and nothing else: the model a freshly generated world effectively
    /// has.
    fn default() -> Self {
        Self::transparent_only(std::iter::once(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small transparent box of cells, everything else void. Opacity is
    /// uniform except for cells listed in `walls`, which are solid, and cells
    /// in `dim`, which carry whatever level they were given — the shape water
    /// and leaves have in a real world, and the only shape under which
    /// [`step_cost`] and `1 + opacity` disagree.
    struct Box {
        size: i32,
        walls: Vec<(i32, i32, i32)>,
        dim: Vec<((i32, i32, i32), u8)>,
        levels: std::collections::BTreeMap<(i32, i32, i32), u8>,
    }

    impl Box {
        fn new(size: i32) -> Self {
            Self {
                size,
                walls: Vec::new(),
                dim: Vec::new(),
                levels: std::collections::BTreeMap::new(),
            }
        }

        fn wall(mut self, x: i32, y: i32, z: i32) -> Self {
            self.walls.push((x, y, z));
            self
        }

        fn dim(mut self, x: i32, y: i32, z: i32, opacity: u8) -> Self {
            self.dim.push(((x, y, z), opacity));
            self
        }

        fn field(&self) -> Vec<((i32, i32, i32), u8)> {
            let mut cells: Vec<_> = self.levels.iter().map(|(c, l)| (*c, *l)).collect();
            cells.sort_unstable();
            cells
        }
    }

    impl LightGraph for Box {
        fn level(&self, x: i32, y: i32, z: i32) -> u8 {
            // Unwritten cells are dark; a map lookup would panic on them.
            self.levels.get(&(x, y, z)).copied().unwrap_or(0)
        }

        fn set_level(&mut self, x: i32, y: i32, z: i32, level: u8) {
            self.levels.insert((x, y, z), level);
        }

        fn opacity(&self, x: i32, y: i32, z: i32) -> u8 {
            if let Some((_, opacity)) = self.dim.iter().find(|(c, _)| *c == (x, y, z)) {
                return *opacity;
            }
            if self.walls.contains(&(x, y, z)) {
                15
            } else {
                0
            }
        }

        fn contains(&self, x: i32, y: i32, z: i32) -> bool {
            x >= 0 && y >= 0 && z >= 0 && x < self.size && y < self.size && z < self.size
        }
    }

    #[test]
    fn light_fades_one_per_step_and_stops_at_zero() {
        // One source in clear air: every cell reads its brightness minus its
        // Manhattan distance from the source, floored at zero -- the whole
        // field follows from that sentence, and the walk is checked against
        // it cell by cell.
        let mut graph = Box::new(6);
        raise(&mut graph, &[(0, 0, 0, 4)], Budget::default()).expect("fits any budget");
        for x in 0..6i32 {
            for y in 0..6i32 {
                for z in 0..6i32 {
                    let expected = 4u8.saturating_sub((x + y + z) as u8);
                    assert_eq!(
                        graph.level(x, y, z),
                        expected,
                        "({x}, {y}, {z}) at distance {}",
                        x + y + z
                    );
                }
            }
        }
    }

    #[test]
    fn a_wall_plane_shadows_everything_behind_it_because_every_path_crosses_it() {
        // The plane x == 2 is opaque edge to edge, so no route exists from
        // the source to the far side that does not enter it -- and entering
        // an opaque cell swallows the whole offer. Cells this side are lit;
        // cells the far side stay dark however they are connected otherwise.
        let mut graph = Box::new(4);
        for y in 0..4i32 {
            for z in 0..4i32 {
                graph = graph.wall(2, y, z);
            }
        }
        raise(&mut graph, &[(0, 1, 1, 12)], Budget::default()).expect("room to spare");

        for y in 0..4i32 {
            for z in 0..4i32 {
                assert_eq!(graph.level(2, y, z), 0, "the wall itself holds nothing");
                assert_eq!(graph.level(3, y, z), 0, "behind the wall at (3, {y}, {z})");
            }
        }
        assert_eq!(graph.level(0, 1, 1), 12);
        assert_eq!(graph.level(1, 1, 1), 11, "this side is lit up to the plane");
    }

    #[test]
    fn raising_twice_takes_the_brighter_offer_without_revisiting_the_whole_field() {
        let mut graph = Box::new(6);
        raise(&mut graph, &[(0, 0, 0, 6)], Budget::default()).expect("first source");
        let spent_first =
            raise(&mut graph, &[(5, 5, 5, 15)], Budget::default()).expect("second source");
        assert_eq!(graph.level(0, 0, 0), 6);
        assert_eq!(graph.level(5, 5, 5), 15);
        // The second pass spent work near its own source; the already-lit
        // corner was left alone once the two fields met.
        assert!(spent_first > 0);
        assert_eq!(graph.level(0, 0, 0), 6, "a dimmer neighbour never rewrites");
    }

    #[test]
    fn darkening_a_source_spares_what_another_source_still_covers() {
        // Two brightness-10 sources three steps apart share some of their
        // field. Taking one away must leave exactly what the other alone
        // produces -- checked against a from-scratch raise over the same box.
        // Darkened cells compare as darkness, not as absence: the map keeps
        // the zeroes it wrote.
        let lit_only = |graph: &Box| -> Vec<((i32, i32, i32), u8)> {
            graph
                .field()
                .into_iter()
                .filter(|(_, level)| *level > 0)
                .collect()
        };

        let mut shared = Box::new(8);
        let both = [(0, 0, 0, 10u8), (3, 0, 0, 10u8)];
        raise(&mut shared, &both, Budget::default()).expect("two sources");

        let mut survivors = Box::new(8);
        raise(&mut survivors, &[(3, 0, 0, 10)], Budget::default()).expect("one source");

        darken(
            &mut shared,
            &[(0, 0, 0)],
            &[(3, 0, 0, 10)],
            Budget::default(),
        )
        .expect("settles");
        assert_eq!(lit_only(&shared), lit_only(&survivors));
        assert_eq!(shared.level(3, 0, 0), 10, "the survivor kept its own level");
    }

    #[test]
    fn darkening_leaves_no_trace_when_nothing_else_lit_the_cells() {
        let mut graph = Box::new(5);
        raise(&mut graph, &[(2, 2, 2, 12)], Budget::default()).expect("one source");
        darken(&mut graph, &[(2, 2, 2)], &[], Budget::default()).expect("removes it");
        assert!(graph.field().iter().all(|(_, l)| *l == 0), "all dark again");
        // And darkening darkness is free.
        let spent =
            darken(&mut graph, &[(2, 2, 2)], &[], Budget::default()).expect("nothing to do");
        assert_eq!(spent, 0);
    }

    #[test]
    fn a_seed_past_fifteen_is_refused_before_anything_is_written() {
        let mut graph = Box::new(3);
        let err = raise(&mut graph, &[(0, 0, 0, 16)], Budget::new(100))
            .expect_err("sixteen does not fit in four bits");
        assert_eq!(err, PropagationError::SeedTooBright { level: 16 });
        assert!(err.to_string().contains("four bits"), "{err}");
        assert_eq!(
            graph.field(),
            vec![],
            "validation happens before the first write"
        );
    }

    #[test]
    fn a_walk_over_a_section_worth_of_edges_notices_its_budget() {
        // One seed at fifteen in a large-enough box needs more examinations
        // than the tiny budget allows; the error names the cap and the spend.
        let mut graph = Box::new(20);
        let err = raise(&mut graph, &[(0, 0, 0, 15)], Budget::new(50))
            .expect_err("fifty examinations cannot finish this");
        assert_eq!(
            err,
            PropagationError::BudgetExhausted {
                spent: 50,
                budget: 50
            }
        );
        // The partial result is consistent: levels only ever rose, and the
        // source itself was written before any edge was examined.
        assert_eq!(graph.level(0, 0, 0), 15);

        // The same run with room to move finishes and reports its spend.
        let mut whole = Box::new(20);
        let spent = raise(&mut whole, &[(0, 0, 0, 15)], Budget::default())
            .expect("the default covers a box this size");
        assert!(spent > 50, "{spent} examinations were needed");
    }

    #[test]
    fn the_stand_in_opacity_model_is_air_clear_and_everything_else_a_wall() {
        let model = OpacityModel::transparent_only([0, 42, 7]);
        assert_eq!(model.opacity(0), 0);
        assert_eq!(model.opacity(42), 0);
        assert_eq!(model.opacity(7), 0);
        assert_eq!(
            model.opacity(1),
            15,
            "an unlisted state costs the full step"
        );
        assert_eq!(model.opacity(43), 15);
        // Order of construction is irrelevant to the answers.
        assert_eq!(OpacityModel::transparent_only([7, 0, 42]), model);
        assert_eq!(OpacityModel::default().opacity(0), 0);
        assert_eq!(OpacityModel::default().opacity(5), 15);
    }

    #[test]
    fn a_per_state_model_answers_what_it_was_given() {
        // Minecraft's three values on 1.21.1, in the order the oracle writes
        // them: air, stone, water.
        let model = OpacityModel::per_state([0, 15, 1]);
        assert_eq!(model.opacity(0), 0);
        assert_eq!(model.opacity(1), 15);
        assert_eq!(model.opacity(2), 1);
    }

    #[test]
    fn a_state_past_the_end_of_a_per_state_model_is_a_wall() {
        // The same direction the stand-in errs in, and for the same reason:
        // an unknown block that stops light is one more under-lit cell, while
        // one that passes it is light inside a sealed room.
        let model = OpacityModel::per_state([0, 15, 1]);
        assert_eq!(model.opacity(3), 15);
        assert_eq!(model.opacity(u32::MAX), 15);
    }

    #[test]
    fn a_per_state_level_above_fifteen_is_clamped_rather_than_trusted() {
        // A step costs at most everything: the walks subtract `1 + opacity`
        // from a nibble, and a level of 200 arriving here would be an
        // arithmetic hazard rather than a dark block.
        assert_eq!(OpacityModel::per_state([200]).opacity(0), 15);
    }

    #[test]
    fn a_step_costs_the_move_or_the_block_whichever_is_larger() {
        // Minecraft's rule, and not `1 + opacity`. The two agree at both ends
        // of the model that used to be the only one — 0 and 15 — which is
        // exactly why this was wrong for the whole of the engine's life
        // without anything being able to see it.
        assert_eq!(step_cost(0), 1, "clear air still costs the move");
        assert_eq!(step_cost(1), 1, "water costs one level, not two");
        assert_eq!(step_cost(2), 2);
        assert_eq!(step_cost(15), 15);
    }

    #[test]
    fn a_column_of_water_dims_one_level_a_block() {
        // The fact `harness light` measures against a world Minecraft lit,
        // written here as the five cells it comes down to. Under `1 + opacity`
        // this column read 15, 13, 11, 9, 7 and the cell four blocks down was
        // half as bright as Minecraft says it is.
        let mut column = Box::new(5)
            .dim(0, 3, 0, 1)
            .dim(0, 2, 0, 1)
            .dim(0, 1, 0, 1)
            .dim(0, 0, 0, 1);
        raise(&mut column, &[(0, 4, 0, 15)], Budget::default()).expect("five cells");
        let down: Vec<u8> = (0..5).rev().map(|y| column.level(0, y, 0)).collect();
        assert_eq!(down, vec![15, 14, 13, 12, 11]);
    }
}
