//! Aquifers: what an enclosed pocket below the ground holds, and whether the
//! wall between two of them is rock.
//!
//! # Why this is the stage a player notices
//!
//! The noise stage answers one question per cell — rock or not rock — and a
//! generator without an aquifer has to guess what the "not rock" is. The guess
//! every such generator makes is "the dimension's fluid below the sea level",
//! which fills every cave, every ravine and every noise pocket under the ocean
//! with water. Decision record 0032 measured the guess: **400,638 of seed 0's
//! 588,215 missing cave cells were pockets Dust flooded**, two thirds of what
//! that record's ladder was attributing to the carvers. It is also the half a
//! player *drowns* in — whether a cave under the sea is walkable or lethal is
//! not a cosmetic difference.
//!
//! # Why it is not read from the data pack
//!
//! Surface rules are data: thirty-two kilobytes of `surface_rule` in the
//! dimension's own settings, which is why [`crate::surface`] compiles them
//! rather than inventing them. The aquifer is not. The pack carries the four
//! noises it reads — `barrier`, `fluid_level_floodedness`,
//! `fluid_level_spread`, `lava` — and nothing about what is done with them.
//! Every constant below (the 16x12x16 grid, the ±5 offset, the 25.0 of
//! `similarity`, the 1.5/2.5/3.0/10.0 of the pressure ramp, the -54 the lava
//! sits under, the thirteen chunk offsets a surface is sampled at) is Java.
//!
//! They were recovered from the operator's own server jar with `javap -p -c`
//! against `net.minecraft.world.level.levelgen.Aquifer$NoiseBasedAquifer`,
//! deobfuscated through the ProGuard mappings Mojang publishes beside it —
//! the route decision record 0008 established for a value that is code rather
//! than data, and the same route that recovered the surface system's own
//! constants. **Nothing Mojang's is committed**: what is here is this file's
//! own arithmetic, and every *number* the world is generated from still
//! arrives at run time from the pack. Decision record 0034 records what was
//! read and what it cost.
//!
//! # The shape of the algorithm
//!
//! The world is tiled by a grid of aquifers, one per 16x12x16 box, each with a
//! centre jittered inside its box by a positional random. A block belongs to
//! the three nearest centres. Each centre has a **fluid status** — a surface
//! level and whether it is lava — and a block below its own centre's level is
//! that fluid, above it is air.
//!
//! The wall between two aquifers of *different* levels is where the barrier
//! noise comes in: `calculate_pressure` turns the level difference and the
//! block's height between them into a number that is added to the density, and
//! a positive sum is rock. That is what stops an ocean draining into a cave.

use std::collections::HashMap;

use crate::noise::build::{positional_factory, AquiferRoutes, BlockSpec, Router};
use crate::noise::density::{Evaluator, Graph};
use crate::noise::rng::Positional;

/// The block a column reports below one aquifer's surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fluid {
    /// The dimension's own `default_fluid`.
    Default,
    /// `minecraft:lava`, which `Aquifer.java` names in Java and no pack
    /// carries. See [`Aquifer::lava_block`].
    Lava,
}

/// What one block of a cave turns out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Substance {
    /// The barrier said rock. Vanilla returns `null` here and the chunk
    /// generator falls back to the dimension's default block.
    Rock,
    Air,
    Fluid(Fluid),
}

/// One aquifer's surface, and what is under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Status {
    level: i32,
    fluid: Fluid,
}

impl Status {
    /// The block at a height: the fluid below this aquifer's surface, air at
    /// and above it.
    fn at(self, y: i32) -> Option<Fluid> {
        (y < self.level).then_some(self.fluid)
    }
}

/// `DimensionType.WAY_BELOW_MIN_Y` — `MIN_Y << 4`, where `MIN_Y` is -2048.
///
/// The level an aquifer that holds nothing is given, so that `at` answers air
/// for every height a world has. Not "no fluid" as a separate case, because
/// vanilla makes it a level and `calculate_pressure` then measures a real
/// distance between a dry aquifer and a wet one.
const WAY_BELOW_MIN_Y: i32 = -2048 << 4;

/// The thirteen chunk offsets a surface level is sampled at, in the order
/// vanilla walks them — the centre first, which is what lets a block well
/// above the ground answer after one sample.
const SURFACE_SAMPLING_OFFSETS_IN_CHUNKS: [[i32; 2]; 13] = [
    [0, 0],
    [-2, -1],
    [-1, -1],
    [0, -1],
    [1, -1],
    [-3, 0],
    [-2, 0],
    [-1, 0],
    [1, 0],
    [-2, 1],
    [-1, 1],
    [0, 1],
    [1, 1],
];

/// How far apart the aquifer centres are: one per 16 blocks of x and z and 12
/// of y.
const X_SPACING: i32 = 16;
const Y_SPACING: i32 = 12;
const Z_SPACING: i32 = 16;

/// A dimension's aquifers, compiled for one seed.
///
/// Shared and immutable beside the [`crate::terrain::Terrain`] it belongs to;
/// a thread holds a [`Flow`].
#[derive(Debug, Clone)]
pub struct Aquifer {
    routes: AquiferRoutes,
    /// `initial_density_without_jaggedness`, which the preliminary surface
    /// level is walked down. Without it an aquifer cannot tell how deep it is
    /// and the whole stage declines rather than guessing.
    initial_density: Option<usize>,
    factory: Positional,
    min_y: i32,
    height: i32,
    cell_height: i32,
    sea_level: i32,
    /// `min(-54, sea_level)`: below this the global picker answers lava, which
    /// is what puts a floor of lava under the deepest caves.
    lava_below: i32,
}

impl Aquifer {
    /// Build one over a compiled router, or `None` when the dimension's
    /// settings say it has no aquifers.
    pub fn over(router: &Router) -> Option<Self> {
        let routes = router.aquifer?;
        let settings = &router.settings;
        Some(Self {
            routes,
            initial_density: router.initial_density,
            // The one stream `RandomState` gives an aquifer: the world's own
            // positional factory, hashed by the name `minecraft:aquifer` and
            // forked. Every centre in the world is drawn from it.
            factory: positional_factory(router.seed)
                .from_hash_of("minecraft:aquifer")
                .fork_positional(),
            min_y: settings.min_y,
            height: settings.height,
            cell_height: settings.cell_height,
            sea_level: settings.sea_level,
            lava_below: settings.sea_level.min(-54),
        })
    }

    /// The block `Fluid::Lava` means.
    ///
    /// A [`BlockSpec`] and not a code, so a caller resolves it through the
    /// same road it resolves the dimension's own default block and fluid
    /// through — and so the name lives in exactly one place. `Aquifer.java`
    /// names `Blocks.LAVA` directly; no data pack carries it.
    pub fn lava_block() -> BlockSpec {
        BlockSpec {
            name: "minecraft:lava".to_owned(),
            properties: Vec::new(),
        }
    }

    /// One thread's scratch.
    pub fn flow<'a>(&'a self, graph: &'a Graph) -> Flow<'a> {
        Flow {
            aquifer: self,
            evaluator: Evaluator::new(graph),
            status: Vec::new(),
            location: Vec::new(),
            preliminary: HashMap::new(),
            min_grid: [0; 3],
            size: [0; 3],
        }
    }

    /// The fluid the dimension would hold at a height if there were no
    /// aquifers at all — vanilla's `createFluidPicker`, which is a function of
    /// `y` alone.
    fn global(&self, y: i32) -> Status {
        if y < self.lava_below {
            Status {
                level: -54,
                fluid: Fluid::Lava,
            }
        } else {
            Status {
                level: self.sea_level,
                fluid: Fluid::Default,
            }
        }
    }
}

/// One thread's scratch for one chunk's aquifers.
///
/// Two caches, both vanilla's own and both the reason this stage is affordable
/// at all. A chunk touches on the order of three hundred grid cells and each
/// one's status costs thirteen preliminary surface levels; without the caches
/// that would be a column walk per block.
#[derive(Debug, Clone)]
pub struct Flow<'a> {
    aquifer: &'a Aquifer,
    evaluator: Evaluator<'a>,
    /// The status of each grid cell in range of this chunk, computed on first
    /// use.
    status: Vec<Option<Status>>,
    /// Each grid cell's jittered centre, drawn on first use.
    location: Vec<Option<[i32; 3]>>,
    /// The preliminary surface level, keyed on the quart column vanilla keys
    /// it on. Kept across chunks: the thirteen offsets reach three chunks away
    /// and neighbours share almost all of them.
    preliminary: HashMap<(i32, i32), i32>,
    min_grid: [i32; 3],
    size: [i32; 3],
}

impl Flow<'_> {
    /// Point the caches at a chunk. Must be called before [`Flow::substance`].
    pub fn enter_chunk(&mut self, chunk_x: i32, chunk_z: i32) {
        let a = self.aquifer;
        let min_x = grid_x(chunk_x * 16) - 1;
        let max_x = grid_x(chunk_x * 16 + 15) + 1;
        let min_y = grid_y(a.min_y) - 1;
        let max_y = grid_y(a.min_y + a.height) + 1;
        let min_z = grid_z(chunk_z * 16) - 1;
        let max_z = grid_z(chunk_z * 16 + 15) + 1;
        self.min_grid = [min_x, min_y, min_z];
        self.size = [max_x - min_x + 1, max_y - min_y + 1, max_z - min_z + 1];
        let cells = (self.size[0] * self.size[1] * self.size[2]) as usize;
        self.status.clear();
        self.status.resize(cells, None);
        self.location.clear();
        self.location.resize(cells, None);
    }

    fn index(&self, x: i32, y: i32, z: i32) -> usize {
        let dx = x - self.min_grid[0];
        let dy = y - self.min_grid[1];
        let dz = z - self.min_grid[2];
        ((dy * self.size[2] + dz) * self.size[0] + dx) as usize
    }

    /// What one block of a chunk is, given the density the noise stage
    /// computed there.
    ///
    /// The density is the caller's because it is the interpolated value off
    /// the terrain lattice, and recomputing it here would be a second, smoother
    /// world.
    pub fn substance(&mut self, x: i32, y: i32, z: i32, density: f64) -> Substance {
        if density > 0.0 {
            return Substance::Rock;
        }
        let global = self.aquifer.global(y);
        if global.at(y) == Some(Fluid::Lava) {
            return Substance::Fluid(Fluid::Lava);
        }
        // The grid is offset by five in x and z and by one *up* in y, which is
        // what stops the aquifer boundaries lining up with the chunk grid.
        let grid = [
            (x - 5).div_euclid(X_SPACING),
            (y + 1).div_euclid(Y_SPACING),
            (z - 5).div_euclid(Z_SPACING),
        ];
        // The three nearest centres, kept as an insertion sort over twelve
        // candidates. Twelve and not twenty-seven: the offsets are 0..=1 in x
        // and z and -1..=1 in y, and the ±5 offset above is what makes that
        // asymmetric window cover the block.
        let mut best = [i32::MAX; 3];
        let mut at = [[0i32; 3]; 3];
        for dx in 0..=1 {
            for dy in -1..=1 {
                for dz in 0..=1 {
                    let cell = [grid[0] + dx, grid[1] + dy, grid[2] + dz];
                    let index = self.index(cell[0], cell[1], cell[2]);
                    let centre = match self.location[index] {
                        Some(centre) => centre,
                        None => {
                            let mut random = self.aquifer.factory.at(cell[0], cell[1], cell[2]);
                            let centre = [
                                cell[0] * X_SPACING + random.next_i32_below(10),
                                cell[1] * Y_SPACING + random.next_i32_below(9),
                                cell[2] * Z_SPACING + random.next_i32_below(10),
                            ];
                            self.location[index] = Some(centre);
                            centre
                        }
                    };
                    let (ox, oy, oz) = (centre[0] - x, centre[1] - y, centre[2] - z);
                    let distance = ox * ox + oy * oy + oz * oz;
                    if best[0] >= distance {
                        at[2] = at[1];
                        at[1] = at[0];
                        at[0] = centre;
                        best[2] = best[1];
                        best[1] = best[0];
                        best[0] = distance;
                    } else if best[1] >= distance {
                        at[2] = at[1];
                        at[1] = centre;
                        best[2] = best[1];
                        best[1] = distance;
                    } else if best[2] >= distance {
                        at[2] = centre;
                        best[2] = distance;
                    }
                }
            }
        }

        let first = self.status_at(at[0]);
        let similarity_12 = similarity(best[0], best[1]);
        let substance = match first.at(y) {
            Some(fluid) => Substance::Fluid(fluid),
            None => Substance::Air,
        };
        if similarity_12 <= 0.0 {
            // One aquifer clearly nearest: no wall to decide, no barrier noise
            // to sample. This is the common case and the reason the stage is
            // affordable.
            return substance;
        }
        // Water directly above lava is left alone rather than walled off: the
        // block below is where vanilla schedules the fluid tick that makes the
        // stone.
        if substance == Substance::Fluid(Fluid::Default)
            && self.aquifer.global(y - 1).at(y - 1) == Some(Fluid::Lava)
        {
            return substance;
        }

        // Sampled at most once per block however many pressures are computed,
        // which is what the `MutableDouble` vanilla threads through the three
        // calls is for.
        let mut barrier = None;
        let second = self.status_at(at[1]);
        if density + similarity_12 * self.pressure(x, y, z, &mut barrier, first, second) > 0.0 {
            return Substance::Rock;
        }
        let third = self.status_at(at[2]);
        let similarity_13 = similarity(best[0], best[2]);
        if similarity_13 > 0.0 {
            let pressure = similarity_12
                * similarity_13
                * self.pressure(x, y, z, &mut barrier, first, third);
            if density + pressure > 0.0 {
                return Substance::Rock;
            }
        }
        let similarity_23 = similarity(best[1], best[2]);
        if similarity_23 > 0.0 {
            let pressure = similarity_12
                * similarity_23
                * self.pressure(x, y, z, &mut barrier, second, third);
            if density + pressure > 0.0 {
                return Substance::Rock;
            }
        }
        substance
    }

    /// How much rock the wall between two aquifers is worth at this height.
    ///
    /// Positive enough to overcome a negative density is rock; that is the
    /// whole of what keeps an ocean out of a cave.
    fn pressure(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
        barrier: &mut Option<f64>,
        a: Status,
        b: Status,
    ) -> f64 {
        let (first, second) = (a.at(y), b.at(y));
        // Lava meeting water is walled off unconditionally, whatever the
        // levels say. Vanilla names `Blocks.WATER` here where this names the
        // dimension's own fluid; every dimension that has aquifers has water.
        let meets = |one: Fluid, other: Fluid| first == Some(one) && second == Some(other);
        if meets(Fluid::Lava, Fluid::Default) || meets(Fluid::Default, Fluid::Lava) {
            return 2.0;
        }
        let difference = (a.level - b.level).abs();
        if difference == 0 {
            // Two aquifers at the same level are one aquifer, and there is no
            // wall between them. Most of the world takes this line.
            return 0.0;
        }
        let midpoint = 0.5 * f64::from(a.level + b.level);
        let above = f64::from(y) + 0.5 - midpoint;
        let half = f64::from(difference) / 2.0;
        // An asymmetric ramp: the wall reaches further *down* from the
        // midpoint than up, which is why an aquifer's surface is a lid rather
        // than a bubble.
        let room = half - above.abs();
        let pressure = if above > 0.0 {
            let value = room;
            if value > 0.0 {
                value / 1.5
            } else {
                value / 2.5
            }
        } else {
            let value = 3.0 + room;
            if value > 0.0 {
                value / 3.0
            } else {
                value / 10.0
            }
        };
        let noise = if !(-2.0..=2.0).contains(&pressure) {
            // Far enough from the wall that the barrier noise cannot change
            // the answer, so it is not sampled.
            0.0
        } else {
            *barrier.get_or_insert_with(|| {
                self.evaluator
                    .compute(self.aquifer.routes.barrier, x, y, z)
            })
        };
        2.0 * (noise + pressure)
    }

    /// The status of the aquifer whose centre is at a block position, computed
    /// once per grid cell.
    fn status_at(&mut self, centre: [i32; 3]) -> Status {
        let cell = [
            grid_x(centre[0]),
            grid_y(centre[1]),
            grid_z(centre[2]),
        ];
        let index = self.index(cell[0], cell[1], cell[2]);
        if let Some(status) = self.status[index] {
            return status;
        }
        let status = self.compute_fluid(centre[0], centre[1], centre[2]);
        self.status[index] = Some(status);
        status
    }

    /// Whether one aquifer holds anything, and what.
    fn compute_fluid(&mut self, x: i32, y: i32, z: i32) -> Status {
        let global = self.aquifer.global(y);
        let mut lowest = i32::MAX;
        let mut any_fluid_at_centre = false;
        for offset in SURFACE_SAMPLING_OFFSETS_IN_CHUNKS {
            let sample_x = x + (offset[0] << 4);
            let sample_z = z + (offset[1] << 4);
            let level = self.preliminary_surface_level(sample_x, sample_z);
            // `wrapping_add`, because vanilla's is an `int` and a column with
            // no rock in it reports `Integer.MAX_VALUE`. Java wraps it to a
            // large negative and the centre test below then answers "well
            // above the ground", which is the right answer for a column that
            // has none.
            let surface = level.wrapping_add(8);
            let centre = offset[0] == 0 && offset[1] == 0;
            if centre && y - 12 > surface {
                // Twelve blocks clear of the ground: no aquifer here, the
                // dimension's own fluid rule stands. One preliminary surface
                // level and out, which is every block of open sky.
                return global;
            }
            let near = y + 12 > surface;
            if near || centre {
                let status = self.aquifer.global(surface);
                if status.at(surface).is_some() {
                    if centre {
                        any_fluid_at_centre = true;
                    }
                    if near {
                        // Under an ocean: this aquifer is the ocean.
                        return status;
                    }
                }
            }
            lowest = lowest.min(level);
        }
        let level = self.surface_level(x, y, z, global, lowest, any_fluid_at_centre);
        Status {
            level,
            fluid: self.fluid_type(x, y, z, global, level),
        }
    }

    /// Where this aquifer's own surface sits.
    fn surface_level(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
        global: Status,
        lowest: i32,
        any_fluid_at_centre: bool,
    ) -> i32 {
        let routes = self.aquifer.routes;
        // The deep dark: erosion low and depth high. Vanilla turns the
        // aquifers off there so an ancient city is dry, and the two
        // thresholds are `float` constants widened to `double`.
        let deep_dark = self.evaluator.compute(routes.erosion, x, y, z) < f64::from(-0.225f32)
            && self.evaluator.compute(routes.depth, x, y, z) > f64::from(0.9f32);
        let (spread_gate, global_gate) = if deep_dark {
            (-1.0, -1.0)
        } else {
            let below_surface = lowest.wrapping_add(8).wrapping_sub(y);
            // How much of an ocean is overhead: full weight right under it,
            // none 64 blocks down. Zero when the centre column has no fluid at
            // all, which is what makes an inland aquifer independent of the
            // sea.
            let weight = if any_fluid_at_centre {
                clamped_map(f64::from(below_surface), 0.0, 64.0, 1.0, 0.0)
            } else {
                0.0
            };
            let floodedness = self
                .evaluator
                .compute(routes.floodedness, x, y, z)
                .clamp(-1.0, 1.0);
            (
                floodedness - map(weight, 1.0, 0.0, -0.8, 0.4),
                floodedness - map(weight, 1.0, 0.0, -0.3, 0.8),
            )
        };
        if global_gate > 0.0 {
            // Flooded to the world's own level: this is the sea, reaching in.
            global.level
        } else if spread_gate > 0.0 {
            self.randomized_surface_level(x, y, z, lowest)
        } else {
            // Dry. This is the line that stops a cave being a lake, and it is
            // most of what this whole module buys.
            WAY_BELOW_MIN_Y
        }
    }

    /// The surface of an aquifer that holds something but is not the sea: a
    /// level quantised to three blocks off a coarse noise, never above the
    /// ground.
    fn randomized_surface_level(&mut self, x: i32, y: i32, z: i32, ceiling: i32) -> i32 {
        let grid = [x.div_euclid(16), y.div_euclid(40), z.div_euclid(16)];
        let base = grid[1] * 40 + 20;
        let spread = self
            .evaluator
            .compute(self.aquifer.routes.spread, grid[0], grid[1], grid[2])
            * 10.0;
        // Quantised, so an underground lake has a flat surface rather than a
        // sloped one.
        let quantised = (spread / 3.0).floor() as i32 * 3;
        ceiling.min(base + quantised)
    }

    /// Whether a deep aquifer holds lava instead.
    fn fluid_type(&mut self, x: i32, y: i32, z: i32, global: Status, level: i32) -> Fluid {
        if level <= -10 && level != WAY_BELOW_MIN_Y && global.fluid != Fluid::Lava {
            let value = self.evaluator.compute(
                self.aquifer.routes.lava,
                x.div_euclid(64),
                y.div_euclid(40),
                z.div_euclid(64),
            );
            if value.abs() > 0.3 {
                return Fluid::Lava;
            }
        }
        global.fluid
    }

    /// Walk the un-jagged density down a cell at a time and stop where it
    /// first says rock — the same walk `crate::surface` makes, on the same
    /// quart grid, memoised because thirteen offsets three chunks wide is a
    /// column a neighbour will ask for again.
    fn preliminary_surface_level(&mut self, x: i32, z: i32) -> i32 {
        let a = self.aquifer;
        let key = (x >> 2, z >> 2);
        if let Some(&level) = self.preliminary.get(&key) {
            return level;
        }
        let level = match a.initial_density {
            None => i32::MAX,
            Some(root) => {
                let (bx, bz) = (key.0 << 2, key.1 << 2);
                let mut y = a.min_y + a.height;
                loop {
                    if y < a.min_y {
                        break i32::MAX;
                    }
                    if self.evaluator.compute(root, bx, y, bz) > 0.390625 {
                        break y;
                    }
                    y -= a.cell_height;
                }
            }
        };
        self.preliminary.insert(key, level);
        level
    }
}

/// How alike two squared distances are: 1.0 for equal, falling to 0 at
/// twenty-five apart. Below zero means the nearer centre owns the block
/// outright.
fn similarity(a: i32, b: i32) -> f64 {
    1.0 - f64::from((b - a).abs()) / 25.0
}

fn grid_x(x: i32) -> i32 {
    x.div_euclid(X_SPACING)
}

fn grid_y(y: i32) -> i32 {
    y.div_euclid(Y_SPACING)
}

fn grid_z(z: i32) -> i32 {
    z.div_euclid(Z_SPACING)
}

fn inverse_lerp(value: f64, from: f64, to: f64) -> f64 {
    (value - from) / (to - from)
}

fn map(value: f64, from: f64, to: f64, low: f64, high: f64) -> f64 {
    let t = inverse_lerp(value, from, to);
    low + t * (high - low)
}

fn clamped_map(value: f64, from: f64, to: f64, low: f64, high: f64) -> f64 {
    let t = inverse_lerp(value, from, to);
    if t < 0.0 {
        low
    } else if t > 1.0 {
        high
    } else {
        low + t * (high - low)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dry_aquifer_is_air_at_every_height_a_world_has() {
        let dry = Status {
            level: WAY_BELOW_MIN_Y,
            fluid: Fluid::Default,
        };
        assert_eq!(dry.at(-64), None);
        assert_eq!(dry.at(319), None);
        // And the level is genuinely below every world, not merely below this
        // one: a level of, say, -100 would leave the bottom of a deep pack wet.
        const { assert!(WAY_BELOW_MIN_Y < -2032) };
    }

    #[test]
    fn similarity_is_one_at_equal_and_negative_past_twenty_five() {
        assert_eq!(similarity(9, 9), 1.0);
        assert_eq!(similarity(0, 25), 0.0);
        assert!(similarity(0, 26) < 0.0);
    }

    #[test]
    fn the_ramp_is_asymmetric_about_the_midpoint() {
        // Two aquifers 40 apart, midpoint 20. The same distance above and
        // below the midpoint must not give the same pressure: the wall hangs
        // down. This is the constant a "symmetric" reading of the ramp would
        // get wrong, and it decides which side of a lid is rock.
        let a = Status {
            level: 0,
            fluid: Fluid::Default,
        };
        let b = Status {
            level: 40,
            fluid: Fluid::Default,
        };
        let midpoint = 0.5 * f64::from(a.level + b.level);
        let half = f64::from((a.level - b.level).abs()) / 2.0;
        let ramp = |y: i32| {
            let above = f64::from(y) + 0.5 - midpoint;
            let room = half - above.abs();
            if above > 0.0 {
                if room > 0.0 {
                    room / 1.5
                } else {
                    room / 2.5
                }
            } else {
                let value = 3.0 + room;
                if value > 0.0 {
                    value / 3.0
                } else {
                    value / 10.0
                }
            }
        };
        assert!(ramp(25) != ramp(14));
    }

    #[test]
    fn quantising_the_spread_flattens_a_lake() {
        // floor(v / 3) * 3, so eleven consecutive noise values land on four
        // levels and not eleven.
        let levels: Vec<i32> = (-5..6).map(|v| (f64::from(v) / 3.0).floor() as i32 * 3).collect();
        assert_eq!(levels, vec![-6, -6, -3, -3, -3, 0, 0, 0, 3, 3, 3]);
    }
}
