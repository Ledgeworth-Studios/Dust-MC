//! The light walks against a naive reference, under random small worlds.
//!
//! A propagation bug that matters never looks like one locally: light bends
//! around a wall by the wrong amount, a removal takes a neighbour's share
//! with it, an offer dies one step early behind opaque glass. Every one of
//! those is invisible unless the whole field is checked against something
//! too dumb to share the bug. So this file plays deterministic random worlds
//! through [`raise`] and [`darken`] and compares the result, cell for cell,
//! against a relaxation that sweeps every cell against every neighbour until
//! nothing changes -- quadratic where the real walks are linear, and right
//! precisely because it cannot help being.
//!
//! Alongside the differential it pins the invariants the module documentation
//! argues for: attenuation is monotone under uniform opacity, the walks end
//! even when the graph is all cycles, one input produces one trace, and a
//! budget that trips is a typed error naming what it refused rather than a
//! hang.
//!
//! **On randomness:** fixed-seed xorshift throughout; a failure replays.
//!
//! **What this does not catch:** a wrongness shared by both sides, and the
//! list has two entries rather than the one it used to have.
//!
//! The reference derives its edges from the same opacity table the walks read;
//! if the *table* misstates the world, both agree on the wrong answer. That
//! seam belongs to whoever wires real block states into [`LightGraph`].
//!
//! And the reference states the **attenuation rule** a second time rather than
//! deriving it a second way. Both sides said a step cost `1 + opacity` for the
//! life of the light engine, and this file passed on every seed while a column
//! of water came out half as bright as Minecraft makes it — see
//! `propagation::step_cost`. A differential catches a divergence between two
//! statements of a rule; it cannot catch a rule that is wrong in both, and
//! nothing inside this crate can, because the rule is Minecraft's. What
//! contradicted it was `cargo xtask harness light`, against light a real
//! server computed.

use dust_world::propagation::{darken, raise, seed_skylight, Budget, LightGraph, PropagationError};

fn xorshift(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

/// A rectangular volume with per-cell opacity and recorded writes.
///
/// The trace is the point: canonical ordering means one input must produce
/// one sequence of writes, and this is where a divergence would show.
#[derive(Clone)]
struct Volume {
    size: (i32, i32, i32),
    /// Opacity per cell, defaulting to zero. Deliberately not sorted into
    /// any canonical order: lookups must not care.
    opaque: Vec<((i32, i32, i32), u8)>,
    levels: Vec<u8>,
    writes: Vec<((i32, i32, i32), u8)>,
}

impl Volume {
    fn new(size: (i32, i32, i32)) -> Self {
        Self {
            size,
            opaque: Vec::new(),
            levels: vec![0; (size.0 * size.1 * size.2) as usize],
            writes: Vec::new(),
        }
    }

    fn with_opacity(mut self, x: i32, y: i32, z: i32, opacity: u8) -> Self {
        self.opaque.push(((x, y, z), opacity));
        self
    }

    fn offset(&self, x: i32, y: i32, z: i32) -> usize {
        (x + y * self.size.0 + z * self.size.0 * self.size.1) as usize
    }

    fn field(&self) -> Vec<u8> {
        self.levels.clone()
    }

    fn lit(&self) -> Vec<(usize, u8)> {
        self.levels
            .iter()
            .enumerate()
            .filter(|(_, l)| **l > 0)
            .map(|(i, l)| (i, *l))
            .collect()
    }
}

impl LightGraph for Volume {
    fn level(&self, x: i32, y: i32, z: i32) -> u8 {
        self.levels[self.offset(x, y, z)]
    }

    fn set_level(&mut self, x: i32, y: i32, z: i32, level: u8) {
        let index = self.offset(x, y, z);
        self.levels[index] = level;
        self.writes.push(((x, y, z), level));
    }

    fn opacity(&self, x: i32, y: i32, z: i32) -> u8 {
        self.opaque
            .iter()
            .find(|((ox, oy, oz), _)| (*ox, *oy, *oz) == (x, y, z))
            .map_or(0, |(_, o)| *o)
    }

    fn contains(&self, x: i32, y: i32, z: i32) -> bool {
        x >= 0 && y >= 0 && z >= 0 && x < self.size.0 && y < self.size.1 && z < self.size.2
    }
}

/// The naive reference: sweep every cell against its six neighbours, taking
/// the best offer, until a full sweep changes nothing. No queue, no early
/// exits, no cleverness -- O(cells * edges) sweeps, which is the point.
fn relax_until_stable(graph: &Volume, sources: &[(i32, i32, i32, u8)]) -> Vec<u8> {
    let mut levels = vec![0u8; graph.levels.len()];
    for &(x, y, z, level) in sources {
        levels[graph.offset(x, y, z)] = levels[graph.offset(x, y, z)].max(level);
    }
    loop {
        let mut changed = false;
        let mut next = levels.clone();
        for z in 0..graph.size.2 {
            for y in 0..graph.size.1 {
                for x in 0..graph.size.0 {
                    if !graph.contains(x, y, z) {
                        continue;
                    }
                    let index = graph.offset(x, y, z);
                    // What entering this cell costs: the move, or the block,
                    // whichever is larger. Minecraft's rule, spelled out here
                    // by hand rather than by calling
                    // `dust_world::propagation::step_cost` — a reference that
                    // called the thing it is checking would agree with it by
                    // construction, which is this file's whole argument.
                    //
                    // Being a second *copy* of the rule is not the same as
                    // being a second *derivation* of it, and the note at the
                    // top of this file now says so: this was `1 + opacity` on
                    // both sides for the life of the engine and the
                    // differential passed every time.
                    let cost = u16::from(graph.opacity(x, y, z).max(1));
                    let mut best = levels[index];
                    for &(dx, dy, dz) in &[
                        (1, 0, 0),
                        (-1, 0, 0),
                        (0, 1, 0),
                        (0, -1, 0),
                        (0, 0, 1),
                        (0, 0, -1),
                    ] {
                        let (nx, ny, nz) = (x + dx, y + dy, z + dz);
                        if !graph.contains(nx, ny, nz) {
                            continue;
                        }
                        let offer =
                            u16::from(levels[graph.offset(nx, ny, nz)]).saturating_sub(cost);
                        best = best.max(u8::try_from(offer).unwrap_or(0));
                    }
                    if best != next[index] {
                        changed = true;
                    }
                    next[index] = best;
                }
            }
        }
        levels = next;
        if !changed {
            return levels;
        }
    }
}

/// A random world: dimensions, opacities drawn from {0, 1, 15} so clear,
/// dimming and blocking cells all appear, and a handful of sources.
fn random_world(seed: u64) -> (Volume, Vec<(i32, i32, i32, u8)>) {
    let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    let size = (
        (xorshift(&mut state) % 4) as i32 + 2,
        (xorshift(&mut state) % 4) as i32 + 2,
        (xorshift(&mut state) % 4) as i32 + 2,
    );
    let mut graph = Volume::new(size);
    for z in 0..size.2 {
        for y in 0..size.1 {
            for x in 0..size.0 {
                match xorshift(&mut state) % 5 {
                    0 => graph = graph.with_opacity(x, y, z, 15),
                    1 => graph = graph.with_opacity(x, y, z, 1),
                    _ => {}
                }
            }
        }
    }
    let source_count = (xorshift(&mut state) % 4) as usize + 1;
    let mut sources = Vec::new();
    for _ in 0..source_count {
        let position = (
            (xorshift(&mut state) % size.0 as u64) as i32,
            (xorshift(&mut state) % size.1 as u64) as i32,
            (xorshift(&mut state) % size.2 as u64) as i32,
        );
        let level = (xorshift(&mut state) % 16) as u8;
        sources.push((position.0, position.1, position.2, level));
    }
    (graph, sources)
}

#[test]
fn random_worlds_match_the_naive_reference_cell_for_cell() {
    for seed in 0..30u64 {
        let (mut graph, sources) = random_world(seed);
        raise(&mut graph, &sources, Budget::default())
            .unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        assert_eq!(
            graph.field(),
            relax_until_stable(&graph, &sources),
            "seed {seed}: the walk disagreed with the sweep"
        );
    }
}

/// The same volume back to dark, keeping its shape and opacity table.
fn blank_like(template: &Volume) -> Volume {
    let mut blank = template.clone();
    blank.writes.clear();
    blank.levels.iter_mut().for_each(|l| *l = 0);
    blank
}

#[test]
fn removing_sources_lands_on_the_field_the_remaining_ones_produce() {
    for seed in 100..130u64 {
        let (mut graph, sources) = random_world(seed);
        raise(&mut graph, &sources, Budget::default())
            .unwrap_or_else(|e| panic!("seed {seed}: {e}"));

        // Distinct positions only: two sources on one block are one
        // brightness question, and a position cannot be simultaneously
        // removed and surviving.
        let mut distinct: Vec<(i32, i32, i32, u8)> = Vec::new();
        for &(x, y, z, level) in &sources {
            match distinct.iter_mut().find(|s| (s.0, s.1, s.2) == (x, y, z)) {
                Some(held) => held.3 = held.3.max(level),
                None => distinct.push((x, y, z, level)),
            }
        }

        // Take away every second distinct source, then compare against a
        // world that never heard of them.
        let removed: Vec<(i32, i32, i32)> = distinct
            .iter()
            .enumerate()
            .filter(|(i, _)| i % 2 == 0)
            .map(|(_, s)| (s.0, s.1, s.2))
            .collect();
        let remaining: Vec<(i32, i32, i32, u8)> = distinct
            .iter()
            .enumerate()
            .filter(|(i, _)| i % 2 != 0)
            .map(|(_, s)| *s)
            .collect();

        let mut fresh = blank_like(&graph);
        raise(&mut fresh, &remaining, Budget::default())
            .unwrap_or_else(|e| panic!("seed {seed} fresh: {e}"));

        darken(&mut graph, &removed, &remaining, Budget::default())
            .unwrap_or_else(|e| panic!("seed {seed} darken: {e}"));
        assert_eq!(
            graph.lit(),
            fresh.lit(),
            "seed {seed}: what survived the removal is not the surviving sources' field"
        );
    }
}

#[test]
fn uniform_opacity_attenuates_exactly_one_step_cost_per_step() {
    // In clear uniform air the shortest path is the Manhattan distance, so
    // the whole field is a closed-form sentence -- and monotonicity is not
    // an approximation but the arithmetic itself.
    //
    // A step costs `max(1, opacity)`: the move, or the block, whichever is
    // larger. Opacity 1 is the case that separates that from `1 + opacity`,
    // and it is the case nearly every translucent block in Minecraft is —
    // water, leaves, grass, ice. This test read `1 + opacity` and passed,
    // because so did the engine.
    for opacity in [0u8, 1, 2] {
        for brightness in [7u8, 15] {
            let mut graph = Volume::new((6, 6, 6));
            for z in 0..6i32 {
                for y in 0..6i32 {
                    for x in 0..6i32 {
                        graph = graph.with_opacity(x, y, z, opacity);
                    }
                }
            }
            raise(&mut graph, &[(2, 2, 2, brightness)], Budget::default())
                .expect("a small box fits any budget");
            for z in 0..6i32 {
                for y in 0..6i32 {
                    for x in 0..6i32 {
                        let distance = (x - 2).abs() + (y - 2).abs() + (z - 2).abs();
                        let expected = brightness.saturating_sub(distance as u8 * opacity.max(1));
                        assert_eq!(
                            graph.level(x, y, z),
                            expected,
                            "opacity {opacity}, brightness {brightness}, ({x}, {y}, {z})"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn a_graph_made_of_cycles_still_settles_and_names_the_field() {
    // A torus: stepping off one face wraps to the opposite one, so every
    // neighbourhood is a ring and every path competes with infinitely many
    // cyclic alternatives. A walk that revisited instead of improving would
    // spin here forever; this one has to drain.
    #[derive(Clone)]
    struct Torus {
        side: i32,
        levels: Vec<u8>,
    }
    impl Torus {
        fn wrap(&self, v: i32) -> i32 {
            v.rem_euclid(self.side)
        }
    }
    impl LightGraph for Torus {
        fn level(&self, x: i32, y: i32, z: i32) -> u8 {
            self.levels[self.offset(x, y, z)]
        }
        fn set_level(&mut self, x: i32, y: i32, z: i32, level: u8) {
            let index = self.offset(x, y, z);
            self.levels[index] = level;
        }
        fn opacity(&self, _: i32, _: i32, _: i32) -> u8 {
            0
        }
        fn contains(&self, _: i32, _: i32, _: i32) -> bool {
            true
        }
    }
    impl Torus {
        fn offset(&self, x: i32, y: i32, z: i32) -> usize {
            let (x, y, z) = (self.wrap(x), self.wrap(y), self.wrap(z));
            (x + y * self.side + z * self.side * self.side) as usize
        }
    }

    let side = 5;
    let mut torus = Torus {
        side,
        levels: vec![0; (side * side * side) as usize],
    };
    // One seed reaches everything around the ring; a budget sized to a few
    // passes proves termination by succeeding, and the wrap makes the far
    // side reachable both ways round.
    let spent = raise(
        &mut torus,
        &[(0, 0, 0, 9), (side - 1, side - 1, side - 1, 9)],
        Budget::new(50_000),
    )
    .expect("a cyclic graph still terminates");
    assert!(spent > 0);

    // On a torus the distance between opposite corners is symmetric and the
    // maximum Manhattan ring-distance is bounded, so every cell must be lit.
    assert!(
        torus.levels.iter().all(|l| *l > 0),
        "light found every way round"
    );

    // And darkening one seed leaves exactly the other's field, cycles and
    // all -- the same claim as the flat worlds, made where cycles live.
    let mut lone = Torus {
        side,
        levels: vec![0; (side * side * side) as usize],
    };
    raise(
        &mut lone,
        &[(side - 1, side - 1, side - 1, 9)],
        Budget::default(),
    )
    .expect("single seed");
    darken(
        &mut torus,
        &[(0, 0, 0)],
        &[(side - 1, side - 1, side - 1, 9)],
        Budget::default(),
    )
    .expect("drains");
    assert_eq!(torus.levels, lone.levels);
}

#[test]
fn one_input_produces_one_trace_whatever_the_run() {
    // Canonical order means the write sequence is a function of the seeds.
    // Two identical runs must agree line for line, and the final field must
    // not depend on the order the seeds were handed over at.
    for seed in 200..212u64 {
        let (template, mut sources) = random_world(seed);

        let mut first = template.clone();
        let mut second = template.clone();
        raise(&mut first, &sources, Budget::default()).expect("first run");
        raise(&mut second, &sources, Budget::default()).expect("second run");
        assert_eq!(first.writes, second.writes, "seed {seed}: traces diverged");

        // Reversed seed order: same field, however different the visits.
        sources.reverse();
        let mut third = template.clone();
        raise(&mut third, &sources, Budget::default()).expect("reversed run");
        assert_eq!(
            first.field(),
            third.field(),
            "seed {seed}: the settled field depends on seed order"
        );
    }
}

#[test]
fn a_budget_that_trips_is_a_typed_error_and_leaves_a_consistent_partial() {
    let (template, sources) = random_world(300);
    let mut graph = template.clone();
    let total_needed = raise(&mut graph, &sources, Budget::default()).expect("unbounded run");

    // Now replay with budgets known to be smaller than the work. The error
    // names the cap and the spend; every seed was written before the first
    // edge was examined, so the sources stand even where the walk stopped,
    // and nothing anywhere ends brighter than the complete pass allows --
    // a capped run is a prefix of the same deterministic evolution.
    for cap in [1u64, 10, 100] {
        let mut rationed = blank_like(&template);
        let err = raise(&mut rationed, &sources, Budget::new(cap))
            .expect_err("this cap cannot cover the pass");
        assert_eq!(
            err,
            PropagationError::BudgetExhausted {
                spent: cap,
                budget: cap
            },
            "cap {cap}"
        );
        assert!(err.to_string().contains("budget"), "{err}");
        for &(x, y, z, level) in sources.iter().filter(|s| s.3 > 0) {
            assert_eq!(
                rationed.level(x, y, z),
                level,
                "seed ({x}, {y}, {z}) after cap {cap}"
            );
        }
        // The stop left nothing brighter than the full pass produced.
        for (index, level) in rationed.levels.iter().enumerate() {
            assert!(
                *level <= graph.levels[index],
                "cap {cap}: cell {index} ended brighter than any complete pass allows"
            );
        }
    }

    // And the full pass reports honestly how much it needed.
    assert!(
        total_needed >= 100,
        "{total_needed}: the caps above only bite because the work exceeds them"
    );

    let mut bright = Volume::new((2, 2, 2));
    let err = raise(&mut bright, &[(0, 0, 0, 16)], Budget::new(1_000))
        .expect_err("sixteen does not fit in four bits");
    assert_eq!(err, PropagationError::SeedTooBright { level: 16 });
    assert!(bright.writes.is_empty(), "refusal wrote nothing");
}

#[test]
fn seeded_sky_columns_fill_to_the_surface_and_idempotence_costs_nothing() {
    // Columns of a small world with surfaces from a fixed stream. Above each
    // surface the field must read fifteen exactly; the row just beneath
    // receives what spilled through; reseeding the same columns must find
    // no improving seed to act on, spending nothing.
    let mut graph = Volume::new((4, 6, 4));
    let mut state = 0xfeed_beef_u64;
    let mut surfaces = Vec::new();
    for z in 0..4i32 {
        for x in 0..4i32 {
            surfaces.push(((x, z), (xorshift(&mut state) % 5) as i32 + 1));
        }
    }
    let columns: Vec<(i32, i32, std::ops::Range<i32>)> = surfaces
        .iter()
        .map(|&((x, z), surface)| (x, z, surface..6))
        .collect();

    let spent_first =
        seed_skylight(&mut graph, columns.clone(), Budget::default()).expect("a small sky fits");
    assert!(spent_first > 0, "seeding did something the first time");

    for ((x, z), surface) in &surfaces {
        for y in *surface..6 {
            assert_eq!(graph.level(*x, y, *z), 15, "open sky at ({x}, {y}, {z})");
        }
        // One step of clear air below the ceiling of the world's own light.
        if *surface > 0 {
            assert_eq!(
                graph.level(*x, surface - 1, *z),
                14,
                "the topmost shadowed row at ({x}, {}, {z})",
                surface - 1
            );
        }
    }

    let spent_again =
        seed_skylight(&mut graph, columns, Budget::default()).expect("reseeding settles");
    assert_eq!(spent_again, 0, "fifteen over fifteen is nothing to do");
}
