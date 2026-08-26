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
use crate::propagation::{seed_skylight, Budget, DefaultOpacity, LightGraph, PropagationError};

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
    /// Every cell above the highest block in its own x/z column starts at
    /// fifteen and the walk carries what it can downwards and sideways. The
    /// heights come from the chunk's own heightmaps, which are recomputed from
    /// the blocks — so the seeds follow the terrain rather than a constant, and
    /// the day the terrain stops being flat nothing here changes.
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

        // One seed range per x/z: from just above the highest solid block to
        // the top of the world. `first_available` is the heightmap's own answer
        // to "where does the sky start", which is why this does not walk the
        // blocks a second time to find out.
        let mut columns = Vec::with_capacity(16 * 16);
        for x in 0..16u32 {
            for z in 0..16u32 {
                let open_from = chunk.heightmaps().get(SKY_HEIGHTMAP).first_available(x, z);
                columns.push((x as i32, z as i32, open_from.max(min_y)..max_y));
            }
        }

        let mut graph = Self::new(chunk, opacity);
        seed_skylight(&mut graph, columns, budget)
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
