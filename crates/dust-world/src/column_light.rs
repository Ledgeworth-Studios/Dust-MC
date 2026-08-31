//! A [`LightGraph`] over one chunk column.
//!
//! `propagation` is written against a trait rather than against chunks,
//! because who owns which block — and therefore what attenuates light — is
//! registry knowledge that crate does not have. This is the wiring that trait
//! was left for: a column's sky-light arrays, plus an opacity model the caller
//! supplies.
//!
//! # Across a chunk boundary
//!
//! A column on its own knows nothing about its neighbours, so light did not
//! cross a chunk boundary: a cell at x = 15 was at the edge of the volume and
//! the walk stopped rather than stepping into the column next door. That was
//! visible as a seam wherever terrain heights differed across a boundary — a
//! cliff face on the low side of a step stayed dark to the boundary and then
//! jumped to daylight.
//!
//! [`ColumnSkyLight::seed_with_neighbours`] takes that away by giving the
//! walk a **skirt**: the four surrounding columns' sky floors, used as light
//! *sources* along the four faces. Where a neighbour is open to the sky and
//! this column is not, the edge cell facing it is seeded with what a cell at
//! fifteen offers across one step, and the walk carries it inward from there.
//!
//! The neighbours are sources and not extra cells of the volume, and that is
//! not only the simpler of the two. A read-only ring inside the volume stores
//! nothing, so it reads back dark however brightly it was just handed light —
//! and a walk that steps along such a ring re-queues every cell of it on every
//! visit and finishes only by running out of budget.
//!
//! **What the skirt is exact for, and what it is not.** Where a neighbouring
//! column is open to the sky, the answer is vanilla's: that cell is fifteen in
//! vanilla too, and one step in is fourteen either way. Where the light would
//! have to travel *through* a neighbour first — around the mouth of a cave
//! three blocks into the next chunk — the ring reports darkness and the
//! result is under-lit. That is the same direction the seam erred in and a
//! strict improvement on it, and it is the case the multi-column version
//! exists for; the trait was given `contains` precisely so that version is a
//! wider volume rather than a rewrite.
//!
//! The skirt costs the neighbours' *sky floors* and nothing else — 256
//! integers per column, not a megabyte of blocks — which is what makes it
//! affordable on the streaming path. Measured (`benches/skylight.rs`, idle
//! machine): against neighbours shaped like itself, which is nearly every
//! column of a real world, it is not distinguishable from lighting the column
//! alone; against open sky on all four faces, which is the most it can ask
//! for, it roughly doubles the lighting and stays under the cost of generating
//! the column.

use crate::chunk::Chunk;
use crate::propagation::{raise, step_cost, Budget, LightGraph, OpacityModel, PropagationError};

/// Sky light for one column, backed by that column's own arrays.
#[derive(Debug)]
pub struct ColumnSkyLight<'a> {
    chunk: &'a mut Chunk,
    opacity: &'a OpacityModel,
}

impl<'a> ColumnSkyLight<'a> {
    pub fn new(chunk: &'a mut Chunk, opacity: &'a OpacityModel) -> Self {
        Self { chunk, opacity }
    }

    /// Fill this column's sky light from the sky down.
    ///
    /// Every cell above the highest motion-blocking block in its own x/z
    /// column ends at fifteen, and the walk carries what it can downwards and
    /// sideways from there. The heights come from the chunk's own heightmaps,
    /// which are recomputed from the blocks — so the lit region follows the
    /// terrain rather than a constant, and the day the terrain stops being flat
    /// nothing here changes.
    ///
    /// # Why the open cells are filled rather than seeded
    ///
    /// The obvious implementation hands every open cell to
    /// [`seed_skylight`](crate::propagation::seed_skylight) as a fifteen. That
    /// is correct, and on an overworld column it costs **8.2 ms in release**
    /// (`benches/skylight.rs`, which times generation apart so this number is
    /// the lighting alone). An overworld column is 16x16x384 and almost all of
    /// it is sky: ninety-odd thousand seeds, each entering a queue and each
    /// offering its level to six neighbours that already have it.
    ///
    /// Almost all of that work cannot change anything. A lit cell whose six
    /// neighbours are also lit has nothing to give any of them. Only a cell on
    /// the *boundary* of the open region — one with an in-volume neighbour the
    /// sky does not reach — can brighten anything, and there are two orders of
    /// magnitude fewer of those.
    ///
    /// So the open region is written to fifteen directly, which is a linear
    /// pass over an array, and the walk is seeded from the boundary alone.
    /// **0.57 ms** on the same column: fourteen times less, and small enough
    /// beside the 2.7 ms of generating the column that lighting has stopped
    /// being the thing worth optimising.
    ///
    /// The result is identical rather than approximately so, and
    /// `tests/column_light.rs` asserts that cell for cell against the
    /// whole-region seeding it replaces, on terrain built to be awkward — a
    /// staircase, a lid, and a hole punched through it. A faster answer that
    /// differs anywhere is not an optimisation.
    ///
    /// # Errors
    ///
    /// [`PropagationError::BudgetExhausted`] if the walk runs past `budget`.
    /// The partial result is consistent: only completed rewrites were made, so
    /// a column that ran out is under-lit rather than corrupt.
    pub fn seed(
        chunk: &'a mut Chunk,
        opacity: &'a OpacityModel,
        budget: Budget,
    ) -> Result<u64, PropagationError> {
        Self::seed_inner(chunk, opacity, None, budget)
    }

    /// Fill this column's sky light with the four columns around it as a
    /// boundary condition.
    ///
    /// This is what stops light from stopping at a chunk boundary. See the
    /// module note for what the skirt is exact for and where it still
    /// under-lights.
    ///
    /// # Errors
    ///
    /// As [`ColumnSkyLight::seed`].
    pub fn seed_with_neighbours(
        chunk: &'a mut Chunk,
        opacity: &'a OpacityModel,
        skirt: Skirt,
        budget: Budget,
    ) -> Result<u64, PropagationError> {
        Self::seed_inner(chunk, opacity, Some(skirt), budget)
    }

    fn seed_inner(
        chunk: &'a mut Chunk,
        opacity: &'a OpacityModel,
        skirt: Option<Skirt>,
        budget: Budget,
    ) -> Result<u64, PropagationError> {
        let min_y = chunk.world().min_y();
        let max_y = min_y + chunk.world().height() as i32;

        // Where the sky starts in each of the 256 x/z columns. Read once
        // rather than per cell: the heightmap lookup is cheap and the inner
        // loops below ask about a column's floor tens of thousands of times.
        let mut sky_floor = [[0i32; 16]; 16];
        for (x, row) in sky_floor.iter_mut().enumerate() {
            for (z, floor) in row.iter_mut().enumerate() {
                *floor = chunk
                    .heightmaps()
                    .get(SKY_HEIGHTMAP)
                    .first_available(x as u32, z as u32)
                    .clamp(min_y, max_y);
            }
        }
        let bounds = Bounds { min_y, max_y };
        let open = |x: i32, y: i32, z: i32| -> bool {
            bounds.contains(x, y, z) && y >= sky_floor[x as usize][z as usize]
        };

        let mut graph = Self::new(chunk, opacity);

        // The fill. Interior open cells reach fifteen here and are never
        // queued, because they have nothing to offer a neighbour that is
        // already there.
        let mut seeds: Vec<(i32, i32, i32, u8)> = Vec::new();
        for x in 0..16i32 {
            for z in 0..16i32 {
                for y in sky_floor[x as usize][z as usize]..max_y {
                    if boundary(&open, &bounds, x, y, z) {
                        // Left dark so the seed takes: `raise` writes and
                        // queues a seed only when it is *brighter* than what
                        // the cell holds, so a boundary cell pre-filled to
                        // fifteen would be skipped and nothing would spread.
                        seeds.push((x, y, z, 15));
                    } else {
                        graph.set_level(x, y, z, 15);
                    }
                }
            }
        }

        // What the four columns around this one shine in.
        //
        // The neighbour is not part of the volume: it is a *source*, and what
        // reaches the edge cell facing it is what a cell at fifteen would
        // offer across one step. That is `step_cost`, called rather than
        // rewritten: this expression and `spread`'s used to be two copies of
        // one rule, and the day the rule turned out to be wrong they were two
        // places to fix it.
        //
        // Modelling the neighbour as a source rather than as extra cells of
        // the volume is not only simpler. A read-only ring inside the volume
        // stores nothing, so it reads back dark however brightly it was just
        // handed light — and a walk that steps along such a ring re-queues
        // every cell of it on every visit, and finishes only by exhausting its
        // budget.
        if let Some(skirt) = skirt {
            for along in 0..16i32 {
                for (rx, rz, ix, iz) in [
                    (-1, along, 0, along),
                    (16, along, 15, along),
                    (along, -1, along, 0),
                    (along, 16, along, 15),
                ] {
                    for y in min_y..max_y {
                        // Only where the neighbour sees sky and this column
                        // does not. A cell already lit by its own sky has
                        // nothing to gain, and seeding it would be ninety
                        // thousand queue entries to find that out.
                        if !skirt.open_at(rx, y, rz) || open(ix, y, iz) {
                            continue;
                        }
                        let offered = 15_u8.saturating_sub(step_cost(graph.opacity(ix, y, iz)));
                        if offered > 0 {
                            seeds.push((ix, y, iz, offered));
                        }
                    }
                }
            }
        }

        raise(&mut graph, &seeds, budget)
    }
}

/// Where the sky reaches down to, in each of a column's 256 x/z positions.
///
/// The lowest y with nothing motion-blocking above it, clamped into the
/// world. Two things use it: the column's own fill, and — as a
/// [`Skirt`] — the ring of cells just outside a *neighbouring* column, which
/// is how light crosses a chunk boundary without carrying a megabyte of
/// blocks across it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkyFloor {
    floors: [[i32; 16]; 16],
}

impl SkyFloor {
    /// Read a column's sky floors out of its heightmaps.
    ///
    /// From [`HeightmapKind::MotionBlocking`](crate::heightmap::HeightmapKind)
    /// rather than the surface map, for the same reason the fill uses it: a
    /// cell under a leaf block is not in direct sky light in vanilla either.
    #[must_use]
    pub fn of(chunk: &Chunk) -> Self {
        let min_y = chunk.world().min_y();
        let max_y = min_y + chunk.world().height() as i32;
        let mut floors = [[0i32; 16]; 16];
        for (x, row) in floors.iter_mut().enumerate() {
            for (z, floor) in row.iter_mut().enumerate() {
                *floor = chunk
                    .heightmaps()
                    .get(SKY_HEIGHTMAP)
                    .first_available(x as u32, z as u32)
                    .clamp(min_y, max_y);
            }
        }
        Self { floors }
    }

    /// A column with no terrain at all: open sky to the world's floor.
    ///
    /// What an absent neighbour is treated as. It is the right default and not
    /// a convenient one: a column that has not been generated is not a wall,
    /// and treating it as one would put the seam back exactly where the skirt
    /// removes it — at the edge of what a player has explored.
    #[must_use]
    pub fn open(min_y: i32) -> Self {
        Self {
            floors: [[min_y; 16]; 16],
        }
    }

    /// Whether the sky reaches this cell of the column.
    #[must_use]
    pub fn open_at(&self, x: u32, y: i32, z: u32) -> bool {
        y >= self.floors[x as usize][z as usize]
    }
}

/// The four columns around one, as a boundary condition for its light.
///
/// Four and not eight. A diagonal neighbour touches the column only along an
/// edge, and light steps face to face — a cell at (0, y, 0) has no neighbour
/// at (-1, y, -1), so a corner column has nothing to contribute that its two
/// shared sides do not already carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Skirt {
    /// The column at x - 1.
    pub west: SkyFloor,
    /// The column at x + 1.
    pub east: SkyFloor,
    /// The column at z - 1.
    pub north: SkyFloor,
    /// The column at z + 1.
    pub south: SkyFloor,
}

impl Skirt {
    /// A skirt of columns with no terrain — open sky on every side.
    #[must_use]
    pub fn open(min_y: i32) -> Self {
        let open = SkyFloor::open(min_y);
        Self {
            west: open,
            east: open,
            north: open,
            south: open,
        }
    }

    /// Whether the sky reaches a cell just outside the column, given in the
    /// *column's* coordinates.
    ///
    /// `x` or `z` is -1 or 16; the other is inside `0..16`. A cell outside on
    /// both axes is a corner, which touches the column along an edge and never
    /// face to face — see the type's note — so this answers `false` for one
    /// rather than reaching for a column it was not given.
    #[must_use]
    fn open_at(&self, x: i32, y: i32, z: i32) -> bool {
        let inside = |v: i32| (0..16).contains(&v);
        match (x, z) {
            (-1, z) if inside(z) => self.west.open_at(15, y, z as u32),
            (16, z) if inside(z) => self.east.open_at(0, y, z as u32),
            (x, -1) if inside(x) => self.north.open_at(x as u32, y, 15),
            (x, 16) if inside(x) => self.south.open_at(x as u32, y, 0),
            _ => false,
        }
    }
}

/// Whether an open cell has an in-volume neighbour the sky does not reach.
///
/// Cells outside the column do not count, and that is the whole subtlety. A
/// neighbour beyond x = 15, or below the world's floor, is outside the
/// *volume* rather than in shadow: the walk stops at the boundary instead of
/// stepping there, so a cell at the edge has nothing to light and seeding it
/// would spend a queue entry to find that out.
fn boundary(
    open: &impl Fn(i32, i32, i32) -> bool,
    bounds: &Bounds,
    x: i32,
    y: i32,
    z: i32,
) -> bool {
    const NEIGHBOURS: [(i32, i32, i32); 6] = [
        (1, 0, 0),
        (-1, 0, 0),
        (0, 1, 0),
        (0, -1, 0),
        (0, 0, 1),
        (0, 0, -1),
    ];
    NEIGHBOURS.iter().any(|(dx, dy, dz)| {
        let (nx, ny, nz) = (x + dx, y + dy, z + dz);
        bounds.contains(nx, ny, nz) && !open(nx, ny, nz)
    })
}

/// The column's extent, kept separately from "is it open" because a buried
/// cell and a cell outside the world answer those two questions differently
/// and the walk treats them differently too.
struct Bounds {
    min_y: i32,
    max_y: i32,
}

impl Bounds {
    fn contains(&self, x: i32, y: i32, z: i32) -> bool {
        (0..16).contains(&x) && (0..16).contains(&z) && (self.min_y..self.max_y).contains(&y)
    }
}

/// Which heightmap says where the sky starts.
///
/// `MotionBlocking` rather than `WorldSurface`: a cell under a leaf block is
/// not in direct sky light in vanilla either, and using the surface map would
/// light the inside of a tree.
const SKY_HEIGHTMAP: crate::heightmap::HeightmapKind =
    crate::heightmap::HeightmapKind::MotionBlocking;

impl LightGraph for ColumnSkyLight<'_> {
    fn level(&self, x: i32, y: i32, z: i32) -> u8 {
        let (_, local_y) = self.split(y);
        self.chunk
            .section(y)
            .sky_light()
            .get(x as u32, local_y, z as u32)
    }

    fn set_level(&mut self, x: i32, y: i32, z: i32, level: u8) {
        let (_, local_y) = self.split(y);
        self.chunk
            .section_mut(y)
            .sky_light_mut()
            .set(x as u32, local_y, z as u32, level);
    }

    fn opacity(&self, x: i32, y: i32, z: i32) -> u8 {
        self.opacity
            .opacity(self.chunk.get_block(x as u32, y, z as u32))
    }

    fn contains(&self, x: i32, y: i32, z: i32) -> bool {
        let min_y = self.chunk.world().min_y();
        let max_y = min_y + self.chunk.world().height() as i32;
        (0..16).contains(&x) && (0..16).contains(&z) && (min_y..max_y).contains(&y)
    }
}

impl ColumnSkyLight<'_> {
    /// The row a world y occupies inside its own section.
    ///
    /// Only ever called for cells `contains` accepted, which is what makes the
    /// arithmetic total. The section itself is found by `Chunk::section`,
    /// which does the same division — asking it rather than indexing keeps one
    /// answer to "which section is this y in" instead of two.
    fn split(&self, y: i32) -> ((), u32) {
        let min_y = self.chunk.world().min_y();
        ((), (y - min_y) as u32 % 16)
    }
}

/// Block light for one column, from the blocks in it that give light off.
///
/// # The other half of lighting
///
/// Sky light comes from above and is a property of where the terrain stops.
/// Block light comes from *cells* — a torch, lava, glowstone — and is a
/// property of what is in them. Same walk, same attenuation, different seeds,
/// and [`propagation::raise`](crate::propagation::raise) does not know or care
/// which it is running.
///
/// # What this does not do, and it shows
///
/// One column, so **a torch on the far side of a chunk boundary lights nothing
/// here**. That is the same gap the sky-light skirt exists to close, and it is
/// worse-looking for block light: sky light's seam is a shade across a cliff
/// face, and this one is a hard edge at a chunk border with a lit room on one
/// side of it.
///
/// It is left that way on purpose rather than patched. The sky-light skirt
/// works because a neighbour's *sky floor* is a complete description of what
/// that neighbour shines in — the light there is fifteen by definition. A
/// neighbour's emitters are not: what reaches the shared face depends on what
/// the light travelled through to get there, and seeding the boundary with
/// `emission - distance` would **over-light**, which is the one kind of wrong
/// this project's light harness treats as unexplained. The honest version is
/// the wider volume, which is measured in the harness and costed in decision
/// record 0010.
#[derive(Debug)]
pub struct ColumnBlockLight<'a> {
    chunk: &'a mut Chunk,
    opacity: &'a OpacityModel,
}

impl<'a> ColumnBlockLight<'a> {
    /// Fill this column's block light from the emitters in it.
    ///
    /// Returns the edge examinations spent, which is zero for a column with
    /// nothing in it that emits — most columns of most worlds.
    ///
    /// The arrays are cleared first. A column is lit from nothing every time
    /// rather than corrected, because a correction needs to know what changed
    /// and the caller here is "a chunk arrived from disk"; the incremental
    /// pair — [`raise`] and `darken` — is what an edit will use.
    ///
    /// # Errors
    ///
    /// [`PropagationError::BudgetExhausted`] if the walk runs past `budget`.
    /// The partial result is consistent: the column is under-lit rather than
    /// corrupt.
    pub fn seed(
        chunk: &'a mut Chunk,
        opacity: &'a OpacityModel,
        emission: &crate::propagation::EmissionModel,
        budget: Budget,
    ) -> Result<u64, PropagationError> {
        for section in chunk.sections_mut() {
            *section.block_light_mut() = crate::light::LightArray::new();
        }
        if emission.is_dark() {
            return Ok(0);
        }

        let min_y = chunk.world().min_y();
        let mut seeds: Vec<(i32, i32, i32, u8)> = Vec::new();
        for (index, section) in chunk.sections().iter().enumerate() {
            // The palette is the shortlist of what this section can hold, so a
            // section whose palette holds no emitter has no emitter and its
            // 4,096 cells are never read. A direct palette answers `None` —
            // any registry id is possible — and is scanned.
            if let Some(entries) = section.states().palette().entries() {
                if !emission.any_emits(entries.iter().copied()) {
                    continue;
                }
            }
            let base = min_y + (index as i32) * 16;
            for y in 0..16i32 {
                for z in 0..16i32 {
                    for x in 0..16i32 {
                        let state = section.states().get_at(x as u32, y as u32, z as u32);
                        let level = emission.emission(state);
                        if level > 0 {
                            seeds.push((x, base + y, z, level));
                        }
                    }
                }
            }
        }
        if seeds.is_empty() {
            return Ok(0);
        }

        let mut graph = Self { chunk, opacity };
        raise(&mut graph, &seeds, budget)
    }
}

impl LightGraph for ColumnBlockLight<'_> {
    fn level(&self, x: i32, y: i32, z: i32) -> u8 {
        let row = self.row(y);
        self.chunk
            .section(y)
            .block_light()
            .get(x as u32, row, z as u32)
    }

    fn set_level(&mut self, x: i32, y: i32, z: i32, level: u8) {
        let row = self.row(y);
        self.chunk
            .section_mut(y)
            .block_light_mut()
            .set(x as u32, row, z as u32, level);
    }

    fn opacity(&self, x: i32, y: i32, z: i32) -> u8 {
        self.opacity
            .opacity(self.chunk.get_block(x as u32, y, z as u32))
    }

    fn contains(&self, x: i32, y: i32, z: i32) -> bool {
        let min_y = self.chunk.world().min_y();
        let max_y = min_y + self.chunk.world().height() as i32;
        (0..16).contains(&x) && (0..16).contains(&z) && (min_y..max_y).contains(&y)
    }
}

impl ColumnBlockLight<'_> {
    /// The row a world y occupies inside its own section.
    ///
    /// The same arithmetic [`ColumnSkyLight`] does, and separate from it
    /// because the two types differ only in which array they reach for —
    /// sharing a graph between them would mean a parameter saying which, in
    /// the hottest loop either of them has, to save four lines.
    fn row(&self, y: i32) -> u32 {
        (y - self.chunk.world().min_y()) as u32 % 16
    }
}
