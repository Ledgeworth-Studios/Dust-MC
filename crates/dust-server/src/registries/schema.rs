//! What a synced registry entry looks like once it is on the wire.
//!
//! # Why a schema and not a JSON walk
//!
//! Minecraft writes these entries as JSON on disk and as NBT on the wire, and
//! JSON has one number type where NBT has six. `"ambient_light": 0.0` is a
//! `TAG_Float`; `"coordinate_scale": 1.0` beside it is a `TAG_Double`; `0.0`
//! tells you neither. A converter that guessed from the value would put a
//! float where a double belongs, and the client would read four of the
//! following eight bytes as a number and the other four as the next field.
//!
//! So the types come from here. This table is a description of an interface —
//! the same kind of thing as a packet definition in `dust-protocol` — and it
//! is written from bytes a real 1.21.1 server sent, not from a wiki. The
//! *values* are not here and never will be: they come from the data the
//! operator's own installation ships. See decision record 0007.
//!
//! # Three kinds of key, and why the third one exists
//!
//! A key in the on-disk JSON is one of:
//!
//! * **Sent** — it appears in [`Registry::fields`] and goes on the wire.
//! * **Server-side** — it appears in [`Registry::server_side`], and is dropped
//!   deliberately. A biome's `features`, `carvers` and `spawners` are real,
//!   load-bearing data that the client is never told: it does not generate
//!   terrain and it does not decide what spawns.
//! * **Neither** — which is an error naming the key.
//!
//! The third case is the reason the second one is written out. Dropping
//! anything that is not recognised would make an unknown key and a known
//! server-side key indistinguishable, so a datapack whose biome carries a
//! misspelled `temperture` would load, send an entry without it, and give the
//! operator a world that was subtly not the one they configured. Listing what
//! is dropped costs eleven lines and turns that into a message.

/// One field's type, as the network codec writes it.
#[derive(Debug, Clone, Copy)]
pub enum Field {
    /// A JSON boolean. `TAG_Byte`, 1 or 0 — NBT has no boolean.
    Bool,
    /// `TAG_Int`.
    Int,
    /// `TAG_Long`.
    Long,
    /// `TAG_Float`.
    Float,
    /// `TAG_Double`.
    Double,
    /// `TAG_String`.
    Str,
    /// A nested object with a schema of its own.
    Object(&'static [Key]),
    /// Either a bare integer or an object — vanilla's "int provider", which is
    /// a number where it is constant and a compound where it is a range.
    /// `dimension_type`'s `monster_spawn_light_level` is both across the four
    /// vanilla dimensions, which is exactly why this variant exists rather
    /// than a guess about which one wins.
    IntOrObject(&'static [Key]),
    /// A JSON array whose elements are all one type. NBT lists are typed —
    /// there is no heterogeneous list tag — so this carries the element type
    /// rather than deriving it from the first element, which would make an
    /// empty list untypeable and a mixed one silently truncate.
    List(&'static Field),
    /// An object whose *keys* are data — a map, not a record. The keys are not
    /// known in advance, so only the value type can be checked.
    /// `trim_material`'s `override_armor_materials` is keyed by armour
    /// material id, and listing those ids here would be committing a slice of
    /// Minecraft's item registry to say something the value type already says.
    Map(&'static Field),
}

/// One key of an entry, with the type it takes and whether it may be absent.
#[derive(Debug, Clone, Copy)]
pub struct Key {
    /// The key, as it appears in both the JSON and the NBT.
    pub name: &'static str,
    /// What it becomes.
    pub field: Field,
    /// `true` if every entry must carry it. An absent optional key is absent
    /// on the wire too, which is what the client's own codec expects: it is
    /// not written as a default, because a default written out is a value the
    /// client cannot tell from one the pack chose.
    pub required: bool,
}

const fn required(name: &'static str, field: Field) -> Key {
    Key {
        name,
        field,
        required: true,
    }
}

const fn optional(name: &'static str, field: Field) -> Key {
    Key {
        name,
        field,
        required: false,
    }
}

/// One registry Dust can serve the contents of.
#[derive(Debug, Clone, Copy)]
pub struct Registry {
    /// The registry's namespaced id, as the sync packet names it.
    pub name: &'static str,
    /// The directory its entries live in under `data/<namespace>/`.
    pub directory: &'static str,
    /// The keys that go on the wire.
    pub fields: &'static [Key],
    /// Keys the on-disk form carries that the network form does not. Dropped
    /// knowingly; see the module note.
    pub server_side: &'static [&'static str],
}

/// A chat style: the subset of a text component's formatting that these
/// registries use.
///
/// Not the whole of the text-component grammar, which is far larger and is
/// `dust-protocol`'s problem rather than this module's. The keys here are the
/// ones vanilla's own registry files actually carry, and anything else is
/// refused by name — which is the right answer for a file somebody wrote by
/// hand, and the wrong one to guess at.
const STYLE: &[Key] = &[
    optional("color", Field::Str),
    optional("bold", Field::Bool),
    optional("italic", Field::Bool),
    optional("underlined", Field::Bool),
    optional("strikethrough", Field::Bool),
    optional("obfuscated", Field::Bool),
    optional("font", Field::Str),
    optional("insertion", Field::Str),
];

/// A translatable label with optional styling — `{"translate": "..."}`.
///
/// Five registries describe themselves this way and all five carry a
/// `translate` key and nothing but style beside it.
const DESCRIPTION: &[Key] = &[
    required("translate", Field::Str),
    optional("fallback", Field::Str),
    optional("color", Field::Str),
    optional("bold", Field::Bool),
    optional("italic", Field::Bool),
    optional("underlined", Field::Bool),
    optional("strikethrough", Field::Bool),
    optional("obfuscated", Field::Bool),
];

const STRING_LIST: Field = Field::List(&Field::Str);

/// `minecraft:dimension_type`.
///
/// Eighteen keys, every one of them sent, and the client needs them: `min_y`
/// and `height` are how it sizes a chunk column, so a client that has these
/// wrong does not render a wrong world, it fails to read the chunk packet at
/// all.
pub const DIMENSION_TYPE: Registry = Registry {
    name: "minecraft:dimension_type",
    directory: "dimension_type",
    fields: &[
        required("ambient_light", Field::Float),
        required("bed_works", Field::Bool),
        required("coordinate_scale", Field::Double),
        required("effects", Field::Str),
        // Absent in the overworld and the end; 18000 in the nether, which is
        // what makes it always noon there.
        optional("fixed_time", Field::Long),
        required("has_ceiling", Field::Bool),
        required("has_raids", Field::Bool),
        required("has_skylight", Field::Bool),
        required("height", Field::Int),
        required("infiniburn", Field::Str),
        required("logical_height", Field::Int),
        required("min_y", Field::Int),
        required("monster_spawn_block_light_limit", Field::Int),
        required(
            "monster_spawn_light_level",
            Field::IntOrObject(&[
                required("type", Field::Str),
                required("min_inclusive", Field::Int),
                required("max_inclusive", Field::Int),
            ]),
        ),
        required("natural", Field::Bool),
        required("piglin_safe", Field::Bool),
        required("respawn_anchor_works", Field::Bool),
        required("ultrawarm", Field::Bool),
    ],
    server_side: &[],
};

/// The `effects` object of a biome: everything the client renders or plays.
const BIOME_EFFECTS: &[Key] = &[
    required("fog_color", Field::Int),
    required("sky_color", Field::Int),
    required("water_color", Field::Int),
    required("water_fog_color", Field::Int),
    // Present only where the biome overrides the colour its temperature and
    // downfall would otherwise give it — six biomes for foliage, four for
    // grass, out of sixty-four.
    optional("foliage_color", Field::Int),
    optional("grass_color", Field::Int),
    // `dark_forest` and `swamp`. Not namespaced, unlike almost every other
    // string here: it is an enum name rather than a resource location.
    optional("grass_color_modifier", Field::Str),
    optional(
        "particle",
        Field::Object(&[
            required("probability", Field::Float),
            // The particle's own parameters. Every vanilla biome uses a
            // particle that takes none, so `type` is all there is; a datapack
            // using one that takes more will be refused by name rather than
            // sent half-encoded.
            required("options", Field::Object(&[required("type", Field::Str)])),
        ]),
    ),
    optional("ambient_sound", Field::Str),
    optional(
        "mood_sound",
        Field::Object(&[
            required("sound", Field::Str),
            required("tick_delay", Field::Int),
            required("block_search_extent", Field::Int),
            required("offset", Field::Double),
        ]),
    ),
    optional(
        "additions_sound",
        Field::Object(&[
            required("sound", Field::Str),
            required("tick_chance", Field::Double),
        ]),
    ),
    optional(
        "music",
        Field::Object(&[
            required("sound", Field::Str),
            required("min_delay", Field::Int),
            required("max_delay", Field::Int),
            required("replace_current_music", Field::Bool),
        ]),
    ),
];

/// `minecraft:worldgen/biome`.
///
/// Far smaller on the wire than on disk. A biome's JSON is mostly `features`
/// and `spawners`; the client is told the climate and the colours and nothing
/// else, because it neither generates terrain nor decides what spawns. That
/// asymmetry was measured — sixty-four entries in twenty kilobytes, against
/// data files several times that — and not assumed.
pub const WORLDGEN_BIOME: Registry = Registry {
    name: "minecraft:worldgen/biome",
    directory: "worldgen/biome",
    fields: &[
        required("has_precipitation", Field::Bool),
        required("temperature", Field::Float),
        required("downfall", Field::Float),
        // `frozen`, on the two frozen oceans. Absent everywhere else.
        optional("temperature_modifier", Field::Str),
        required("effects", Field::Object(BIOME_EFFECTS)),
    ],
    server_side: &[
        // How the biome generates and what lives in it. All of it real, none
        // of it the client's business.
        "carvers",
        "features",
        "spawners",
        "spawn_costs",
        "creature_spawn_probability",
    ],
};

/// `minecraft:chat_type`.
///
/// How a chat message is turned into a line and into speech. A client that
/// receives a chat packet is given a chat-type *id*, so a client with no copy
/// of this registry and no entry for that id has a message it cannot render.
pub const CHAT_TYPE: Registry = Registry {
    name: "minecraft:chat_type",
    directory: "chat_type",
    fields: &[
        required(
            "chat",
            Field::Object(&[
                required("translation_key", Field::Str),
                required("parameters", STRING_LIST),
                optional("style", Field::Object(STYLE)),
            ]),
        ),
        required(
            "narration",
            Field::Object(&[
                required("translation_key", Field::Str),
                required("parameters", STRING_LIST),
                optional("style", Field::Object(STYLE)),
            ]),
        ),
    ],
    server_side: &[],
};

/// `minecraft:damage_type`.
///
/// The death message, the hunger it costs and how it scales with difficulty.
/// Same reasoning as `chat_type`: damage arrives by id.
pub const DAMAGE_TYPE: Registry = Registry {
    name: "minecraft:damage_type",
    directory: "damage_type",
    fields: &[
        required("message_id", Field::Str),
        required("scaling", Field::Str),
        required("exhaustion", Field::Float),
        // `burning`, `freezing`, `drowning` — the sound and screen effect.
        // Absent on most, which is the default of "hurt".
        optional("effects", Field::Str),
        // Only `fell_out_of_world` and `outside_border` carry this.
        optional("death_message_type", Field::Str),
    ],
    server_side: &[],
};

/// `minecraft:banner_pattern`.
pub const BANNER_PATTERN: Registry = Registry {
    name: "minecraft:banner_pattern",
    directory: "banner_pattern",
    fields: &[
        required("asset_id", Field::Str),
        required("translation_key", Field::Str),
    ],
    server_side: &[],
};

/// `minecraft:painting_variant`.
///
/// Width and height in blocks, and the texture. All three are the client's:
/// a painting entity names its variant and nothing else.
pub const PAINTING_VARIANT: Registry = Registry {
    name: "minecraft:painting_variant",
    directory: "painting_variant",
    fields: &[
        required("asset_id", Field::Str),
        required("width", Field::Int),
        required("height", Field::Int),
    ],
    server_side: &[],
};

/// `minecraft:wolf_variant`.
///
/// `biomes` is a single string in every vanilla file — a biome id or a
/// `#`-prefixed tag — and it is typed as one string rather than as a list
/// because that is what the data says. A pack that writes a list will be
/// refused by name, which is the honest failure: the alternative is accepting
/// a shape nothing has been observed to accept.
pub const WOLF_VARIANT: Registry = Registry {
    name: "minecraft:wolf_variant",
    directory: "wolf_variant",
    fields: &[
        required("wild_texture", Field::Str),
        required("tame_texture", Field::Str),
        required("angry_texture", Field::Str),
        required("biomes", Field::Str),
    ],
    server_side: &[],
};

/// `minecraft:trim_pattern`.
pub const TRIM_PATTERN: Registry = Registry {
    name: "minecraft:trim_pattern",
    directory: "trim_pattern",
    fields: &[
        required("asset_id", Field::Str),
        required("template_item", Field::Str),
        required("description", Field::Object(DESCRIPTION)),
        required("decal", Field::Bool),
    ],
    server_side: &[],
};

/// `minecraft:trim_material`.
///
/// `override_armor_materials` is the one map in these two dozen schemas: its
/// keys are armour material ids, which are data rather than a fixed set.
pub const TRIM_MATERIAL: Registry = Registry {
    name: "minecraft:trim_material",
    directory: "trim_material",
    fields: &[
        required("asset_name", Field::Str),
        required("ingredient", Field::Str),
        required("item_model_index", Field::Float),
        required("description", Field::Object(DESCRIPTION)),
        optional("override_armor_materials", Field::Map(&Field::Str)),
    ],
    server_side: &[],
};

/// `minecraft:jukebox_song`.
pub const JUKEBOX_SONG: Registry = Registry {
    name: "minecraft:jukebox_song",
    directory: "jukebox_song",
    fields: &[
        required("sound_event", Field::Str),
        required("description", Field::Object(DESCRIPTION)),
        required("length_in_seconds", Field::Float),
        required("comparator_output", Field::Int),
    ],
    server_side: &[],
};

/// Every registry whose contents Dust can build.
///
/// Ten of the eleven. The missing one is `minecraft:enchantment`, and decision
/// record 0009 is why, from a measurement rather than an impression:
/// `harness registries --dump minecraft:enchantment` reports **470 key paths,
/// eleven levels deep**, of which nine are the flat part and 461 live under
/// `effects`. Three of its seventy-nine floating-point paths are doubles and
/// the rest floats, written identically in the JSON, separated only by what
/// dispatched the object holding them — so the table cannot reach them, and a
/// table of the 470 paths vanilla happens to exercise would refuse a datapack
/// enchantment that used a 471st.
///
/// **Sending the nine flat keys and leaving `effects` out is worse than
/// sending nothing**, which is the part worth stating here. It parses — one
/// vanilla enchantment has no `effects` — and it makes Protection stop
/// protecting on a client that would otherwise have used its own correct copy.
///
/// A registry not on this list is sent as *names* to a client that
/// acknowledged the core pack, and **not sent at all** to one that did not —
/// all of a registry or none of it, the same rule tags follow. A client told
/// nothing falls back to its own copy; a client told a list of names it has no
/// definitions for believes those things exist and are empty, which is how a
/// bot ends up reading `undefined` in its own registry loader.
pub const SERVED: &[Registry] = &[
    DIMENSION_TYPE,
    WORLDGEN_BIOME,
    CHAT_TYPE,
    DAMAGE_TYPE,
    BANNER_PATTERN,
    PAINTING_VARIANT,
    WOLF_VARIANT,
    TRIM_PATTERN,
    TRIM_MATERIAL,
    JUKEBOX_SONG,
];

/// The registry with this name, if Dust can build its contents.
pub fn by_name(name: &str) -> Option<&'static Registry> {
    SERVED.iter().find(|registry| registry.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walk(keys: &'static [Key], visit: &mut impl FnMut(&'static Key)) {
        for key in keys {
            visit(key);
            match key.field {
                Field::Object(inner) | Field::IntOrObject(inner) => walk(inner, visit),
                _ => {}
            }
        }
    }

    #[test]
    fn no_object_repeats_a_key() {
        // A repeated key is a compound with two bindings for one name, and
        // `Compound::get` resolves the last. The client would read whichever
        // vanilla's map happened to keep, so the two would silently disagree.
        fn check(keys: &'static [Key], where_: &str) {
            let mut seen = std::collections::BTreeSet::new();
            for key in keys {
                assert!(seen.insert(key.name), "{where_} lists {} twice", key.name);
                match key.field {
                    Field::Object(inner) | Field::IntOrObject(inner) => {
                        check(inner, &format!("{where_}.{}", key.name));
                    }
                    _ => {}
                }
            }
        }
        for registry in SERVED {
            check(registry.fields, registry.name);
        }
    }

    #[test]
    fn nothing_is_both_sent_and_dropped() {
        // The two lists answer the same question, so an overlap is a schema
        // that says a key both goes and does not go.
        for registry in SERVED {
            for dropped in registry.server_side {
                assert!(
                    !registry.fields.iter().any(|key| key.name == *dropped),
                    "{} both sends and drops {dropped}",
                    registry.name
                );
            }
        }
    }

    #[test]
    fn every_registry_is_one_the_names_table_also_knows() {
        // The contents and the names are two tables over one list, and the
        // order in the names table is what assigns the ids. A registry served
        // with contents that the names table does not hold would be sent
        // twice or not at all.
        for registry in SERVED {
            assert!(
                dust_registry::synced::by_name(registry.name).is_some(),
                "{} has contents but no name table",
                registry.name
            );
        }
    }

    #[test]
    fn exactly_one_synced_registry_is_left_unserved() {
        // The omission is deliberate and it is one registry. An eleventh name
        // appearing here means either a new registry nobody wrote a schema
        // for — which would be sent as bare names to a client with no
        // definitions for them, the failure the `SERVED` note describes — or
        // `minecraft:enchantment` quietly gaining one. Both want a reader,
        // and decision record 0009 is what they should read.
        let unserved: Vec<&str> = dust_registry::synced::all()
            .iter()
            .map(|registry| registry.name)
            .filter(|name| by_name(name).is_none())
            .collect();
        assert_eq!(
            unserved,
            ["minecraft:enchantment"],
            "see docs/decisions/0009-enchantment-registry.md"
        );
    }

    #[test]
    fn the_int_provider_is_the_only_union() {
        // Not a rule about Minecraft; a rule about this schema. Every other
        // key resolves to one NBT type, which is what lets the converter
        // report a type error against a single expectation.
        let mut unions = 0;
        for registry in SERVED {
            walk(registry.fields, &mut |key| {
                if matches!(key.field, Field::IntOrObject(_)) {
                    unions += 1;
                }
            });
        }
        assert_eq!(unions, 1, "monster_spawn_light_level and nothing else");
    }
}
