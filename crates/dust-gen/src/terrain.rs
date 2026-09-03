//! Terrain: turning `final_density` into the rock and water a column is made
//! of.
//!
//! One function decides everything about a column's shape. `final_density` is
//! positive where the world is the dimension's default block and not positive
//! where it is air or fluid, and every mountain, overhang, sea floor and
//! noise cave in the overworld is that one sign change.
//!
//! # The lattice, which is the terrain and not an approximation of it
//!
//! Minecraft does not evaluate that function at every block. It evaluates it
//! at the corners of a cell — four blocks wide and eight tall in the
//! overworld, both read from the dimension's own settings — and lerps
//! trilinearly inside. Evaluating at every block instead would be more
//! samples of the same noise and **a different world**: smoother, without the
//! flat shelves and the straight cliff faces a player recognises. So the
//! lattice is reproduced exactly, corner for corner, and the saving is a
//! consequence rather than the point — a chunk takes about six thousand
//! corner samples where it holds ninety-eight thousand blocks.
//!
//! # What is not here yet
//!
//! Surface rules, which put grass on the stone; aquifers, which decide what
//! fluid an enclosed pocket holds; carvers; and features. This is the output
//! of vanilla's own noise stage and nothing beyond it — the default block, the
//! default fluid below the sea level the settings name, and air. Decision
//! record 0012 orders those and this is stage two of it.

use std::path::Path;

use crate::noise::build::{router, BuildError, NoiseSettings, Router};
use crate::noise::density::Evaluator;

/// What the noise stage puts in a cell.
///
/// Three answers and not a block state, because which block state each one is
/// belongs to the caller's registry and not to a generator. A `u8` because a
/// chunk's worth is ninety-six kibibytes of scratch and it is reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Material {
    Air = 0,
    /// The dimension's `default_block`.
    Solid = 1,
    /// The dimension's `default_fluid`, below the level the settings name.
    Fluid = 2,
}

impl Material {
    pub fn from_code(code: u8) -> Self {
        match code {
            1 => Self::Solid,
            2 => Self::Fluid,
            _ => Self::Air,
        }
    }
}

/// A dimension's terrain, compiled for one seed.
///
/// Shared and immutable: two threads generating two chunks share every noise
/// table and every node, and hold nothing between them but a [`Filler`] each.
#[derive(Debug, Clone)]
pub struct Terrain {
    router: Router,
    cell_width: usize,
    cell_height: usize,
    /// Cells across a chunk, which is 16 divided by the cell width.
    cells_xz: usize,
    /// Cells up the world.
    cells_y: usize,
    /// The cell index the world's floor is in.
    cell_min_y: i32,
}

impl Terrain {
    pub fn new(data_root: &Path, dimension: &str, seed: i64) -> Result<Self, BuildError> {
        Self::from_router(router(data_root, dimension, seed)?)
    }

    /// Build one over a router that has already been compiled, so a caller
    /// that also wants the climate half pays for one graph rather than two.
    pub fn from_router(router: Router) -> Result<Self, BuildError> {
        let settings = &router.settings;
        let cell_width = settings.cell_width;
        let cell_height = settings.cell_height;
        if cell_width <= 0 || cell_height <= 0 || 16 % cell_width != 0 {
            return Err(BuildError::Malformed {
                path: std::path::PathBuf::from(""),
                detail: format!(
                    "a cell {cell_width} wide and {cell_height} tall does not tile a chunk"
                ),
            });
        }
        if settings.height <= 0 || settings.height % cell_height != 0 {
            return Err(BuildError::Malformed {
                path: std::path::PathBuf::from(""),
                detail: format!(
                    "a world {} tall is not a whole number of {cell_height}-block cells",
                    settings.height
                ),
            });
        }
        Ok(Self {
            cell_width: cell_width as usize,
            cell_height: cell_height as usize,
            cells_xz: (16 / cell_width) as usize,
            cells_y: (settings.height / cell_height) as usize,
            cell_min_y: settings.min_y.div_euclid(cell_height),
            router,
        })
    }

    pub fn settings(&self) -> &NoiseSettings {
        &self.router.settings
    }

    pub fn router(&self) -> &Router {
        &self.router
    }

    /// How many cells a [`Filler`]'s output holds, which is 256 per world row.
    pub fn cells_per_chunk(&self) -> usize {
        256 * self.router.settings.height as usize
    }

    /// One thread's scratch space.
    pub fn filler(&self) -> Filler<'_> {
        let corners = (self.cells_xz + 1) * (self.cells_y + 1);
        let slots = self.router.graph.interpolated.len();
        Filler {
            terrain: self,
            evaluator: Evaluator::new(&self.router.graph),
            slice0: vec![0.0; corners * slots],
            slice1: vec![0.0; corners * slots],
            corners,
            lerp: vec![Cell::default(); slots],
            skipped: 0,
            walked: 0,
        }
    }
}

/// The eight corners of one cell for one interpolated node, and the three
/// lerps that walk in from them.
#[derive(Debug, Clone, Copy, Default)]
struct Cell {
    corner: [f64; 8],
    xz00: f64,
    xz10: f64,
    xz01: f64,
    xz11: f64,
    z0: f64,
    z1: f64,
}

/// One thread's view of a [`Terrain`].
#[derive(Debug, Clone)]
pub struct Filler<'a> {
    terrain: &'a Terrain,
    evaluator: Evaluator<'a>,
    /// The interpolated nodes' values at the corners on this cell column's
    /// low-x face, then the high-x face. One block of `corners` per slot.
    slice0: Vec<f64>,
    slice1: Vec<f64>,
    corners: usize,
    lerp: Vec<Cell>,
    /// Cells the bounds walk answered whole, and cells it did not. Counted
    /// rather than assumed: a skip that never fires is a slower generator with
    /// more code in it.
    skipped: u64,
    walked: u64,
}

impl Filler<'_> {
    /// Fill one chunk's worth of materials.
    ///
    /// `out` is `256 * height` codes, indexed `(y - min_y) * 256 + z * 16 + x`,
    /// and is written in full. It is a caller's buffer rather than a return
    /// value because a server generating chunks forever should allocate this
    /// once per thread, not once per chunk.
    pub fn fill(&mut self, chunk_x: i32, chunk_z: i32, out: &mut [u8]) {
        self.fill_inner(chunk_x, chunk_z, out, true);
    }

    /// The same fill with the whole-cell skip switched off: every block's
    /// density evaluated, none of it inferred.
    ///
    /// This is the control the skip is checked against, and it is public
    /// because a check that lives only inside the thing it checks is not a
    /// check. The two must agree byte for byte on every chunk.
    pub fn fill_without_skipping(&mut self, chunk_x: i32, chunk_z: i32, out: &mut [u8]) {
        self.fill_inner(chunk_x, chunk_z, out, false);
    }

    /// Cells answered by the bounds walk, and cells walked block by block.
    pub fn cells(&self) -> (u64, u64) {
        (self.skipped, self.walked)
    }

    fn fill_inner(&mut self, chunk_x: i32, chunk_z: i32, out: &mut [u8], skip: bool) {
        let terrain = self.terrain;
        let settings = &terrain.router.settings;
        assert_eq!(
            out.len(),
            terrain.cells_per_chunk(),
            "the material buffer is one chunk of the world's own height"
        );
        let width = terrain.cell_width as i32;
        let height = terrain.cell_height as i32;
        let base_x = chunk_x * 16;
        let base_z = chunk_z * 16;
        let min_y = settings.min_y;
        let sea_level = settings.sea_level;
        let final_density = terrain.router.final_density;

        self.fill_slice(base_x, base_z, true);
        for cell_x in 0..terrain.cells_xz {
            self.fill_slice(base_x + (cell_x as i32 + 1) * width, base_z, false);
            for cell_z in 0..terrain.cells_xz {
                for cell_y in 0..terrain.cells_y {
                    self.select(cell_y, cell_z);
                    let cell_base_y = (terrain.cell_min_y + cell_y as i32) * height;
                    // The whole cell at once, when the interval says it can be.
                    // A trilinear interpolation never leaves the interval its
                    // eight corners span, so a cell whose `final_density` can
                    // not reach zero holds no rock anywhere and the hundred and
                    // twenty-eight blocks in it are decided by their y alone.
                    // This cannot change an answer, and above a mountain it is
                    // most of the column.
                    if skip {
                        let corners = &self.lerp;
                        self.evaluator.enter_cell(|slot| corners[slot].corner);
                        let (low, high) = self.evaluator.cell_bounds(final_density);
                        if high <= 0.0 || low > 0.0 {
                            self.skipped += 1;
                            let solid = low > 0.0;
                            let lx = cell_x * terrain.cell_width;
                            let lz = cell_z * terrain.cell_width;
                            for step_y in 0..terrain.cell_height {
                                let block_y = cell_base_y + step_y as i32;
                                let material = if solid {
                                    Material::Solid
                                } else if block_y < sea_level {
                                    Material::Fluid
                                } else {
                                    Material::Air
                                };
                                let row = (block_y - min_y) as usize * 256;
                                for step_z in 0..terrain.cell_width {
                                    let line = row + (lz + step_z) * 16 + lx;
                                    out[line..line + terrain.cell_width].fill(material as u8);
                                }
                            }
                            continue;
                        }
                    }
                    self.walked += 1;
                    for step_y in 0..terrain.cell_height {
                        let block_y = (terrain.cell_min_y + cell_y as i32) * height + step_y as i32;
                        self.update_y(step_y as f64 / f64::from(height));
                        for step_x in 0..terrain.cell_width {
                            let block_x = base_x + (cell_x * terrain.cell_width + step_x) as i32;
                            self.update_x(step_x as f64 / f64::from(width));
                            for step_z in 0..terrain.cell_width {
                                let block_z =
                                    base_z + (cell_z * terrain.cell_width + step_z) as i32;
                                self.update_z(step_z as f64 / f64::from(width));
                                let density = self.evaluator.compute(
                                    final_density,
                                    block_x,
                                    block_y,
                                    block_z,
                                );
                                // The sign is the whole terrain. Below the
                                // level the settings name, what is not rock is
                                // the default fluid — and the level names the
                                // surface the fluid reaches *to*, so the top
                                // water block is the one below it. Decision
                                // record 0012 measured that off-by-one on
                                // every ocean column of seed 1 before anything
                                // was written.
                                let material = if density > 0.0 {
                                    Material::Solid
                                } else if block_y < sea_level {
                                    Material::Fluid
                                } else {
                                    Material::Air
                                };
                                let row = (block_y - min_y) as usize;
                                out[row * 256
                                    + (block_z - base_z) as usize * 16
                                    + (block_x - base_x) as usize] = material as u8;
                            }
                        }
                    }
                }
            }
            std::mem::swap(&mut self.slice0, &mut self.slice1);
        }
    }

    /// Sample every interpolated node at one face of the cell column.
    fn fill_slice(&mut self, block_x: i32, base_z: i32, low: bool) {
        let terrain = self.terrain;
        let width = terrain.cell_width as i32;
        let height = terrain.cell_height as i32;
        let rows = terrain.cells_y + 1;
        for slot in 0..self.lerp.len() {
            for z in 0..=terrain.cells_xz {
                let block_z = base_z + z as i32 * width;
                for y in 0..rows {
                    let block_y = (terrain.cell_min_y + y as i32) * height;
                    let value = self.evaluator.corner(slot, block_x, block_y, block_z);
                    let at = slot * self.corners + z * rows + y;
                    if low {
                        self.slice0[at] = value;
                    } else {
                        self.slice1[at] = value;
                    }
                }
            }
        }
    }

    fn select(&mut self, cell_y: usize, cell_z: usize) {
        let rows = self.terrain.cells_y + 1;
        for (slot, cell) in self.lerp.iter_mut().enumerate() {
            let base = slot * self.corners;
            let low = base + cell_z * rows + cell_y;
            let high = base + (cell_z + 1) * rows + cell_y;
            cell.corner = [
                self.slice0[low],
                self.slice0[low + 1],
                self.slice0[high],
                self.slice0[high + 1],
                self.slice1[low],
                self.slice1[low + 1],
                self.slice1[high],
                self.slice1[high + 1],
            ];
        }
    }

    fn update_y(&mut self, delta: f64) {
        for cell in &mut self.lerp {
            cell.xz00 = lerp(delta, cell.corner[0], cell.corner[1]);
            cell.xz10 = lerp(delta, cell.corner[4], cell.corner[5]);
            cell.xz01 = lerp(delta, cell.corner[2], cell.corner[3]);
            cell.xz11 = lerp(delta, cell.corner[6], cell.corner[7]);
        }
    }

    fn update_x(&mut self, delta: f64) {
        for cell in &mut self.lerp {
            cell.z0 = lerp(delta, cell.xz00, cell.xz10);
            cell.z1 = lerp(delta, cell.xz01, cell.xz11);
        }
    }

    fn update_z(&mut self, delta: f64) {
        for (slot, cell) in self.lerp.iter().enumerate() {
            self.evaluator
                .set_interpolated(slot, lerp(delta, cell.z0, cell.z1));
        }
    }
}

fn lerp(delta: f64, start: f64, end: f64) -> f64 {
    start + delta * (end - start)
}

/// A dimension's whole generator: one graph, the terrain read off it and the
/// biomes read off it.
///
/// One object because it is one graph. `shift_x` is under five of the six
/// climate functions *and* under the offset spline a mountain's height comes
/// from, and `minecraft:temperature` is sampled by the biome search and by the
/// surface the next stage will write. Built as a biome source beside a terrain
/// they would be two of every noise table, two permutation arrays each, and
/// every shared node sampled twice per column.
#[derive(Debug, Clone)]
pub struct Generator {
    terrain: Terrain,
    parameters: crate::biome::BiomeParameters,
}

impl Generator {
    pub fn new(
        data_root: &Path,
        dimension: &str,
        seed: i64,
        parameters: crate::biome::BiomeParameters,
    ) -> Result<Self, BuildError> {
        Ok(Self {
            terrain: Terrain::new(data_root, dimension, seed)?,
            parameters,
        })
    }

    pub fn terrain(&self) -> &Terrain {
        &self.terrain
    }

    pub fn settings(&self) -> &NoiseSettings {
        self.terrain.settings()
    }

    pub fn parameters(&self) -> &crate::biome::BiomeParameters {
        &self.parameters
    }

    pub fn parameters_mut(&mut self) -> &mut crate::biome::BiomeParameters {
        &mut self.parameters
    }

    /// One thread's scratch: a terrain filler and a biome sampler over the one
    /// graph.
    pub fn columns(&self) -> Columns<'_> {
        let router = self.terrain.router();
        Columns {
            filler: self.terrain.filler(),
            biomes: crate::biome::Sampler::over(&router.graph, router.climate, &self.parameters),
            materials: vec![0u8; self.terrain.cells_per_chunk()],
        }
    }
}

/// The scratch one thread needs to generate columns: two evaluators over one
/// shared graph, and the chunk-sized material buffer they fill.
#[derive(Debug, Clone)]
pub struct Columns<'a> {
    filler: Filler<'a>,
    biomes: crate::biome::Sampler<'a>,
    materials: Vec<u8>,
}

impl<'a> Columns<'a> {
    /// Generate one chunk's materials and hand back the buffer holding them.
    pub fn terrain(&mut self, chunk_x: i32, chunk_z: i32) -> &[u8] {
        self.filler.fill(chunk_x, chunk_z, &mut self.materials);
        &self.materials
    }

    pub fn biomes(&mut self) -> &mut crate::biome::Sampler<'a> {
        &mut self.biomes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("dust-gen-terrain-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write(
            &root,
            "minecraft/worldgen/noise/scratch.json",
            r#"{"firstOctave": -5, "amplitudes": [1.0, 1.0, 1.0]}"#,
        );
        root
    }

    fn write(root: &Path, relative: &str, text: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().expect("has a parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    }

    /// A settings file with `body` as its whole `final_density`.
    fn dimension(root: &Path, name: &str, body: &str) {
        write(
            root,
            &format!("minecraft/worldgen/noise_settings/{name}.json"),
            &format!(
                r#"{{"noise": {{"height": 384, "min_y": -64,
                                "size_horizontal": 1, "size_vertical": 2}},
                    "sea_level": 63,
                    "default_block": {{"Name": "minecraft:stone"}},
                    "default_fluid": {{"Name": "minecraft:water"}},
                    "noise_router": {{"temperature": 0.0, "vegetation": 0.0,
                                      "continents": 0.0, "erosion": 0.0,
                                      "depth": 0.0, "ridges": 0.0,
                                      "final_density": {body}}}}}"#
            ),
        );
    }

    /// Ground that falls away with height, roughened by a noise — the shape
    /// vanilla's own router has, with the y-dependence *inside* the
    /// interpolated node rather than beside it.
    const TERRAIN: &str = r#"{
        "type": "minecraft:interpolated",
        "argument": {
          "type": "minecraft:add",
          "argument1": {"type": "minecraft:y_clamped_gradient",
                        "from_y": -64, "to_y": 224,
                        "from_value": 1.0, "to_value": -1.0},
          "argument2": {"type": "minecraft:mul", "argument1": 0.8,
                        "argument2": {"type": "minecraft:noise",
                                      "noise": "minecraft:scratch",
                                      "xz_scale": 1.0, "y_scale": 1.0}}
        }}"#;

    fn at(terrain: &Terrain, out: &[u8], x: usize, y: i32, z: usize) -> Material {
        let row = (y - terrain.settings().min_y) as usize;
        Material::from_code(out[row * 256 + z * 16 + x])
    }

    /// The highest y holding anything that is not air, per column.
    fn surface(terrain: &Terrain, out: &[u8]) -> Vec<i32> {
        let settings = terrain.settings();
        let top = settings.min_y + settings.height;
        (0..256)
            .map(|column| {
                (settings.min_y..top)
                    .rev()
                    .find(|&y| at(terrain, out, column % 16, y, column / 16) != Material::Air)
                    .unwrap_or(settings.min_y)
            })
            .collect()
    }

    #[test]
    fn rock_below_water_between_and_air_above() {
        let root = scratch("stack");
        dimension(&root, "overworld", TERRAIN);
        let terrain = Terrain::new(&root, "overworld", 42).expect("the pack compiles");
        let mut filler = terrain.filler();
        let mut out = vec![0u8; terrain.cells_per_chunk()];
        filler.fill(0, 0, &mut out);

        // Every column: solid at the floor, air at the ceiling, and no air
        // anywhere below the sea level — what is not rock down there is the
        // dimension's own fluid.
        for z in 0..16 {
            for x in 0..16 {
                assert_eq!(at(&terrain, &out, x, -64, z), Material::Solid, "{x},{z}");
                assert_eq!(at(&terrain, &out, x, 319, z), Material::Air, "{x},{z}");
                for y in -64..63 {
                    assert_ne!(
                        at(&terrain, &out, x, y, z),
                        Material::Air,
                        "air at {x},{y},{z} is below the sea level"
                    );
                }
                for y in 63..320 {
                    assert_ne!(
                        at(&terrain, &out, x, y, z),
                        Material::Fluid,
                        "fluid at {x},{y},{z} is above the sea level"
                    );
                }
            }
        }
        let heights = surface(&terrain, &out);
        assert!(
            heights.iter().any(|&y| y > 63),
            "no column reached above the sea level"
        );
        assert!(
            heights.iter().any(|&y| y != heights[0]),
            "every column came out the same height, which is a flat world"
        );
    }

    #[test]
    fn a_chunk_is_the_same_wherever_you_ask_from() {
        let root = scratch("pure");
        dimension(&root, "overworld", TERRAIN);
        let terrain = Terrain::new(&root, "overworld", 7).expect("the pack compiles");
        let mut out = vec![0u8; terrain.cells_per_chunk()];
        let mut other = vec![0u8; terrain.cells_per_chunk()];
        // Two fillers, and one of them asked for three other chunks first. A
        // generator whose answer depends on what was generated before it is a
        // world that comes out differently depending on how you walked toward
        // it.
        terrain.filler().fill(3, -5, &mut out);
        let mut used = terrain.filler();
        for pos in [(0, 0), (100, 100), (-7, 2)] {
            used.fill(pos.0, pos.1, &mut other);
        }
        used.fill(3, -5, &mut other);
        assert_eq!(out, other);
    }

    #[test]
    fn the_whole_cell_skip_changes_nothing_and_fires() {
        let root = scratch("skip");
        dimension(&root, "overworld", TERRAIN);
        let terrain = Terrain::new(&root, "overworld", 11).expect("the pack compiles");
        let mut skipped = vec![0u8; terrain.cells_per_chunk()];
        let mut walked = vec![0u8; terrain.cells_per_chunk()];
        let mut fast = terrain.filler();
        let mut slow = terrain.filler();
        for pos in [(0, 0), (12, -30), (-400, 900)] {
            fast.fill(pos.0, pos.1, &mut skipped);
            slow.fill_without_skipping(pos.0, pos.1, &mut walked);
            assert_eq!(skipped, walked, "the skip moved a block at {pos:?}");
        }
        let (skipped_cells, walked_cells) = fast.cells();
        assert!(
            skipped_cells > walked_cells,
            "the skip answered {skipped_cells} cells of {} and is not worth its code",
            skipped_cells + walked_cells
        );
        assert_eq!(
            slow.cells(),
            (0, 3 * 4 * 4 * 48),
            "the control skips nothing"
        );
    }

    /// The lattice is the terrain, not an approximation of it.
    ///
    /// Watched to fail, in both directions and by construction. `BAND` is
    /// positive only for `3 <= y < 4`, and a cell corner is always a multiple
    /// of eight — so an interpolated `BAND` is negative *everywhere* and the
    /// same function unwrapped is solid on exactly one layer. If
    /// `interpolated` were compiled to a passthrough, the first assertion
    /// below would find rock at y = 3; if the second stopped finding it, the
    /// band itself would have stopped working and the first would be passing
    /// for the wrong reason.
    #[test]
    fn an_interpolated_node_is_sampled_at_the_corners_and_nowhere_else() {
        const BAND: &str = r#"{"type": "minecraft:range_choice",
                               "input": "minecraft:y",
                               "min_inclusive": 3.0, "max_exclusive": 4.0,
                               "when_in_range": 1.0, "when_out_of_range": -1.0}"#;
        let root = scratch("band");
        write(
            &root,
            "minecraft/worldgen/density_function/y.json",
            r#"{"type": "minecraft:y_clamped_gradient", "from_y": -4064, "to_y": 4062,
                "from_value": -4064.0, "to_value": 4062.0}"#,
        );
        dimension(
            &root,
            "overworld",
            &format!(r#"{{"type": "minecraft:interpolated", "argument": {BAND}}}"#),
        );
        dimension(&root, "the_nether", BAND);

        let lerped = Terrain::new(&root, "overworld", 1).expect("compiles");
        let mut out = vec![0u8; lerped.cells_per_chunk()];
        lerped.filler().fill(0, 0, &mut out);
        assert_eq!(
            at(&lerped, &out, 0, 3, 0),
            Material::Fluid,
            "a band that misses every cell corner must not reach a block"
        );

        let direct = Terrain::new(&root, "the_nether", 1).expect("compiles");
        let mut raw = vec![0u8; direct.cells_per_chunk()];
        direct.filler().fill(0, 0, &mut raw);
        assert_eq!(
            at(&direct, &raw, 0, 3, 0),
            Material::Solid,
            "the same band unwrapped is the one layer it names"
        );
        assert_eq!(at(&direct, &raw, 0, 2, 0), Material::Fluid);
    }
}
