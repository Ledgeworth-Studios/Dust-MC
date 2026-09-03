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
//! Aquifers, which decide what fluid an enclosed pocket holds; carvers; and
//! features. This module is the output of vanilla's own noise stage and
//! nothing beyond it — the default block, the default fluid below the sea
//! level the settings name, and air. [`crate::surface`] is the stage after it
//! and [`Columns::surface`] is the two together, which is what a server
//! serves. Decision record 0012 orders the rest and records 0026 and 0032 are
//! stages two and three of it.

use std::path::Path;

use crate::noise::build::{router, BuildError, NoiseSettings, Router};
use crate::noise::density::Evaluator;

/// What a generated cell holds.
///
/// Not a block state, because which block state each one is belongs to the
/// caller's registry and not to a generator. A `u8` because a chunk's worth is
/// ninety-six kibibytes of scratch and it is reused.
///
/// The noise stage answers with the first three. [`Material::Lava`] is the
/// fourth and only an aquifer writes it; [`Material::Surface`] is what a
/// surface rule claimed, and its index is into the rules' own palette — which
/// the caller resolves once, at boot, rather than per block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Material {
    Air,
    /// The dimension's `default_block`.
    Solid,
    /// The dimension's `default_fluid`, below the level the settings name or
    /// below an aquifer's own.
    Fluid,
    /// `minecraft:lava`. Named in `Aquifer.java` and in no data pack, which is
    /// why it is a code of its own rather than a second `default_fluid`; see
    /// [`crate::aquifer::Aquifer::lava_block`].
    Lava,
    /// `crate::surface::Rules::palette()[index]`.
    Surface(u8),
}

impl Material {
    pub fn from_code(code: u8) -> Self {
        match code {
            0 => Self::Air,
            1 => Self::Solid,
            2 => Self::Fluid,
            3 => Self::Lava,
            other => Self::Surface(other - 4),
        }
    }

    pub fn code(self) -> u8 {
        match self {
            Self::Air => 0,
            Self::Solid => 1,
            Self::Fluid => 2,
            Self::Lava => 3,
            Self::Surface(index) => 4 + index,
        }
    }

    /// Whether this material is a fluid of any kind, which is what a surface
    /// rule's water height and a stone run's floor both ask.
    pub fn is_fluid(self) -> bool {
        matches!(self, Self::Fluid | Self::Lava)
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
    /// The dimension's aquifers, or `None` when its settings say it has none.
    aquifer: Option<crate::aquifer::Aquifer>,
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
            aquifer: crate::aquifer::Aquifer::over(&router),
            router,
        })
    }

    pub fn settings(&self) -> &NoiseSettings {
        &self.router.settings
    }

    pub fn router(&self) -> &Router {
        &self.router
    }

    pub fn router_mut(&mut self) -> &mut Router {
        &mut self.router
    }

    /// How many cells a [`Filler`]'s output holds, which is 256 per world row.
    pub fn cells_per_chunk(&self) -> usize {
        256 * self.router.settings.height as usize
    }

    /// Whether this dimension runs aquifers at all.
    pub fn has_aquifer(&self) -> bool {
        self.aquifer.is_some()
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
            skipped_open: 0,
            walked: 0,
            flow: self
                .aquifer
                .as_ref()
                .map(|aquifer| aquifer.flow(&self.router.graph)),
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
    /// Cells the skip answered *without* rock in them, which is the half that
    /// only survives an aquifer when every candidate is the global one.
    skipped_open: u64,
    walked: u64,
    /// The aquifer's own scratch, when the dimension has one. A [`Filler`]
    /// holds it rather than a caller because the aquifer needs the density at
    /// the block, which only exists inside this walk.
    flow: Option<crate::aquifer::Flow<'a>>,
}

impl Filler<'_> {
    /// Fill one chunk's worth of materials.
    ///
    /// `out` is `256 * height` codes, indexed `(y - min_y) * 256 + z * 16 + x`,
    /// and is written in full. It is a caller's buffer rather than a return
    /// value because a server generating chunks forever should allocate this
    /// once per thread, not once per chunk.
    pub fn fill(&mut self, chunk_x: i32, chunk_z: i32, out: &mut [u8]) {
        self.fill_inner(chunk_x, chunk_z, out, true, false);
    }

    /// The same fill with the dimension's aquifers run over it: what a pocket
    /// below the ground actually holds, instead of the flat "fluid below the
    /// sea level" the noise stage alone can say.
    ///
    /// A separate entry point and not a flag on [`Terrain`], because the
    /// ladder in `cargo xtask harness worldgen` scores the two as separate
    /// rungs and a rung that could only be reached through the one above it
    /// would not say what the aquifers bought.
    pub fn fill_with_aquifer(&mut self, chunk_x: i32, chunk_z: i32, out: &mut [u8]) {
        self.fill_inner(chunk_x, chunk_z, out, true, true);
    }

    /// The same fill with the whole-cell skip switched off: every block's
    /// density evaluated, none of it inferred.
    ///
    /// This is the control the skip is checked against, and it is public
    /// because a check that lives only inside the thing it checks is not a
    /// check. The two must agree byte for byte on every chunk.
    pub fn fill_without_skipping(&mut self, chunk_x: i32, chunk_z: i32, out: &mut [u8]) {
        self.fill_inner(chunk_x, chunk_z, out, false, false);
    }

    /// The aquifer fill with the whole-cell skip switched off, which is the
    /// control the skip is checked against on a world that has aquifers.
    pub fn fill_with_aquifer_without_skipping(
        &mut self,
        chunk_x: i32,
        chunk_z: i32,
        out: &mut [u8],
    ) {
        self.fill_inner(chunk_x, chunk_z, out, false, true);
    }

    /// Cells answered by the bounds walk, and cells walked block by block.
    pub fn cells(&self) -> (u64, u64) {
        (self.skipped, self.walked)
    }

    /// Of the cells the bounds walk answered, how many held no rock.
    ///
    /// Counted apart because that half of the skip is the one an aquifer can
    /// take away, and a number is the only thing that says whether it did.
    pub fn open_cells(&self) -> u64 {
        self.skipped_open
    }

    fn fill_inner(
        &mut self,
        chunk_x: i32,
        chunk_z: i32,
        out: &mut [u8],
        skip: bool,
        aquifer: bool,
    ) {
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
        let aquifer = aquifer && self.flow.is_some();
        if aquifer {
            self.flow
                .as_mut()
                .expect("checked above")
                .enter_chunk(chunk_x, chunk_z);
        }

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
                        // With an aquifer running, a cell that holds no rock
                        // cannot be answered by its sign alone: what fills it
                        // is a function of the density at *each* block, which
                        // is what the pressure between two aquifers is added
                        // to. The all-rock half of the skip is unaffected —
                        // the aquifer's own first line is "density positive is
                        // rock" — and the all-air half survives only where the
                        // aquifer can be shown not to look at the density,
                        // which is where every candidate is the dimension's
                        // own global one. Decision record 0035 measures both
                        // halves.
                        let empty = high <= 0.0;
                        let whole = if aquifer {
                            low > 0.0
                                || (empty && {
                                    let lx = base_x + (cell_x * terrain.cell_width) as i32;
                                    let lz = base_z + (cell_z * terrain.cell_width) as i32;
                                    let span = terrain.cell_width as i32 - 1;
                                    let flow = self.flow.as_mut().expect("checked above");
                                    flow.box_is_global(
                                        (lx, lx + span),
                                        (cell_base_y, cell_base_y + height - 1),
                                        (lz, lz + span),
                                    )
                                })
                        } else {
                            empty || low > 0.0
                        };
                        if whole {
                            self.skipped += 1;
                            let solid = low > 0.0;
                            if !solid {
                                self.skipped_open += 1;
                            }
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
                                    out[line..line + terrain.cell_width].fill(material.code());
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
                                let material = match self.flow.as_mut() {
                                    Some(flow) if aquifer => {
                                        match flow.substance(block_x, block_y, block_z, density) {
                                            crate::aquifer::Substance::Rock => Material::Solid,
                                            crate::aquifer::Substance::Air => Material::Air,
                                            crate::aquifer::Substance::Fluid(
                                                crate::aquifer::Fluid::Default,
                                            ) => Material::Fluid,
                                            crate::aquifer::Substance::Fluid(
                                                crate::aquifer::Fluid::Lava,
                                            ) => Material::Lava,
                                        }
                                    }
                                    _ => {
                                        if density > 0.0 {
                                            Material::Solid
                                        } else if block_y < sea_level {
                                            Material::Fluid
                                        } else {
                                            Material::Air
                                        }
                                    }
                                };
                                let row = (block_y - min_y) as usize;
                                out[row * 256
                                    + (block_z - base_z) as usize * 16
                                    + (block_x - base_x) as usize] = material.code();
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
    /// The seed the biome blur fiddles with, which is a hash of the world seed
    /// and not the world seed. Computed once because it is a SHA-256 and a
    /// surface rule asks for a biome at every solid block of every column.
    zoom_seed: i64,
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
            zoom_seed: crate::noise::rng::obfuscate_seed(seed),
        })
    }

    /// The dimension's surface rules, or `None` if its settings carry none.
    pub fn surface(&self) -> Option<&crate::surface::Rules> {
        self.terrain.router().surface.as_ref()
    }

    /// Point the surface rules' biome conditions at a registry's own ids.
    ///
    /// Separate from [`Generator::new`] because a generator is built from a
    /// data pack and bound to a *running* registry, and the two are not the
    /// same thing — the pack is the operator's and the ids are this build's.
    pub fn bind_surface_biomes(&mut self, id_of: impl Fn(&str) -> Option<u32>) -> Vec<String> {
        match self.terrain.router_mut().surface.as_mut() {
            Some(rules) => rules.bind_biomes(id_of),
            None => Vec::new(),
        }
    }

    pub fn terrain(&self) -> &Terrain {
        &self.terrain
    }

    /// Whether this dimension runs aquifers.
    pub fn has_aquifer(&self) -> bool {
        self.terrain.has_aquifer()
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

    /// One thread's scratch: a terrain filler, a biome sampler and a surface
    /// painter over the one graph.
    pub fn columns(&self) -> Columns<'_> {
        let router = self.terrain.router();
        Columns {
            filler: self.terrain.filler(),
            biomes: crate::biome::Sampler::over(&router.graph, router.climate, &self.parameters),
            painter: router.surface.as_ref().map(|rules| {
                crate::surface::Painter::new(
                    rules,
                    &router.graph,
                    &router.settings,
                    router.initial_density,
                )
            }),
            zoom_seed: self.zoom_seed,
            materials: vec![0u8; self.terrain.cells_per_chunk()],
        }
    }
}

/// The scratch one thread needs to generate columns: three evaluators over one
/// shared graph, and the chunk-sized material buffer they fill.
#[derive(Debug, Clone)]
pub struct Columns<'a> {
    filler: Filler<'a>,
    biomes: crate::biome::Sampler<'a>,
    painter: Option<crate::surface::Painter<'a>>,
    zoom_seed: i64,
    materials: Vec<u8>,
}

impl<'a> Columns<'a> {
    /// Generate one chunk's materials — the noise stage and nothing past it.
    ///
    /// Kept beside [`Columns::surface`] rather than replaced by it, because
    /// the ladder in `cargo xtask harness worldgen` scores the two as separate
    /// rungs and a rung that could only be reached through the one above it
    /// would not say what the surface rules bought.
    pub fn terrain(&mut self, chunk_x: i32, chunk_z: i32) -> &[u8] {
        self.filler.fill(chunk_x, chunk_z, &mut self.materials);
        &self.materials
    }

    /// The noise stage, the dimension's aquifers and its surface rules — the
    /// three stages in the order vanilla runs them, which is the whole of what
    /// a server generates today.
    ///
    /// The aquifers go *under* the rules and not over them because that is
    /// where vanilla puts them: they are part of the noise stage, and a rule
    /// that reads `water` reads what the aquifer decided.
    pub fn aquifer(&mut self, chunk_x: i32, chunk_z: i32) -> &[u8] {
        self.filler
            .fill_with_aquifer(chunk_x, chunk_z, &mut self.materials);
        if let Some(painter) = self.painter.as_mut() {
            painter.paint(
                chunk_x,
                chunk_z,
                &mut self.materials,
                &mut self.biomes,
                self.zoom_seed,
            );
        }
        &self.materials
    }

    /// The noise stage with the dimension's surface rules painted over it.
    ///
    /// The same buffer, so a caller pays for one chunk of scratch and not two.
    pub fn surface(&mut self, chunk_x: i32, chunk_z: i32) -> &[u8] {
        self.filler.fill(chunk_x, chunk_z, &mut self.materials);
        if let Some(painter) = self.painter.as_mut() {
            painter.paint(
                chunk_x,
                chunk_z,
                &mut self.materials,
                &mut self.biomes,
                self.zoom_seed,
            );
        }
        &self.materials
    }

    /// How many times the rules asked something this generator declines to
    /// answer, and how many blocks a badlands band decided.
    pub fn declined(&self) -> (u64, u64) {
        self.painter.as_ref().map_or((0, 0), |p| p.declined())
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
                    "aquifers_enabled": false,
                    "noise_router": {{"temperature": 0.0, "vegetation": 0.0,
                                      "continents": 0.0, "erosion": 0.0,
                                      "depth": 0.0, "ridges": 0.0,
                                      "final_density": {body}}}}}"#
            ),
        );
    }

    /// A land world's un-jagged density: ground about seventy blocks above the
    /// sea level, so that nothing in the fixture is under an ocean and every
    /// aquifer has to decide its own level rather than inheriting the sea's.
    ///
    /// This is what `initial_density_without_jaggedness` is in a real pack,
    /// and what the preliminary surface level — and therefore every aquifer —
    /// is walked down.
    const UNJAGGED: &str = r#"{
        "type": "minecraft:add",
        "argument1": {"type": "minecraft:y_clamped_gradient",
                      "from_y": -64, "to_y": 600,
                      "from_value": 1.0, "to_value": -1.0},
        "argument2": {"type": "minecraft:mul", "argument1": 0.2,
                      "argument2": {"type": "minecraft:noise",
                                    "noise": "minecraft:scratch",
                                    "xz_scale": 1.0, "y_scale": 1.0}}
      }"#;

    /// A settings file with aquifers on, the four noises they read, and
    /// `erosion` and `depth` pinned to constants so a test can put the whole
    /// world inside or outside the deep dark on purpose.
    fn wet_dimension(root: &Path, erosion: f64, depth: f64) {
        for (name, octave) in [
            ("aq_barrier", -3),
            ("aq_flood", -7),
            ("aq_spread", -5),
            ("aq_lava", -1),
            ("aq_cave", -4),
        ] {
            write(
                root,
                &format!("minecraft/worldgen/noise/{name}.json"),
                &format!(
                    r#"{{"firstOctave": {octave}, "amplitudes": {amplitudes}}}"#,
                    // The four aquifer noises are vanilla's own, octave for
                    // octave. The cave is this fixture's, and is wide enough
                    // to open pockets at every depth including under the lava
                    // line.
                    amplitudes = if name == "aq_cave" {
                        "[1.0, 1.0, 1.0]"
                    } else {
                        "[1.0]"
                    }
                ),
            );
        }
        let noise = |name: &str, y_scale: f64| {
            format!(
                r#"{{"type": "minecraft:noise", "noise": "minecraft:{name}",
                     "xz_scale": 1.0, "y_scale": {y_scale}}}"#
            )
        };
        write(
            root,
            "minecraft/worldgen/noise_settings/overworld.json",
            &format!(
                r#"{{"noise": {{"height": 384, "min_y": -64,
                                "size_horizontal": 1, "size_vertical": 2}},
                    "sea_level": 63,
                    "default_block": {{"Name": "minecraft:stone"}},
                    "default_fluid": {{"Name": "minecraft:water"}},
                    "aquifers_enabled": true,
                    "noise_router": {{"temperature": 0.0, "vegetation": 0.0,
                                      "continents": 0.0, "ridges": 0.0,
                                      "erosion": {erosion}, "depth": {depth},
                                      "barrier": {barrier},
                                      "fluid_level_floodedness": {flood},
                                      "fluid_level_spread": {spread},
                                      "lava": {lava},
                                      "initial_density_without_jaggedness": {UNJAGGED},
                                      "final_density": {{"type": "minecraft:interpolated",
                                                         "argument": {{
                                        "type": "minecraft:add",
                                        "argument1": {UNJAGGED},
                                        "argument2": {cave}}}}}}}}}"#,
                cave = format!(
                    r#"{{"type": "minecraft:mul", "argument1": 3.5, "argument2": {}}}"#,
                    noise("aq_cave", 1.0)
                ),
                barrier = noise("aq_barrier", 0.5),
                flood = noise("aq_flood", 0.67),
                spread = noise("aq_spread", 0.7142857142857143),
                lava = noise("aq_lava", 1.0),
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

    /// The same control on a world that has aquifers.
    ///
    /// The whole-cell skip has a second half there — a cell that holds no rock
    /// may still be answered wholesale, but only when every aquifer it could
    /// belong to is the dimension's own. That is a claim about which cells the
    /// skip is allowed to take, and this is the check that it took no others:
    /// the two fills must agree byte for byte.
    ///
    /// **Watched to fail** by making `box_is_global` answer `true` for every
    /// box that holds no rock, which is the claim the skip would be making if
    /// it were unsound. Narrowing its grid window by one row instead leaves
    /// this green — a mutation that does not bite is not evidence, and the
    /// window is defended by the byte-for-byte comparison only where a
    /// neighbouring aquifer actually differs.
    #[test]
    fn the_skip_over_an_aquifer_moves_no_block_and_still_fires() {
        let root = scratch("aquifer-skip");
        wet_dimension(&root, 0.0, 0.0);
        let terrain = Terrain::new(&root, "overworld", 7).expect("the pack compiles");
        assert!(terrain.has_aquifer(), "the settings say aquifers are on");
        let mut skipped = vec![0u8; terrain.cells_per_chunk()];
        let mut walked = vec![0u8; terrain.cells_per_chunk()];
        let mut fast = terrain.filler();
        let mut slow = terrain.filler();
        for pos in [(0, 0), (12, -30), (-400, 900)] {
            fast.fill_with_aquifer(pos.0, pos.1, &mut skipped);
            slow.fill_with_aquifer_without_skipping(pos.0, pos.1, &mut walked);
            assert_eq!(skipped, walked, "the skip moved a block at {pos:?}");
        }
        let (skipped_cells, walked_cells) = fast.cells();
        assert!(
            fast.open_cells() > 0,
            "the skip answered {skipped_cells} of {} cells and every one of them was \
             solid rock, so the half this test is about never fired",
            skipped_cells + walked_cells
        );
    }

    /// An aquifer leaves a pocket dry, which is the whole point of it.
    ///
    /// Counted only between y -36 and the sea level, and that band is the
    /// check rather than a convenience. The lowest aquifer centre a block at
    /// y -36 can belong to is at -48, above the -54 the global picker starts
    /// answering lava at, so the pre-aquifer answer for every one of these
    /// cells is the dimension's fluid and nothing else. **A wider band made this test
    /// vacuous**: below -54 the global status is lava at level -54, so a
    /// generator that flooded every aquifer to its own global level would
    /// still leave air between -54 and the sea, and the mutation that was
    /// supposed to make this go red left it green.
    ///
    /// **Watched to fail** by making `surface_level` answer
    /// `self.aquifer.sea_level`, which is the rule the noise stage alone has:
    /// `dried` drops to zero.
    #[test]
    fn an_aquifer_leaves_a_pocket_below_the_sea_level_dry() {
        let root = scratch("aquifer-dry");
        wet_dimension(&root, 0.0, 0.0);
        let terrain = Terrain::new(&root, "overworld", 7).expect("the pack compiles");
        let settings = terrain.settings().clone();
        let mut flooded = vec![0u8; terrain.cells_per_chunk()];
        let mut aquifer = vec![0u8; terrain.cells_per_chunk()];
        let mut plain = terrain.filler();
        let mut wet = terrain.filler();
        let mut dried = 0u32;
        let mut still_wet = 0u32;
        // Several chunks, spread far enough apart to see more than one draw of
        // a noise whose first octave is 1/128 of a block.
        for (chunk_x, chunk_z) in [(0, 0), (40, 40), (-300, 700), (900, -1200)] {
            plain.fill(chunk_x, chunk_z, &mut flooded);
            wet.fill_with_aquifer(chunk_x, chunk_z, &mut aquifer);
            for y in -36..settings.sea_level {
                let row = (y - settings.min_y) as usize * 256;
                for column in 0..256 {
                    if Material::from_code(flooded[row + column]) != Material::Fluid {
                        continue;
                    }
                    match Material::from_code(aquifer[row + column]) {
                        Material::Air => dried += 1,
                        Material::Fluid => still_wet += 1,
                        _ => {}
                    }
                }
            }
        }
        assert!(
            dried > 0,
            "every one of the {still_wet} flooded cell(s) is still flooded"
        );
        assert!(
            still_wet > 0,
            "{dried} cells dried and none stayed wet, which is not an aquifer either"
        );
    }

    /// Under the deep dark the aquifers are switched off, so an ancient city
    /// is not at the bottom of a lake.
    ///
    /// Two worlds and not one, because one is not a check. The same pack is
    /// built twice with only `erosion` and `depth` moved — outside the deep
    /// dark and inside it — and the test requires that the first holds the
    /// dimension's fluid below its sea level and the second holds none at all.
    /// **The single-world version of this test was vacuous**: this fixture's
    /// aquifers are mostly dry anyway, so flipping either comparison in
    /// `surface_level` left it green. The difference between the two worlds is
    /// the only thing that can only be the deep dark.
    ///
    /// **Watched to fail** by flipping either comparison, and by moving either
    /// constant past the value the fixture pins.
    #[test]
    fn the_deep_dark_has_no_aquifers_at_all() {
        let mut wet = 0u32;
        let mut dry_world_fluid = 0u32;
        for (label, erosion, depth) in [("shallow", 0.0, 0.0), ("deep-dark", -1.0, 1.0)] {
            let root = scratch(&format!("aquifer-{label}"));
            wet_dimension(&root, erosion, depth);
            let terrain = Terrain::new(&root, "overworld", 7).expect("the pack compiles");
            let settings = terrain.settings().clone();
            let mut materials = vec![0u8; terrain.cells_per_chunk()];
            let mut filler = terrain.filler();
            for (chunk_x, chunk_z) in [(0, 0), (40, 40), (-300, 700), (900, -1200)] {
                filler.fill_with_aquifer(chunk_x, chunk_z, &mut materials);
                // Above the lava line, so the global picker is not the thing
                // being measured here.
                for y in -54..settings.sea_level {
                    let row = (y - settings.min_y) as usize * 256;
                    for column in 0..256 {
                        if Material::from_code(materials[row + column]) == Material::Fluid {
                            if erosion == 0.0 {
                                wet += 1;
                            } else {
                                dry_world_fluid += 1;
                            }
                        }
                    }
                }
            }
        }
        assert!(
            wet > 0,
            "the world outside the deep dark holds no fluid either, so this test \
             would pass on a generator with no aquifers at all"
        );
        assert_eq!(
            dry_world_fluid, 0,
            "the deep dark holds {dry_world_fluid} cell(s) of the dimension's fluid \
             where the same pack outside it holds {wet}"
        );
    }

    /// Below the level the global picker names, what is not rock is lava.
    ///
    /// **Watched to fail** by deleting the early `global.at(y) == Lava` branch
    /// in `substance`: the cells come back as water.
    #[test]
    fn the_floor_of_the_world_is_lava_and_not_water() {
        let root = scratch("aquifer-lava");
        wet_dimension(&root, 0.0, 0.0);
        let terrain = Terrain::new(&root, "overworld", 7).expect("the pack compiles");
        let settings = terrain.settings().clone();
        let mut materials = vec![0u8; terrain.cells_per_chunk()];
        terrain.filler().fill_with_aquifer(0, 0, &mut materials);
        let mut lava = 0u32;
        for y in settings.min_y..-54 {
            let row = (y - settings.min_y) as usize * 256;
            for column in 0..256 {
                match Material::from_code(materials[row + column]) {
                    Material::Lava => lava += 1,
                    Material::Solid => {}
                    other => panic!("{other:?} at y {y}, below the lava level"),
                }
            }
        }
        assert!(lava > 0, "the fixture has no open cells below -54");
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
