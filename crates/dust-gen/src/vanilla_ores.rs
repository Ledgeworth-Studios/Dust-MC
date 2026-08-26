//! The extracted vanilla ore baseline, and what turns it into [`Baseline`]s.
//!
//! [`crate::ore_density`] deliberately knows nothing about vanilla's numbers —
//! see that module's header for why, twice over. This is the other side of that
//! line: the table produced by `cargo xtask extract` from a server jar on the
//! operator's own machine, and the small amount of hand-written code that hands
//! it to the resolver.
//!
//! Keeping the two apart is not tidiness. A resolver that reached for a vanilla
//! constant would be quietly wrong on every modded world; a resolver that takes
//! a `&[Baseline]` is right on all of them, and this module is one caller that
//! happens to supply vanilla's.
//!
//! # What the table is not
//!
//! It is one Minecraft version's placements, generated once and committed. It
//! is not the baseline a running server uses: a world with datapacks has
//! different placements, and reading *those* is a Phase 2 job that does not
//! exist. Until it does, [`baselines`] is what Dust knows about ore, and it is
//! right for exactly the vanilla case.
//!
//! # Why there are two tables
//!
//! [`generated::ores::PLACEMENTS`] is the interpretation: counts turned into
//! attempts, `below_top 10` turned into y=117, features gathered into groups.
//! [`generated::ores::SOURCE_ROWS`] is the same placements as the worldgen
//! files literally write them, copied by a second pass in the extractor that
//! shares no interpretation with the first.
//!
//! The tests below check one against the other, and the reason is the one the
//! block extractor's golden sample makes at length: a table that agrees with
//! itself proves the reader is self-consistent, which is not the question. The
//! question is whether it agrees with Minecraft, and only a row that never went
//! through the resolution can fail when the resolution is systematically wrong.
//!
//! Nothing here asserts a vanilla number by hand. Typing `vein_size == 8` into
//! this file would put Mojang's data in the repository by the back door, which
//! is the thing the whole extraction pipeline exists to avoid. Every assertion
//! is structural, or is one table against the other.

use dust_config::ore::OreGroup;

use crate::generated::ores;
use crate::ore_density::{Attempts, Baseline, HeightRange};

/// A dimension's vertical generation context.
///
/// `min_y` and `height` are the `dimension_type`'s bounds already narrowed by
/// the generator's, which is what Minecraft resolves a relative height anchor
/// against. The Nether is 256 blocks tall by its dimension type and 128 by its
/// noise settings, and 128 is the one that counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dimension {
    pub id: &'static str,
    pub min_y: i32,
    pub height: i32,
}

impl Dimension {
    /// The highest y that generates — one below the top of the range.
    pub fn max_y(&self) -> i32 {
        self.min_y + self.height - 1
    }
}

/// One ore placement, as the generated table holds it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    /// The placed feature's id, e.g. `minecraft:ore_diamond_buried`.
    pub id: &'static str,
    /// The ore group this placement belongs to — the knob an operator turns.
    pub group: &'static str,
    pub dimension: &'static str,
    pub attempts: Attempts,
    pub vein_size: u32,
    pub height: HeightRange,
    /// `minecraft:uniform` or `minecraft:trapezoid`.
    ///
    /// Nothing reads this. A [`HeightRange`] is a pair of bounds and cannot
    /// hold the shape of the distribution between them, and 13 of vanilla's 40
    /// placements are trapezoids — so the fact is carried rather than lost. The
    /// day a height override has to preserve the character of a distribution
    /// instead of just its ends, it should not need another extraction.
    pub distribution: &'static str,
    /// The ore feature's chance of dropping a block that would be exposed to
    /// air. Carried for the same reason as [`Placement::distribution`].
    pub discard_chance_on_air_exposure: f64,
}

/// An ore group and the block states that define it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OreGroupDef {
    pub name: &'static str,
    /// The block ids the group's placements put down.
    ///
    /// This is the derivation and not a description of it: sharing one of these
    /// is what gathered the placements, and their common part is what named the
    /// group. It is also what makes the table answer "why is `dirt` a knob" on
    /// the page rather than in a document — because `minecraft:dirt` is placed
    /// by the ore feature like anything else here.
    pub targets: &'static [&'static str],
}

/// One placement's source facts, copied out of the worldgen JSON uninterpreted.
///
/// See the module header. Every field is a literal that appears in a file: the
/// count as written, each height bound's spelling kept apart from its number,
/// and the two pairs of numbers the dimension's extent was narrowed from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SourceRow {
    pub placed_feature: &'static str,
    pub configured_feature: &'static str,
    /// `"4"`, `"0..=1"`, or `""` where the placement has no count step at all.
    pub count: &'static str,
    /// The rarity chance as written, or `""` where there is no such step.
    pub rarity: &'static str,
    pub size: u32,
    pub min_anchor: &'static str,
    pub min_value: i32,
    pub max_anchor: &'static str,
    pub max_value: i32,
    pub dimension: &'static str,
    pub dimension_type_min_y: i32,
    pub dimension_type_height: i32,
    pub noise_min_y: i32,
    pub noise_height: i32,
    /// The target block ids, sorted, comma-joined.
    pub targets: &'static str,
}

/// The Minecraft version the table was extracted from.
pub const DATA_VERSION: &str = ores::DATA_VERSION;

/// Vanilla's ore placements, as [`crate::ore_density::resolve_all`] wants them.
///
/// Allocates, because [`Baseline`] owns its id — it has to, since a world's
/// placements come from files at runtime and cannot be `'static`. Called once
/// per world, so the allocation is not worth designing around.
pub fn baselines() -> Vec<Baseline> {
    ores::PLACEMENTS
        .iter()
        .map(|placement| Baseline {
            id: placement.id.to_owned(),
            group: OreGroup::new(placement.group),
            attempts: placement.attempts,
            vein_size: placement.vein_size,
            height: placement.height,
        })
        .collect()
}

/// Every ore group vanilla has, in the form
/// [`dust_config::ore::OresConfig::validate_against`] takes.
///
/// This is what turns a misspelled `[worldgen.ores.overrides.diamnod]` into an
/// error naming the nearest real ore, instead of a server that started and a
/// setting that did nothing.
pub fn groups() -> std::collections::BTreeSet<OreGroup> {
    ores::GROUPS
        .iter()
        .map(|group| OreGroup::new(group.name))
        .collect()
}

/// The generation context of a dimension in the table, by id.
pub fn dimension(id: &str) -> Option<&'static Dimension> {
    ores::DIMENSIONS.iter().find(|d| d.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dust_config::ore::{OreOverride, OresConfig, VANILLA_ORE_GROUPS};

    use crate::ore_density::{resolve, resolve_all, Note};

    #[test]
    fn the_table_is_not_empty_and_every_placement_has_a_group() {
        // The cheapest possible guard against the extractor emitting a file
        // that compiles and says nothing, which is exactly what a wrong path or
        // a changed data layout would produce.
        assert!(!ores::PLACEMENTS.is_empty());
        assert!(!ores::GROUPS.is_empty());
        assert_eq!(ores::PLACEMENTS.len(), ores::SOURCE_ROWS.len());
        for placement in ores::PLACEMENTS {
            assert!(
                ores::GROUPS.iter().any(|g| g.name == placement.group),
                "{} is in group {}, which is not in GROUPS",
                placement.id,
                placement.group
            );
            assert!(
                dimension(placement.dimension).is_some(),
                "{} generates in {}, which is not in DIMENSIONS",
                placement.id,
                placement.dimension
            );
        }
    }

    #[test]
    fn every_group_the_configuration_documents_is_a_group_the_table_has() {
        // `VANILLA_ORE_GROUPS` is hand-written, and a name in it that no world
        // produces is a setting an operator can write and nothing can apply.
        // The extractor checks this too; it is asserted here as well because
        // this is the copy that runs on every pull request.
        let groups = groups();
        for name in VANILLA_ORE_GROUPS {
            assert!(
                groups.contains(&OreGroup::new(*name)),
                "the configuration documents `{name}` and the extracted table has no \
                 such group"
            );
        }
    }

    #[test]
    fn a_group_gathers_every_placement_that_places_its_blocks() {
        // D6's whole design: one knob reaches all of an ore's placements. The
        // assertion is structural — nothing here names diamond — so it holds
        // for a datapack's ores as well as vanilla's.
        for group in ores::GROUPS {
            let members: Vec<&Placement> = ores::PLACEMENTS
                .iter()
                .filter(|p| p.group == group.name)
                .collect();
            assert!(
                !members.is_empty(),
                "group {} has no placements",
                group.name
            );
            for target in group.targets {
                let sources: Vec<&SourceRow> = ores::SOURCE_ROWS
                    .iter()
                    .filter(|row| row.targets.split(',').any(|t| t == *target))
                    .collect();
                assert!(
                    !sources.is_empty(),
                    "group {} claims {target}, and no placement places it",
                    group.name
                );
                for row in sources {
                    let placement = ores::PLACEMENTS
                        .iter()
                        .find(|p| p.id == row.placed_feature)
                        .expect("every source row has a placement");
                    assert_eq!(
                        placement.group, group.name,
                        "{} places {target}, which is {}'s, and is filed under {}",
                        row.placed_feature, group.name, placement.group
                    );
                }
            }
        }
    }

    #[test]
    fn at_least_one_group_has_several_placements() {
        // Without this the test above is satisfied by a table where every
        // placement is its own group — which would compile, pass everything
        // else, and quietly undo the one thing D6 decided.
        assert!(
            ores::GROUPS.iter().any(|group| {
                ores::PLACEMENTS
                    .iter()
                    .filter(|p| p.group == group.name)
                    .count()
                    > 1
            }),
            "no ore group gathers more than one placement"
        );
    }

    /// The height bounds in [`ores::PLACEMENTS`], re-derived from the source
    /// rows.
    ///
    /// A second implementation of the anchor arithmetic, written against the
    /// literals rather than against the extractor's reading of them. If the
    /// extractor resolved `below_top` from the wrong ceiling, or took the
    /// dimension type's height where the generator's was narrower, the two
    /// disagree here.
    fn height_from_source(row: &SourceRow) -> HeightRange {
        let min_y = row.dimension_type_min_y.max(row.noise_min_y);
        let height = row.dimension_type_height.min(row.noise_height);
        let resolve = |anchor: &str, value: i32| match anchor {
            "absolute" => value,
            "above_bottom" => min_y + value,
            "below_top" => min_y + height - 1 - value,
            other => panic!("{}: unknown anchor {other}", row.placed_feature),
        };
        HeightRange::new(
            resolve(row.min_anchor, row.min_value),
            resolve(row.max_anchor, row.max_value),
        )
    }

    /// The attempts in [`ores::PLACEMENTS`], re-derived from the source rows.
    ///
    /// The three shapes the data uses, read again from the literals: a count, a
    /// rarity chance, and — the one that is a written *absence* — neither,
    /// which is one attempt per chunk and not none.
    fn attempts_from_source(row: &SourceRow) -> Attempts {
        match (row.count, row.rarity) {
            ("", "") => Attempts::PerChunk(1),
            ("", chance) => Attempts::RarityFilter {
                one_in: chance.parse().expect("a rarity chance is a number"),
            },
            ("0..=1", "") => Attempts::RarityFilter { one_in: 2 },
            (count, "") => Attempts::PerChunk(count.parse().unwrap_or_else(|_| {
                panic!("{}: cannot read a count from {count:?}", row.placed_feature)
            })),
            (count, chance) => panic!(
                "{}: has both a count ({count}) and a rarity chance ({chance})",
                row.placed_feature
            ),
        }
    }

    #[test]
    fn every_row_of_the_table_agrees_with_the_source_it_came_from() {
        // The check that is not a round trip. Everything asserted here is
        // re-derived from literals copied out of the worldgen files by a pass
        // that shares no interpretation with the one that built the table, so a
        // systematically wrong reading fails it instead of matching itself.
        for row in ores::SOURCE_ROWS {
            let placement = ores::PLACEMENTS
                .iter()
                .find(|p| p.id == row.placed_feature)
                .unwrap_or_else(|| panic!("{} is in no placement", row.placed_feature));

            assert_eq!(
                placement.vein_size, row.size,
                "{}: vein size",
                row.placed_feature
            );
            assert_eq!(
                placement.attempts,
                attempts_from_source(row),
                "{}: attempts",
                row.placed_feature
            );
            assert_eq!(
                placement.height,
                height_from_source(row),
                "{}: height ({} {} .. {} {} in {})",
                row.placed_feature,
                row.min_anchor,
                row.min_value,
                row.max_anchor,
                row.max_value,
                row.dimension
            );
            assert_eq!(
                placement.dimension, row.dimension,
                "{}: dimension",
                row.placed_feature
            );

            let dimension = dimension(row.dimension).expect("a known dimension");
            assert_eq!(
                dimension.min_y,
                row.dimension_type_min_y.max(row.noise_min_y),
                "{}: the dimension's floor",
                row.dimension
            );
            assert_eq!(
                dimension.height,
                row.dimension_type_height.min(row.noise_height),
                "{}: the dimension's height",
                row.dimension
            );
        }
    }

    #[test]
    fn only_a_trapezoid_reaches_outside_the_dimension_it_generates_in() {
        // A bound outside the dimension looks like a wrong anchor reading and
        // is not: six of vanilla's placements really do declare a range wider
        // than the world. All six are trapezoids, and that is the reason —
        // a trapezoid's range positions its *peak*, and the tails are meant to
        // be clipped by the world's floor and ceiling. Diamond's `above_bottom
        // -80` is y = -144, eighty blocks below bedrock, which is what puts the
        // peak of its distribution at y = -59.
        //
        // So the assertion is not "inside the dimension", which would be false.
        // It is that nothing *uniform* leaves its dimension — a uniform
        // placement that did would be an anchor resolved against the wrong
        // ceiling, which is the mistake worth catching.
        for placement in ores::PLACEMENTS {
            let dimension = dimension(placement.dimension).expect("a known dimension");
            assert!(
                !placement.height.is_empty(),
                "{} has an empty height range",
                placement.id
            );
            let outside = placement.height.min_y < dimension.min_y
                || placement.height.max_y > dimension.max_y();
            assert!(
                !outside || placement.distribution == "minecraft:trapezoid",
                "{} is {} and runs {}..={}, outside {}'s {}..={}",
                placement.id,
                placement.distribution,
                placement.height.min_y,
                placement.height.max_y,
                dimension.id,
                dimension.min_y,
                dimension.max_y()
            );
        }
    }

    #[test]
    fn the_defaults_return_vanilla_unchanged_exactly() {
        // The identity property of D6, now against vanilla's real figures
        // rather than invented ones. Exactly, not approximately: this is the
        // switch the Phase 6 seed-for-seed parity run leaves on.
        let config = OresConfig::default();
        for baseline in baselines() {
            let resolved = resolve(&baseline, &config);
            assert!(resolved.generate, "{}", baseline.id);
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
    fn the_master_switch_off_returns_vanilla_unchanged_exactly() {
        // And with extreme values underneath it, because a switch that is only
        // identity over a quiet file is not the switch the parity run needs.
        let mut config = OresConfig {
            enabled: false,
            default_frequency: 20.0,
            ..Default::default()
        };
        for group in ores::GROUPS {
            config.overrides.insert(
                OreGroup::new(group.name),
                OreOverride {
                    frequency: Some(50.0),
                    vein_size: Some(8.0),
                    min_y: Some(200),
                    max_y: Some(-100),
                    enabled: false,
                },
            );
        }

        let baselines = baselines();
        let (resolved, notes) = resolve_all(&baselines, &config);
        assert!(notes.is_empty(), "{notes:?}");
        for (baseline, resolved) in baselines.iter().zip(&resolved) {
            assert!(resolved.generate, "{}", baseline.id);
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
    fn a_multiplier_reaches_every_placement_of_its_group_and_no_other() {
        // Vanilla generates several placements per ore, which is the reason the
        // knob is keyed by group at all. Asserted over whichever group happens
        // to have the most placements, so the test does not name an ore.
        let biggest = ores::GROUPS
            .iter()
            .max_by_key(|g| {
                ores::PLACEMENTS
                    .iter()
                    .filter(|p| p.group == g.name)
                    .count()
            })
            .expect("the table has groups");

        let mut config = OresConfig::default();
        config.overrides.insert(
            OreGroup::new(biggest.name),
            OreOverride {
                frequency: Some(2.0),
                ..Default::default()
            },
        );

        let baselines = baselines();
        let (resolved, _) = resolve_all(&baselines, &config);
        for (baseline, resolved) in baselines.iter().zip(&resolved) {
            let expected = baseline.attempts.expected_per_chunk()
                * if baseline.group.as_str() == biggest.name {
                    2.0
                } else {
                    1.0
                };
            assert!(
                (resolved.expected_attempts_per_chunk() - expected).abs() < 1e-12,
                "{}: wanted {expected}, got {}",
                baseline.id,
                resolved.expected_attempts_per_chunk()
            );
        }
    }

    #[test]
    fn a_height_override_that_misses_a_real_ore_is_reported_against_it() {
        // `min_y = 320` is a valid number that validation passes and that no
        // placement in any dimension can satisfy. Every group must say so,
        // which is the check that the reported bound is the placement's own
        // rather than a constant.
        let mut config = OresConfig::default();
        for group in ores::GROUPS {
            config.overrides.insert(
                OreGroup::new(group.name),
                OreOverride {
                    min_y: Some(2031),
                    ..Default::default()
                },
            );
        }
        let (resolved, notes) = resolve_all(&baselines(), &config);
        assert!(resolved.iter().all(|r| !r.generate));
        assert_eq!(notes.len(), ores::PLACEMENTS.len());
        assert!(notes
            .iter()
            .all(|n| matches!(n, Note::HeightRangeEmpty { .. })));
    }

    // What these tests do not catch:
    //
    // - Nothing here places a block. Every assertion is about the numbers
    //   handed to an ore feature, and the feature that would consume them does
    //   not exist. Only the Phase 6 seed-for-seed differential against a real
    //   vanilla server can say whether Dust's ore placement matches vanilla's.
    // - The source rows share the extractor's dimension attribution, so a
    //   placement filed under the wrong dimension would produce a consistent
    //   pair of rows and pass. That case is guarded in the extractor instead: a
    //   placement whose biomes span two dimensions, or none, stops it outright.
    // - `distribution` is carried and never checked against anything, because
    //   nothing consumes it yet. A wrong value there would be invisible here.
}
