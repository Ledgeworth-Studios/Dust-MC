//! What the reports say about fluids, joined across three of them.
//!
//! The fluid registry itself is five names and five numbers, already emitted
//! with every other flat registry. What this module adds is the relationships
//! the other reports carry and the fluid report alone does not: which block a
//! fluid *is*, which item carries it, and which still fluid a flowing one is
//! the movement of. Each leg of that join is a fact from a different file, and
//! each is checked against the table it came from rather than assumed.
//!
//! # What is derivable, and what is not
//!
//! - **Fluid to block** is a name join: `minecraft:water` is also a block,
//!   listed in `blocks.json`. On 1.21.1 that is exactly the still fluids —
//!   `flowing_water` has no block of its own, because a fluid's level lives in
//!   the fluid rather than in the block state — and the join refuses a still
//!   fluid whose block is missing. `minecraft:empty` has no block either; air
//!   is what an empty fluid looks like, but that is knowledge from outside
//!   these files, and inventing it here is exactly what the provenance line
//!   forbids.
//! - **Fluid to bucket** is likewise a name join against the item registry:
//!   `minecraft:water` pairs with `minecraft:water_bucket`. Only still fluids
//!   get one — nobody carries a flowing-water bucket, and the derivation says
//!   so rather than special-casing it.
//! - **Flowing to still** is the `flowing_` prefix, checked both ways: every
//!   flowing fluid names a still one, and every still fluid except `empty`
//!   has its flowing partner beside it in the same registry.
//!
//! Bounding boxes, flow rates, tick delays: none of that is anywhere in the
//! 1.21.1 generator output. A future version whose generators publish it can
//! extend [`Fluids`] without changing the shape of what is here.

use std::collections::{BTreeMap, BTreeSet};

use super::blocks::Blocks;
use super::registries::Registries;

/// The fluid registry, as every other module spells it.
const FLUID_REGISTRY: &str = "minecraft:fluid";
const ITEM_REGISTRY: &str = "minecraft:item";

/// One fluid's relationships, once the joins above have held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fluid {
    /// Namespaced id, e.g. `minecraft:lava`. Protocol-id order is taken from
    /// the registry, so this sits at its own index in the generated table.
    pub name: String,
    pub protocol_id: u32,
    /// The block this fluid fills, when the block report lists one.
    pub block: Option<String>,
    /// The item that carries this fluid, when the item registry lists one.
    pub bucket: Option<String>,
    /// For `flowing_water`, the still fluid it moves: `minecraft:water`.
    pub flowing_of: Option<String>,
}

#[derive(Debug)]
pub struct Fluids {
    /// Every fluid, in protocol-id order.
    pub fluids: Vec<Fluid>,
    /// The same relationships copied out of the three reports by [`source_rows`],
    /// a pass that shares no reading with the one above. Rendered into the
    /// generated file as the golden sample.
    pub source: Vec<SourceFluid>,
}

/// One row of the copying pass: plain text, `""` for "none".
///
/// Nothing here is resolved or checked beyond existence of the names; it is
/// what the files say, spelled out, so that a systematically wrong *reading*
/// up in [`parse`] shows up as a disagreement between two tables rather than
/// as one consistent wrong answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFluid {
    pub name: String,
    pub block: String,
    pub bucket: String,
    pub flowing_of: String,
}

/// Join the fluid registry against the block report and the item registry.
///
/// Both joins are checked as they happen. On 1.21.1 the still fluids are
/// exactly the ones with a same-named block — `flowing_water` has no block of
/// its own, because since the flattening a fluid's level lives in the fluid,
/// not in the block state — and a still fluid whose flowing partner is missing
/// stops here rather than reaching the tables half-joined.
pub fn parse(registries: &Registries, blocks: &Blocks) -> Result<Fluids, String> {
    let fluid_registry = find_registry(registries, FLUID_REGISTRY)?;
    let item_registry = find_registry(registries, ITEM_REGISTRY)?;

    let blocks_by_name: BTreeMap<&str, &super::blocks::Block> =
        blocks.blocks.iter().map(|b| (b.name.as_str(), b)).collect();

    let mut fluids = Vec::with_capacity(fluid_registry.entries.len());
    for entry in &fluid_registry.entries {
        let body = entry
            .name
            .strip_prefix("minecraft:")
            .ok_or_else(|| format!("{FLUID_REGISTRY} entry {} is outside the minecraft namespace, and every rule below is written for it", entry.name))?;

        let flowing_of = body
            .strip_prefix("flowing_")
            .map(|still| format!("minecraft:{still}"));

        // Only still, non-empty fluids fill a same-named block on 1.21.1.
        let block = if flowing_of.is_some() || body == "empty" {
            None
        } else {
            match blocks_by_name.get(entry.name.as_str()) {
                Some(block) => Some(block.name.clone()),
                None => {
                    return Err(format!(
                        "{} is not held by any block in the block report, and every still \
                         fluid but minecraft:empty fills one. A report that lost its water \
                         block is a report this cannot join against.",
                        entry.name
                    ));
                }
            }
        };

        let bucket = if flowing_of.is_some() || body == "empty" {
            None
        } else {
            let candidate = format!("minecraft:{body}_bucket");
            if item_registry.entries.iter().any(|e| e.name == candidate) {
                Some(candidate)
            } else {
                return Err(format!(
                    "{candidate} is not an item, though {FLUID_REGISTRY} lists {body}. The \
                     fluid-to-bucket join came up empty, which on 1.21.1 is not something \
                     the data does."
                ));
            }
        };

        fluids.push(Fluid {
            name: entry.name.clone(),
            protocol_id: entry.protocol_id,
            block,
            bucket,
            flowing_of,
        });
    }

    fluids.sort_by_key(|f| f.protocol_id);
    let source = source_rows(registries, blocks)?;

    let fluids = Fluids { fluids, source };
    check_still_and_flowing_pair_up(&fluids.fluids)?;
    Ok(fluids)
}

/// The copying pass: the same three reports, read with no shared code.
///
/// This does not call [`parse`] and [`parse`] does not call it; they read the
/// same inputs and nothing else. In particular the rules are restated here in
/// their plainest form — a name is present or it is not — so that a wrong rule
/// up above cannot drag this table along with it. `""` means "the reports name
/// nothing here", which is exactly what the golden row should say.
pub fn source_rows(registries: &Registries, blocks: &Blocks) -> Result<Vec<SourceFluid>, String> {
    let fluid_registry = find_registry(registries, FLUID_REGISTRY)?;
    let item_registry = find_registry(registries, ITEM_REGISTRY)?;

    let block_names: BTreeSet<&str> = blocks.blocks.iter().map(|b| b.name.as_str()).collect();
    let item_names: BTreeSet<&str> = item_registry
        .entries
        .iter()
        .map(|e| e.name.as_str())
        .collect();

    let mut out = Vec::with_capacity(fluid_registry.entries.len());
    for entry in &fluid_registry.entries {
        let body = entry.name.strip_prefix("minecraft:").ok_or_else(|| {
            format!(
                "{FLUID_REGISTRY} entry {} has no minecraft prefix",
                entry.name
            )
        })?;
        let still = format!("minecraft:{body}");
        let flowing_of = match body.strip_prefix("flowing_") {
            Some(base) => format!("minecraft:{base}"),
            None => String::new(),
        };
        // Restated in its plainest form: the empty fluid fills nothing, a
        // flowing fluid fills its own name when that is a block (it is not,
        // on 1.21.1), and everything else joins by name on both sides.
        let block = if block_names.contains(entry.name.as_str()) {
            entry.name.clone()
        } else {
            String::new()
        };
        let bucket = if flowing_of.is_empty() && body != "empty" {
            let candidate = format!("minecraft:{body}_bucket");
            if item_names.contains(candidate.as_str()) {
                candidate
            } else {
                return Err(format!("{still} pairs with no bucket item"));
            }
        } else {
            String::new()
        };
        out.push(SourceFluid {
            name: entry.name.clone(),
            block,
            bucket,
            flowing_of,
        });
    }
    out.sort_by_key(|f| {
        fluid_registry
            .entries
            .iter()
            .find(|e| e.name == f.name)
            .map(|e| e.protocol_id)
            .unwrap_or(u32::MAX)
    });
    Ok(out)
}

fn find_registry<'a>(
    registries: &'a Registries,
    name: &str,
) -> Result<&'a super::registries::Registry, String> {
    registries
        .registries
        .iter()
        .find(|r| r.name == name)
        .ok_or_else(|| format!("the registry report has no {name}"))
}

/// Every flowing fluid names a still one that exists, and every still fluid
/// apart from `minecraft:empty` has its flowing partner.
///
/// Worth insisting on in both directions: the flowing half is what makes a
/// lookup by "which fluid is in this block" ambiguous otherwise, and a missing
/// partner means one of the two numbers never reaches the tables.
fn check_still_and_flowing_pair_up(fluids: &[Fluid]) -> Result<(), String> {
    for fluid in fluids {
        if let Some(still) = &fluid.flowing_of {
            if !fluids.iter().any(|f| &f.name == still) {
                return Err(format!(
                    "{} names {still} as the fluid it flows from, and the registry has no such \
                     entry",
                    fluid.name
                ));
            }
        }
    }
    for fluid in fluids {
        let Some(body) = fluid.name.strip_prefix("minecraft:") else {
            continue;
        };
        if body == "empty" || body.starts_with("flowing_") {
            continue;
        }
        let flowing = format!("minecraft:flowing_{body}");
        if !fluids.iter().any(|f| f.name == flowing) {
            return Err(format!(
                "{} has no `{flowing}` partner in the registry. Every still fluid on 1.21.1 \
                 is paired; a lone one means the join is reading a registry this was not \
                 written for.",
                fluid.name
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::registries::Registry;
    use super::*;

    fn registry_entry(name: &str, id: u32) -> super::super::registries::Entry {
        super::super::registries::Entry {
            name: name.to_owned(),
            protocol_id: id,
        }
    }

    fn registry(name: &str, entries: Vec<(&str, u32)>) -> Registry {
        Registry {
            name: name.to_owned(),
            protocol_id: 0,
            entries: entries
                .into_iter()
                .map(|(n, i)| registry_entry(n, i))
                .collect(),
            default: None,
            name_order_disagrees: false,
        }
    }

    fn blocks(names: &[&str]) -> Blocks {
        Blocks {
            blocks: names
                .iter()
                .map(|n| super::super::blocks::Block {
                    name: (*n).to_owned(),
                    base_state_id: 0,
                    state_count: 1,
                    default_state_id: 0,
                    properties: Vec::new(),
                    alphabetical_order_disagrees: false,
                })
                .collect(),
            state_count: names.len() as u32,
            reported: BTreeMap::new(),
        }
    }

    fn registries() -> Registries {
        Registries {
            registries: vec![
                registry(
                    FLUID_REGISTRY,
                    vec![
                        ("minecraft:empty", 0),
                        ("minecraft:flowing_lava", 1),
                        ("minecraft:flowing_water", 2),
                        ("minecraft:lava", 3),
                        ("minecraft:water", 4),
                    ],
                ),
                registry(
                    ITEM_REGISTRY,
                    vec![
                        ("minecraft:lava_bucket", 0),
                        ("minecraft:milk_bucket", 1),
                        ("minecraft:water_bucket", 2),
                    ],
                ),
            ],
            block: registry("minecraft:block", vec![]),
            entry_count: 8,
            namespaces: ["minecraft".to_owned()].into(),
            reported: BTreeMap::new(),
        }
    }

    #[test]
    fn still_fluids_pair_with_blocks_buckets_and_their_flowing_partner() {
        let parsed = parse(
            &registries(),
            &blocks(&["minecraft:water", "minecraft:lava"]),
        )
        .expect("parses");
        let water = &parsed.fluids[4];
        assert_eq!(water.block.as_deref(), Some("minecraft:water"));
        assert_eq!(water.bucket.as_deref(), Some("minecraft:water_bucket"));
        assert!(water.flowing_of.is_none());

        let flowing = &parsed.fluids[2];
        assert_eq!(
            flowing.flowing_of.as_deref(),
            Some("minecraft:water"),
            "flowing water is the movement of water"
        );
        assert!(flowing.bucket.is_none(), "nobody picks up flowing water");
    }

    #[test]
    fn the_empty_fluid_has_no_block_and_no_bucket() {
        let parsed = parse(
            &registries(),
            &blocks(&["minecraft:water", "minecraft:lava"]),
        )
        .expect("parses");
        let empty = &parsed.fluids[0];
        assert!(
            empty.block.is_none(),
            "inventing a block for empty would be knowledge \
            from outside the reports"
        );
        assert!(empty.bucket.is_none());
        assert!(empty.flowing_of.is_none());
    }

    #[test]
    fn a_still_fluid_whose_block_report_has_no_entry_fails() {
        // The block half of the join is a claim about two reports agreeing
        // that water exists. Losing it is not an empty row; it is the join
        // silently producing nothing where the game has a fluid.
        let err = parse(&registries(), &blocks(&["minecraft:water"]))
            .expect_err("lava's block is missing");
        assert!(err.contains("minecraft:lava"), "{err}");
    }

    #[test]
    fn flowing_fluids_have_no_block_on_this_version() {
        // On 1.21.1 a fluid's level lives in the fluid, not in the block
        // state, so only the still fluids are blocks. If that ever changes —
        // if flowing_water turns up in the block report — this test is where
        // the change is met.
        let parsed = parse(
            &registries(),
            &blocks(&["minecraft:water", "minecraft:lava"]),
        )
        .expect("parses");
        for fluid in &parsed.fluids {
            if fluid.flowing_of.is_some() {
                assert!(
                    fluid.block.is_none(),
                    "{} unexpectedly has a block",
                    fluid.name
                );
            }
        }
    }

    #[test]
    fn a_still_fluid_without_a_flowing_partner_fails() {
        let mut regs = registries();
        regs.registries[0]
            .entries
            .retain(|e| e.name != "minecraft:flowing_lava");
        let err = parse(&regs, &blocks(&["minecraft:water", "minecraft:lava"]))
            .expect_err("lava lost its flowing partner");
        assert!(err.contains("flowing_lava"), "{err}");
    }
}
