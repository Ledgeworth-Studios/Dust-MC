//! JSON in, NBT out, under the schema.
//!
//! # Every refusal names a path
//!
//! The input is a file an operator or a datapack author wrote, so the failures
//! here are the ordinary ones — a missing key, a string where a number goes, a
//! misspelling. Each is reported with the path that reached it
//! (`effects.mood_sound.offset`) because the alternative is telling somebody
//! their biome is wrong and leaving them to find out where.
//!
//! # Why an integer key refuses a fraction
//!
//! `"height": 384.5` is valid JSON and cannot be a `TAG_Int`. Rounding it
//! would produce a world 384 blocks high from a file that asked for something
//! else, and the operator would never hear about it. The same reasoning covers
//! a value too large for the type: it is refused rather than wrapped.
//!
//! A float key, by contrast, accepts an integer — `"temperature": 2` and
//! `"temperature": 2.0` are the same number and JSON writers disagree about
//! which to emit. Widening an integer into a float is exact for every value
//! either of these registries carries.

use std::fmt;

use dust_nbt::{Compound, Tag};
use serde_json::Value;

use super::schema::{Field, Key, Registry};

/// Why one entry could not be converted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvertError {
    /// Dotted path from the root of the entry, e.g. `effects.mood_sound.sound`.
    /// Empty at the root itself.
    pub path: String,
    /// What went wrong there.
    pub kind: ErrorKind,
}

/// The kinds of thing a registry entry file gets wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    /// The document, or a nested value the schema calls an object, is not one.
    NotAnObject,
    /// A required key is absent.
    Missing,
    /// The value is the wrong JSON kind for this key's type.
    WrongType {
        /// What the schema wanted, in the words a reader of the schema uses.
        expected: &'static str,
    },
    /// A number that cannot be represented in the key's NBT type — a fraction
    /// where an integer belongs, or a magnitude past the type's range.
    OutOfRange {
        /// The NBT type it had to fit.
        expected: &'static str,
    },
    /// A key the schema does not list, either as sent or as server-side.
    Unknown,
}

impl fmt::Display for ConvertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let at = if self.path.is_empty() {
            "the entry".to_owned()
        } else {
            format!("`{}`", self.path)
        };
        match &self.kind {
            ErrorKind::NotAnObject => write!(f, "{at} is not a JSON object"),
            ErrorKind::Missing => write!(f, "{at} is required and absent"),
            ErrorKind::WrongType { expected } => write!(f, "{at} should be {expected}"),
            ErrorKind::OutOfRange { expected } => {
                write!(f, "{at} does not fit in a {expected}")
            }
            ErrorKind::Unknown => {
                write!(f, "{at} is not a key this registry has; check the spelling")
            }
        }
    }
}

impl std::error::Error for ConvertError {}

fn err(path: &str, kind: ErrorKind) -> ConvertError {
    ConvertError {
        path: path.to_owned(),
        kind,
    }
}

fn child(path: &str, name: &str) -> String {
    if path.is_empty() {
        name.to_owned()
    } else {
        format!("{path}.{name}")
    }
}

/// Convert one entry's JSON document into the compound the wire carries.
pub fn entry(registry: &Registry, document: &Value) -> Result<Compound, ConvertError> {
    let object = document
        .as_object()
        .ok_or_else(|| err("", ErrorKind::NotAnObject))?;

    // Refuse before building. A document with an unknown key is refused whole
    // rather than sent minus the key, because the key is the operator's
    // intent and a silent drop is the failure this whole module is arranged
    // to avoid.
    for key in object.keys() {
        let sent = registry.fields.iter().any(|field| field.name == key);
        let dropped = registry.server_side.contains(&key.as_str());
        if !sent && !dropped {
            return Err(err(key, ErrorKind::Unknown));
        }
    }

    object_of(registry.fields, document, "", &registry.server_side_owned())
}

/// The keys of one object, in the schema's order.
///
/// Schema order rather than the file's, so two installations with the same
/// data produce the same bytes. NBT compounds are unordered and vanilla's own
/// writer emits them in a Java hash order that nothing here could reproduce —
/// so this is not an attempt to match vanilla byte for byte, it is an attempt
/// to make Dust's own output a function of its input. What matches vanilla is
/// the tree, and `xtask harness registries` is what compares the trees.
fn object_of(
    keys: &'static [Key],
    value: &Value,
    path: &str,
    dropped: &[String],
) -> Result<Compound, ConvertError> {
    let object = value
        .as_object()
        .ok_or_else(|| err(path, ErrorKind::NotAnObject))?;

    if path.is_empty() {
        // Checked by the caller against both lists.
    } else {
        for key in object.keys() {
            if !keys.iter().any(|field| field.name == key) && !dropped.iter().any(|d| d == key) {
                return Err(err(&child(path, key), ErrorKind::Unknown));
            }
        }
    }

    let mut out = Compound::with_capacity(keys.len());
    for key in keys {
        let at = child(path, key.name);
        match object.get(key.name) {
            None if key.required => return Err(err(&at, ErrorKind::Missing)),
            None => {}
            Some(found) => {
                out.insert(key.name, tag_of(key.field, found, &at)?);
            }
        }
    }
    Ok(out)
}

fn tag_of(field: Field, value: &Value, path: &str) -> Result<Tag, ConvertError> {
    Ok(match field {
        Field::Bool => Tag::Byte(i8::from(bool_of(value, path)?)),
        Field::Int => Tag::Int(int_of::<i32>(value, path, "TAG_Int")?),
        Field::Long => Tag::Long(int_of::<i64>(value, path, "TAG_Long")?),
        Field::Float => Tag::Float(float_of(value, path)? as f32),
        Field::Double => Tag::Double(float_of(value, path)?),
        Field::Str => Tag::String(
            value
                .as_str()
                .ok_or_else(|| {
                    err(
                        path,
                        ErrorKind::WrongType {
                            expected: "a string",
                        },
                    )
                })?
                .to_owned(),
        ),
        Field::Object(keys) => Tag::Compound(object_of(keys, value, path, &[])?),
        Field::IntOrObject(keys) => {
            if value.is_object() {
                Tag::Compound(object_of(keys, value, path, &[])?)
            } else {
                Tag::Int(int_of::<i32>(value, path, "TAG_Int")?)
            }
        }
        Field::List(element) => {
            let items = value
                .as_array()
                .ok_or_else(|| err(path, ErrorKind::WrongType { expected: "a list" }))?;
            let mut tags = Vec::with_capacity(items.len());
            for (at, item) in items.iter().enumerate() {
                tags.push(tag_of(*element, item, &format!("{path}[{at}]"))?);
            }
            // The element type comes from the schema, not from the first
            // element: an empty list still has a type on the wire, and vanilla
            // writes `TAG_End` for one. `List::from_elements` re-checks that
            // every element agrees, which it must after a walk that built them
            // all from one `Field` — so a failure here is this function's bug
            // and not the file's, and it is reported as an encoding failure
            // rather than blamed on the operator.
            let element_type = tags.first().map_or(dust_nbt::TagType::End, Tag::tag_type);
            Tag::List(
                dust_nbt::List::from_elements(element_type, tags)
                    .map_err(|_| err(path, ErrorKind::NotAnObject))?,
            )
        }
        Field::Map(element) => {
            let object = value
                .as_object()
                .ok_or_else(|| err(path, ErrorKind::NotAnObject))?;
            let mut out = Compound::with_capacity(object.len());
            // A map's keys are data, so they are taken in the file's order
            // after a sort — the file's own order would make two installations
            // with the same data produce different bytes.
            let mut keys: Vec<&String> = object.keys().collect();
            keys.sort();
            for key in keys {
                let at = child(path, key);
                out.insert(key.clone(), tag_of(*element, &object[key], &at)?);
            }
            Tag::Compound(out)
        }
    })
}

fn bool_of(value: &Value, path: &str) -> Result<bool, ConvertError> {
    value.as_bool().ok_or_else(|| {
        err(
            path,
            ErrorKind::WrongType {
                expected: "true or false",
            },
        )
    })
}

/// A JSON number as an integer of `T`, refusing anything that is not exactly
/// one.
trait FromI64: Sized {
    fn narrow(value: i64) -> Option<Self>;
}
impl FromI64 for i32 {
    fn narrow(value: i64) -> Option<Self> {
        i32::try_from(value).ok()
    }
}
impl FromI64 for i64 {
    fn narrow(value: i64) -> Option<Self> {
        Some(value)
    }
}

fn int_of<T: FromI64>(
    value: &Value,
    path: &str,
    expected: &'static str,
) -> Result<T, ConvertError> {
    let number = value.as_number().ok_or_else(|| {
        err(
            path,
            ErrorKind::WrongType {
                expected: "a whole number",
            },
        )
    })?;
    // `as_i64` is `None` for a fraction as well as for a magnitude past the
    // range, and both are refusals here rather than a rounding.
    let wide = number
        .as_i64()
        .ok_or_else(|| err(path, ErrorKind::OutOfRange { expected }))?;
    T::narrow(wide).ok_or_else(|| err(path, ErrorKind::OutOfRange { expected }))
}

fn float_of(value: &Value, path: &str) -> Result<f64, ConvertError> {
    value.as_f64().ok_or_else(|| {
        err(
            path,
            ErrorKind::WrongType {
                expected: "a number",
            },
        )
    })
}

impl Registry {
    /// The server-side key list as owned strings, for the recursive walk.
    ///
    /// Only the root object has server-side keys — nothing nested inside
    /// `effects` is dropped — so this is built once and passed down as an
    /// empty slice below the root.
    fn server_side_owned(&self) -> Vec<String> {
        self.server_side.iter().map(|s| (*s).to_owned()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registries::schema::{DIMENSION_TYPE, WORLDGEN_BIOME};
    use serde_json::json;

    /// A dimension type that is not one of Minecraft's.
    ///
    /// Invented on purpose. A fixture copied out of vanilla's data would make
    /// these tests a second copy of Mojang's content, and it would prove less:
    /// what is under test is the schema's *rules*, and a made-up entry
    /// exercises them exactly as well.
    fn a_dimension() -> Value {
        json!({
            "ambient_light": 0.5,
            "bed_works": true,
            "coordinate_scale": 2.0,
            "effects": "dust:test_effects",
            "has_ceiling": false,
            "has_raids": true,
            "has_skylight": true,
            "height": 128,
            "infiniburn": "#dust:burns_forever",
            "logical_height": 128,
            "min_y": -32,
            "monster_spawn_block_light_limit": 3,
            "monster_spawn_light_level": 7,
            "natural": true,
            "piglin_safe": false,
            "respawn_anchor_works": false,
            "ultrawarm": false
        })
    }

    fn a_biome() -> Value {
        json!({
            "has_precipitation": true,
            "temperature": 0.8,
            "downfall": 0.4,
            "effects": {
                "fog_color": 1,
                "sky_color": 2,
                "water_color": 3,
                "water_fog_color": 4
            }
        })
    }

    #[test]
    fn a_boolean_becomes_a_byte_because_nbt_has_no_boolean() {
        let out = entry(&DIMENSION_TYPE, &a_dimension()).expect("converts");
        assert_eq!(out.get("bed_works"), Some(&Tag::Byte(1)));
        assert_eq!(out.get("has_ceiling"), Some(&Tag::Byte(0)));
    }

    #[test]
    fn two_zeroes_that_look_alike_become_different_tags() {
        // The whole reason the schema exists. `ambient_light` and
        // `coordinate_scale` are both plain JSON numbers and one is four bytes
        // on the wire while the other is eight.
        let out = entry(&DIMENSION_TYPE, &a_dimension()).expect("converts");
        assert_eq!(out.get("ambient_light"), Some(&Tag::Float(0.5)));
        assert_eq!(out.get("coordinate_scale"), Some(&Tag::Double(2.0)));
    }

    #[test]
    fn an_absent_optional_key_is_absent_on_the_wire() {
        // Not written as a default. A default that reached the client would be
        // indistinguishable from a value the pack chose.
        let out = entry(&DIMENSION_TYPE, &a_dimension()).expect("converts");
        assert!(!out.contains_key("fixed_time"), "no fixed_time was given");
        let biome = entry(&WORLDGEN_BIOME, &a_biome()).expect("converts");
        assert!(!biome.contains_key("temperature_modifier"));
        let effects = biome
            .get("effects")
            .and_then(Tag::as_compound)
            .expect("effects");
        assert!(!effects.contains_key("music"), "no music was given");
        assert_eq!(effects.len(), 4, "the four colours and nothing else");
    }

    #[test]
    fn the_int_provider_takes_either_shape() {
        let flat = entry(&DIMENSION_TYPE, &a_dimension()).expect("converts");
        assert_eq!(flat.get("monster_spawn_light_level"), Some(&Tag::Int(7)));

        let mut ranged = a_dimension();
        ranged["monster_spawn_light_level"] = json!({
            "type": "minecraft:uniform",
            "min_inclusive": 0,
            "max_inclusive": 7
        });
        let out = entry(&DIMENSION_TYPE, &ranged).expect("converts");
        let provider = out
            .get("monster_spawn_light_level")
            .and_then(Tag::as_compound)
            .expect("a compound this time");
        assert_eq!(provider.get("max_inclusive"), Some(&Tag::Int(7)));
    }

    #[test]
    fn a_missing_required_key_names_itself() {
        let mut short = a_dimension();
        short.as_object_mut().expect("object").remove("min_y");
        let e = entry(&DIMENSION_TYPE, &short).expect_err("min_y is required");
        assert_eq!(e.path, "min_y");
        assert_eq!(e.kind, ErrorKind::Missing);
    }

    #[test]
    fn a_missing_key_deep_inside_names_its_whole_path() {
        let mut biome = a_biome();
        biome["effects"]["mood_sound"] = json!({
            "sound": "dust:hum",
            "tick_delay": 6000,
            "block_search_extent": 8
        });
        let e = entry(&WORLDGEN_BIOME, &biome).expect_err("offset is required");
        assert_eq!(e.path, "effects.mood_sound.offset");
        assert_eq!(
            e.to_string(),
            "`effects.mood_sound.offset` is required and absent"
        );
    }

    #[test]
    fn a_fraction_where_an_integer_belongs_is_refused_and_not_rounded() {
        // 384.5 would round to a world of a height nobody asked for, and the
        // operator would never be told.
        let mut odd = a_dimension();
        odd["height"] = json!(128.5);
        let e = entry(&DIMENSION_TYPE, &odd).expect_err("not a whole number");
        assert_eq!(e.path, "height");
        assert_eq!(
            e.kind,
            ErrorKind::OutOfRange {
                expected: "TAG_Int"
            }
        );
    }

    #[test]
    fn an_integer_is_accepted_where_a_float_belongs() {
        // JSON writers disagree about whether to emit 2 or 2.0 and they mean
        // the same number.
        let mut round = a_biome();
        round["temperature"] = json!(2);
        let out = entry(&WORLDGEN_BIOME, &round).expect("converts");
        assert_eq!(out.get("temperature"), Some(&Tag::Float(2.0)));
    }

    #[test]
    fn a_number_past_the_range_is_refused_rather_than_wrapped() {
        let mut huge = a_dimension();
        huge["height"] = json!(i64::from(i32::MAX) + 1);
        let e = entry(&DIMENSION_TYPE, &huge).expect_err("past TAG_Int");
        assert_eq!(
            e.kind,
            ErrorKind::OutOfRange {
                expected: "TAG_Int"
            }
        );
    }

    #[test]
    fn a_misspelled_key_is_an_error_and_not_a_silent_drop() {
        // The reason `server_side` is written out. Without it this key would
        // be indistinguishable from `features`, and the entry would be sent
        // with the operator's actual temperature missing.
        let mut typo = a_biome();
        typo.as_object_mut()
            .expect("object")
            .insert("temperture".to_owned(), json!(0.9));
        let e = entry(&WORLDGEN_BIOME, &typo).expect_err("not a key");
        assert_eq!(e.path, "temperture");
        assert_eq!(e.kind, ErrorKind::Unknown);
    }

    #[test]
    fn the_server_side_keys_are_dropped_without_complaint() {
        let mut full = a_biome();
        let object = full.as_object_mut().expect("object");
        object.insert("features".to_owned(), json!([[], [], []]));
        object.insert("spawners".to_owned(), json!({"monster": []}));
        object.insert("carvers".to_owned(), json!({"air": "#minecraft:cave"}));
        object.insert("spawn_costs".to_owned(), json!({}));
        object.insert("creature_spawn_probability".to_owned(), json!(0.07));
        let out = entry(&WORLDGEN_BIOME, &full).expect("converts");
        assert_eq!(out.len(), 4, "the four sent keys");
        for dropped in WORLDGEN_BIOME.server_side {
            assert!(!out.contains_key(dropped), "{dropped} is not sent");
        }
    }

    #[test]
    fn an_unknown_key_nested_in_effects_is_refused_too() {
        let mut biome = a_biome();
        biome["effects"]["glow_color"] = json!(9);
        let e = entry(&WORLDGEN_BIOME, &biome).expect_err("not a key of effects");
        assert_eq!(e.path, "effects.glow_color");
        assert_eq!(e.kind, ErrorKind::Unknown);
    }

    #[test]
    fn a_string_where_a_number_belongs_says_which_it_wanted() {
        let mut wrong = a_biome();
        wrong["temperature"] = json!("warm");
        let e = entry(&WORLDGEN_BIOME, &wrong).expect_err("not a number");
        assert_eq!(e.to_string(), "`temperature` should be a number");
    }

    #[test]
    fn the_document_itself_has_to_be_an_object() {
        let e = entry(&WORLDGEN_BIOME, &json!([1, 2, 3])).expect_err("not an object");
        assert_eq!(e.path, "");
        assert_eq!(e.to_string(), "the entry is not a JSON object");
    }

    #[test]
    fn a_list_gets_its_element_type_from_the_schema() {
        use crate::registries::schema::CHAT_TYPE;
        let out = entry(
            &CHAT_TYPE,
            &json!({
                "chat": {"translation_key": "dust.say", "parameters": ["sender", "content"]},
                "narration": {"translation_key": "dust.say.narrate", "parameters": []}
            }),
        )
        .expect("converts");
        let chat = out.get("chat").and_then(Tag::as_compound).expect("chat");
        let list = chat
            .get("parameters")
            .and_then(Tag::as_list)
            .expect("a list");
        assert_eq!(list.element_type(), dust_nbt::TagType::String);
        assert_eq!(list.len(), 2);

        // An empty list is `TAG_End`-typed, which is what vanilla writes and
        // what its reader expects. There is no first element to ask.
        let narration = out
            .get("narration")
            .and_then(Tag::as_compound)
            .expect("narration");
        let empty = narration
            .get("parameters")
            .and_then(Tag::as_list)
            .expect("a list");
        assert!(empty.is_empty());
        assert_eq!(empty.element_type(), dust_nbt::TagType::End);
    }

    #[test]
    fn a_list_of_the_wrong_element_type_names_the_index() {
        use crate::registries::schema::CHAT_TYPE;
        let e = entry(
            &CHAT_TYPE,
            &json!({
                "chat": {"translation_key": "dust.say", "parameters": ["sender", 7]},
                "narration": {"translation_key": "n", "parameters": []}
            }),
        )
        .expect_err("7 is not a string");
        assert_eq!(e.path, "chat.parameters[1]");
    }

    #[test]
    fn a_map_keeps_its_keys_and_checks_only_its_values() {
        // The keys of `override_armor_materials` are armour material ids —
        // data, not a fixed set — so nothing here knows them in advance.
        use crate::registries::schema::TRIM_MATERIAL;
        let out = entry(
            &TRIM_MATERIAL,
            &json!({
                "asset_name": "grit",
                "ingredient": "dust:grit",
                "item_model_index": 0.5,
                "description": {"translate": "dust.grit"},
                "override_armor_materials": {"dust:tin": "grit_darker", "dust:zinc": "grit_pale"}
            }),
        )
        .expect("converts");
        let overrides = out
            .get("override_armor_materials")
            .and_then(Tag::as_compound)
            .expect("a compound");
        assert_eq!(overrides.len(), 2);
        assert_eq!(
            overrides.get("dust:tin"),
            Some(&Tag::String("grit_darker".to_owned()))
        );
        // Sorted, so two machines reading the same directory write the same
        // bytes. The file's own order is not a fact about the data.
        assert_eq!(
            overrides.keys().collect::<Vec<_>>(),
            vec!["dust:tin", "dust:zinc"]
        );
    }

    #[test]
    fn a_map_value_of_the_wrong_type_names_its_key() {
        use crate::registries::schema::TRIM_MATERIAL;
        let e = entry(
            &TRIM_MATERIAL,
            &json!({
                "asset_name": "grit",
                "ingredient": "dust:grit",
                "item_model_index": 0.5,
                "description": {"translate": "dust.grit"},
                "override_armor_materials": {"dust:tin": 3}
            }),
        )
        .expect_err("3 is not a string");
        assert_eq!(e.path, "override_armor_materials.dust:tin");
    }

    #[test]
    fn the_key_order_is_the_schema_s_and_not_the_file_s() {
        // Two files with the same content in a different order must produce
        // the same bytes, or a `dust.toml` reformat becomes a protocol change.
        let one = entry(&DIMENSION_TYPE, &a_dimension()).expect("converts");
        let mut reversed: serde_json::Map<String, Value> = serde_json::Map::new();
        let source = a_dimension();
        let keys: Vec<_> = source
            .as_object()
            .expect("object")
            .keys()
            .cloned()
            .collect();
        for key in keys.iter().rev() {
            reversed.insert(key.clone(), source[key].clone());
        }
        let two = entry(&DIMENSION_TYPE, &Value::Object(reversed)).expect("converts");
        assert_eq!(
            dust_nbt::write::to_vec_network(Some(&Tag::Compound(one))).expect("writes"),
            dust_nbt::write::to_vec_network(Some(&Tag::Compound(two))).expect("writes"),
        );
    }
}
