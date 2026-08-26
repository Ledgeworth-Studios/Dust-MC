//! Skeletons: the identifying spine of a recipe, loot table or advancement,
//! pinned to the raw document it came from.
//!
//! # Why a skeleton and not a struct
//!
//! The crate documentation draws the line twice, so this module states which
//! side of it this is on. A generated Rust mirror of the datapack schema would
//! be a **second reader**: every new serializer Mojang adds would land in the
//! JSON and miss the structs, and the two would disagree about what a file
//! means. What this module holds instead is the part of those files that is
//! *identity* rather than *meaning*:
//!
//! * the **serializer id** every one of them opens with — `"type":
//!   "minecraft:crafting_shaped"` — because without it nothing can even pick a
//!   schema to read by;
//! * the **reference targets** a report needs — a recipe's result and
//!   ingredients, an advancement's parent, the items an entry names — because
//!   "which vanilla things does this pack reach into" is precisely the
//!   question the Phase 10 feasibility tooling has to answer per pack;
//! * the **raw document**, kept whole beside the parsed parts, so the
//!   skeleton is a summary pinned to its source rather than a replacement for
//!   it.
//!
//! Nothing here validates. An ingredient naming a block instead of an item
//! parses fine and will fail later, in whatever crate reads recipes for real;
//! pretending to catch it here would be the second reader again, arriving by
//! the back door. The property that makes the losslessness testable instead of
//! promised: [`RecipeSkeleton::raw`] and friends hold the original
//! [`serde_json::Value`] untouched, so re-serialising it reproduces the file's
//! content exactly as serde_json sees it — every unknown field, in every
//! version of every format, forever.
//!
//! # The registry is open
//!
//! The known-kind tables below ([`RECIPE_KINDS`], [`LOOT_ENTRY_KINDS`],
//! [`LOOT_CONDITION_KINDS`], [`LOOT_FUNCTION_KINDS`]) cover what vanilla 1.21.1
//! ships. They exist to be counted against — "this pack uses 12 recipe kinds,
//! 3 of them outside the table" is a compatibility measurement — never to gate
//! loading. A kind outside the table parses identically to one inside it;
//! [`LootNode::is_known`] is how a caller says so.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::location::ResourceLocation;
use crate::registry::RegistryId;
use crate::LoadedData;

/// Recipe serializers vanilla 1.21.1 ships, as `type` ids.
pub const RECIPE_KINDS: &[&str] = &[
    "minecraft:crafting_shaped",
    "minecraft:crafting_shapeless",
    "minecraft:crafting_transmute",
    "minecraft:crafting_special_armordye",
    "minecraft:crafting_special_bannerduplicate",
    "minecraft:crafting_special_bookcloning",
    "minecraft:crafting_special_firework_rocket",
    "minecraft:crafting_special_firework_star",
    "minecraft:crafting_special_firework_star_fade",
    "minecraft:crafting_special_mapcloning",
    "minecraft:crafting_special_mapextending",
    "minecraft:crafting_special_repairitem",
    "minecraft:crafting_special_shielddecoration",
    "minecraft:crafting_special_shulkerboxcoloring",
    "minecraft:crafting_special_suspiciousstew",
    "minecraft:crafting_special_tippedarrow",
    "minecraft:blasting",
    "minecraft:campfire_cooking",
    "minecraft:smelting",
    "minecraft:smoking",
    "minecraft:stonecutting",
];

/// Loot-table entry types vanilla 1.21.1 ships.
pub const LOOT_ENTRY_KINDS: &[&str] = &[
    "minecraft:alternatives",
    "minecraft:dynamic",
    "minecraft:empty",
    "minecraft:group",
    "minecraft:item",
    "minecraft:loot_table",
    "minecraft:sequence",
    "minecraft:tag",
];

/// Loot condition types vanilla 1.21.1 ships.
pub const LOOT_CONDITION_KINDS: &[&str] = &[
    "minecraft:block_state_property",
    "minecraft:damage_source_properties",
    "minecraft:entity_properties",
    "minecraft:entity_scores",
    "minecraft:killed_by_player",
    "minecraft:location_check",
    "minecraft:match_tool",
    "minecraft:random_chance",
    "minecraft:random_chance_with_enchanted_bonus",
    "minecraft:reference",
    "minecraft:survives_explosion",
    "minecraft:table_bonus",
    "minecraft:time_check",
    "minecraft:value_check",
    "minecraft:weather_check",
];

/// Loot function types vanilla 1.21.1 ships.
pub const LOOT_FUNCTION_KINDS: &[&str] = &[
    "minecraft:apply_bonus",
    "minecraft:copy_components",
    "minecraft:copy_nbt",
    "minecraft:copy_state",
    "minecraft:enchant_randomly",
    "minecraft:enchant_with_levels",
    "minecraft:exploration_map",
    "minecraft:explosion_decay",
    "minecraft:furnace_smelt",
    "minecraft:fill_player_head",
    "minecraft:set_attributes",
    "minecraft:set_banner_pattern",
    "minecraft:set_components",
    "minecraft:set_contents",
    "minecraft:set_count",
    "minecraft:set_custom_data",
    "minecraft:set_custom_model_data",
    "minecraft:set_damage",
    "minecraft:set_enchantments",
    "minecraft:set_instrument",
    "minecraft:set_loot_table",
    "minecraft:set_name",
    "minecraft:set_lore",
    "minecraft:set_potion",
    "minecraft:set_stew_effect",
    "minecraft:set_ticket",
    "minecraft:toggle_tooltips",
    "minecraft:smelt_item",
    "minecraft:limit_count",
    "minecraft:modify_contents",
    "minecraft:sequence",
];

/// Whether `kind` is in `table`. The whole point of the open registry is that
/// this returning `false` changes no behaviour anywhere — it is a fact for a
/// report, not a decision.
fn is_known(table: &[&str], kind: &str) -> bool {
    table.contains(&kind)
}

/// One typed thing inside a loot table: an entry, a condition or a function.
///
/// All three are the same shape — an object opening with a `"type"` id, plus
/// whatever that serializer means — so all three come back as this. `children`
/// carries nested entries (`alternatives`, `sequence`, `group` nest); the
/// tree is walked however deep the file goes.
#[derive(Debug, Clone, PartialEq)]
pub struct LootNode {
    /// The `type` id as written, or the empty string if there was none.
    pub kind: String,
    /// The target this node names, when it has one: an item entry's `name`,
    /// a tag entry's tag, a `loot_table` entry's table. Best effort by design.
    pub target: Option<ResourceLocation>,
    /// Nested entries, in written order.
    pub children: Vec<LootNode>,
    /// Conditions attached to this node.
    pub conditions: Vec<LootNode>,
    /// Functions attached to this node.
    pub functions: Vec<LootNode>,
    /// The whole node, verbatim.
    pub raw: Value,
}

impl LootNode {
    /// Whether [`Self::kind`] is one vanilla 1.21.1 ships.
    ///
    /// Conditions, functions and entries have different tables; the caller
    /// says which it is asking about through the method it calls.
    pub fn is_known_entry(&self) -> bool {
        is_known(LOOT_ENTRY_KINDS, &self.kind)
    }

    pub fn is_known_condition(&self) -> bool {
        is_known(LOOT_CONDITION_KINDS, &self.kind)
    }

    pub fn is_known_function(&self) -> bool {
        is_known(LOOT_FUNCTION_KINDS, &self.kind)
    }
}

/// One pool of a loot table.
#[derive(Debug, Clone, PartialEq)]
pub struct LootPoolSkeleton {
    pub entries: Vec<LootNode>,
    pub conditions: Vec<LootNode>,
    pub functions: Vec<LootNode>,
    /// The pool as written, verbatim.
    pub raw: Value,
}

/// A loot table's spine.
#[derive(Debug, Clone, PartialEq)]
pub struct LootTableSkeleton {
    /// Pools in written order. Table-level conditions and functions live on
    /// the skeleton itself, since they sit beside `pools`, not inside one.
    pub conditions: Vec<LootNode>,
    pub functions: Vec<LootNode>,
    pub pools: Vec<LootPoolSkeleton>,
    /// The whole table, verbatim.
    pub raw: Value,
}

/// A recipe's spine.
#[derive(Debug, Clone, PartialEq)]
pub struct RecipeSkeleton {
    /// The `type` id as written, or the empty string when the recipe names
    /// none — which [`crate::load`] reports as a warning on the file.
    pub kind: String,
    /// What the recipe produces, when it says: `result.item`, or `result`
    /// where an older spelling wrote a bare name.
    pub result: Option<ResourceLocation>,
    /// Everything the recipe's inputs name — flat, in written order, with
    /// duplicates left in so counts stay meaningful. Ingredient *alternatives*
    /// (a list where one would do) contribute every alternative, because
    /// "what might this pack consume" is the question being answered.
    pub ingredients: Vec<ResourceLocation>,
    /// The whole recipe, verbatim.
    pub raw: Value,
}

/// An advancement's spine.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AdvancementSkeleton {
    /// `parent`, which is how an advancement hangs off another — and how a
    /// pack silently depends on whoever defined the parent.
    pub parent: Option<ResourceLocation>,
    /// `display.icon.item`, when the advancement is displayed at all.
    pub icon: Option<ResourceLocation>,
    /// Criteria by name; the value is the trigger id each one uses.
    pub criteria: BTreeMap<String, String>,
    /// `rewards.recipes` and `rewards.loot` — the advancement granting things.
    pub granted_recipes: Vec<ResourceLocation>,
    pub granted_loot: Vec<ResourceLocation>,
    /// The whole advancement, verbatim.
    pub raw: Value,
}

/// Skeletons for every recipe, loot table and advancement in a load.
///
/// Built once over a [`LoadedData`] with [`ShapeReport::scan`]; the resources
/// themselves stay where they were, so this is a view and costs nothing until
/// asked for.
#[derive(Debug, Clone, Default)]
pub struct ShapeReport {
    recipes: BTreeMap<ResourceLocation, RecipeSkeleton>,
    loot_tables: BTreeMap<ResourceLocation, LootTableSkeleton>,
    advancements: BTreeMap<ResourceLocation, AdvancementSkeleton>,
}

impl ShapeReport {
    /// Pull the three spines out of a finished load.
    ///
    /// Files whose spine cannot be read at all — no `type`, unparseable
    /// references — still appear, with the unreadable part empty; the findings
    /// about *those* belong to [`crate::load`], which already reported them,
    /// so none are produced twice here.
    pub fn scan(data: &LoadedData) -> Self {
        let recipe_key = RegistryId::new("recipe");
        let loot_key = RegistryId::new("loot_table");
        let advancement_key = RegistryId::new("advancement");

        let mut out = Self::default();
        if let Some(registry) = data.registry(&recipe_key) {
            for (name, resource) in registry {
                out.recipes
                    .insert(name.clone(), recipe_skeleton(&resource.value));
            }
        }
        if let Some(registry) = data.registry(&loot_key) {
            for (name, resource) in registry {
                out.loot_tables
                    .insert(name.clone(), loot_skeleton(&resource.value));
            }
        }
        if let Some(registry) = data.registry(&advancement_key) {
            for (name, resource) in registry {
                out.advancements
                    .insert(name.clone(), advancement_skeleton(&resource.value));
            }
        }
        out
    }

    pub fn recipes(&self) -> &BTreeMap<ResourceLocation, RecipeSkeleton> {
        &self.recipes
    }

    pub fn loot_tables(&self) -> &BTreeMap<ResourceLocation, LootTableSkeleton> {
        &self.loot_tables
    }

    pub fn advancements(&self) -> &BTreeMap<ResourceLocation, AdvancementSkeleton> {
        &self.advancements
    }
}

fn type_id(value: &Value) -> String {
    value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// The serializer id of a loot-table node, under whichever key it is written.
///
/// Entries open with `"type"`, conditions with `"condition"` and functions
/// with `"function"` — the format's own inconsistency, not ours, and the
/// reason this is one function rather than three copies of a lookup.
fn loot_kind(value: &Value) -> String {
    for key in ["type", "condition", "function"] {
        if let Some(Value::String(text)) = value.get(key) {
            return text.clone();
        }
    }
    String::new()
}

/// Every `"item"` / `"tag"` / `"id"` string under `value`, shallowly — used
/// for ingredient collection, which wants references without caring which key
/// spelled them.
fn named_references(value: &Value, out: &mut Vec<ResourceLocation>) {
    match value {
        Value::String(text) => {
            // Bare strings in ingredient positions are item names; `#`-led
            // ones name tags. Both parse the same way.
            if let Ok(name) = ResourceLocation::parse(text.trim_start_matches('#')) {
                out.push(name);
            }
        }
        Value::Object(object) => {
            for key in ["item", "tag", "id"] {
                if let Some(Value::String(text)) = object.get(key) {
                    if let Ok(name) = ResourceLocation::parse(text) {
                        out.push(name);
                    }
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                named_references(item, out);
            }
        }
        _ => {}
    }
}

fn recipe_skeleton(value: &Value) -> RecipeSkeleton {
    let mut ingredients = Vec::new();
    match type_id(value).as_str() {
        "minecraft:crafting_shapeless" => {
            if let Some(list) = value.get("ingredients").and_then(Value::as_array) {
                for item in list {
                    named_references(item, &mut ingredients);
                }
            }
        }
        "minecraft:crafting_shaped" => {
            if let Some(key) = value.get("key").and_then(Value::as_object) {
                for symbol_value in key.values() {
                    named_references(symbol_value, &mut ingredients);
                }
            }
        }
        // Smelting, blasting, smoking, campfire cooking and stonecutting all
        // take a single `ingredient`.
        _ => {
            if let Some(ingredient) = value.get("ingredient") {
                named_references(ingredient, &mut ingredients);
            }
        }
    }

    let result = match value.get("result") {
        Some(Value::String(text)) => ResourceLocation::parse(text).ok(),
        Some(Value::Object(object)) => object
            .get("item")
            .and_then(Value::as_str)
            .and_then(|text| ResourceLocation::parse(text).ok()),
        _ => None,
    };

    RecipeSkeleton {
        kind: type_id(value),
        result,
        ingredients,
        raw: value.clone(),
    }
}

/// Walk one loot-table node: entry, condition or function.
fn loot_node(value: &Value) -> LootNode {
    let mut node = LootNode {
        kind: loot_kind(value),
        target: None,
        children: Vec::new(),
        conditions: Vec::new(),
        functions: Vec::new(),
        raw: value.clone(),
    };
    // The three keys an entry can name something by: `name` for items, tags
    // and inline tables; `loot_table` for a reference written that way.
    for key in ["name", "loot_table"] {
        if let Some(Value::String(text)) = value.get(key) {
            // A `name` under some functions is arbitrary text (set_name's
            // component source); only a parseable location becomes the
            // target, and anything else simply stays unparsed.
            if let Ok(name) = ResourceLocation::parse(text.trim_start_matches('#')) {
                node.target = Some(name);
                break;
            }
        }
    }
    if let Some(children) = value.get("children") {
        collect_children(children, &mut node.children);
    }
    if let Some(conditions) = value.get("conditions").and_then(Value::as_array) {
        node.conditions = conditions.iter().map(loot_node).collect();
    }
    if let Some(functions) = value.get("functions").and_then(Value::as_array) {
        node.functions = functions.iter().map(loot_node).collect();
    }
    node
}

/// `children` is a list on the nesting entry types and may be a single object
/// where a writer minimised; both spellings walk.
fn collect_children(value: &Value, out: &mut Vec<LootNode>) {
    match value {
        Value::Array(items) => out.extend(items.iter().map(loot_node)),
        Value::Object(_) => out.push(loot_node(value)),
        _ => {}
    }
}

fn loot_skeleton(value: &Value) -> LootTableSkeleton {
    let pools = value
        .get("pools")
        .and_then(Value::as_array)
        .map(|pools| {
            pools
                .iter()
                .map(|pool| LootPoolSkeleton {
                    entries: pool
                        .get("entries")
                        .and_then(Value::as_array)
                        .map(|entries| entries.iter().map(loot_node).collect())
                        .unwrap_or_default(),
                    conditions: pool
                        .get("conditions")
                        .and_then(Value::as_array)
                        .map(|items| items.iter().map(loot_node).collect())
                        .unwrap_or_default(),
                    functions: pool
                        .get("functions")
                        .and_then(Value::as_array)
                        .map(|items| items.iter().map(loot_node).collect())
                        .unwrap_or_default(),
                    raw: pool.clone(),
                })
                .collect()
        })
        .unwrap_or_default();

    LootTableSkeleton {
        conditions: value
            .get("conditions")
            .and_then(Value::as_array)
            .map(|items| items.iter().map(loot_node).collect())
            .unwrap_or_default(),
        functions: value
            .get("functions")
            .and_then(Value::as_array)
            .map(|items| items.iter().map(loot_node).collect())
            .unwrap_or_default(),
        pools,
        raw: value.clone(),
    }
}

fn advancement_skeleton(value: &Value) -> AdvancementSkeleton {
    let parent = value
        .get("parent")
        .and_then(Value::as_str)
        .and_then(|text| ResourceLocation::parse(text).ok());
    let icon = value
        .get("display")
        .and_then(|display| display.get("icon"))
        .and_then(|icon| icon.get("item"))
        .and_then(Value::as_str)
        .and_then(|text| ResourceLocation::parse(text).ok());
    let mut criteria = BTreeMap::new();
    if let Some(map) = value.get("criteria").and_then(Value::as_object) {
        for (name, criterion) in map {
            if let Some(trigger) = criterion.get("trigger").and_then(Value::as_str) {
                criteria.insert(name.clone(), trigger.to_owned());
            }
        }
    }
    let (granted_recipes, granted_loot) = match value.get("rewards") {
        Some(rewards) => (
            rewards
                .get("recipes")
                .and_then(named_list)
                .unwrap_or_default(),
            rewards.get("loot").and_then(named_list).unwrap_or_default(),
        ),
        None => (Vec::new(), Vec::new()),
    };

    AdvancementSkeleton {
        parent,
        icon,
        criteria,
        granted_recipes,
        granted_loot,
        raw: value.clone(),
    }
}

/// `rewards.recipes` is written as a list of names, or as one where a writer
/// minimised.
fn named_list(value: &Value) -> Option<Vec<ResourceLocation>> {
    match value {
        Value::Array(items) => Some(
            items
                .iter()
                .filter_map(|item| item.as_str())
                .filter_map(|text| ResourceLocation::parse(text).ok())
                .collect(),
        ),
        Value::String(text) => ResourceLocation::parse(text).ok().map(|name| vec![name]),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shaped_recipe_keeps_its_spine_and_every_unknown_field() {
        let value: Value = serde_json::json!({
            "type": "minecraft:crafting_shaped",
            "pattern": ["XX", "XX"],
            "key": {"X": {"item": "minecraft:copper_ingot"}},
            "result": {"item": "somemod:copper_block", "count": 4},
            "somemod:heat": 900
        });
        let recipe = recipe_skeleton(&value);
        assert_eq!(recipe.kind, "minecraft:crafting_shaped");
        assert_eq!(
            recipe.result.as_ref().map(ResourceLocation::to_string),
            Some("somemod:copper_block".to_owned())
        );
        assert_eq!(
            recipe.ingredients,
            vec![ResourceLocation::parse("minecraft:copper_ingot").unwrap()]
        );
        // Losslessness is the contract: the raw form is the input, byte for
        // byte as serde_json saw it.
        assert_eq!(recipe.raw, value);
    }

    #[test]
    fn an_alternative_ingredient_contributes_every_option() {
        let value: Value = serde_json::json!({
            "type": "minecraft:crafting_shapeless",
            "ingredients": [
                [{"item": "minecraft:oak_planks"}, {"item": "minecraft:bamboo_planks"}],
                "minecraft:honeycomb"
            ],
            "result": {"item": "minecraft:slime_ball"}
        });
        let recipe = recipe_skeleton(&value);
        let names: Vec<String> = recipe
            .ingredients
            .iter()
            .map(ResourceLocation::to_string)
            .collect();
        assert_eq!(
            names,
            vec![
                "minecraft:oak_planks",
                "minecraft:bamboo_planks",
                "minecraft:honeycomb"
            ]
        );
    }

    #[test]
    fn a_recipe_without_a_type_still_scans_with_an_empty_kind() {
        // The warning about the missing `type` belongs to `load`; scanning
        // must not double-report it, but must not drop the recipe either.
        let value: Value = serde_json::json!({"pattern": ["X"], "key": {}});
        let recipe = recipe_skeleton(&value);
        assert_eq!(recipe.kind, "");
        assert!(recipe.result.is_none());
    }

    #[test]
    fn a_loot_table_walks_pools_entries_conditions_and_functions() {
        let value: Value = serde_json::json!({
            "type": "minecraft:block",
            "pools": [{
                "rolls": 1,
                "entries": [{
                    "type": "minecraft:alternatives",
                    "children": [
                        {
                            "type": "minecraft:item",
                            "name": "minecraft:silk_touch_shears",
                            "conditions": [{
                                "condition": "minecraft:match_tool",
                                "predicate": {"enchantments": []}
                            }],
                            "functions": [{"function": "minecraft:set_count", "count": 3}]
                        },
                        {"type": "minecraft:item", "name": "minecraft:cobblestone"}
                    ]
                }]
            }]
        });
        let table = loot_skeleton(&value);
        assert_eq!(table.pools.len(), 1);
        let entry = &table.pools[0].entries[0];
        assert_eq!(entry.kind, "minecraft:alternatives");
        assert!(entry.is_known_entry());
        assert_eq!(entry.children.len(), 2);

        let first = &entry.children[0];
        assert_eq!(
            first.target.as_ref().map(ResourceLocation::to_string),
            Some("minecraft:silk_touch_shears".to_owned())
        );
        assert_eq!(first.conditions.len(), 1);
        assert!(first.conditions[0].is_known_condition());
        assert_eq!(first.functions.len(), 1);
        assert!(first.functions[0].is_known_function());

        // And the whole tree is still there verbatim.
        assert_eq!(entry.raw, value["pools"][0]["entries"][0]);
    }

    #[test]
    fn a_kind_outside_the_table_is_countable_and_loads_the_same() {
        let value: Value = serde_json::json!({"type": "somemod:custom_roll", "name": "x"});
        let node = loot_node(&value);
        assert_eq!(node.kind, "somemod:custom_roll");
        assert!(
            !node.is_known_entry(),
            "an open registry knows what it knows"
        );
        // …and the node is intact regardless.
        assert_eq!(node.raw, value);
    }

    #[test]
    fn an_advancement_keeps_parent_icon_criteria_and_rewards() {
        let value: Value = serde_json::json!({
            "parent": "minecraft:husbandry/root",
            "display": {"icon": {"item": "minecraft:wheat"}, "title": "Grow"},
            "criteria": {
                "grown": {"trigger": "minecraft:item_used_on_block"}
            },
            "rewards": {"recipes": ["somemod:bread"], "loot": ["somemod:seed_drop"]}
        });
        let advancement = advancement_skeleton(&value);
        assert_eq!(
            advancement.parent.map(|p| p.to_string()),
            Some("minecraft:husbandry/root".to_owned())
        );
        assert_eq!(
            advancement.icon.map(|i| i.to_string()),
            Some("minecraft:wheat".to_owned())
        );
        assert_eq!(
            advancement.criteria["grown"],
            "minecraft:item_used_on_block"
        );
        assert_eq!(advancement.granted_recipes[0].to_string(), "somemod:bread");
        assert_eq!(advancement.raw, value);
    }

    #[test]
    fn rewards_written_as_a_single_name_are_read_too() {
        let value: Value = serde_json::json!({
            "criteria": {},
            "rewards": {"recipes": "somemod:one_recipe"}
        });
        let advancement = advancement_skeleton(&value);
        assert_eq!(
            advancement.granted_recipes[0].to_string(),
            "somemod:one_recipe"
        );
    }
}
