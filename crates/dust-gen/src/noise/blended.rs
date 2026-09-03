//! `old_blended_noise`: the 1.17-and-earlier terrain noise, still under every
//! overworld column.
//!
//! Vanilla's density-function language has one node that is not built out of
//! the others: `minecraft:old_blended_noise` names a whole sampler with three
//! Perlin stacks inside it, forty octaves in total, and a main stack that
//! chooses per point which of the other two the answer comes from. It is what
//! makes a mountain a mountain rather than a smooth swell, and there is no way
//! to spell it in the language, so it is a node.
//!
//! Nothing here is data. The five shape numbers arrive from the operator's own
//! data pack with the rest of the graph; what this file holds is the
//! arithmetic between them.

use super::perlin::{wrap, PerlinNoise};
use super::rng::Xoroshiro;

/// The scale every blended noise is written in terms of.
const BASE_SCALE: f64 = 684.412;

/// The noise `old_blended_noise` names, built for one seed.
#[derive(Debug, Clone)]
pub struct BlendedNoise {
    min_limit: PerlinNoise,
    max_limit: PerlinNoise,
    main: PerlinNoise,
    xz_multiplier: f64,
    y_multiplier: f64,
    xz_factor: f64,
    y_factor: f64,
    smear_scale_multiplier: f64,
}

/// The five numbers a data pack writes for one, before a seed is applied.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlendedShape {
    pub xz_scale: f64,
    pub y_scale: f64,
    pub xz_factor: f64,
    pub y_factor: f64,
    pub smear_scale_multiplier: f64,
}

impl BlendedNoise {
    /// Draw the three stacks out of one stream, in the order vanilla draws
    /// them: sixteen octaves of minimum, sixteen of maximum, eight of main.
    ///
    /// The order is the whole contract — the second stack is the continuation
    /// of the first stream, not a fresh one — which is why this takes the
    /// stream rather than a seed.
    pub fn new(random: &mut Xoroshiro, shape: BlendedShape) -> Self {
        let min_limit = PerlinNoise::create_legacy_for_blended_noise(random, -15);
        let max_limit = PerlinNoise::create_legacy_for_blended_noise(random, -15);
        let main = PerlinNoise::create_legacy_for_blended_noise(random, -7);
        Self {
            min_limit,
            max_limit,
            main,
            xz_multiplier: BASE_SCALE * shape.xz_scale,
            y_multiplier: BASE_SCALE * shape.y_scale,
            xz_factor: shape.xz_factor,
            y_factor: shape.y_factor,
            smear_scale_multiplier: shape.smear_scale_multiplier,
        }
    }

    /// How far the main stack leans towards the maximum limit. Outside
    /// `[0, 1]` one of the two limit stacks is not asked at all.
    fn blend(&self, x: i32, y: i32, z: i32) -> f64 {
        let mx = f64::from(x) * self.xz_multiplier / self.xz_factor;
        let my = f64::from(y) * self.y_multiplier / self.y_factor;
        let mz = f64::from(z) * self.xz_multiplier / self.xz_factor;
        let smear = self.y_multiplier * self.smear_scale_multiplier / self.y_factor;
        let mut main = 0.0;
        let mut scale = 1.0;
        for octave in 0..8 {
            if let Some(level) = self.main.octave(octave) {
                main += level.noise_with_y_step(
                    wrap(mx * scale),
                    wrap(my * scale),
                    wrap(mz * scale),
                    smear * scale,
                    my * scale,
                ) / scale;
            }
            scale /= 2.0;
        }
        (main / 10.0 + 1.0) / 2.0
    }

    pub fn value(&self, x: i32, y: i32, z: i32) -> f64 {
        // The main stack picks the blend, and the two decisions it makes are
        // whether either limit stack is asked at all. That is not an
        // optimisation — `clamped_lerp` would give the same number — but the
        // sixteen-octave loop below is where nearly all of a column's terrain
        // time goes, and the blend leaves [0, 1] at most points.
        let blend = self.blend(x, y, z);
        let all_max = blend >= 1.0;
        let all_min = blend <= 0.0;

        let sx = f64::from(x) * self.xz_multiplier;
        let sy = f64::from(y) * self.y_multiplier;
        let sz = f64::from(z) * self.xz_multiplier;
        let smear = self.y_multiplier * self.smear_scale_multiplier;

        let mut low = 0.0;
        let mut high = 0.0;
        let mut scale = 1.0;
        for octave in 0..16 {
            let px = wrap(sx * scale);
            let py = wrap(sy * scale);
            let pz = wrap(sz * scale);
            let step = smear * scale;
            if !all_max {
                if let Some(level) = self.min_limit.octave(octave) {
                    low += level.noise_with_y_step(px, py, pz, step, sy * scale) / scale;
                }
            }
            if !all_min {
                if let Some(level) = self.max_limit.octave(octave) {
                    high += level.noise_with_y_step(px, py, pz, step, sy * scale) / scale;
                }
            }
            scale /= 2.0;
        }
        clamped_lerp(low / 512.0, high / 512.0, blend) / 128.0
    }
}

fn clamped_lerp(start: f64, end: f64, delta: f64) -> f64 {
    if delta < 0.0 {
        start
    } else if delta > 1.0 {
        end
    } else {
        start + delta * (end - start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::noise::perlin::ImprovedNoise;

    fn shape() -> BlendedShape {
        BlendedShape {
            xz_scale: 0.25,
            y_scale: 0.125,
            xz_factor: 80.0,
            y_factor: 160.0,
            smear_scale_multiplier: 8.0,
        }
    }

    fn samples(noise: &BlendedNoise) -> Vec<f64> {
        (0..64)
            .map(|i| noise.value(i * 7 - 200, i * 5 - 60, 300 - i * 11))
            .collect()
    }

    #[test]
    fn the_three_stacks_are_three_stretches_of_one_stream() {
        // Watched to fail: drawing `main` first makes this pass and every
        // terrain number in the harness move.
        //
        // Sixty-four points and not one, and that is a finding rather than
        // caution. Advancing the stream by exactly one stack lines the second
        // noise's *minimum* stack up with the first's *maximum*; the blend
        // saturates at most points, and wherever the two saturate opposite
        // ways the same stack answers both. The one-point version of this test
        // asserted nothing at two of the four points it was tried at.
        let mut stream = Xoroshiro::from_seed(0);
        let a = BlendedNoise::new(&mut stream, shape());
        let mut other = Xoroshiro::from_seed(0);
        let _first = PerlinNoise::create_legacy_for_blended_noise(&mut other, -15);
        let b = BlendedNoise::new(&mut other, shape());
        assert_ne!(samples(&a), samples(&b));
    }

    #[test]
    fn one_seed_gives_one_noise_and_another_seed_gives_another() {
        assert_eq!(
            samples(&BlendedNoise::new(&mut Xoroshiro::from_seed(7), shape())),
            samples(&BlendedNoise::new(&mut Xoroshiro::from_seed(7), shape()))
        );
        assert_ne!(
            samples(&BlendedNoise::new(&mut Xoroshiro::from_seed(7), shape())),
            samples(&BlendedNoise::new(&mut Xoroshiro::from_seed(8), shape()))
        );
    }

    #[test]
    fn the_blend_leaves_its_interval_at_most_points() {
        // The branch in `value` is worth its line only if the blend really
        // does saturate. Measured, not assumed.
        let noise = BlendedNoise::new(&mut Xoroshiro::from_seed(3), shape());
        let total = 64i32;
        let saturated = (0..total)
            .filter(|i| {
                let blend = noise.blend(i * 7 - 200, i * 5 - 60, 300 - i * 11);
                !(0.0..1.0).contains(&blend)
            })
            .count();
        assert!(
            saturated * 2 >= total as usize,
            "{saturated} of {total} points saturated"
        );
    }

    #[test]
    fn a_blended_noise_stays_in_a_terrain_sized_range() {
        let noise = BlendedNoise::new(&mut Xoroshiro::from_seed(1), shape());
        let mut seen_low = false;
        let mut seen_high = false;
        for x in (-512..512).step_by(37) {
            for y in (-64..320).step_by(29) {
                let value = noise.value(x, y, x / 2 - 100);
                assert!(value.abs() < 4.0, "{value} at {x},{y}");
                seen_low |= value < -0.05;
                seen_high |= value > 0.05;
            }
        }
        assert!(seen_low && seen_high, "the noise never left zero");
    }

    #[test]
    fn the_octave_index_counts_from_the_top_of_the_stack() {
        let mut stream = Xoroshiro::from_seed(2);
        let stack = PerlinNoise::create_legacy_for_blended_noise(&mut stream, -7);
        assert!(stack.octave(0).is_some());
        assert!(stack.octave(7).is_some());
        assert!(stack.octave(8).is_none(), "eight octaves, not nine");
        let first_drawn = ImprovedNoise::new(&mut Xoroshiro::from_seed(2));
        assert_eq!(
            stack.octave(0).unwrap().noise(1.5, 2.5, 3.5),
            first_drawn.noise(1.5, 2.5, 3.5),
            "octave 0 is the octave drawn first"
        );
    }
}
