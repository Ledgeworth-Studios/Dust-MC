//! Choosing a biome from a climate.
//!
//! Minecraft 1.21.1 does not paint biomes on a map. It samples six values at
//! every 4x4x4 cell — temperature, humidity, continentalness, erosion, depth
//! and weirdness — and picks the biome whose published region of that space is
//! nearest the sample. The regions are Mojang's, so per decision records 0006,
//! 0007 and 0008 they are not in this repository: they arrive at run time as
//! `dust-biomes.tsv`, which `cargo xtask extract --only worldgen` writes out of
//! the operator's own server jar beside `dust-constants.tsv`.
//!
//! # Why a name sits beside every id
//!
//! A biome id is an index into a registry, and a registry is a list that a
//! version can reorder. A table of ids alone would keep loading after a
//! reorder and would put jungles in the tundra; a table that carries the name
//! too is checked against the running registry on the row where it disagrees,
//! and says which biome moved.
//!
//! # Integers, not floats
//!
//! Every value here is quantised by ten thousand, which is what Minecraft does
//! before it compares anything. The search is then integer arithmetic over a
//! flat array — no map lookups, no float compares, and a squared distance that
//! cannot round two different regions into a tie that a float would invent.

use std::path::Path;

use crate::noise::build::{climate_graph, BuildError, ClimateGraph};
use crate::noise::density::Evaluator;

/// The file `cargo xtask extract --only worldgen` writes, for the operator to
/// copy into `[data] path`.
pub const FILE: &str = "dust-biomes.tsv";

/// How many of Minecraft's units one whole climate unit is worth.
pub const QUANTISATION: f32 = 10_000.0;

/// The six climate axes, in the order every array here holds them.
pub const AXES: [&str; 6] = [
    "temperature",
    "humidity",
    "continentalness",
    "erosion",
    "depth",
    "weirdness",
];

/// One axis of one biome's region: an inclusive span of quantised units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub min: i64,
    pub max: i64,
}

impl Span {
    /// How far `value` is outside this span, or 0 inside it.
    fn distance(self, value: i64) -> i64 {
        let above = value - self.max;
        if above > 0 {
            above
        } else {
            (self.min - value).max(0)
        }
    }
}

/// One row of the parameter list: a biome and the region it claims.
#[derive(Debug, Clone)]
pub struct Region {
    pub id: u32,
    pub name: String,
    pub axes: [Span; 6],
    /// A fixed cost added to every distance, which is how vanilla makes one
    /// biome lose a region it would otherwise share.
    pub offset: i64,
}

impl Region {
    /// The squared distance from `point` to this region, or `None` once the
    /// running total has passed `best` and the answer can no longer win.
    ///
    /// The early exit is exact: every term is a square and therefore
    /// non-negative, so a total that has already passed the best cannot come
    /// back down. It is here because this runs six times per biome cell and
    /// there are 124,416 cells in a 9x9.
    fn fitness(&self, point: &[i64; 6], best: i64) -> Option<i64> {
        let mut total = self.offset * self.offset;
        if total >= best {
            return None;
        }
        for (axis, &value) in self.axes.iter().zip(point) {
            let distance = axis.distance(value);
            total += distance * distance;
            if total >= best {
                return None;
            }
        }
        Some(total)
    }
}

/// A run of consecutive rows and the smallest box holding all of them.
///
/// The search is a scan in table order, because the first of two equally near
/// regions wins and the table's order is the answer. A box over a run of rows
/// keeps that order exactly and lets the scan skip the whole run: every term of
/// a distance is a square, so the distance to the box is a floor under the
/// distance to any row inside it, and a floor that has already passed the best
/// answer cannot be beaten by anything the run holds.
///
/// This is not a heuristic and it cannot change an answer. It is checked
/// against the exhaustive scan over the real table, on both seeds.
#[derive(Debug, Clone, Copy)]
struct Run {
    axes: [Span; 6],
    /// The smallest offset in the run, since offset is a cost added to every
    /// row and the smallest one is the floor for the run.
    offset: i64,
    start: usize,
    end: usize,
}

/// How many rows a run holds.
///
/// 64, measured on 1.21.1's own 7,593-row overworld table over the same 81
/// chunks the ladder scores: 16 and 32 rows per run come out at about 200
/// chunk columns per second and 64 at about 290, because a smaller run pays
/// its own box test more often than the rows it saves. Wider than 64 the boxes
/// stop being tight enough to skip anything. See decision record 0021.
const RUN: usize = 64;

impl Run {
    /// Whether anything in this run can still beat `best`.
    ///
    /// The same early exit `Region::fitness` has and for the same reason: a
    /// total that has already passed the best cannot come back down, and most
    /// runs are rejected on their first or second axis. With the whole floor
    /// computed every time, the run tests were themselves the largest cost in
    /// the search once the skipping worked.
    fn can_beat(&self, point: &[i64; 6], best: i64) -> bool {
        let mut total = self.offset * self.offset;
        if total >= best {
            return false;
        }
        for (axis, &value) in self.axes.iter().zip(point) {
            let distance = axis.distance(value);
            total += distance * distance;
            if total >= best {
                return false;
            }
        }
        true
    }
}

/// The quart cell `BiomeManager` reads a block position's biome out of.
///
/// A biome is stored per 4x4x4 cell, and Minecraft does **not** simply divide:
/// it offsets the position by two, then picks whichever of the eight
/// surrounding cell corners wins a hash-fiddled distance. That is what makes a
/// biome edge wobble by a block or two instead of running down a grid line, and
/// a surface rule that asked the grid directly would put beach sand in a
/// straight line along a coast.
///
/// `zoom_seed` is [`crate::noise::rng::obfuscate_seed`] of the world seed, not
/// the world seed.
pub fn blurred_quart(zoom_seed: i64, x: i32, y: i32, z: i32) -> (i32, i32, i32) {
    let (bx, by, bz) = (x - 2, y - 2, z - 2);
    let (cx, cy, cz) = (bx >> 2, by >> 2, bz >> 2);
    let dx = f64::from(bx & 3) / 4.0;
    let dy = f64::from(by & 3) / 4.0;
    let dz = f64::from(bz & 3) / 4.0;
    let mut best = 0usize;
    let mut nearest = f64::INFINITY;
    for corner in 0..8usize {
        let low_x = corner & 4 == 0;
        let low_y = corner & 2 == 0;
        let low_z = corner & 1 == 0;
        let distance = fiddled_distance(
            zoom_seed,
            if low_x { cx } else { cx + 1 },
            if low_y { cy } else { cy + 1 },
            if low_z { cz } else { cz + 1 },
            if low_x { dx } else { dx - 1.0 },
            if low_y { dy } else { dy - 1.0 },
            if low_z { dz } else { dz - 1.0 },
        );
        if nearest > distance {
            best = corner;
            nearest = distance;
        }
    }
    (
        if best & 4 == 0 { cx } else { cx + 1 },
        if best & 2 == 0 { cy } else { cy + 1 },
        if best & 1 == 0 { cz } else { cz + 1 },
    )
}

fn fiddled_distance(seed: i64, x: i32, y: i32, z: i32, dx: f64, dy: f64, dz: f64) -> f64 {
    let mut state = seed;
    for coordinate in [x, y, z, x, y, z] {
        state = next_seed(state, i64::from(coordinate));
    }
    let first = fiddle(state);
    state = next_seed(state, seed);
    let second = fiddle(state);
    state = next_seed(state, seed);
    let third = fiddle(state);
    square(dz + third) + square(dy + second) + square(dx + first)
}

/// `LinearCongruentialGenerator.next`, which is a hash and not a generator
/// here: it is stepped six times over the coordinates before anything is read.
fn next_seed(seed: i64, step: i64) -> i64 {
    seed.wrapping_mul(
        seed.wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407),
    )
    .wrapping_add(step)
}

fn fiddle(state: i64) -> f64 {
    (f64::from((state >> 24).rem_euclid(1024) as i32) / 1024.0 - 0.5) * 0.9
}

fn square(value: f64) -> f64 {
    value * value
}

/// The whole parameter list for one dimension.
#[derive(Debug, Clone, Default)]
pub struct BiomeParameters {
    regions: Vec<Region>,
    runs: Vec<Run>,
}

/// What is wrong with a `dust-biomes.tsv`.
#[derive(Debug)]
pub enum ParametersError {
    NoHeader,
    MissingColumn(&'static str),
    Malformed { line: usize, detail: String },
    Empty,
}

impl std::fmt::Display for ParametersError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoHeader => write!(f, "no `#` header naming the columns"),
            Self::MissingColumn(name) => write!(f, "the header has no `{name}` column"),
            Self::Malformed { line, detail } => write!(f, "line {line}: {detail}"),
            Self::Empty => write!(f, "no biome regions in it"),
        }
    }
}

impl std::error::Error for ParametersError {}

impl BiomeParameters {
    /// Read the table. Columns are found by name, so a later version that adds
    /// one does not shift every reader by a field.
    pub fn parse(text: &str) -> Result<Self, ParametersError> {
        let header = text
            .lines()
            .find(|line| line.starts_with('#'))
            .ok_or(ParametersError::NoHeader)?;
        let columns: Vec<&str> = header
            .trim_start_matches('#')
            .trim()
            .split('\t')
            .map(str::trim)
            .collect();
        let find = |name: &'static str| {
            columns
                .iter()
                .position(|column| *column == name)
                .ok_or(ParametersError::MissingColumn(name))
        };
        let id_at = find("biome_id")?;
        let name_at = find("biome")?;
        let offset_at = find("offset")?;
        let mut axis_at = [(0usize, 0usize); 6];
        for (slot, axis) in axis_at.iter_mut().zip(AXES) {
            // Leaked so the error can name the column without allocating a
            // lifetime; there are twelve of them and they live for the run.
            let min: &'static str = Box::leak(format!("{axis}_min").into_boxed_str());
            let max: &'static str = Box::leak(format!("{axis}_max").into_boxed_str());
            *slot = (find(min)?, find(max)?);
        }

        let mut regions = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let line = line.trim_end_matches('\r');
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let at = index + 1;
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() != columns.len() {
                return Err(ParametersError::Malformed {
                    line: at,
                    detail: format!(
                        "{} field(s) where the header names {}",
                        fields.len(),
                        columns.len()
                    ),
                });
            }
            let cell = |column: usize, what: &str| -> Result<i64, ParametersError> {
                fields[column]
                    .parse::<i64>()
                    .map_err(|e| ParametersError::Malformed {
                        line: at,
                        detail: format!("{what}: {e}"),
                    })
            };
            let id = cell(id_at, "biome_id")? as u32;
            let name = fields[name_at].to_owned();
            let mut axes = [Span { min: 0, max: 0 }; 6];
            for (slot, ((min_at, max_at), axis)) in
                axes.iter_mut().zip(axis_at.iter().copied().zip(AXES))
            {
                *slot = Span {
                    min: cell(min_at, axis)?,
                    max: cell(max_at, axis)?,
                };
                if slot.min > slot.max {
                    return Err(ParametersError::Malformed {
                        line: at,
                        detail: format!("{axis} runs backwards"),
                    });
                }
            }
            regions.push(Region {
                id,
                name,
                axes,
                offset: cell(offset_at, "offset")?,
            });
        }
        if regions.is_empty() {
            return Err(ParametersError::Empty);
        }
        Ok(Self::over(regions))
    }

    /// Build the skip index over a list of regions.
    fn over(regions: Vec<Region>) -> Self {
        let mut runs = Vec::with_capacity(regions.len().div_ceil(RUN));
        for start in (0..regions.len()).step_by(RUN) {
            let end = (start + RUN).min(regions.len());
            let mut axes = [Span {
                min: i64::MAX,
                max: i64::MIN,
            }; 6];
            let mut offset = i64::MAX;
            for region in &regions[start..end] {
                for (slot, span) in axes.iter_mut().zip(region.axes) {
                    slot.min = slot.min.min(span.min);
                    slot.max = slot.max.max(span.max);
                }
                offset = offset.min(region.offset);
            }
            runs.push(Run {
                axes,
                offset,
                start,
                end,
            });
        }
        Self { regions, runs }
    }

    pub fn len(&self) -> usize {
        self.regions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    pub fn regions(&self) -> &[Region] {
        &self.regions
    }

    /// How many distinct biomes the table names.
    pub fn distinct_biomes(&self) -> usize {
        let mut names: Vec<&str> = self.regions.iter().map(|r| r.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        names.len()
    }

    /// The nearest region to a quantised climate point.
    ///
    /// The first of equal-distance regions wins, which is the order the table
    /// was written in and therefore the order vanilla's own list is in.
    pub fn nearest(&self, point: &[i64; 6]) -> Option<&Region> {
        let mut best = i64::MAX;
        let mut found = None;
        for run in &self.runs {
            if !run.can_beat(point, best) {
                continue;
            }
            for region in &self.regions[run.start..run.end] {
                if let Some(fitness) = region.fitness(point, best) {
                    best = fitness;
                    found = Some(region);
                }
            }
        }
        found
    }

    /// Re-point every row at the ids a running registry uses, by name.
    ///
    /// Returns the rows whose id the table disagreed with, so an operator is
    /// told which biome moved rather than being handed a world with the wrong
    /// ones in it.
    pub fn rebind(&mut self, id_of: impl Fn(&str) -> Option<u32>) -> Vec<Moved> {
        let mut moved = Vec::new();
        for region in &mut self.regions {
            match id_of(&region.name) {
                Some(id) if id == region.id => {}
                Some(id) => {
                    moved.push(Moved {
                        name: region.name.clone(),
                        was: region.id,
                        now: Some(id),
                    });
                    region.id = id;
                }
                None => moved.push(Moved {
                    name: region.name.clone(),
                    was: region.id,
                    now: None,
                }),
            }
        }
        moved.sort_by(|a, b| a.name.cmp(&b.name));
        moved.dedup_by(|a, b| a.name == b.name);
        moved
    }
}

/// A biome whose id in the table is not the id the registry gives its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moved {
    pub name: String,
    pub was: u32,
    /// `None` when the running registry does not know the name at all.
    pub now: Option<u32>,
}

/// A biome source: the climate graph and the regions it is matched against.
#[derive(Debug, Clone)]
pub struct BiomeSource {
    climate: ClimateGraph,
    parameters: BiomeParameters,
}

impl BiomeSource {
    /// Build one for a seed, reading the graph from a data pack directory.
    pub fn new(
        data_root: &Path,
        dimension: &str,
        seed: i64,
        parameters: BiomeParameters,
    ) -> Result<Self, BuildError> {
        Ok(Self {
            climate: climate_graph(data_root, dimension, seed)?,
            parameters,
        })
    }

    pub fn parameters(&self) -> &BiomeParameters {
        &self.parameters
    }

    /// A sampler holding this thread's scratch space.
    pub fn sampler(&self) -> Sampler<'_> {
        Sampler::over(&self.climate.graph, self.climate.roots, &self.parameters)
    }
}

/// One thread's view of a biome source.
///
/// It holds the parameter list and the six climate roots rather than a whole
/// [`BiomeSource`], so a caller that compiled the climate half as part of a
/// larger router can sample biomes off *that* graph instead of building a
/// second copy of twenty-five noise tables.
#[derive(Debug, Clone)]
pub struct Sampler<'a> {
    parameters: &'a BiomeParameters,
    roots: [usize; 6],
    evaluator: Evaluator<'a>,
}

impl<'a> Sampler<'a> {
    /// A sampler over a graph the caller already has.
    pub fn over(
        graph: &'a crate::noise::density::Graph,
        roots: [usize; 6],
        parameters: &'a BiomeParameters,
    ) -> Self {
        Self {
            parameters,
            roots,
            evaluator: Evaluator::new(graph),
        }
    }
}

impl Sampler<'_> {
    /// The quantised climate at a biome cell, given in **quart** coordinates —
    /// the 4x4x4 cell index, which is what a chunk's biome container is
    /// addressed by.
    pub fn climate(&mut self, quart_x: i32, quart_y: i32, quart_z: i32) -> [i64; 6] {
        let mut raw = [0.0f64; 6];
        self.evaluator.compute_all(
            &self.roots,
            quart_x << 2,
            quart_y << 2,
            quart_z << 2,
            &mut raw,
        );
        // Narrowed to f32 before the multiply, because Minecraft narrows here
        // and the truncation below turns a last-bit difference into a
        // different integer — and, at a region boundary, a different biome.
        std::array::from_fn(|axis| ((raw[axis] as f32) * QUANTISATION) as i64)
    }

    /// The biome at a cell, in quart coordinates.
    pub fn biome(&mut self, quart_x: i32, quart_y: i32, quart_z: i32) -> Option<u32> {
        let point = self.climate(quart_x, quart_y, quart_z);
        self.parameters.nearest(&point).map(|region| region.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "# biome_id\tbiome\ttemperature_min\ttemperature_max\thumidity_min\thumidity_max\tcontinentalness_min\tcontinentalness_max\terosion_min\terosion_max\tdepth_min\tdepth_max\tweirdness_min\tweirdness_max\toffset";

    fn row(id: u32, name: &str, temperature: (i64, i64)) -> String {
        format!(
            "{id}\t{name}\t{}\t{}\t-10000\t10000\t-10000\t10000\t-10000\t10000\t-10000\t10000\t-10000\t10000\t0",
            temperature.0, temperature.1
        )
    }

    fn table(rows: &[String]) -> String {
        let mut text = HEADER.to_owned();
        for row in rows {
            text.push('\n');
            text.push_str(row);
        }
        text
    }

    #[test]
    fn a_span_is_zero_distance_inside_and_the_gap_outside() {
        let span = Span {
            min: -100,
            max: 100,
        };
        assert_eq!(span.distance(0), 0);
        assert_eq!(span.distance(-100), 0);
        assert_eq!(span.distance(100), 0);
        assert_eq!(span.distance(150), 50);
        assert_eq!(span.distance(-150), 50);
    }

    #[test]
    fn the_nearest_region_is_the_one_the_point_is_in() {
        let text = table(&[
            row(1, "minecraft:cold", (-10000, -3000)),
            row(2, "minecraft:mild", (-3000, 3000)),
            row(3, "minecraft:hot", (3000, 10000)),
        ]);
        let parameters = BiomeParameters::parse(&text).expect("parsed");
        assert_eq!(parameters.len(), 3);
        assert_eq!(parameters.distinct_biomes(), 3);
        let at = |t: i64| {
            parameters
                .nearest(&[t, 0, 0, 0, 0, 0])
                .expect("a nearest")
                .name
                .clone()
        };
        assert_eq!(at(-9000), "minecraft:cold");
        assert_eq!(at(0), "minecraft:mild");
        assert_eq!(at(9000), "minecraft:hot");
        // Outside every span on the low side: the nearest is still the coldest.
        assert_eq!(at(-99999), "minecraft:cold");
    }

    #[test]
    fn the_early_exit_does_not_change_which_region_wins() {
        // The pruning in `fitness` is the only clever thing in the search, so
        // it is checked against the same search without it.
        let text = table(&[
            row(1, "minecraft:a", (-10000, -5000)),
            row(2, "minecraft:b", (-1000, 1000)),
            row(3, "minecraft:c", (6000, 10000)),
        ]);
        let parameters = BiomeParameters::parse(&text).expect("parsed");
        for step in -120..120 {
            let point = [step * 100, 0, 0, 0, 0, 0];
            let pruned = parameters.nearest(&point).expect("a nearest").id;
            let exhaustive = parameters
                .regions()
                .iter()
                .map(|region| {
                    (
                        region.fitness(&point, i64::MAX).expect("no bound"),
                        region.id,
                    )
                })
                .min_by_key(|(fitness, _)| *fitness)
                .expect("a nearest")
                .1;
            assert_eq!(pruned, exhaustive, "at {}", point[0]);
        }
    }

    /// The skip index cannot change an answer, over a table big enough to have
    /// one.
    ///
    /// The test above covers three regions, which is one run and therefore no
    /// skipping at all. This one is 500 regions over eight runs, with spans
    /// that overlap on every axis so that the box around a run is genuinely
    /// wider than its rows, and it checks the answer against the exhaustive
    /// scan the index is supposed to be indistinguishable from — the *region*
    /// and not just its id, so a tie resolved to a different row is caught.
    #[test]
    fn the_skip_index_answers_exactly_what_the_whole_scan_answers() {
        let mut rows = Vec::new();
        for index in 0..500i64 {
            // Spans that wander rather than tile, so a run's box is loose and
            // the floor under it is a real floor rather than the rows again.
            let low = (index * 37) % 20_001 - 10_000;
            let width = (index * 911) % 4_000;
            rows.push(format!(
                "{}\tminecraft:b{index}\t{}\t{}\t-10000\t10000\t-10000\t10000\
                 \t-10000\t10000\t-10000\t10000\t{}\t{}\t{}",
                index,
                low,
                low + width,
                -10_000 + (index * 53) % 20_001,
                -10_000 + (index * 53) % 20_001 + width,
                index % 3 * 100,
            ));
        }
        let parameters = BiomeParameters::parse(&table(&rows)).expect("parsed");
        assert_eq!(parameters.len(), 500);
        assert!(
            parameters.runs.len() > 1,
            "a single run would not exercise the skipping at all"
        );

        for step in -100..=100i64 {
            let point = [step * 137, 0, 0, 0, 0, step * -91];
            let indexed = parameters.nearest(&point).expect("a nearest");
            // The whole scan, in table order, with no index and no early exit.
            let mut best = i64::MAX;
            let mut exhaustive: Option<&Region> = None;
            for region in parameters.regions() {
                let mut total = region.offset * region.offset;
                for (axis, &value) in region.axes.iter().zip(&point) {
                    let distance = axis.distance(value);
                    total += distance * distance;
                }
                if total < best {
                    best = total;
                    exhaustive = Some(region);
                }
            }
            let exhaustive = exhaustive.expect("a nearest");
            assert_eq!(
                indexed.id, exhaustive.id,
                "at {point:?}: the index picked {} and the whole scan picked {}",
                indexed.name, exhaustive.name
            );
        }
    }

    #[test]
    fn an_offset_pushes_a_region_away_even_where_it_would_have_won() {
        let mut rows = vec![row(1, "minecraft:plain", (-10000, 10000))];
        rows.push(row(2, "minecraft:special", (-10000, 10000)).replace("\t0", "\t5000"));
        let parameters = BiomeParameters::parse(&table(&rows)).expect("parsed");
        assert_eq!(
            parameters.nearest(&[0, 0, 0, 0, 0, 0]).expect("found").name,
            "minecraft:plain",
            "an offset is a cost, so the offset row loses a tie it would win"
        );
    }

    #[test]
    fn the_first_of_two_equal_regions_wins() {
        let rows = vec![
            row(1, "minecraft:first", (-10000, 10000)),
            row(2, "minecraft:second", (-10000, 10000)),
        ];
        let parameters = BiomeParameters::parse(&table(&rows)).expect("parsed");
        assert_eq!(
            parameters.nearest(&[0, 0, 0, 0, 0, 0]).expect("found").name,
            "minecraft:first"
        );
    }

    #[test]
    fn a_table_with_a_renumbered_biome_says_which_row_moved() {
        let rows = vec![
            row(1, "minecraft:plains", (-10000, 0)),
            row(2, "minecraft:desert", (0, 10000)),
        ];
        let mut parameters = BiomeParameters::parse(&table(&rows)).expect("parsed");
        let moved = parameters.rebind(|name| match name {
            "minecraft:plains" => Some(1),
            "minecraft:desert" => Some(9),
            _ => None,
        });
        assert_eq!(
            moved,
            vec![Moved {
                name: "minecraft:desert".to_owned(),
                was: 2,
                now: Some(9),
            }]
        );
        assert_eq!(parameters.regions()[1].id, 9, "and is corrected, not kept");
    }

    #[test]
    fn a_biome_the_registry_has_never_heard_of_is_reported_rather_than_dropped() {
        let rows = vec![row(1, "someone_elses:biome", (-10000, 10000))];
        let mut parameters = BiomeParameters::parse(&table(&rows)).expect("parsed");
        let moved = parameters.rebind(|_| None);
        assert_eq!(moved.len(), 1);
        assert_eq!(moved[0].now, None);
    }

    #[test]
    fn a_malformed_table_is_refused_with_the_line_that_is_wrong() {
        assert!(matches!(
            BiomeParameters::parse("1\tminecraft:plains"),
            Err(ParametersError::NoHeader)
        ));
        assert!(matches!(
            BiomeParameters::parse(HEADER),
            Err(ParametersError::Empty)
        ));
        let short = format!("{HEADER}\n1\tminecraft:plains\t0");
        assert!(matches!(
            BiomeParameters::parse(&short),
            Err(ParametersError::Malformed { line: 2, .. })
        ));
        let backwards = table(&[row(1, "minecraft:plains", (5000, -5000))]);
        assert!(matches!(
            BiomeParameters::parse(&backwards),
            Err(ParametersError::Malformed { line: 2, .. })
        ));
    }
}
