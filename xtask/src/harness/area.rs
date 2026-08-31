//! Sky light over a square of columns, lit as one volume.
//!
//! # What this is for
//!
//! Dust lights one chunk column at a time. Its four neighbours enter the walk
//! as a **skirt** — their sky floors used as sources along the four faces —
//! which is exact where a neighbour is open to the sky and under-lights where
//! the light would have to travel *through* one, around the mouth of a cave
//! three blocks into the next chunk. `dust_world::column_light` says so in its
//! own module note, and says the propagation trait was given `contains`
//! precisely so the wider version is a bigger volume rather than a rewrite.
//!
//! This is that bigger volume, and it lives in the harness rather than in the
//! engine **on purpose**. With Minecraft's own opacity in force the whole
//! remaining sky-light shortfall is 611 cells of 2.4 million on seed 0 and
//! nothing at all on seed 1. Whether closing it is worth reading nine columns
//! to serve one is a question with a number in it, and building the production
//! version before taking that number is the mistake decision record 0008 spent
//! two months making.
//!
//! So: the same walk, the same opacity, the same chunks, over a `(2k+1)²`
//! block of columns with only the centre one read back. What it buys shows up
//! beside the one-column answer in the same report.
//!
//! # Why it under-lights at its own edge, and why that does not matter here
//!
//! This volume has a boundary too — the outer ring of the block, where light
//! stops for exactly the reason a single column's edge does. The centre column
//! is `k` chunks away from it, and light crossing sixteen blocks of anything
//! has lost fifteen levels and stopped. That is the whole argument for a finite
//! `k` and it is worth stating rather than assuming: at `k = 1` the nearest
//! boundary is sixteen blocks from the nearest cell of the centre column, which
//! is one more than a level can travel.

use dust_world::chunk::Chunk;
use dust_world::propagation::{raise, Budget, LightGraph, OpacityModel, PropagationError};

/// Which heightmap says where the sky starts.
///
/// The same one `dust_world::column_light` uses, and it has to be: a
/// measurement that lit against a different sky floor would be measuring the
/// heightmap choice rather than the volume.
const SKY_HEIGHTMAP: dust_world::heightmap::HeightmapKind =
    dust_world::heightmap::HeightmapKind::MotionBlocking;

/// A square block of columns, lit together.
///
/// `chunks` is row-major in chunk coordinates: index `cx + cz * side`. Cell
/// coordinates run `0..16 * side` in x and z, and world y throughout.
pub struct AreaSkyLight<'a> {
    chunks: &'a mut [Chunk],
    side: i32,
    opacity: &'a OpacityModel,
    min_y: i32,
    max_y: i32,
}

impl std::fmt::Debug for AreaSkyLight<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AreaSkyLight")
            .field("side", &self.side)
            .finish_non_exhaustive()
    }
}

impl<'a> AreaSkyLight<'a> {
    /// Light every column of the block from the sky down.
    ///
    /// The same two-part seeding `ColumnSkyLight` does and for the same
    /// reason: the open region is written to fifteen directly because a lit
    /// cell whose six neighbours are also lit has nothing to give any of them,
    /// and only the boundary of the open region can brighten anything.
    ///
    /// # Errors
    ///
    /// [`PropagationError::BudgetExhausted`] if the walk runs past `budget`.
    pub fn seed(
        chunks: &'a mut [Chunk],
        side: i32,
        opacity: &'a OpacityModel,
        budget: Budget,
    ) -> Result<u64, PropagationError> {
        assert_eq!(
            chunks.len(),
            (side * side) as usize,
            "a square block of columns"
        );
        let min_y = chunks[0].world().min_y();
        let max_y = min_y + chunks[0].world().height() as i32;
        let span = 16 * side;

        // Every x/z column's sky floor, across the whole block. Read once:
        // the loops below ask about a column's floor tens of thousands of
        // times and the heightmap lookup is not free.
        let mut sky_floor = vec![0i32; (span * span) as usize];
        for x in 0..span {
            for z in 0..span {
                let chunk = &chunks[(x / 16 + (z / 16) * side) as usize];
                sky_floor[(x + z * span) as usize] = chunk
                    .heightmaps()
                    .get(SKY_HEIGHTMAP)
                    .first_available((x % 16) as u32, (z % 16) as u32)
                    .clamp(min_y, max_y);
            }
        }

        let inside = |x: i32, y: i32, z: i32| -> bool {
            (0..span).contains(&x) && (0..span).contains(&z) && (min_y..max_y).contains(&y)
        };
        let open = |x: i32, y: i32, z: i32| -> bool {
            inside(x, y, z) && y >= sky_floor[(x + z * span) as usize]
        };
        let boundary = |x: i32, y: i32, z: i32| -> bool {
            [
                (1, 0, 0),
                (-1, 0, 0),
                (0, 1, 0),
                (0, -1, 0),
                (0, 0, 1),
                (0, 0, -1),
            ]
            .iter()
            .any(|(dx, dy, dz)| {
                let (nx, ny, nz) = (x + dx, y + dy, z + dz);
                inside(nx, ny, nz) && !open(nx, ny, nz)
            })
        };

        let mut seeds: Vec<(i32, i32, i32, u8)> = Vec::new();
        let mut graph = Self {
            chunks,
            side,
            opacity,
            min_y,
            max_y,
        };
        for x in 0..span {
            for z in 0..span {
                for y in sky_floor[(x + z * span) as usize]..max_y {
                    if boundary(x, y, z) {
                        // Left dark so the seed takes: `raise` writes only
                        // what is brighter than what is already there.
                        seeds.push((x, y, z, 15));
                    } else {
                        graph.set_level(x, y, z, 15);
                    }
                }
            }
        }
        raise(&mut graph, &seeds, budget)
    }

    /// Which chunk of the block a cell belongs to, and where in it.
    fn locate(&self, x: i32, z: i32) -> (usize, u32, u32) {
        (
            (x / 16 + (z / 16) * self.side) as usize,
            (x % 16) as u32,
            (z % 16) as u32,
        )
    }
}

impl LightGraph for AreaSkyLight<'_> {
    fn level(&self, x: i32, y: i32, z: i32) -> u8 {
        let (chunk, lx, lz) = self.locate(x, z);
        let row = (y - self.min_y) as u32 % 16;
        self.chunks[chunk].section(y).sky_light().get(lx, row, lz)
    }

    fn set_level(&mut self, x: i32, y: i32, z: i32, level: u8) {
        let (chunk, lx, lz) = self.locate(x, z);
        let row = (y - self.min_y) as u32 % 16;
        self.chunks[chunk]
            .section_mut(y)
            .sky_light_mut()
            .set(lx, row, lz, level);
    }

    fn opacity(&self, x: i32, y: i32, z: i32) -> u8 {
        let (chunk, lx, lz) = self.locate(x, z);
        self.opacity
            .opacity(self.chunks[chunk].get_block(lx, y, lz))
    }

    fn contains(&self, x: i32, y: i32, z: i32) -> bool {
        let span = 16 * self.side;
        (0..span).contains(&x) && (0..span).contains(&z) && (self.min_y..self.max_y).contains(&y)
    }
}
