//! Applies `[worldgen.ores]` to the ore placements a world actually has.
//!
//! # Where the baseline comes from
//!
//! Nothing in this module knows how much diamond vanilla generates, and that is
//! deliberate twice over.
//!
//! The first reason is licensing. Mojang's data may not be redistributed, so
//! the vanilla placement values reach Dust through `xtask extract` running
//! against a server jar on the operator's own machine, never as a table typed
//! into this repository. See `Code Provenance.md`.
//!
//! The second reason is that the baseline is not a constant. A world running
//! Terralith has different ore placements from a vanilla world, and an operator
//! who asks for twice as much iron means twice as much of *their* world's iron.
//! A resolver that knew vanilla's numbers would quietly be wrong on every
//! modded world, and would be right in exactly the case that needs it least.
//!
//! So: [`Baseline`] is whatever the loaded world says, and this module scales
//! it.
//!
//! # The identity property
//!
//! With default settings, [`resolve`] returns the baseline unchanged — not
//! approximately, exactly. That is what allows the Phase 6 seed-for-seed parity
//! test to run against a Dust that has this feature compiled in, and it is
//! asserted directly in the tests below. Any change here that breaks it is a
//! change that breaks vanilla parity.

use dust_config::ore::{OreGroup, OresConfig};

/// The vertical span an ore may generate in, inclusive at both ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeightRange {
    pub min_y: i32,
    pub max_y: i32,
}

impl HeightRange {
    pub fn new(min_y: i32, max_y: i32) -> Self {
        Self { min_y, max_y }
    }

    /// Whether this range has any room in it at all.
    pub fn is_empty(self) -> bool {
        self.min_y > self.max_y
    }
}

/// How often a placement is attempted, as the world's data expresses it.
///
/// Vanilla writes this two ways — a count of attempts per chunk, or a rarity
/// filter meaning "one attempt in one chunk out of N" — and both have to
/// survive being multiplied.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Attempts {
    /// `n` attempts in every chunk.
    PerChunk(u32),
    /// One attempt in one chunk out of `one_in`.
    RarityFilter { one_in: u32 },
}

impl Attempts {
    /// Attempts per chunk on average. This is the quantity a frequency
    /// multiplier multiplies, and expressing both forms as one number is what
    /// lets a single rule cover them.
    pub fn expected_per_chunk(self) -> f64 {
        match self {
            Self::PerChunk(n) => f64::from(n),
            Self::RarityFilter { one_in } => 1.0 / f64::from(one_in.max(1)),
        }
    }
}

/// One ore placement as the loaded world defines it, before Dust touches it.
#[derive(Debug, Clone, PartialEq)]
pub struct Baseline {
    /// The placed feature's identifier, e.g. `minecraft:ore_diamond_buried`.
    pub id: String,
    /// The ore group this placement belongs to — the knob an operator turns.
    pub group: OreGroup,
    pub attempts: Attempts,
    /// Blocks a single vein tries to place.
    pub vein_size: u32,
    pub height: HeightRange,
}

/// A placement after `[worldgen.ores]` has been applied to it.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    /// `false` when the ore is switched off. The generator skips it entirely
    /// rather than placing zero of it, so no random numbers are drawn for it.
    pub generate: bool,
    /// Attempts made in every chunk.
    pub attempts_per_chunk: u32,
    /// Probability of one further attempt, in `0.0..1.0`.
    ///
    /// This is what carries a fractional multiplier. It is also exactly what a
    /// vanilla rarity filter already is — one attempt with probability `1/N` —
    /// so the two forms collapse into one representation instead of the
    /// generator having to handle both.
    pub extra_attempt_chance: f64,
    pub vein_size: u32,
    pub height: HeightRange,
}

impl Resolved {
    /// Attempts per chunk on average, for reporting and for tests.
    pub fn expected_attempts_per_chunk(&self) -> f64 {
        if self.generate {
            f64::from(self.attempts_per_chunk) + self.extra_attempt_chance
        } else {
            0.0
        }
    }
}

/// Vanilla's ceiling on how many blocks one ore vein places.
///
/// Scaling past it silently produces a vein the feature cannot make, so the
/// multiplier is clamped here and the clamp is reported by
/// [`resolve_reporting`] rather than happening in silence.
pub const MAX_VEIN_SIZE: u32 = 64;

/// Something that happened during resolution that an operator should know
/// about, because it means the world will not do quite what the file asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Note {
    /// The requested vein size was above what the ore feature can place.
    VeinSizeClamped {
        id: String,
        requested: u32,
        used: u32,
    },
    /// The configured height bounds left no room, so the ore cannot generate.
    HeightRangeEmpty { id: String, min_y: i32, max_y: i32 },
}

impl std::fmt::Display for Note {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::VeinSizeClamped {
                id,
                requested,
                used,
            } => write!(
                f,
                "{id}: vein size {requested} is above the {MAX_VEIN_SIZE}-block maximum, \
                 generating {used} instead"
            ),
            Self::HeightRangeEmpty { id, min_y, max_y } => write!(
                f,
                "{id}: min_y {min_y} is above max_y {max_y} for this placement, so it will \
                 not generate at all"
            ),
        }
    }
}

/// Apply the configuration to one placement.
pub fn resolve(baseline: &Baseline, config: &OresConfig) -> Resolved {
    resolve_reporting(baseline, config).0
}

/// [`resolve`], plus anything worth telling the operator.
pub fn resolve_reporting(baseline: &Baseline, config: &OresConfig) -> (Resolved, Vec<Note>) {
    let settings = config.resolve_group(&baseline.group);
    let mut notes = Vec::new();

    // The fast, and much the most common, path. Written as an early return
    // rather than as arithmetic that happens to come out the same, because
    // "happens to come out the same" is a floating-point claim and this one has
    // to be exact — vanilla parity depends on it.
    if settings.is_identity() {
        return (identity(baseline), notes);
    }

    if !settings.enabled || settings.frequency <= 0.0 {
        return (
            Resolved {
                generate: false,
                attempts_per_chunk: 0,
                extra_attempt_chance: 0.0,
                vein_size: baseline.vein_size,
                height: baseline.height,
            },
            notes,
        );
    }

    let expected = baseline.attempts.expected_per_chunk() * settings.frequency;
    let whole = expected.floor();
    let attempts_per_chunk = whole.min(f64::from(u32::MAX)) as u32;
    let extra_attempt_chance = (expected - whole).clamp(0.0, 1.0);

    let scaled = (f64::from(baseline.vein_size) * settings.vein_size).round();
    let requested = scaled.clamp(1.0, f64::from(u32::MAX)) as u32;
    let vein_size = requested.min(MAX_VEIN_SIZE);
    if requested > vein_size {
        notes.push(Note::VeinSizeClamped {
            id: baseline.id.clone(),
            requested,
            used: vein_size,
        });
    }

    let height = HeightRange {
        min_y: settings.min_y.unwrap_or(baseline.height.min_y),
        max_y: settings.max_y.unwrap_or(baseline.height.max_y),
    };

    // An empty range is reachable from a configuration that validated fine:
    // `min_y = 100` is a legal value, and it is above the top of the range
    // vanilla gives diamond. The config parser cannot know that, because it has
    // not seen the world's data yet. This is where it becomes knowable, so this
    // is where it gets said.
    if height.is_empty() {
        notes.push(Note::HeightRangeEmpty {
            id: baseline.id.clone(),
            min_y: height.min_y,
            max_y: height.max_y,
        });
        return (
            Resolved {
                generate: false,
                attempts_per_chunk: 0,
                extra_attempt_chance: 0.0,
                vein_size,
                height,
            },
            notes,
        );
    }

    (
        Resolved {
            generate: true,
            attempts_per_chunk,
            extra_attempt_chance,
            vein_size,
            height,
        },
        notes,
    )
}

/// The baseline, expressed as a [`Resolved`], with nothing changed.
fn identity(baseline: &Baseline) -> Resolved {
    let (attempts_per_chunk, extra_attempt_chance) = match baseline.attempts {
        Attempts::PerChunk(n) => (n, 0.0),
        Attempts::RarityFilter { one_in } => (0, 1.0 / f64::from(one_in.max(1))),
    };
    Resolved {
        generate: true,
        attempts_per_chunk,
        extra_attempt_chance,
        vein_size: baseline.vein_size,
        height: baseline.height,
    }
}

/// Apply the configuration to every placement in a world.
pub fn resolve_all(baselines: &[Baseline], config: &OresConfig) -> (Vec<Resolved>, Vec<Note>) {
    let mut resolved = Vec::with_capacity(baselines.len());
    let mut notes = Vec::new();
    for baseline in baselines {
        let (one, mut its_notes) = resolve_reporting(baseline, config);
        resolved.push(one);
        notes.append(&mut its_notes);
    }
    (resolved, notes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dust_config::ore::OreOverride;

    /// A stand-in for the placements `xtask extract` will produce.
    ///
    /// These numbers are invented, and that is on purpose: if they were the
    /// real vanilla figures, a test asserting `3.0 × diamond` would be testing
    /// this module's arithmetic *and* quietly asserting a copy of Mojang's data
    /// that has no business being in the repository. Invented numbers test the
    /// arithmetic and nothing else.
    fn fixture() -> Vec<Baseline> {
        vec![
            Baseline {
                id: "test:ore_diamond".to_owned(),
                group: OreGroup::new("diamond"),
                attempts: Attempts::PerChunk(7),
                vein_size: 8,
                height: HeightRange::new(-64, 16),
            },
            Baseline {
                id: "test:ore_diamond_large".to_owned(),
                group: OreGroup::new("diamond"),
                attempts: Attempts::RarityFilter { one_in: 9 },
                vein_size: 12,
                height: HeightRange::new(-64, 16),
            },
            Baseline {
                id: "test:ore_iron".to_owned(),
                group: OreGroup::new("iron"),
                attempts: Attempts::PerChunk(10),
                vein_size: 9,
                height: HeightRange::new(-24, 56),
            },
        ]
    }

    fn config_with(group: &str, over: OreOverride) -> OresConfig {
        let mut config = OresConfig::default();
        config.overrides.insert(OreGroup::new(group), over);
        config
    }

    #[test]
    fn the_defaults_change_nothing_at_all() {
        // The parity guard. Not "close enough" — identical, including for the
        // rarity-filter form, which is the one that would drift if the two
        // representations were reconciled with arithmetic.
        let config = OresConfig::default();
        for baseline in fixture() {
            let resolved = resolve(&baseline, &config);
            assert!(resolved.generate);
            assert_eq!(resolved.vein_size, baseline.vein_size, "{}", baseline.id);
            assert_eq!(resolved.height, baseline.height, "{}", baseline.id);
            assert_eq!(
                resolved.expected_attempts_per_chunk(),
                baseline.attempts.expected_per_chunk(),
                "{}",
                baseline.id
            );
        }
    }

    #[test]
    fn the_master_switch_off_changes_nothing_either() {
        // The switch the Phase 6 differential test uses. It has to be identity
        // even when the file below it is full of extreme values.
        let mut config = config_with(
            "diamond",
            OreOverride {
                frequency: Some(50.0),
                vein_size: Some(8.0),
                ..Default::default()
            },
        );
        config.enabled = false;
        config.default_frequency = 20.0;
        for baseline in fixture() {
            assert_eq!(
                resolve(&baseline, &config),
                identity(&baseline),
                "{}",
                baseline.id
            );
        }
    }

    #[test]
    fn tripling_the_frequency_triples_the_attempts() {
        let config = config_with(
            "diamond",
            OreOverride {
                frequency: Some(3.0),
                ..Default::default()
            },
        );
        let resolved = resolve(&fixture()[0], &config);
        assert_eq!(resolved.attempts_per_chunk, 21);
        assert_eq!(resolved.extra_attempt_chance, 0.0);
    }

    #[test]
    fn a_multiplier_applies_to_every_placement_of_the_ore() {
        // One knob, three placements: this is the whole reason the setting is
        // keyed by ore group rather than by placed feature.
        let config = config_with(
            "diamond",
            OreOverride {
                frequency: Some(2.0),
                ..Default::default()
            },
        );
        let (resolved, _) = resolve_all(&fixture(), &config);
        assert_eq!(resolved[0].expected_attempts_per_chunk(), 14.0);
        assert!((resolved[1].expected_attempts_per_chunk() - 2.0 / 9.0).abs() < 1e-12);
        // ...and not to any other ore.
        assert_eq!(resolved[2].expected_attempts_per_chunk(), 10.0);
    }

    #[test]
    fn a_rarity_filter_can_be_scaled_past_one_attempt_per_chunk() {
        // 1-in-9 chunks, twenty-seven times as often, is three attempts in
        // every chunk. Getting this wrong by keeping the rarity form and
        // dividing 9 by 27 would give "one chunk in zero", which is where a
        // naive implementation divides by zero or silently stops scaling.
        let config = config_with(
            "diamond",
            OreOverride {
                frequency: Some(27.0),
                ..Default::default()
            },
        );
        let resolved = resolve(&fixture()[1], &config);
        assert_eq!(resolved.attempts_per_chunk, 3);
        assert!(resolved.extra_attempt_chance < 1e-12);
    }

    #[test]
    fn a_fractional_result_becomes_a_probability_rather_than_being_rounded_away() {
        // Cutting an ore to a twentieth of what it was has to leave a twentieth
        // of it, not none of it and not all of it.
        let config = config_with(
            "iron",
            OreOverride {
                frequency: Some(0.05),
                ..Default::default()
            },
        );
        let resolved = resolve(&fixture()[2], &config);
        assert_eq!(resolved.attempts_per_chunk, 0);
        assert!((resolved.extra_attempt_chance - 0.5).abs() < 1e-12);
        assert!(resolved.generate, "a rare ore still generates");
    }

    #[test]
    fn zero_frequency_and_disabled_both_stop_the_ore_generating() {
        for over in [
            OreOverride {
                frequency: Some(0.0),
                ..Default::default()
            },
            OreOverride {
                enabled: false,
                ..Default::default()
            },
        ] {
            let resolved = resolve(&fixture()[0], &config_with("diamond", over));
            assert!(!resolved.generate);
            assert_eq!(resolved.expected_attempts_per_chunk(), 0.0);
        }
    }

    #[test]
    fn vein_size_scales_independently_of_frequency() {
        let config = config_with(
            "diamond",
            OreOverride {
                frequency: Some(0.5),
                vein_size: Some(2.0),
                ..Default::default()
            },
        );
        let resolved = resolve(&fixture()[0], &config);
        assert_eq!(resolved.vein_size, 16);
        assert_eq!(resolved.expected_attempts_per_chunk(), 3.5);
    }

    #[test]
    fn an_impossible_vein_size_is_clamped_and_said_out_loud() {
        let config = config_with(
            "diamond",
            OreOverride {
                vein_size: Some(8.0),
                ..Default::default()
            },
        );
        let (resolved, notes) = resolve_reporting(&fixture()[1], &config);
        assert_eq!(resolved.vein_size, MAX_VEIN_SIZE);
        assert!(
            matches!(
                notes.as_slice(),
                [Note::VeinSizeClamped { requested: 96, .. }]
            ),
            "{notes:?}"
        );
    }

    #[test]
    fn a_height_override_replaces_only_the_bound_it_sets() {
        let config = config_with(
            "iron",
            OreOverride {
                max_y: Some(200),
                ..Default::default()
            },
        );
        let resolved = resolve(&fixture()[2], &config);
        assert_eq!(resolved.height, HeightRange::new(-24, 200));
    }

    #[test]
    fn a_height_range_that_misses_the_ore_entirely_is_reported() {
        // `min_y = 100` is a perfectly valid number and validation passes it.
        // Only here, with the world's data in hand, is it knowable that diamond
        // has nothing above y=16 to raise.
        let config = config_with(
            "diamond",
            OreOverride {
                min_y: Some(100),
                ..Default::default()
            },
        );
        let (resolved, notes) = resolve_reporting(&fixture()[0], &config);
        assert!(!resolved.generate);
        assert!(
            matches!(notes.as_slice(), [Note::HeightRangeEmpty { .. }]),
            "{notes:?}"
        );
    }

    #[test]
    fn the_default_frequency_reaches_ores_with_no_entry_of_their_own() {
        let config = OresConfig {
            default_frequency: 4.0,
            ..Default::default()
        };
        let (resolved, _) = resolve_all(&fixture(), &config);
        assert_eq!(resolved[2].expected_attempts_per_chunk(), 40.0);
    }

    #[test]
    fn resolution_does_not_depend_on_the_order_placements_arrive_in() {
        // Cheap to assert and worth asserting: the moment resolution carries
        // state between placements, ore density becomes chunk-order dependent
        // and the world stops being reproducible from its seed.
        let config = OresConfig {
            default_frequency: 2.5,
            ..Default::default()
        };
        let forward = resolve_all(&fixture(), &config).0;
        let mut reversed_input = fixture();
        reversed_input.reverse();
        let mut backward = resolve_all(&reversed_input, &config).0;
        backward.reverse();
        assert_eq!(forward, backward);
    }

    // What these tests do not catch, per the rule in `Testing.md`:
    //
    // - Nothing here places a block. Every assertion is about the numbers handed
    //   to the ore feature, and the feature that consumes them does not exist.
    //   A `Resolved` that is right and a generator that ignores it would pass
    //   every test above.
    // - `extra_attempt_chance` is asserted as a probability, never as an
    //   outcome. Whether the generator draws it from the chunk's own random
    //   source — which is what makes a world reproducible from its seed — is a
    //   property of the generator, and is untestable until there is one.
    // - The baselines are invented. These tests cannot tell whether Dust's real
    //   ore placements match vanilla's; only the Phase 6 seed-for-seed
    //   differential can.
}
