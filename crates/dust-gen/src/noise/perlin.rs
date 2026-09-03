//! Perlin noise, the way Minecraft samples it.
//!
//! Three layers, each one a thin wrapper on the last:
//!
//! * [`ImprovedNoise`] is one octave — a permutation table, three offsets, and
//!   a trilinear interpolation between gradient dot products.
//! * [`PerlinNoise`] is a stack of octaves at doubling frequency and halving
//!   weight, each seeded from its own *name* rather than from the stream
//!   position, so an octave with amplitude zero is not built and does not shift
//!   the ones after it.
//! * [`NormalNoise`] is two of those stacks summed, the second sampled at a
//!   slightly different rate, which is what turns Perlin's flat-ish
//!   distribution into something closer to normal.
//!
//! Nothing here is data. The amplitudes and first octave that shape a noise are
//! Mojang's and arrive at run time from the operator's own copy, as
//! [`NoiseParameters`].

use super::rng::{Positional, Xoroshiro};

/// The shape of one noise: which octave it starts at and what each octave is
/// worth.
///
/// Read from the operator's data pack, never compiled in.
#[derive(Debug, Clone, PartialEq)]
pub struct NoiseParameters {
    pub first_octave: i32,
    pub amplitudes: Vec<f64>,
}

/// The 16 gradients Minecraft's improved noise picks between.
///
/// Twelve distinct directions with four repeats, which is Perlin's own trick
/// for making the index a mask rather than a modulo.
const GRADIENT: [[f64; 3]; 16] = [
    [1.0, 1.0, 0.0],
    [-1.0, 1.0, 0.0],
    [1.0, -1.0, 0.0],
    [-1.0, -1.0, 0.0],
    [1.0, 0.0, 1.0],
    [-1.0, 0.0, 1.0],
    [1.0, 0.0, -1.0],
    [-1.0, 0.0, -1.0],
    [0.0, 1.0, 1.0],
    [0.0, -1.0, 1.0],
    [0.0, 1.0, -1.0],
    [0.0, -1.0, -1.0],
    [1.0, 1.0, 0.0],
    [0.0, -1.0, 1.0],
    [-1.0, 1.0, 0.0],
    [0.0, -1.0, -1.0],
];

/// One octave.
#[derive(Debug, Clone)]
pub struct ImprovedNoise {
    xo: f64,
    yo: f64,
    zo: f64,
    permutation: [u8; 256],
}

impl ImprovedNoise {
    /// Draw an octave out of a stream: three offsets, then a Fisher-Yates
    /// shuffle of 0..256.
    ///
    /// The draw order is the whole contract. Three doubles and then exactly 256
    /// bounded ints, in that sequence — a shuffle written the other way round
    /// (`nextInt(i + 1)` counting up) consumes the same number of draws and
    /// produces a different table.
    pub fn new(random: &mut Xoroshiro) -> Self {
        let xo = random.next_f64() * 256.0;
        let yo = random.next_f64() * 256.0;
        let zo = random.next_f64() * 256.0;
        let mut permutation: [u8; 256] = std::array::from_fn(|i| i as u8);
        for i in 0..256usize {
            let j = random.next_i32_below(256 - i as i32) as usize;
            permutation.swap(i, i + j);
        }
        Self {
            xo,
            yo,
            zo,
            permutation,
        }
    }

    fn p(&self, index: i32) -> i32 {
        i32::from(self.permutation[(index & 0xFF) as usize])
    }

    /// The value at a point, with no y quantisation.
    pub fn noise(&self, x: f64, y: f64, z: f64) -> f64 {
        self.noise_with_y_step(x, y, z, 0.0, 0.0)
    }

    /// The value at a point, optionally snapping y to a step.
    ///
    /// The step exists for the 3D terrain noises and is 0 for every climate
    /// noise, but it is here rather than left out because a function that
    /// quietly ignores an argument is the shape of defect this project keeps
    /// finding.
    pub fn noise_with_y_step(&self, x: f64, y: f64, z: f64, y_scale: f64, y_max: f64) -> f64 {
        let dx = x + self.xo;
        let dy = y + self.yo;
        let dz = z + self.zo;
        let grid_x = dx.floor();
        let grid_y = dy.floor();
        let grid_z = dz.floor();
        let delta_x = dx - grid_x;
        let delta_y = dy - grid_y;
        let delta_z = dz - grid_z;
        let step = if y_scale != 0.0 {
            let bounded = if y_max >= 0.0 && y_max < delta_y {
                y_max
            } else {
                delta_y
            };
            (bounded / y_scale + 1.0E-7_f32 as f64).floor() * y_scale
        } else {
            0.0
        };
        self.sample_and_lerp(
            grid_x as i32,
            grid_y as i32,
            grid_z as i32,
            delta_x,
            delta_y - step,
            delta_z,
            delta_y,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn sample_and_lerp(
        &self,
        grid_x: i32,
        grid_y: i32,
        grid_z: i32,
        delta_x: f64,
        weird_delta_y: f64,
        delta_z: f64,
        delta_y: f64,
    ) -> f64 {
        let i = self.p(grid_x);
        let j = self.p(grid_x + 1);
        let k = self.p(i + grid_y);
        let l = self.p(i + grid_y + 1);
        let m = self.p(j + grid_y);
        let n = self.p(j + grid_y + 1);

        let d = grad_dot(self.p(k + grid_z), delta_x, weird_delta_y, delta_z);
        let e = grad_dot(self.p(m + grid_z), delta_x - 1.0, weird_delta_y, delta_z);
        let f = grad_dot(self.p(l + grid_z), delta_x, weird_delta_y - 1.0, delta_z);
        let g = grad_dot(
            self.p(n + grid_z),
            delta_x - 1.0,
            weird_delta_y - 1.0,
            delta_z,
        );
        let h = grad_dot(
            self.p(k + grid_z + 1),
            delta_x,
            weird_delta_y,
            delta_z - 1.0,
        );
        let o = grad_dot(
            self.p(m + grid_z + 1),
            delta_x - 1.0,
            weird_delta_y,
            delta_z - 1.0,
        );
        let p = grad_dot(
            self.p(l + grid_z + 1),
            delta_x,
            weird_delta_y - 1.0,
            delta_z - 1.0,
        );
        let q = grad_dot(
            self.p(n + grid_z + 1),
            delta_x - 1.0,
            weird_delta_y - 1.0,
            delta_z - 1.0,
        );

        let r = smoothstep(delta_x);
        let s = smoothstep(delta_y);
        let t = smoothstep(delta_z);
        lerp3(r, s, t, d, e, f, g, h, o, p, q)
    }
}

fn grad_dot(index: i32, x: f64, y: f64, z: f64) -> f64 {
    let g = GRADIENT[(index & 15) as usize];
    g[0] * x + g[1] * y + g[2] * z
}

fn smoothstep(value: f64) -> f64 {
    value * value * value * (value * (value * 6.0 - 15.0) + 10.0)
}

fn lerp(delta: f64, start: f64, end: f64) -> f64 {
    start + delta * (end - start)
}

#[allow(clippy::too_many_arguments)]
fn lerp3(
    dx: f64,
    dy: f64,
    dz: f64,
    v000: f64,
    v100: f64,
    v010: f64,
    v110: f64,
    v001: f64,
    v101: f64,
    v011: f64,
    v111: f64,
) -> f64 {
    lerp(
        dz,
        lerp(dy, lerp(dx, v000, v100), lerp(dx, v010, v110)),
        lerp(dy, lerp(dx, v001, v101), lerp(dx, v011, v111)),
    )
}

/// A stack of octaves.
#[derive(Debug, Clone)]
pub struct PerlinNoise {
    /// One slot per amplitude. `None` where the amplitude is zero, because
    /// Minecraft does not build that octave at all — and, crucially, does not
    /// consume a name for it either.
    levels: Vec<Option<ImprovedNoise>>,
    amplitudes: Vec<f64>,
    lowest_freq_input_factor: f64,
    lowest_freq_value_factor: f64,
    max_value: f64,
}

impl PerlinNoise {
    pub fn create(random: &mut Xoroshiro, parameters: &NoiseParameters) -> Self {
        let count = parameters.amplitudes.len();
        let factory: Positional = random.fork_positional();
        let levels: Vec<Option<ImprovedNoise>> = parameters
            .amplitudes
            .iter()
            .enumerate()
            .map(|(index, &amplitude)| {
                if amplitude == 0.0 {
                    None
                } else {
                    let octave = parameters.first_octave + index as i32;
                    let mut stream = factory.from_hash_of(&format!("octave_{octave}"));
                    Some(ImprovedNoise::new(&mut stream))
                }
            })
            .collect();
        let lowest_freq_input_factor = 2f64.powi(parameters.first_octave);
        let lowest_freq_value_factor =
            2f64.powi(count as i32 - 1) / (2f64.powi(count as i32) - 1.0);
        let mut noise = Self {
            levels,
            amplitudes: parameters.amplitudes.clone(),
            lowest_freq_input_factor,
            lowest_freq_value_factor,
            max_value: 0.0,
        };
        noise.max_value = noise.edge_value(2.0);
        noise
    }

    fn edge_value(&self, edge: f64) -> f64 {
        let mut total = 0.0;
        let mut value_factor = self.lowest_freq_value_factor;
        for (index, level) in self.levels.iter().enumerate() {
            if level.is_some() {
                total += self.amplitudes[index] * edge * value_factor;
            }
            value_factor /= 2.0;
        }
        total
    }

    pub fn max_value(&self) -> f64 {
        self.max_value
    }

    pub fn value(&self, x: f64, y: f64, z: f64) -> f64 {
        let mut total = 0.0;
        let mut input_factor = self.lowest_freq_input_factor;
        let mut value_factor = self.lowest_freq_value_factor;
        for (index, level) in self.levels.iter().enumerate() {
            if let Some(level) = level {
                let sampled = level.noise(
                    wrap(x * input_factor),
                    wrap(y * input_factor),
                    wrap(z * input_factor),
                );
                total += self.amplitudes[index] * sampled * value_factor;
            }
            input_factor *= 2.0;
            value_factor /= 2.0;
        }
        total
    }
}

/// Fold a coordinate back towards the origin every 2^25 blocks.
///
/// Perlin's grid is a table of 256 entries and the offsets are doubles; far
/// enough out, the fractional part stops carrying information. This is
/// Minecraft's fold and it is part of the answer, not a guard: a world at
/// x = 50,000,000 is the world this wrap produces.
fn wrap(value: f64) -> f64 {
    value - lfloor(value / 3.3554432E7 + 0.5) as f64 * 3.3554432E7
}

fn lfloor(value: f64) -> i64 {
    let floored = value as i64;
    if value < floored as f64 {
        floored - 1
    } else {
        floored
    }
}

/// Two Perlin stacks summed, scaled to something close to a normal
/// distribution.
#[derive(Debug, Clone)]
pub struct NormalNoise {
    first: PerlinNoise,
    second: PerlinNoise,
    value_factor: f64,
    max_value: f64,
}

/// Why the second stack is sampled at a slightly different rate: at exactly the
/// same rate the two would be correlated and the sum would not be normal.
const INPUT_FACTOR: f64 = 1.0181268882175227;

impl NormalNoise {
    /// Build the pair. Both stacks come out of *one* stream, in this order, so
    /// swapping them is a different noise.
    pub fn create(random: &mut Xoroshiro, parameters: &NoiseParameters) -> Self {
        let first = PerlinNoise::create(random, parameters);
        let second = PerlinNoise::create(random, parameters);
        let mut lowest = usize::MAX;
        let mut highest = 0usize;
        let mut any = false;
        for (index, &amplitude) in parameters.amplitudes.iter().enumerate() {
            if amplitude != 0.0 {
                lowest = lowest.min(index);
                highest = highest.max(index);
                any = true;
            }
        }
        let span = if any { highest - lowest } else { 0 };
        let value_factor = 0.16666666666666666 / expected_deviation(span as i32);
        let max_value = (first.max_value() + second.max_value()) * value_factor;
        Self {
            first,
            second,
            value_factor,
            max_value,
        }
    }

    pub fn max_value(&self) -> f64 {
        self.max_value
    }

    pub fn value(&self, x: f64, y: f64, z: f64) -> f64 {
        let sx = x * INPUT_FACTOR;
        let sy = y * INPUT_FACTOR;
        let sz = z * INPUT_FACTOR;
        (self.first.value(x, y, z) + self.second.value(sx, sy, sz)) * self.value_factor
    }
}

fn expected_deviation(octaves: i32) -> f64 {
    0.1 * (1.0 + 1.0 / f64::from(octaves + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parameters(first_octave: i32, amplitudes: &[f64]) -> NoiseParameters {
        NoiseParameters {
            first_octave,
            amplitudes: amplitudes.to_vec(),
        }
    }

    #[test]
    fn an_octave_permutation_is_a_permutation() {
        let mut random = Xoroshiro::from_seed(0);
        let noise = ImprovedNoise::new(&mut random);
        let mut seen = [false; 256];
        for &value in &noise.permutation {
            assert!(!seen[value as usize], "{value} appears twice");
            seen[value as usize] = true;
        }
        assert!(seen.iter().all(|&s| s));
    }

    #[test]
    fn a_zero_amplitude_octave_is_not_built_and_does_not_shift_the_rest() {
        // The whole point of naming octaves rather than counting them. If this
        // stopped being true, `temperature` — whose amplitudes are
        // [1.5, 0, 1, 0, 0, 0] — would get the octaves `vegetation` gets.
        let mut random = Xoroshiro::from_seed(7);
        let sparse = PerlinNoise::create(&mut random, &parameters(-10, &[1.5, 0.0, 1.0]));
        let mut random = Xoroshiro::from_seed(7);
        let dense = PerlinNoise::create(&mut random, &parameters(-10, &[1.5, 1.0, 1.0]));
        assert!(sparse.levels[1].is_none());
        assert!(dense.levels[1].is_some());
        let a = sparse.levels[2].as_ref().expect("built");
        let b = dense.levels[2].as_ref().expect("built");
        assert_eq!(a.permutation, b.permutation, "octave 2 must be octave 2");
    }

    #[test]
    fn the_same_seed_gives_the_same_noise_and_a_different_one_does_not() {
        let params = parameters(-9, &[1.0, 1.0, 2.0]);
        let mut a = Xoroshiro::from_seed(3);
        let mut b = Xoroshiro::from_seed(3);
        let mut c = Xoroshiro::from_seed(4);
        let first = NormalNoise::create(&mut a, &params);
        let second = NormalNoise::create(&mut b, &params);
        let other = NormalNoise::create(&mut c, &params);
        assert_eq!(first.value(11.0, 3.0, -7.0), second.value(11.0, 3.0, -7.0));
        assert_ne!(first.value(11.0, 3.0, -7.0), other.value(11.0, 3.0, -7.0));
    }

    #[test]
    fn a_noise_stays_inside_the_range_it_claims() {
        let params = parameters(-7, &[1.0, 1.0, 1.0, 0.0, 1.0]);
        let mut random = Xoroshiro::from_seed(99);
        let noise = NormalNoise::create(&mut random, &params);
        let limit = noise.max_value();
        for step in -200..200 {
            let point = f64::from(step) * 13.7;
            let value = noise.value(point, 0.0, point * 0.5);
            assert!(value.abs() <= limit, "{value} outside +/-{limit}");
        }
    }

    #[test]
    fn the_far_coordinate_fold_is_the_one_minecraft_uses() {
        // Not a guard: it changes the answer, and it changes it at 2^25 blocks.
        assert_eq!(wrap(0.0), 0.0);
        assert_eq!(wrap(1.0), 1.0);
        assert!((wrap(3.3554432E7) - 0.0).abs() < 1e-9);
        // Beyond the period, a coordinate comes back inside a half-period of
        // the origin. That is the fold, and it is why two places 2^25 blocks
        // apart share a noise value.
        for step in 0..64 {
            let far = f64::from(step) * 1.0E7 - 3.0E8;
            assert!(wrap(far).abs() <= 3.3554432E7 / 2.0 + 1.0, "{far}");
        }
        assert!(wrap(5.0E7) < 5.0E7);
    }
}
