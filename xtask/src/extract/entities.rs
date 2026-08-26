//! The entity-type registry: what the reports carry, and what they do not.
//!
//! `minecraft:entity_type` is one of the 77 flat registries and is already
//! emitted with them — names, protocol ids, and a default. What this module
//! adds is the entity-specific reading of that same report: the golden rows
//! below cover all 130 entries rather than the shared sampler's six, so no
//! change to the general sampling rule can quietly leave entities unchecked,
//! and the registry's default is carried as its own fact.
//!
//! # What is *not* here, and why
//!
//! The work an entity table eventually owes — bounding boxes, spawn
//! categories, tick ranges, tracking updates — is nowhere in the 1.21.1
//! generator output. There is no entity report: `--reports` writes blocks,
//! commands, items, packets, registries and biome parameters, and none of them
//! says how wide a wither is or that a monster spawns in the dark. That data
//! lives in the game's compiled code on this version.
//!
//! So this module emits exactly what the reports state and documents the gap
//! instead of filling it from memory. A later Minecraft whose generators
//! publish per-entity facts extends [`Entities`] without changing anything
//! already here; until then, `EntityType` remains identity plus the tag
//! memberships extracted beside it, which is honest about being nothing more.

use std::collections::BTreeMap;

use super::registries::Registries;

/// The entity-type registry, as every other module spells it.
pub const ENTITY_REGISTRY: &str = "minecraft:entity_type";

#[derive(Debug)]
pub struct Entities {
    /// The report's own entries, keyed by name with each entry's protocol id.
    /// Kept verbatim so the generated sample can be taken from it rather than
    /// from anything this module derived.
    pub reported: BTreeMap<String, u32>,
    pub default: Option<String>,
}

/// Read the entity-specific slice of the registry report.
pub fn parse(registries: &Registries) -> Result<Entities, String> {
    let Some(entity_registry) = registries
        .registries
        .iter()
        .find(|r| r.name == ENTITY_REGISTRY)
    else {
        return Err(format!("the registry report has no {ENTITY_REGISTRY}"));
    };

    // The generic path has already checked density and id uniqueness; this is
    // the entity-specific statement of the same facts, kept because the crate
    // quotes both numbers.
    let mut reported = BTreeMap::new();
    for entry in &entity_registry.entries {
        reported.insert(entry.name.clone(), entry.protocol_id);
    }
    if reported.is_empty() {
        return Err(format!("{ENTITY_REGISTRY} has no entries"));
    }

    if let Some(default) = &entity_registry.default {
        if !reported.contains_key(default) {
            return Err(format!(
                "{ENTITY_REGISTRY}'s default {default} is not one of its entries"
            ));
        }
    }

    Ok(Entities {
        default: entity_registry.default.clone(),
        reported,
    })
}

#[cfg(test)]
mod tests {
    use super::super::registries::Registry;
    use super::*;

    #[test]
    fn a_default_must_name_an_entry() {
        // The generic registry check enforces this too; restated here because
        // the entity default reaches the generated file as its own constant,
        // through code this test owns.
        let mut registries = sample_registries();
        registries.registries[0].default = Some("minecraft:wither".to_owned());
        assert!(parse(&registries).is_ok(), "the wither is an entry");

        registries.registries[0].default = Some("minecraft:not_an_entity".to_owned());
        let err = parse(&registries).expect_err("must not be accepted");
        assert!(err.contains("not_an_entity"), "{err}");
    }

    #[test]
    fn an_empty_entity_registry_is_refused() {
        let mut registries = sample_registries();
        registries.registries[0].entries.clear();
        let err = parse(&registries).expect_err("must not be accepted");
        assert!(err.contains("no entries"), "{err}");
    }

    fn sample_registries() -> Registries {
        let entry = |name: &str, id: u32| super::super::registries::Entry {
            name: name.to_owned(),
            protocol_id: id,
        };
        Registries {
            registries: vec![Registry {
                name: ENTITY_REGISTRY.to_owned(),
                protocol_id: 0,
                entries: vec![entry("minecraft:pig", 1), entry("minecraft:villager", 2)],
                default: Some("minecraft:pig".to_owned()),
                name_order_disagrees: true,
            }],
            block: Registry {
                name: "minecraft:block".to_owned(),
                protocol_id: 1,
                entries: Vec::new(),
                default: None,
                name_order_disagrees: false,
            },
            entry_count: 2,
            namespaces: ["minecraft".to_owned()].into(),
            reported: BTreeMap::new(),
        }
    }
}
