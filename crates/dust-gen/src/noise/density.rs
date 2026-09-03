//! Density functions: the small language Minecraft's worldgen is written in.
//!
//! A density function is a pure function of a block position. Vanilla defines
//! them as JSON in its own data pack, references them by name, and wires six of
//! them together into the climate half of the noise router — temperature,
//! vegetation, continentalness, erosion, depth and weirdness. This module is
//! the evaluator for that language and holds none of the language's content:
//! the graph is built at run time from whatever data pack the operator has, so
//! a pack that reshapes the overworld reshapes Dust's biomes too.
//!
//! # Why an arena rather than a tree of boxes
//!
//! Vanilla's climate graph is a DAG, not a tree: `shift_x` appears in five
//! shifted noises and `overworld/continents` in both the offset spline and the
//! biome sample. Held as a tree it would be *evaluated* five times. Held as
//! indices into one `Vec`, each node is evaluated once per point and the memo
//! below is a flat array rather than a map.
//!
//! # The two caches, and why they are not an optimisation
//!
//! `flat_cache` and `cache_2d` are nodes in vanilla's own language, and what
//! they mean is "this value does not depend on y". Honouring them is what makes
//! a column of 24 biome cells cost one continentalness sample instead of 24.
//! The point memo below is the other half: within one point, a node reached
//! twice is computed once. Both are exact — every node here is a pure function
//! of the point — so neither can change an answer, only the time it takes.

use super::perlin::NormalNoise;

/// One node of a compiled density-function graph.
///
/// Indices refer to earlier entries in the same arena, so evaluation never has
/// to guard against a cycle: the builder cannot produce one.
#[derive(Debug, Clone, Copy)]
pub enum Node {
    Constant(f64),
    Abs(usize),
    Add(usize, usize),
    Mul(usize, usize),
    Min(usize, usize),
    Max(usize, usize),
    /// Old-world blending, which a world with no old chunks in it answers
    /// with a constant. Kept as its own node rather than folded to 1.0 and 0.0
    /// so the graph still says what vanilla's said.
    BlendAlpha,
    BlendOffset,
    /// `cache_once` and `interpolated`: markers with no effect at a point.
    Passthrough(usize),
    /// `flat_cache` and `cache_2d`: markers that promise y-independence.
    ColumnCache(usize),
    /// `shift_a`: the offset noise sampled at (x, 0, z).
    ShiftA(usize),
    /// `shift_b`: the offset noise sampled at (z, x, 0). Not a typo — that
    /// rotation is how one noise yields two independent-looking offsets.
    ShiftB(usize),
    ShiftedNoise {
        noise: usize,
        shift_x: usize,
        shift_y: usize,
        shift_z: usize,
        xz_scale: f64,
        y_scale: f64,
    },
    Noise {
        noise: usize,
        xz_scale: f64,
        y_scale: f64,
    },
    Spline(usize),
    YClampedGradient {
        from_y: f64,
        to_y: f64,
        from_value: f64,
        to_value: f64,
    },
}

/// A point on a cubic spline: where it is, what it is worth there, and how
/// steeply it leaves.
#[derive(Debug, Clone)]
pub struct SplinePoint {
    pub location: f32,
    pub value: SplineValue,
    pub derivative: f32,
}

/// A spline's value at a point is either a number or another spline.
#[derive(Debug, Clone)]
pub enum SplineValue {
    Constant(f32),
    Nested(usize),
}

/// A spline over one density function.
#[derive(Debug, Clone)]
pub struct Spline {
    /// The node whose value picks the interval.
    pub coordinate: usize,
    pub points: Vec<SplinePoint>,
}

/// A compiled graph, its splines, and the noises it names.
#[derive(Debug, Clone)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub splines: Vec<Spline>,
    pub noises: Vec<NormalNoise>,
}

/// A graph plus the scratch space one thread needs to evaluate it.
///
/// Separate from [`Graph`] because the graph is shared and immutable while the
/// memo is per-thread: two threads generating two chunks share every noise
/// table and nothing else.
#[derive(Debug, Clone)]
pub struct Evaluator<'a> {
    graph: &'a Graph,
    point_memo: Vec<f64>,
    point_stamp: Vec<u64>,
    point_generation: u64,
    column_memo: Vec<f64>,
    column_stamp: Vec<u64>,
    column_generation: u64,
    column: (i32, i32),
}

impl<'a> Evaluator<'a> {
    pub fn new(graph: &'a Graph) -> Self {
        let size = graph.nodes.len();
        Self {
            graph,
            point_memo: vec![0.0; size],
            point_stamp: vec![0; size],
            point_generation: 0,
            column_memo: vec![0.0; size],
            column_stamp: vec![0; size],
            column_generation: 0,
            column: (i32::MIN, i32::MIN),
        }
    }

    /// Evaluate `root` at a block position.
    pub fn compute(&mut self, root: usize, x: i32, y: i32, z: i32) -> f64 {
        self.start_point(x, z);
        self.eval(root, x, y, z)
    }

    /// Evaluate several roots at one point, sharing everything they share.
    ///
    /// The six climate functions are not six graphs; they are six views of one,
    /// and `shift_x` alone is under five of them. Evaluating them one call at a
    /// time would compute it five times.
    pub fn compute_all(&mut self, roots: &[usize], x: i32, y: i32, z: i32, out: &mut [f64]) {
        debug_assert_eq!(roots.len(), out.len());
        self.start_point(x, z);
        for (slot, &root) in out.iter_mut().zip(roots) {
            *slot = self.eval(root, x, y, z);
        }
    }

    fn start_point(&mut self, x: i32, z: i32) {
        self.point_generation += 1;
        if self.column != (x, z) {
            self.column = (x, z);
            self.column_generation += 1;
        }
    }

    fn eval(&mut self, index: usize, x: i32, y: i32, z: i32) -> f64 {
        if self.point_stamp[index] == self.point_generation {
            return self.point_memo[index];
        }
        let value = self.eval_uncached(index, x, y, z);
        self.point_stamp[index] = self.point_generation;
        self.point_memo[index] = value;
        value
    }

    fn eval_uncached(&mut self, index: usize, x: i32, y: i32, z: i32) -> f64 {
        match self.graph.nodes[index] {
            Node::Constant(value) => value,
            Node::Abs(argument) => self.eval(argument, x, y, z).abs(),
            Node::Add(a, b) => self.eval(a, x, y, z) + self.eval(b, x, y, z),
            Node::Mul(a, b) => {
                // Minecraft short-circuits a zero first argument rather than
                // multiplying. That is not the same as multiplying when the
                // second argument is infinite or NaN, so it is reproduced.
                let first = self.eval(a, x, y, z);
                if first == 0.0 {
                    0.0
                } else {
                    first * self.eval(b, x, y, z)
                }
            }
            Node::Min(a, b) => self.eval(a, x, y, z).min(self.eval(b, x, y, z)),
            Node::Max(a, b) => self.eval(a, x, y, z).max(self.eval(b, x, y, z)),
            Node::BlendAlpha => 1.0,
            Node::BlendOffset => 0.0,
            Node::Passthrough(argument) => self.eval(argument, x, y, z),
            Node::ColumnCache(argument) => {
                if self.column_stamp[index] == self.column_generation {
                    return self.column_memo[index];
                }
                let value = self.eval(argument, x, y, z);
                self.column_stamp[index] = self.column_generation;
                self.column_memo[index] = value;
                value
            }
            Node::ShiftA(noise) => self.shift(noise, f64::from(x), 0.0, f64::from(z)),
            Node::ShiftB(noise) => self.shift(noise, f64::from(z), f64::from(x), 0.0),
            Node::ShiftedNoise {
                noise,
                shift_x,
                shift_y,
                shift_z,
                xz_scale,
                y_scale,
            } => {
                let sx = f64::from(x) * xz_scale + self.eval(shift_x, x, y, z);
                let sy = f64::from(y) * y_scale + self.eval(shift_y, x, y, z);
                let sz = f64::from(z) * xz_scale + self.eval(shift_z, x, y, z);
                self.graph.noises[noise].value(sx, sy, sz)
            }
            Node::Noise {
                noise,
                xz_scale,
                y_scale,
            } => self.graph.noises[noise].value(
                f64::from(x) * xz_scale,
                f64::from(y) * y_scale,
                f64::from(z) * xz_scale,
            ),
            Node::Spline(spline) => f64::from(self.spline(spline, x, y, z)),
            Node::YClampedGradient {
                from_y,
                to_y,
                from_value,
                to_value,
            } => clamped_map(f64::from(y), from_y, to_y, from_value, to_value),
        }
    }

    /// `shift_a` and `shift_b` share one body; only the coordinates they feed
    /// it differ.
    fn shift(&self, noise: usize, x: f64, y: f64, z: f64) -> f64 {
        self.graph.noises[noise].value(x * 0.25, y * 0.25, z * 0.25) * 4.0
    }

    /// Evaluate a cubic spline. Deliberately `f32`, because Minecraft's is.
    ///
    /// Widening this to `f64` would be more accurate and would be wrong: the
    /// value lands in a biome parameter that is quantised by ten thousand, and
    /// a cell near a boundary would fall the other side of it.
    fn spline(&mut self, index: usize, x: i32, y: i32, z: i32) -> f32 {
        let spline = &self.graph.splines[index];
        let coordinate = spline.coordinate;
        let count = spline.points.len();
        let point = self.eval(coordinate, x, y, z) as f32;

        // Read through the arena rather than gathering the locations into a
        // `Vec` first. This runs once per spline per climate sample and there
        // are 124,416 biome cells in a 9x9: a heap allocation here is one per
        // sample, forever, for a slice that is only ever indexed.
        let start = self.interval_start(index, point);
        let last = count - 1;
        if start < 0 {
            let value = self.spline_value(index, 0, x, y, z);
            let at = self.location(index, 0);
            return linear_extend(point, at, value, self.derivative(index, 0));
        }
        let start = start as usize;
        if start == last {
            let value = self.spline_value(index, last, x, y, z);
            let at = self.location(index, last);
            return linear_extend(point, at, value, self.derivative(index, last));
        }
        let low = self.location(index, start);
        let high = self.location(index, start + 1);
        let t = (point - low) / (high - low);
        let value_low = self.spline_value(index, start, x, y, z);
        let value_high = self.spline_value(index, start + 1, x, y, z);
        let derivative_low = self.derivative(index, start);
        let derivative_high = self.derivative(index, start + 1);
        let n = derivative_low * (high - low) - (value_high - value_low);
        let o = -derivative_high * (high - low) + (value_high - value_low);
        lerp_f32(t, value_low, value_high) + t * (1.0 - t) * lerp_f32(t, n, o)
    }

    fn derivative(&self, spline: usize, point: usize) -> f32 {
        self.graph.splines[spline].points[point].derivative
    }

    fn location(&self, spline: usize, point: usize) -> f32 {
        self.graph.splines[spline].points[point].location
    }

    /// The index of the last point whose location is not greater than `value`,
    /// or -1 when every location is.
    ///
    /// Minecraft's `Mth.binarySearch` over the same predicate, walked against
    /// the arena so nothing is copied out of it first.
    fn interval_start(&self, spline: usize, value: f32) -> i32 {
        let points = &self.graph.splines[spline].points;
        let mut low = 0usize;
        let mut span = points.len();
        while span > 0 {
            let half = span / 2;
            let mid = low + half;
            if value < points[mid].location {
                span = half;
            } else {
                low = mid + 1;
                span -= half + 1;
            }
        }
        low as i32 - 1
    }

    fn spline_value(&mut self, spline: usize, point: usize, x: i32, y: i32, z: i32) -> f32 {
        match self.graph.splines[spline].points[point].value {
            SplineValue::Constant(value) => value,
            SplineValue::Nested(nested) => self.spline(nested, x, y, z),
        }
    }
}

fn linear_extend(point: f32, location: f32, value: f32, derivative: f32) -> f32 {
    if derivative == 0.0 {
        value
    } else {
        value + derivative * (point - location)
    }
}

fn lerp_f32(delta: f32, start: f32, end: f32) -> f32 {
    start + delta * (end - start)
}

fn clamped_map(value: f64, from: f64, to: f64, from_value: f64, to_value: f64) -> f64 {
    let delta = (value - from) / (to - from);
    if delta < 0.0 {
        from_value
    } else if delta > 1.0 {
        to_value
    } else {
        from_value + delta * (to_value - from_value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(nodes: Vec<Node>, splines: Vec<Spline>) -> Graph {
        Graph {
            nodes,
            splines,
            noises: Vec::new(),
        }
    }

    #[test]
    fn the_arithmetic_nodes_do_the_arithmetic() {
        let g = graph(
            vec![
                Node::Constant(3.0),
                Node::Constant(-4.0),
                Node::Add(0, 1),
                Node::Mul(0, 1),
                Node::Abs(1),
                Node::Min(0, 1),
                Node::Max(0, 1),
            ],
            Vec::new(),
        );
        let mut e = Evaluator::new(&g);
        assert_eq!(e.compute(2, 0, 0, 0), -1.0);
        assert_eq!(e.compute(3, 0, 0, 0), -12.0);
        assert_eq!(e.compute(4, 0, 0, 0), 4.0);
        assert_eq!(e.compute(5, 0, 0, 0), -4.0);
        assert_eq!(e.compute(6, 0, 0, 0), 3.0);
    }

    #[test]
    fn a_zero_first_argument_short_circuits_a_multiply() {
        let g = graph(
            vec![
                Node::Constant(0.0),
                Node::Constant(f64::INFINITY),
                Node::Mul(0, 1),
                Node::Mul(1, 0),
            ],
            Vec::new(),
        );
        let mut e = Evaluator::new(&g);
        assert_eq!(e.compute(2, 0, 0, 0), 0.0, "0 * inf is 0 here, not NaN");
        assert!(e.compute(3, 0, 0, 0).is_nan(), "the other order is not");
    }

    #[test]
    fn the_y_gradient_clamps_at_both_ends() {
        let g = graph(
            vec![Node::YClampedGradient {
                from_y: -64.0,
                to_y: 320.0,
                from_value: 1.5,
                to_value: -1.5,
            }],
            Vec::new(),
        );
        let mut e = Evaluator::new(&g);
        assert_eq!(e.compute(0, 0, -1000, 0), 1.5);
        assert_eq!(e.compute(0, 0, 1000, 0), -1.5);
        assert_eq!(e.compute(0, 0, -64, 0), 1.5);
        assert_eq!(e.compute(0, 0, 320, 0), -1.5);
        assert!((e.compute(0, 0, 128, 0) - 0.0).abs() < 0.01);
    }

    #[test]
    fn a_column_cache_holds_across_y_and_is_dropped_across_x() {
        // Watched to fail: with `ColumnCache` replaced by `Passthrough` the
        // first assertion below still passes and the third still passes, so
        // this test earns its place only because of the second.
        let g = graph(
            vec![
                Node::YClampedGradient {
                    from_y: 0.0,
                    to_y: 100.0,
                    from_value: 0.0,
                    to_value: 100.0,
                },
                Node::ColumnCache(0),
            ],
            Vec::new(),
        );
        let mut e = Evaluator::new(&g);
        assert_eq!(e.compute(1, 5, 10, 5), 10.0);
        assert_eq!(
            e.compute(1, 5, 40, 5),
            10.0,
            "a column cache must not notice y changing"
        );
        assert_eq!(
            e.compute(1, 6, 40, 6),
            40.0,
            "and must notice the column changing"
        );
    }

    #[test]
    fn a_spline_interpolates_between_its_points_and_extends_past_them() {
        let g = graph(
            vec![
                // An identity over the range this test asks about, so the
                // spline's coordinate is simply y.
                Node::YClampedGradient {
                    from_y: -1000.0,
                    to_y: 1000.0,
                    from_value: -1000.0,
                    to_value: 1000.0,
                },
                Node::Spline(0),
            ],
            vec![Spline {
                coordinate: 0,
                points: vec![
                    SplinePoint {
                        location: 0.0,
                        value: SplineValue::Constant(0.0),
                        derivative: 0.0,
                    },
                    SplinePoint {
                        location: 10.0,
                        value: SplineValue::Constant(10.0),
                        derivative: 1.0,
                    },
                ],
            }],
        );
        let mut e = Evaluator::new(&g);
        assert_eq!(e.compute(1, 0, 0, 0), 0.0);
        assert_eq!(e.compute(1, 0, 10, 0), 10.0);
        assert_eq!(e.compute(1, 0, -5, 0), 0.0, "a zero derivative holds flat");
        assert_eq!(e.compute(1, 0, 20, 0), 20.0, "a unit derivative extends");
        let middle = e.compute(1, 0, 5, 0);
        assert!((0.0..=10.0).contains(&middle), "{middle} left the interval");
    }

    #[test]
    fn the_interval_search_answers_the_ends_the_way_minecraft_does() {
        let g = graph(
            vec![Node::Constant(0.0)],
            vec![Spline {
                coordinate: 0,
                points: [-1.0f32, 0.0, 1.0]
                    .into_iter()
                    .map(|location| SplinePoint {
                        location,
                        value: SplineValue::Constant(0.0),
                        derivative: 0.0,
                    })
                    .collect(),
            }],
        );
        let e = Evaluator::new(&g);
        assert_eq!(e.interval_start(0, -2.0), -1);
        assert_eq!(e.interval_start(0, -1.0), 0);
        assert_eq!(e.interval_start(0, 0.5), 1);
        assert_eq!(e.interval_start(0, 1.0), 2);
        assert_eq!(e.interval_start(0, 9.0), 2);
    }
}
