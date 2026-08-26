//! Reading `reports/blocks.json` into something that can be generated from.
//!
//! # The thing this module exists to get right
//!
//! A block state id encodes the block plus one value for each of the block's
//! properties. The ids of a block's states run contiguously from a base, and
//! the encoding is mixed-radix: the **first** property varies slowest.
//!
//! Which leaves the question of what "first" means, and the report answers it
//! three different ways depending on how you read it. On 1.21.1:
//!
//! - The order Mojang's report *serialises* the `properties` object in is not
//!   the id order for four blocks — chest, trapped_chest, piston_head and
//!   moving_piston. An extractor that preserved that order and trusted it would
//!   put chests and piston heads at the wrong ids.
//! - Alphabetical order — which is what parsing into a `BTreeMap` produces, and
//!   therefore what this module would see if it trusted its own input — happens
//!   to be the id order for all 1,060 blocks. That is very likely because
//!   Minecraft sorts a block's property definitions by name, but "very likely"
//!   is not something to generate code from, and it is a fact about this
//!   version rather than about the format.
//!
//! So the order is **derived from the ids** and then **verified against every
//! state**, which depends on neither reading. An extraction that cannot
//! reproduce every id fails rather than emitting code. See
//! [`Block::derive_property_order`].
//!
//! Four blocks out of 1,060 is exactly the size of defect that ships: the
//! generated code compiles, 99.6% of the world is right, and chests face the
//! wrong way.

use std::collections::BTreeMap;

use serde::Deserialize;

/// One block, as `reports/blocks.json` describes it.
#[derive(Debug, Deserialize)]
pub struct ReportedBlock {
    #[serde(default)]
    pub properties: BTreeMap<String, Vec<String>>,
    pub states: Vec<ReportedState>,
}

#[derive(Debug, Deserialize)]
pub struct ReportedState {
    pub id: u32,
    #[serde(default)]
    pub properties: BTreeMap<String, String>,
    #[serde(default)]
    pub default: bool,
}

/// A block, with its property order settled and every id accounted for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub name: String,
    pub base_state_id: u32,
    pub state_count: u32,
    pub default_state_id: u32,
    /// Properties in radix order: the first varies slowest, the last fastest.
    pub properties: Vec<Property>,
    /// True when alphabetical order is *not* the state-id order.
    ///
    /// Alphabetical is what parsing into a `BTreeMap` yields, so this is
    /// "a naive extractor would get this block wrong". On 1.21.1 it is false
    /// for every block, and it is recorded rather than assumed because that is
    /// a fact about one version and not about the format.
    pub alphabetical_order_disagrees: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Property {
    pub name: String,
    pub values: Vec<String>,
}

/// Everything the block report says, once it has been checked.
#[derive(Debug)]
pub struct Blocks {
    pub blocks: Vec<Block>,
    pub state_count: u32,
    /// The report as it was read, kept so the golden sample can be taken from
    /// it rather than from anything this module derived.
    pub reported: BTreeMap<String, ReportedBlock>,
}

pub fn parse(json: &[u8]) -> Result<Blocks, String> {
    let reported: BTreeMap<String, ReportedBlock> =
        serde_json::from_slice(json).map_err(|e| format!("could not read blocks.json: {e}"))?;

    let mut blocks = Vec::with_capacity(reported.len());
    for (name, block) in &reported {
        blocks.push(Block::from_report(name, block)?);
    }
    blocks.sort_by_key(|b| b.base_state_id);

    let state_count = check_states_tile_the_space(&blocks)?;
    Ok(Blocks {
        blocks,
        state_count,
        reported,
    })
}

impl Block {
    fn from_report(name: &str, reported: &ReportedBlock) -> Result<Self, String> {
        if reported.states.is_empty() {
            return Err(format!("{name} has no states"));
        }

        let base_state_id = reported
            .states
            .iter()
            .map(|s| s.id)
            .min()
            .expect("non-empty");
        let highest = reported
            .states
            .iter()
            .map(|s| s.id)
            .max()
            .expect("non-empty");
        let state_count = reported.states.len() as u32;
        if highest - base_state_id + 1 != state_count {
            return Err(format!(
                "{name} has {state_count} states spanning ids {base_state_id}..={highest}, \
                 which is not contiguous"
            ));
        }

        let defaults: Vec<u32> = reported
            .states
            .iter()
            .filter(|s| s.default)
            .map(|s| s.id)
            .collect();
        let [default_state_id] = defaults[..] else {
            return Err(format!(
                "{name} has {} default states, and needs one",
                defaults.len()
            ));
        };

        let properties = Self::derive_property_order(name, reported, base_state_id)?;

        // `reported.properties` is a BTreeMap, so its key order is alphabetical
        // rather than the order Mojang serialised — which is the point: this
        // compares the derivation against what a naive reading would produce.
        let alphabetical: Vec<&str> = reported.properties.keys().map(String::as_str).collect();
        let derived_order: Vec<&str> = properties.iter().map(|p| p.name.as_str()).collect();
        let block = Self {
            name: name.to_owned(),
            base_state_id,
            state_count,
            default_state_id,
            alphabetical_order_disagrees: alphabetical != derived_order,
            properties,
        };
        block.verify_every_state(reported)?;
        Ok(block)
    }

    /// Work out which property varies fastest by looking at what the ids do.
    ///
    /// The state at the base id is the one with every property at its first
    /// value. From there, the state that differs from it in exactly one
    /// property — that property at its *second* value — sits `stride` ids
    /// further on, where `stride` is the product of the value counts of every
    /// property that varies faster than it. Sorting the properties by
    /// descending stride is therefore the radix order, whatever order the
    /// report happened to serialise them in.
    fn derive_property_order(
        name: &str,
        reported: &ReportedBlock,
        base_state_id: u32,
    ) -> Result<Vec<Property>, String> {
        if reported.properties.is_empty() {
            return Ok(Vec::new());
        }

        for (property, values) in &reported.properties {
            if values.len() < 2 {
                return Err(format!(
                    "{name}.{property} has {} value(s); the order of a property that cannot \
                     vary is not derivable from the ids",
                    values.len()
                ));
            }
        }

        let base: BTreeMap<&str, &str> = reported
            .properties
            .iter()
            .map(|(property, values)| (property.as_str(), values[0].as_str()))
            .collect();

        let base_state = reported
            .states
            .iter()
            .find(|s| matches(&s.properties, &base))
            .ok_or_else(|| format!("{name} has no state with every property at its first value"))?;
        if base_state.id != base_state_id {
            return Err(format!(
                "{name}: the state with every property at its first value is id {}, and the \
                 lowest id is {base_state_id}",
                base_state.id
            ));
        }

        let mut strides = Vec::with_capacity(reported.properties.len());
        for (property, values) in &reported.properties {
            let mut wanted = base.clone();
            wanted.insert(property.as_str(), values[1].as_str());
            let state = reported
                .states
                .iter()
                .find(|s| matches(&s.properties, &wanted))
                .ok_or_else(|| {
                    format!(
                        "{name} has no state with {property} = {} and the rest at their \
                             first values",
                        values[1]
                    )
                })?;
            strides.push((state.id - base_state_id, property.clone(), values.clone()));
        }

        // Descending stride: the property that moves the id furthest varies
        // slowest, so it is the most significant digit.
        strides.sort_by_key(|(stride, _, _)| std::cmp::Reverse(*stride));
        Ok(strides
            .into_iter()
            .map(|(_, name, values)| Property { name, values })
            .collect())
    }

    /// Re-encode every state from the derived order and insist on the report's
    /// own id. This is what turns the derivation above from an argument into a
    /// check.
    fn verify_every_state(&self, reported: &ReportedBlock) -> Result<(), String> {
        for state in &reported.states {
            let mut index = 0u32;
            for property in &self.properties {
                let value = state.properties.get(&property.name).ok_or_else(|| {
                    format!(
                        "{}: state {} does not set {}",
                        self.name, state.id, property.name
                    )
                })?;
                let position = property
                    .values
                    .iter()
                    .position(|v| v == value)
                    .ok_or_else(|| {
                        format!(
                            "{}: state {} sets {} = {value}, which is not one of its values",
                            self.name, state.id, property.name
                        )
                    })? as u32;
                index = index * property.values.len() as u32 + position;
            }
            let computed = self.base_state_id + index;
            if computed != state.id {
                return Err(format!(
                    "{}: state {} re-encodes to {computed}. The property order derived from \
                     the ids does not reproduce them, so the generated code would be wrong.",
                    self.name, state.id
                ));
            }
        }

        let product: u32 = self
            .properties
            .iter()
            .map(|p| p.values.len() as u32)
            .product();
        let expected = if self.properties.is_empty() {
            1
        } else {
            product
        };
        if expected != self.state_count {
            return Err(format!(
                "{}: its properties describe {expected} combinations and it has {} states",
                self.name, self.state_count
            ));
        }
        Ok(())
    }
}

fn matches(state: &BTreeMap<String, String>, wanted: &BTreeMap<&str, &str>) -> bool {
    wanted
        .iter()
        .all(|(k, v)| state.get(*k).map(String::as_str) == Some(*v))
}

/// Every id from 0 upward belongs to exactly one block, with no gap and no
/// overlap.
///
/// Worth checking separately from the per-block contiguity above: each block
/// can be internally sound while the set of them leaves a hole, and a hole is a
/// state id that decodes to nothing at runtime.
fn check_states_tile_the_space(blocks: &[Block]) -> Result<u32, String> {
    let mut next = 0u32;
    for block in blocks {
        if block.base_state_id != next {
            return Err(format!(
                "state ids {next}..{} belong to no block; {} starts at {}",
                block.base_state_id, block.name, block.base_state_id
            ));
        }
        next += block.state_count;
    }
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A block whose report lists its properties in an order that is *not* the
    /// order its state ids use — the chest case, reduced.
    ///
    /// `first` has two values and `second` has three. The ids say `first`
    /// varies fastest, so the radix order is `[second, first]`, while the
    /// report's map is alphabetical and hands them over as `[first, second]`.
    /// An implementation that trusts the report's order produces the wrong id
    /// for four of the six states, and this fixture is what catches it.
    fn report_with_a_misleading_key_order() -> BTreeMap<String, ReportedBlock> {
        let mut states = Vec::new();
        for (i, (second, first)) in [
            ("a", "x"),
            ("a", "y"),
            ("b", "x"),
            ("b", "y"),
            ("c", "x"),
            ("c", "y"),
        ]
        .into_iter()
        .enumerate()
        {
            states.push(ReportedState {
                id: i as u32,
                properties: [
                    ("first".to_owned(), first.to_owned()),
                    ("second".to_owned(), second.to_owned()),
                ]
                .into(),
                default: i == 0,
            });
        }
        [(
            "test:block".to_owned(),
            ReportedBlock {
                properties: [
                    ("first".to_owned(), vec!["x".to_owned(), "y".to_owned()]),
                    (
                        "second".to_owned(),
                        vec!["a".to_owned(), "b".to_owned(), "c".to_owned()],
                    ),
                ]
                .into(),
                states,
            },
        )]
        .into()
    }

    fn parse_map(map: BTreeMap<String, ReportedBlock>) -> Result<Blocks, String> {
        parse(&serde_json::to_vec(&as_json(&map)).expect("serialises"))
    }

    /// The deserialise-side types are the only definition, so the fixtures are
    /// turned back into JSON rather than a second set of types being written.
    fn as_json(map: &BTreeMap<String, ReportedBlock>) -> serde_json::Value {
        let mut out = serde_json::Map::new();
        for (name, block) in map {
            let states: Vec<serde_json::Value> = block
                .states
                .iter()
                .map(|s| {
                    let mut o = serde_json::Map::new();
                    o.insert("id".into(), s.id.into());
                    if s.default {
                        o.insert("default".into(), true.into());
                    }
                    if !s.properties.is_empty() {
                        o.insert(
                            "properties".into(),
                            serde_json::to_value(&s.properties).expect("map"),
                        );
                    }
                    serde_json::Value::Object(o)
                })
                .collect();
            let mut o = serde_json::Map::new();
            if !block.properties.is_empty() {
                o.insert(
                    "properties".into(),
                    serde_json::to_value(&block.properties).expect("map"),
                );
            }
            o.insert("states".into(), states.into());
            out.insert(name.clone(), serde_json::Value::Object(o));
        }
        serde_json::Value::Object(out)
    }

    #[test]
    fn the_property_order_comes_from_the_ids_and_not_from_the_key_order() {
        let parsed = parse_map(report_with_a_misleading_key_order()).expect("parses");
        let block = &parsed.blocks[0];
        let order: Vec<&str> = block.properties.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            order,
            ["second", "first"],
            "the report's key order is [first, second]"
        );
        assert_eq!(parsed.state_count, 6);
    }

    #[test]
    fn a_report_whose_ids_cannot_be_reproduced_fails_the_extraction() {
        // The whole point of verifying rather than deriving-and-hoping: if the
        // encoding is ever something other than mixed-radix, this has to stop
        // rather than emit code that is wrong in a way nothing else notices.
        let mut map = report_with_a_misleading_key_order();
        let block = map.get_mut("test:block").expect("present");
        block.states[3].id = 99;
        let err = parse_map(map).expect_err("must not be accepted");
        assert!(
            err.contains("contiguous") || err.contains("re-encodes"),
            "{err}"
        );
    }

    #[test]
    fn a_block_with_no_properties_has_one_state() {
        let map: BTreeMap<String, ReportedBlock> = [(
            "test:stone".to_owned(),
            ReportedBlock {
                properties: BTreeMap::new(),
                states: vec![ReportedState {
                    id: 0,
                    properties: BTreeMap::new(),
                    default: true,
                }],
            },
        )]
        .into();
        let parsed = parse_map(map).expect("parses");
        assert!(parsed.blocks[0].properties.is_empty());
        assert_eq!(parsed.blocks[0].state_count, 1);
    }

    #[test]
    fn a_gap_between_two_blocks_fails_the_extraction() {
        // Each block can be internally sound while the set of them leaves a
        // hole, and a hole is a state id that decodes to nothing at runtime.
        let map: BTreeMap<String, ReportedBlock> = [
            (
                "test:a".to_owned(),
                ReportedBlock {
                    properties: BTreeMap::new(),
                    states: vec![ReportedState {
                        id: 0,
                        properties: BTreeMap::new(),
                        default: true,
                    }],
                },
            ),
            (
                "test:b".to_owned(),
                ReportedBlock {
                    properties: BTreeMap::new(),
                    states: vec![ReportedState {
                        id: 7,
                        properties: BTreeMap::new(),
                        default: true,
                    }],
                },
            ),
        ]
        .into();
        let err = parse_map(map).expect_err("must not be accepted");
        assert!(err.contains("belong to no block"), "{err}");
    }

    #[test]
    fn a_block_with_two_defaults_fails_the_extraction() {
        let mut map = report_with_a_misleading_key_order();
        map.get_mut("test:block").expect("present").states[2].default = true;
        let err = parse_map(map).expect_err("must not be accepted");
        assert!(err.contains("default"), "{err}");
    }

    // What these tests do not catch: they say nothing about whether Mojang's
    // report means what this module reads it as meaning. The check that catches
    // that is the extraction itself refusing to emit code it cannot verify,
    // run against the real 1.21.1 report — and, downstream, the round-trip over
    // all 26,684 real states in dust-registry.
}
