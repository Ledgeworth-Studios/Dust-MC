//! Reading `reports/registries.json` into something that can be generated from.
//!
//! # What a flat registry is
//!
//! Most of Minecraft's registries are a list of namespaced names with a number
//! attached to each: `minecraft:diamond_sword` is item 963, and 963 is what
//! goes on the wire. There are 78 of them in 1.21.1 and between them they name
//! 5,825 things. Unlike the block registry there is no state space, no
//! properties and no radix — which makes the extraction a great deal simpler
//! than [`super::blocks`], and moves the risk somewhere else entirely: nothing
//! about a flat registry is *self*-checking. A table with every id shifted by
//! one is a perfectly consistent table. See [`samples`] in
//! `super::codegen` for what is done about that.
//!
//! [`samples`]: super::codegen
//!
//! # What was checked rather than assumed, and what the data said
//!
//! - **Protocol ids are contiguous `0..n` within each registry.** Every one of
//!   the 78 is, on 1.21.1. That is worth knowing rather than expecting, because
//!   a sparse registry would make `by_id` a lookup rather than an index, and
//!   would make `Item(u16)` — a newtype holding a protocol id — the wrong
//!   shape. The generated tables encode density, so [`Registry::from_report`]
//!   refuses a registry that is sparse or repeats an id, naming it, rather than
//!   emitting a table the crate's assumptions do not fit.
//! - **`default`, where a registry has one, names an entry that exists.** Ten
//!   registries have one; all ten point at a real entry.
//! - **Not every entry is in the `minecraft` namespace.** Six entries of
//!   `minecraft:command_argument_type` are `brigadier:bool` and friends. Nothing
//!   here may therefore strip or assume a namespace, which is also why the
//!   crate's `from_name` takes a namespaced id and nothing else.
//! - **Name order is not id order.** For 68 of the 78 registries, sorting the
//!   entries by name gives a different sequence than sorting them by protocol
//!   id. Both orders are wanted — one for lookup, one for decoding — so the
//!   generated table carries the names in name order and two index arrays.
//!
//! # Why `minecraft:block` is not emitted here
//!
//! [`super::blocks`] already generates a block table, from a different report,
//! and a second one here would be a second answer to "what is block 42". The
//! block registry is still read: its protocol ids are the same sequence as the
//! order of `blocks.json`'s base state ids, so a block's protocol id is its
//! index into `BLOCKS` and no new table is needed to know it. That sentence is
//! a claim about two independently generated reports agreeing, so it is checked
//! — see [`check_block_ids_match_state_order`] — rather than believed.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use super::blocks::Blocks;

/// The registry `blocks.rs` owns. Read, checked, and not emitted.
pub const BLOCK_REGISTRY: &str = "minecraft:block";

/// One registry, as `reports/registries.json` describes it.
#[derive(Debug, Deserialize)]
pub struct ReportedRegistry {
    /// The registry's own id in Minecraft's root registry.
    pub protocol_id: u32,
    #[serde(default)]
    pub default: Option<String>,
    pub entries: BTreeMap<String, ReportedEntry>,
}

#[derive(Debug, Deserialize)]
pub struct ReportedEntry {
    pub protocol_id: u32,
}

/// A registry whose entries have been checked and put in name order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registry {
    /// Namespaced registry id, e.g. `minecraft:item`.
    pub name: String,
    pub protocol_id: u32,
    /// Entries sorted by name — the order a lookup binary-searches.
    pub entries: Vec<Entry>,
    pub default: Option<String>,
    /// True when name order is *not* protocol-id order.
    ///
    /// Recorded rather than assumed either way: it is true for 68 of the 78
    /// registries on 1.21.1, which is why the table needs both orders, and a
    /// version where it became false everywhere would make one of the two index
    /// arrays dead weight worth removing.
    pub name_order_disagrees: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub protocol_id: u32,
}

/// Everything the registry report says, once it has been checked.
#[derive(Debug)]
pub struct Registries {
    /// Every registry except `minecraft:block`, in name order.
    pub registries: Vec<Registry>,
    /// `minecraft:block`: checked like the rest, emitted by `blocks.rs` and not
    /// by this module.
    pub block: Registry,
    /// Entries across `registries` — not counting the block registry's, since
    /// those are not emitted here.
    pub entry_count: usize,
    /// Namespaces seen across every entry of every registry.
    pub namespaces: BTreeSet<String>,
    /// The report as it was read, kept so the golden sample can be taken from
    /// it rather than from anything this module derived.
    pub reported: BTreeMap<String, ReportedRegistry>,
}

pub fn parse(json: &[u8]) -> Result<Registries, String> {
    let reported: BTreeMap<String, ReportedRegistry> =
        serde_json::from_slice(json).map_err(|e| format!("could not read registries.json: {e}"))?;
    if reported.is_empty() {
        return Err("registries.json describes no registries".to_owned());
    }

    let mut seen_ids = BTreeMap::new();
    for (name, registry) in &reported {
        if let Some(other) = seen_ids.insert(registry.protocol_id, name.as_str()) {
            return Err(format!(
                "{name} and {other} are both registry {}",
                registry.protocol_id
            ));
        }
    }

    // A BTreeMap iterates in key order, so this is already name order.
    let mut registries = Vec::with_capacity(reported.len());
    let mut block = None;
    for (name, registry) in &reported {
        let checked = Registry::from_report(name, registry)?;
        if name == BLOCK_REGISTRY {
            block = Some(checked);
        } else {
            registries.push(checked);
        }
    }
    let Some(block) = block else {
        return Err(format!(
            "registries.json has no {BLOCK_REGISTRY}, which the block table is cross-checked \
             against"
        ));
    };

    let entry_count = registries.iter().map(|r| r.entries.len()).sum();
    let namespaces = registries
        .iter()
        .chain(std::iter::once(&block))
        .flat_map(|r| r.entries.iter())
        .map(|e| e.name.split(':').next().unwrap_or_default().to_owned())
        .collect();

    Ok(Registries {
        registries,
        block,
        entry_count,
        namespaces,
        reported,
    })
}

impl Registry {
    fn from_report(name: &str, reported: &ReportedRegistry) -> Result<Self, String> {
        if reported.entries.is_empty() {
            return Err(format!("{name} has no entries"));
        }

        let mut entries: Vec<Entry> = reported
            .entries
            .iter()
            .map(|(entry, e)| Entry {
                name: entry.clone(),
                protocol_id: e.protocol_id,
            })
            .collect();
        // The report's map is keyed by name and a BTreeMap hands it over in
        // that order, but sorting explicitly says which order this is, since
        // the crate's binary search depends on it.
        entries.sort_by(|a, b| a.name.cmp(&b.name));

        for entry in &entries {
            if !entry.name.contains(':') {
                return Err(format!(
                    "{name} has an entry named {:?}, which is not a namespaced id. Every \
                     lookup in dust-registry is by namespaced id, so a bare name here would \
                     be unreachable.",
                    entry.name
                ));
            }
        }

        check_ids_are_dense(name, &entries)?;

        if let Some(default) = &reported.default {
            if !entries.iter().any(|e| &e.name == default) {
                return Err(format!(
                    "{name}'s default is {default}, which is not one of its entries"
                ));
            }
        }

        let mut by_id = entries.clone();
        by_id.sort_by_key(|e| e.protocol_id);
        let name_order_disagrees = by_id != entries;

        Ok(Self {
            name: name.to_owned(),
            protocol_id: reported.protocol_id,
            entries,
            default: reported.default.clone(),
            name_order_disagrees,
        })
    }
}

/// Protocol ids within a registry run `0..n` with no gap and no repeat.
///
/// This is the assumption the generated tables are shaped around: `by_id` is
/// indexed by protocol id rather than searched, and a first-class type like
/// `Item` is a newtype over the id itself with `0..entry_count` as its whole
/// domain. All 78 registries on 1.21.1 satisfy it. If a future version ships a
/// sparse registry, the right move is to change the shape — an `Option` per
/// slot, or a sorted lookup — and not to widen this check, so it fails loudly
/// and names the registry instead of quietly emitting a table with a hole in it.
fn check_ids_are_dense(name: &str, entries: &[Entry]) -> Result<(), String> {
    let mut ids: Vec<u32> = entries.iter().map(|e| e.protocol_id).collect();
    ids.sort_unstable();
    for (expected, id) in ids.iter().copied().enumerate() {
        let expected = expected as u32;
        if id == expected {
            continue;
        }
        return Err(if ids.iter().filter(|&&other| other == id).count() > 1 {
            format!("{name} gives protocol id {id} to more than one entry")
        } else {
            format!(
                "{name} has {} entries but no entry with protocol id {expected}; it is sparse, \
                 and the generated tables index by protocol id",
                entries.len()
            )
        });
    }
    Ok(())
}

/// The block registry's protocol ids and the block report's state order are two
/// independent statements about the same list of 1,060 names, and this insists
/// they agree.
///
/// It is what earns the right not to emit a block name table here: if a block's
/// protocol id is its index into `BLOCKS`, then `BLOCKS` already carries it. On
/// 1.21.1 the two agree exactly. If they ever stop, the answer is to emit the
/// block registry after all — so this fails rather than being relaxed.
///
/// What it does not catch: it says nothing about whether either report is
/// *right*, only that two of Mojang's generators tell the same story. The
/// golden samples are what compare the generated code against the report.
pub fn check_block_ids_match_state_order(
    registries: &Registries,
    blocks: &Blocks,
) -> Result<(), String> {
    let mut by_id = registries.block.entries.clone();
    by_id.sort_by_key(|e| e.protocol_id);

    if by_id.len() != blocks.blocks.len() {
        return Err(format!(
            "the block registry has {} entries and the block report has {} blocks",
            by_id.len(),
            blocks.blocks.len()
        ));
    }
    // `blocks.blocks` is in base-state-id order.
    for (entry, block) in by_id.iter().zip(&blocks.blocks) {
        if entry.name != block.name {
            return Err(format!(
                "block protocol id {} is {} in the registry report and {} in the block \
                 report's state order. A block's protocol id is no longer its index into \
                 BLOCKS, so the block registry has to be emitted rather than skipped.",
                entry.protocol_id, entry.name, block.name
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(entries: &[(&str, u32)], default: Option<&str>) -> Vec<u8> {
        let mut body = serde_json::Map::new();
        for (name, id) in entries {
            body.insert((*name).to_owned(), serde_json::json!({ "protocol_id": id }));
        }
        let mut registry = serde_json::Map::new();
        registry.insert("protocol_id".into(), 0.into());
        registry.insert("entries".into(), body.into());
        if let Some(default) = default {
            registry.insert("default".into(), default.into());
        }
        let mut root = serde_json::Map::new();
        root.insert("test:thing".into(), registry.into());
        // The block registry is read by every parse, so a fixture needs one.
        root.insert(
            BLOCK_REGISTRY.into(),
            serde_json::json!({
                "protocol_id": 1,
                "entries": { "minecraft:air": { "protocol_id": 0 } },
            }),
        );
        serde_json::to_vec(&serde_json::Value::Object(root)).expect("serialises")
    }

    #[test]
    fn entries_come_out_in_name_order_whatever_the_ids_say() {
        let parsed = parse(&report(
            &[("test:c", 0), ("test:a", 1), ("test:b", 2)],
            None,
        ))
        .expect("parses");
        let registry = &parsed.registries[0];
        let names: Vec<&str> = registry.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["test:a", "test:b", "test:c"]);
        assert!(registry.name_order_disagrees);
        assert_eq!(parsed.entry_count, 3);
    }

    #[test]
    fn a_sparse_registry_is_refused_by_name() {
        // The generated table indexes by protocol id, so a hole would decode to
        // whatever sits next to it. Refusing names the registry, because the
        // fix is per registry.
        let err = parse(&report(&[("test:a", 0), ("test:b", 2)], None))
            .expect_err("must not be accepted");
        assert!(
            err.contains("test:thing") && err.contains("sparse"),
            "{err}"
        );
    }

    #[test]
    fn two_entries_with_the_same_id_are_refused() {
        let err = parse(&report(&[("test:a", 0), ("test:b", 0)], None))
            .expect_err("must not be accepted");
        assert!(err.contains("more than one entry"), "{err}");
    }

    #[test]
    fn a_default_that_names_nothing_is_refused() {
        let err = parse(&report(&[("test:a", 0)], Some("test:missing")))
            .expect_err("must not be accepted");
        assert!(err.contains("not one of its entries"), "{err}");
    }

    #[test]
    fn a_bare_entry_name_is_refused() {
        // Everything downstream looks entries up by namespaced id, so a bare
        // name would be an entry nothing could ask for.
        let err = parse(&report(&[("bare", 0)], None)).expect_err("must not be accepted");
        assert!(err.contains("namespaced"), "{err}");
    }

    #[test]
    fn the_block_registry_is_read_but_kept_out_of_the_emitted_set() {
        let parsed = parse(&report(&[("test:a", 0)], None)).expect("parses");
        assert!(parsed.registries.iter().all(|r| r.name != BLOCK_REGISTRY));
        assert_eq!(parsed.block.entries.len(), 1);
    }

    // What these tests do not catch: whether the generated table says what
    // Mojang's report says. Nothing derived from the table can answer that —
    // see the golden samples in `codegen::registry_samples` and the tests in
    // `crates/dust-registry/tests/registries.rs`.
}
