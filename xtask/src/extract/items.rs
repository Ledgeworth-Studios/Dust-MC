//! Reading `reports/items.json` — every item's default data components.
//!
//! # Why this report matters more than its size suggests
//!
//! D3 chose 1.21.1 because data components landed in 1.20.5, and components are
//! what let a server hand an unmodified client a custom item. This report is
//! the baseline that mechanism starts from: what `minecraft:diamond_sword`
//! already carries before anything is customised — attack damage, attack speed,
//! a max damage of 1,561, and a tool block with five mining rules.
//!
//! # What the report turned out to be, measured rather than assumed
//!
//! - 1,333 items, exactly the entries of the `minecraft:item` registry. Two
//!   reports from two generators agreeing on a list, checked over all 1,333 by
//!   [`check_items_match_registry`], which is what lets the generated table be
//!   indexed by protocol id instead of carrying its own copy of the names.
//! - 30 distinct components appear as defaults, out of the 57 in the
//!   `minecraft:data_component_type` registry. Every one of the 30 is a registry
//!   entry; the other 27 have no default on any item.
//! - Six components are on every item: `attribute_modifiers`, `enchantments`,
//!   `lore`, `max_stack_size`, `rarity`, `repair_cost`. The first three are
//!   empty on almost everything — 1,333 empty lores, 1,333 empty enchantment
//!   maps, 1,301 empty modifier lists.
//! - The values are shallow but not uniform: `minecraft:tool` appears in four
//!   different key shapes and `minecraft:food` in six, and `tool.rules[].blocks`
//!   is sometimes a string and sometimes a list of them. That non-uniformity is
//!   the argument for the representation the generated table uses; see
//!   `crates/dust-registry/src/items.rs`.
//! - 54 distinct strings appear as values. All but six are namespaced ids or
//!   `#tags`; the six are `add_value`, `mainhand`, and the four rarities. There
//!   is no free text anywhere in the report — no display names, no lore lines,
//!   nothing written by a person. The extraction prints the non-id strings so
//!   that stays a fact somebody looked at.
//!
//! # Where this sits relative to the provenance line
//!
//! The project's line is that no Mojang file is committed and what lands is the
//! Rust that resulted from reading one. `blocks.rs` already commits Mojang's
//! numbers — 26,684 state ids and the property values that index them — so the
//! question here is whether component *values* are the same kind of thing or a
//! different one.
//!
//! They are the same kind. What this emits is a table of constants that a
//! client already knows and that the protocol requires both ends to agree on:
//! an item that stacks to 64 here and 16 there is a desynchronised inventory.
//! It is not Mojang's file, not their arrangement of it, and not a substitute
//! for the game — a server that knows the max stack size of an apple does not
//! save anybody the purchase of Minecraft.
//!
//! The thing that would change that answer is expressive content: display
//! names, lore lines, descriptions — text somebody wrote, where the value is
//! the writing rather than the number. This report contains none. Every string
//! value in it is a namespaced id, a `#tag`, or one of six enum tokens, and the
//! extraction prints the non-id strings on every run precisely so that stays a
//! checked fact rather than a remembered one. A future report that started
//! carrying text would want this paragraph re-read before it was committed.
//!
//! # Floats
//!
//! `minecraft:diamond_sword`'s attack speed is `-2.4000000953674316`. That is a
//! Java `float` widened to a `double` and printed at double width: the report
//! spells some numbers as the shortest text that round-trips through an `f32`
//! (`1.2`, `7.2000003`) and others as the shortest that round-trips through an
//! `f64`. Storing either kind at the wrong width changes the value — `1.2` read
//! as `f32` and widened again is `1.2000000476837158`, which is a different
//! number from the one the report states.
//!
//! So every number is kept at the width the report's own text implies, which is
//! `f64`, and [`check_every_number_reprints`] tokenises the raw bytes and
//! insists that every one of the 3,021 numbers in the file re-prints to exactly
//! the text Mojang wrote. Not a sample of them: all of them, because a width
//! defect would hit only the numbers whose two spellings differ, and there are
//! 15 such literals out of 41.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value as Json;

use super::blocks::Blocks;
use super::registries::Registries;

/// A component value, in the shape the generated table holds it.
///
/// Deliberately a value tree rather than 30 typed structs. The argument is in
/// `crates/dust-registry/src/items.rs`, where the type this mirrors lives.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<Value>),
    /// Keys sorted. The report's own key order is not preserved because it is
    /// not semantic — a component is serialised to the client by its codec's
    /// field order, not by the order a report happened to print.
    Map(BTreeMap<String, Value>),
}

/// One item's default components, keys sorted.
pub type ComponentMap = BTreeMap<String, Value>;

#[derive(Debug)]
pub struct Items {
    /// Item names in protocol-id order, so `items[id]` is the item with that
    /// protocol id. Filled from the item registry, not from this report's key
    /// order.
    pub items: Vec<String>,
    /// The distinct component maps, deduplicated. 1,333 items share 136 of them
    /// on 1.21.1, which is why the table interns at this level.
    pub maps: Vec<ComponentMap>,
    /// Indexed by item protocol id: which of `maps` that item has.
    pub map_of_item: Vec<usize>,
    /// How many items share each of `maps`, for the generated file's comments.
    pub sharers: Vec<Vec<String>>,
    /// Every component name that appears, with how many items carry it.
    pub components: BTreeMap<String, usize>,
    /// String values that are not namespaced ids or `#tags`, so that the fact
    /// there is no free text in this report is something the extraction states
    /// rather than something a reader assumes.
    pub non_id_strings: BTreeSet<String>,
    /// How many numbers the report contains, all of which were checked to
    /// re-print to their own text.
    pub number_count: usize,
}

pub fn parse(json: &[u8], registries: &Registries, blocks: &Blocks) -> Result<Items, String> {
    let reported: BTreeMap<String, ReportedItem> =
        serde_json::from_slice(json).map_err(|e| format!("could not read items.json: {e}"))?;

    let item_registry = registries
        .registries
        .iter()
        .find(|r| r.name == "minecraft:item")
        .ok_or("the registry report has no minecraft:item")?;

    // Protocol-id order, taken from the registry rather than from this report,
    // so the generated table is indexed by the same numbers the wire uses.
    let mut items = vec![String::new(); item_registry.entries.len()];
    for entry in &item_registry.entries {
        items[entry.protocol_id as usize] = entry.name.clone();
    }
    check_items_match_registry(&reported, &items)?;
    let number_count = check_every_number_reprints(json)?;

    let mut maps: Vec<ComponentMap> = Vec::new();
    let mut map_of_item = Vec::with_capacity(items.len());
    let mut sharers: Vec<Vec<String>> = Vec::new();
    let mut components: BTreeMap<String, usize> = BTreeMap::new();
    let mut non_id_strings = BTreeSet::new();

    for name in &items {
        let reported = &reported[name];
        let mut map = ComponentMap::new();
        for (component, value) in &reported.components {
            *components.entry(component.clone()).or_default() += 1;
            let value = convert(value, &format!("{name}[{component}]"))?;
            collect_non_id_strings(&value, &mut non_id_strings);
            map.insert(component.clone(), value);
        }
        let index = match maps.iter().position(|existing| *existing == map) {
            Some(index) => index,
            None => {
                maps.push(map);
                sharers.push(Vec::new());
                maps.len() - 1
            }
        };
        map_of_item.push(index);
        sharers[index].push(name.clone());
    }

    let items = Items {
        items,
        maps,
        map_of_item,
        sharers,
        components,
        non_id_strings,
        number_count,
    };
    check_components_are_registry_entries(&items, registries)?;
    check_scalars_have_the_shape_the_typed_accessors_promise(&items)?;
    check_attribute_modifiers_name_real_attributes(&items, registries)?;
    check_tool_rules_name_real_blocks(&items, blocks)?;
    Ok(items)
}

#[derive(Debug, serde::Deserialize)]
pub struct ReportedItem {
    #[serde(default)]
    pub components: BTreeMap<String, Json>,
}

fn convert(value: &Json, path: &str) -> Result<Value, String> {
    Ok(match value {
        Json::Null => return Err(format!("{path} is null, which nothing here can represent")),
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                // u64 above i64::MAX. Refused rather than narrowed, because a
                // narrowed number is a wrong number that still compiles.
                return Err(format!(
                    "{path} is {n}, which does not fit an i64 or an f64"
                ));
            }
        }
        Json::String(s) => Value::Str(s.clone()),
        Json::Array(items) => Value::List(
            items
                .iter()
                .enumerate()
                .map(|(i, v)| convert(v, &format!("{path}[{i}]")))
                .collect::<Result<_, _>>()?,
        ),
        Json::Object(fields) => Value::Map(
            fields
                .iter()
                .map(|(k, v)| Ok((k.clone(), convert(v, &format!("{path}.{k}"))?)))
                .collect::<Result<BTreeMap<_, _>, String>>()?,
        ),
    })
}

fn collect_non_id_strings(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::Str(s) => {
            let body = s.strip_prefix('#').unwrap_or(s);
            let namespaced = body
                .split_once(':')
                .is_some_and(|(ns, path)| !ns.is_empty() && !path.is_empty());
            if !namespaced {
                out.insert(s.clone());
            }
        }
        Value::List(items) => items.iter().for_each(|v| collect_non_id_strings(v, out)),
        Value::Map(fields) => fields.values().for_each(|v| collect_non_id_strings(v, out)),
        _ => {}
    }
}

/// The item report and the item registry describe the same 1,333 items.
///
/// This is what earns the generated table the right to be indexed by protocol
/// id and carry no names of its own. If the two ever disagree, indexing by id
/// would silently attach one item's components to another, so it fails.
fn check_items_match_registry(
    reported: &BTreeMap<String, ReportedItem>,
    items: &[String],
) -> Result<(), String> {
    let registry: BTreeSet<&str> = items.iter().map(String::as_str).collect();
    let report: BTreeSet<&str> = reported.keys().map(String::as_str).collect();
    let missing: Vec<&&str> = registry.difference(&report).take(5).collect();
    if !missing.is_empty() {
        return Err(format!(
            "{} items are in the registry and not in the item report, e.g. {missing:?}",
            registry.difference(&report).count()
        ));
    }
    let extra: Vec<&&str> = report.difference(&registry).take(5).collect();
    if !extra.is_empty() {
        return Err(format!(
            "{} items are in the item report and not in the registry, e.g. {extra:?}",
            report.difference(&registry).count()
        ));
    }
    Ok(())
}

/// Every number in the report re-prints to exactly the text Mojang wrote.
///
/// The width trap in this report is real and it is quiet. The report spells
/// some numbers as the shortest text that round-trips through an `f32` and
/// others as the shortest that round-trips through an `f64`; storing one kind
/// at the other's width changes the value while leaving something that still
/// looks like a number. Reading `1.2` into an `f32` and widening it back gives
/// `1.2000000476837158`, which is not what the report says.
///
/// So this tokenises the raw bytes — every number token in the file, outside
/// strings — and compares that multiset against the numbers the parse produced,
/// formatted the way the generated code will spell them. All 3,021 of them, not
/// a sample: only 15 of the 41 distinct float literals have two spellings that
/// differ, so a sample is most of a check.
///
/// What it does not catch: a value read at the right width and then attached to
/// the wrong item. That is what the golden samples are for.
fn check_every_number_reprints(json: &[u8]) -> Result<usize, String> {
    let mut in_file: Vec<String> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    let mut number: Option<usize> = None;
    for (index, byte) in json.iter().copied().enumerate() {
        if in_string {
            match byte {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        // A number *starts* with a digit or a minus. Continuation is wider than
        // that, and must not be mistaken for a start: `false` ends in an `e`,
        // and a scanner that treats every `e` as a number produces a token
        // nothing in the parse can match.
        let continues = byte.is_ascii_digit() || matches!(byte, b'.' | b'e' | b'E' | b'+' | b'-');
        match number {
            Some(begin) if !continues => {
                in_file.push(String::from_utf8_lossy(&json[begin..index]).into_owned());
                number = None;
            }
            _ => {}
        }
        if number.is_none() && (byte.is_ascii_digit() || byte == b'-') {
            number = Some(index);
        } else if byte == b'"' {
            in_string = true;
        }
    }
    if let Some(begin) = number {
        in_file.push(String::from_utf8_lossy(&json[begin..]).into_owned());
    }

    let parsed: Json =
        serde_json::from_slice(json).map_err(|e| format!("could not read items.json: {e}"))?;
    let mut from_parse = Vec::new();
    collect_numbers(&parsed, &mut from_parse);

    in_file.sort();
    from_parse.sort();
    if in_file != from_parse {
        let mut only_in_file: Vec<&String> =
            in_file.iter().filter(|n| !from_parse.contains(n)).collect();
        only_in_file.dedup();
        return Err(format!(
            "{} numbers in items.json, {} from the parse, and they do not agree. The first \
             few the file has and the parse does not: {:?}. A number that does not re-print \
             to its own text is a number this extraction is storing at the wrong width.",
            in_file.len(),
            from_parse.len(),
            &only_in_file[..only_in_file.len().min(5)]
        ));
    }
    Ok(in_file.len())
}

fn collect_numbers(value: &Json, out: &mut Vec<String>) {
    match value {
        Json::Number(n) => out.push(match n.as_i64() {
            Some(i) => i.to_string(),
            // `{:?}` on an f64 is the shortest decimal that parses back to the
            // same bits, which is the same rule Mojang's serialiser used. The
            // comparison this feeds is what says so rather than assuming it.
            None => format!("{:?}", n.as_f64().unwrap_or(f64::NAN)),
        }),
        Json::Array(items) => items.iter().for_each(|v| collect_numbers(v, out)),
        Json::Object(fields) => fields.values().for_each(|v| collect_numbers(v, out)),
        _ => {}
    }
}

fn check_components_are_registry_entries(
    items: &Items,
    registries: &Registries,
) -> Result<(), String> {
    let registry = registries
        .registries
        .iter()
        .find(|r| r.name == "minecraft:data_component_type")
        .ok_or("the registry report has no minecraft:data_component_type")?;
    for component in items.components.keys() {
        if !registry.entries.iter().any(|e| &e.name == component) {
            return Err(format!(
                "{component} is a default on some item and is not in the \
                 data_component_type registry"
            ));
        }
    }
    Ok(())
}

/// The shapes the crate's typed accessors depend on, checked against all 1,333
/// items so that a typed accessor cannot be a lie.
///
/// `Item::max_stack_size` returns a `u8` and `Item::rarity` returns an enum with
/// four variants. Those are claims about the data, and this is where they are
/// paid for. A version that ships a fifth rarity or a stack size of 200 stops
/// the extraction rather than being rounded into the existing type.
fn check_scalars_have_the_shape_the_typed_accessors_promise(items: &Items) -> Result<(), String> {
    const RARITIES: [&str; 4] = ["common", "uncommon", "rare", "epic"];
    for (name, map) in items
        .items
        .iter()
        .zip(items.map_of_item.iter().map(|i| &items.maps[*i]))
    {
        match map.get("minecraft:max_stack_size") {
            Some(Value::Int(size)) if (1..=99).contains(size) => {}
            other => {
                return Err(format!(
                    "{name}'s max_stack_size is {other:?}; the crate returns it as a u8 in 1..=99"
                ))
            }
        }
        match map.get("minecraft:rarity") {
            Some(Value::Str(rarity)) if RARITIES.contains(&rarity.as_str()) => {}
            other => {
                return Err(format!(
                    "{name}'s rarity is {other:?}, and the crate's Rarity enum is {RARITIES:?}"
                ))
            }
        }
        for field in [
            "minecraft:repair_cost",
            "minecraft:max_damage",
            "minecraft:damage",
        ] {
            match map.get(field) {
                None => {}
                Some(Value::Int(value)) if (0..=i64::from(u32::MAX)).contains(value) => {}
                other => {
                    return Err(format!(
                        "{name}'s {field} is {other:?}; the crate returns it as a u32"
                    ))
                }
            }
        }
        match map.get("minecraft:fire_resistant") {
            None => {}
            Some(Value::Map(fields)) if fields.is_empty() => {}
            other => {
                return Err(format!(
                    "{name}'s fire_resistant is {other:?}. The crate reads it as a unit \
                     component — present or absent, with an empty value — so a value with \
                     something in it would be information being dropped."
                ))
            }
        }
    }
    Ok(())
}

/// Every attribute modifier names an attribute that exists.
///
/// The 64 modifiers on 1.21.1 are all `generic.attack_damage` and
/// `generic.attack_speed`, and both are entries of the `minecraft:attribute`
/// registry extracted beside this. Two reports agreeing again — and the check
/// that would notice if a later version started naming an attribute Dust has no
/// number for.
fn check_attribute_modifiers_name_real_attributes(
    items: &Items,
    registries: &Registries,
) -> Result<(), String> {
    let registry = registries
        .registries
        .iter()
        .find(|r| r.name == "minecraft:attribute")
        .ok_or("the registry report has no minecraft:attribute")?;
    for (index, map) in items.maps.iter().enumerate() {
        let Some(Value::Map(component)) = map.get("minecraft:attribute_modifiers") else {
            continue;
        };
        let Some(Value::List(modifiers)) = component.get("modifiers") else {
            continue;
        };
        for modifier in modifiers {
            let Value::Map(fields) = modifier else {
                return Err(format!(
                    "an attribute modifier is {modifier:?}, not an object"
                ));
            };
            let Some(Value::Str(attribute)) = fields.get("type") else {
                return Err(format!("an attribute modifier has no type: {fields:?}"));
            };
            if !registry.entries.iter().any(|e| &e.name == attribute) {
                return Err(format!(
                    "{} carries a modifier for {attribute}, which is not in the attribute \
                     registry",
                    items.sharers[index]
                        .first()
                        .map_or("an item", String::as_str)
                ));
            }
        }
    }
    Ok(())
}

/// Every mining rule that names a block names one that exists.
///
/// `tool.rules[].blocks` is either a `#tag` — which this cannot check, because
/// tags come from the data pack and not from any report read here — or a block
/// id, which it can. `minecraft:shears` naming `minecraft:cobweb` is the block
/// report and the item report agreeing about a name, and worth insisting on:
/// a rule against a block that does not exist is a rule that never fires.
fn check_tool_rules_name_real_blocks(items: &Items, blocks: &Blocks) -> Result<(), String> {
    for (index, map) in items.maps.iter().enumerate() {
        let Some(Value::Map(tool)) = map.get("minecraft:tool") else {
            continue;
        };
        let Some(Value::List(rules)) = tool.get("rules") else {
            continue;
        };
        for rule in rules {
            let Value::Map(fields) = rule else {
                return Err(format!("a tool rule is {rule:?}, not an object"));
            };
            let named: Vec<&String> = match fields.get("blocks") {
                Some(Value::Str(one)) => vec![one],
                Some(Value::List(many)) => many
                    .iter()
                    .map(|v| match v {
                        Value::Str(s) => Ok(s),
                        other => Err(format!("a tool rule names {other:?} as a block")),
                    })
                    .collect::<Result<_, _>>()?,
                other => return Err(format!("a tool rule's blocks is {other:?}")),
            };
            for block in named {
                if block.starts_with('#') {
                    continue;
                }
                if !blocks.blocks.iter().any(|b| &b.name == block) {
                    return Err(format!(
                        "{}'s tool rules name {block}, which is not a block",
                        items.sharers[index]
                            .first()
                            .map_or("an item", String::as_str)
                    ));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_number_spelled_a_way_it_does_not_re_print_is_refused() {
        // `1.10` parses to the same f64 as `1.1` and re-prints as `1.1`, so the
        // multiset does not match. That is exactly the shape of the defect this
        // check exists for: a number whose text and whose value disagree about
        // what it is.
        let err = check_every_number_reprints(br#"{"a": {"components": {"x": 1.10}}}"#)
            .expect_err("must not be accepted");
        assert!(err.contains("wrong width"), "{err}");
    }

    #[test]
    fn the_widths_that_matter_pass_the_check() {
        // The positive control, and it is not a formality: these two literals
        // are the two spellings the report actually mixes, and a check that
        // refused either would stop the extraction on real data.
        let count = check_every_number_reprints(
            br#"{"speed": -2.4000000953674316, "saturation": 1.2, "n": 7.2000003, "i": 1561}"#,
        )
        .expect("these are the report's own spellings");
        assert_eq!(count, 4);
    }

    #[test]
    fn the_scanner_does_not_find_a_number_inside_false() {
        // `false` ends in an `e`, which is a number's continuation character.
        // A scanner that treats every `e` as the start of one produces a token
        // the parse cannot match, and the check fails on valid data.
        let count = check_every_number_reprints(br#"{"a": false, "b": true, "c": 3}"#)
            .expect("booleans are not numbers");
        assert_eq!(count, 1);
    }

    #[test]
    fn a_number_inside_a_string_is_not_a_number() {
        let count = check_every_number_reprints(br#"{"a": "minecraft:music_disc_13", "b": 5}"#)
            .expect("parses");
        assert_eq!(count, 1);
    }

    #[test]
    fn a_null_component_value_is_refused() {
        // Nothing in the generated tree can hold one, and the alternative to
        // refusing is dropping it, which is information disappearing quietly.
        let err = convert(&serde_json::Value::Null, "test").expect_err("must not be accepted");
        assert!(err.contains("null"), "{err}");
    }

    #[test]
    fn only_strings_that_are_not_ids_are_collected() {
        let mut out = BTreeSet::new();
        collect_non_id_strings(
            &Value::Map(
                [
                    ("a".to_owned(), Value::Str("minecraft:stone".to_owned())),
                    ("b".to_owned(), Value::Str("#minecraft:leaves".to_owned())),
                    (
                        "c".to_owned(),
                        Value::List(vec![Value::Str("add_value".to_owned())]),
                    ),
                ]
                .into(),
            ),
            &mut out,
        );
        assert_eq!(out, ["add_value".to_owned()].into());
    }
}
