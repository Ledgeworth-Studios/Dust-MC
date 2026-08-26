//! A [`LightGraph`] over one chunk column.
//!
//! `propagation` is written against a trait rather than against chunks,
//! because who owns which block — and therefore what attenuates light — is
//! registry knowledge that crate does not have. This is the wiring that trait
//! was left for: a column's sky-light arrays, plus an opacity model the caller
//! supplies.
//!
//! # What "one column" costs, and why it is still the right first wiring
//!
//! A column knows nothing about its neighbours, so light does not cross a
//! chunk boundary here: a cell at x = 15 is at the edge of the volume and the
//! walk stops rather than stepping into the column next door. That is visible
//! as a seam wherever terrain heights differ across a boundary, and it is not
//! how a finished light engine behaves.
//!
//! It is still worth having, because the alternative — every chunk sent fully
//! lit — is not a smaller lie, it is a total one. Sky light that follows the
//! terrain is right everywhere except at the boundaries; sky light that is
//! fifteen everywhere is right nowhere, and a cave lit like a meadow is a bug
//! nobody can see until they walk into it. The multi-column version is the
//! same walks over a graph whose `contains` spans more than one chunk, which
//! is exactly the shape the trait was given for.

use crate::chunk::Chunk;
use crate::propagation::{raise, Budget, DefaultOpacity, LightGraph, PropagationError};

/// Sky light for one column, backed by that column's own arrays.
#[derive(Debug)]
pub struct ColumnSkyLight<'a> {
    chunk: &'a mut Chunk,
    opacity: &'a DefaultOpacity,
}

impl<'a> ColumnSkyLight<'a> {
    pub fn new(chunk: &'a mut Chunk, opacity: &'a DefaultOpacity) -> Self {
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
        opacity: &'a DefaultOpacity,
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

        raise(&mut graph, &seeds, budget)
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
