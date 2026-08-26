//! The skeleton layer: spines extracted, raw documents preserved.
//!
//! The property under test here is the module's whole contract: whatever a
//! file holds that the skeletons do not *understand* must still be there
//! afterwards, byte-semantically. Random JSON is poured into the unknown
//! corners of synthetic resources and everything must come back out.

mod support;

use dust_data::registry::RegistryId;
use dust_data::{load, LoadOptions, PackSource, ResourceLocation, ShapeReport};
use support::{PackBuilder, Rng};

fn location(text: &str) -> ResourceLocation {
    ResourceLocation::parse(text).expect("valid")
}

#[test]
fn scan_extracts_the_spines_of_all_three_shapes() {
    let pack = PackBuilder::new("shaped")
        .resource(
            "minecraft",
            "recipe",
            "gems",
            r#"{
                "type": "minecraft:crafting_shapeless",
                "ingredients": [{"item":"minecraft:amethyst_shard"},{"item":"minecraft:quartz"}],
                "result": {"item":"somemod:gems","count":2}
            }"#,
        )
        .resource(
            "minecraft",
            "loot_table",
            "blocks/ore",
            include_str!("fixtures/synthetic_ore_loot.json"),
        )
        .resource(
            "minecraft",
            "advancement",
            "mining/root",
            r#"{
                "parent": "minecraft:story/root",
                "display": {"icon": {"item": "minecraft:iron_pickaxe"}},
                "criteria": {"struck": {"trigger": "minecraft:block_placed"}},
                "rewards": {"recipes": ["somemod:reward"]}
            }"#,
        )
        .build();

    let data = load(&[&pack as &dyn PackSource], &LoadOptions::default());
    let report = ShapeReport::scan(&data);

    let recipe = report
        .recipes()
        .get(&location("minecraft:gems"))
        .expect("recipe");
    assert_eq!(recipe.kind, "minecraft:crafting_shapeless");
    assert_eq!(
        recipe.result.as_ref().map(|r| r.to_string()),
        Some("somemod:gems".to_owned())
    );
    assert_eq!(recipe.ingredients.len(), 2);

    let table = report
        .loot_tables()
        .get(&location("minecraft:blocks/ore"))
        .expect("table");
    assert_eq!(table.pools.len(), 1);
    // The alternatives node's two children plus the modded sibling.
    let top = &table.pools[0].entries;
    assert_eq!(top.len(), 2);
    assert_eq!(top[0].children.len(), 2);
    assert!(top[0].children[0]
        .functions
        .iter()
        .any(|f| f.is_known_function()));
    // A vanilla entry kind naming a modded item: known kind, extracted
    // target — the two facts travel separately on purpose.
    assert!(top[1].is_known_entry());
    assert_eq!(
        top[1].target.as_ref().map(|t| t.to_string()),
        Some("somemod:not_a_vanilla_item".to_owned())
    );

    let advancement = report
        .advancements()
        .get(&location("minecraft:mining/root"))
        .expect("advancement");
    assert_eq!(
        advancement.parent.as_ref().map(|p| p.to_string()),
        Some("minecraft:story/root".to_owned())
    );
    assert_eq!(advancement.granted_recipes.len(), 1);
}

/// A random JSON value, depth-limited, with keys drawn from a pool so objects
/// sometimes collide in interesting ways.
fn random_value(rng: &mut Rng, depth: usize) -> serde_json::Value {
    use serde_json::Value;
    if depth == 0 {
        return match rng.below(5) {
            0 => Value::Null,
            1 => Value::Bool(rng.next_u64() % 2 == 0),
            2 => Value::Number((rng.next_u64() % 1_000_000).into()),
            3 => Value::Number(
                serde_json::Number::from_f64((rng.next_u64() % 100_000) as f64 / 8.0)
                    .expect("finite"),
            ),
            _ => Value::String(format!("v{}", rng.next_u64() % 50)),
        };
    }
    if rng.below(2) == 0 {
        Value::Array(
            (0..rng.below(4))
                .map(|_| random_value(rng, depth - 1))
                .collect(),
        )
    } else {
        let mut object = serde_json::Map::new();
        for _ in 0..rng.below(5) {
            object.insert(
                format!("k{}", rng.next_u64() % 30),
                random_value(rng, depth - 1),
            );
        }
        Value::Object(object)
    }
}

#[test]
fn unknown_fields_survive_scan_byte_semantically_across_many_seeds() {
    // The forward-compat guarantee, made adversarial: a recipe carrying a
    // deep random payload nobody parses must come back from scanning exactly
    // as it went in. "Byte-semantically" means every key/value pair, number,
    // string and null identical after a re-parse — whitespace and key order
    // are not information in JSON, and serde_json normalises both; nothing
    // else may change.
    for seed in 0..200_u64 {
        let mut rng = Rng::new(seed);
        let payload = random_value(&mut rng, 4);

        let mut recipe: serde_json::Value = serde_json::from_str(
            r#"{"type":"minecraft:crafting_shapeless",
                "ingredients":[{"item":"minecraft:stone"}],
                "result":{"item":"minecraft:cobblestone"}}"#,
        )
        .unwrap();
        recipe
            .as_object_mut()
            .unwrap()
            .insert(format!("somemod:field_{seed}"), payload.clone());

        let pack = PackBuilder::new("lossless")
            .resource(
                "minecraft",
                "recipe",
                &format!("seeded_{seed}"),
                &recipe.to_string(),
            )
            .build();
        let data = load(&[&pack as &dyn PackSource], &LoadOptions::default());
        let report = ShapeReport::scan(&data);
        let scanned = report
            .recipes()
            .get(&location(&format!("minecraft:seeded_{seed}")))
            .expect("scanned");

        assert_eq!(scanned.raw, recipe, "seed {seed}: the raw form moved");
        // And re-serialising reproduces the same content — what any
        // downstream consumer would actually do with it.
        let round_trip: serde_json::Value =
            serde_json::from_str(&scanned.raw.to_string()).expect("re-parses");
        assert_eq!(round_trip, recipe, "seed {seed}: not stable under re-parse");
        // The unknown corner specifically: the injected payload is intact
        // after both hops.
        assert_eq!(payload_in(&round_trip, seed), payload, "seed {seed}");
    }
}

fn payload_in(recipe: &serde_json::Value, seed: u64) -> serde_json::Value {
    recipe[&format!("somemod:field_{seed}")].clone()
}

#[test]
fn a_missing_type_is_reported_by_the_load_and_the_file_is_still_scannable() {
    let pack = PackBuilder::new("untyped")
        .resource(
            "minecraft",
            "recipe",
            "mystery",
            r#"{"ingredients":[{"item":"minecraft:clay"}]}"#,
        )
        .build();

    let data = load(&[&pack as &dyn PackSource], &LoadOptions::default());
    // Exactly one finding, naming the file and the resource.
    assert_eq!(data.findings().len(), 1, "{:?}", data.findings());
    assert!(
        data.findings()[0].message.contains("no string `type`"),
        "{}",
        data.findings()[0]
    );
    assert!(data.findings()[0].file.ends_with("mystery.json"));

    // Scan does not double-report; it just hands back what it found.
    let report = ShapeReport::scan(&data);
    let recipe = report
        .recipes()
        .get(&location("minecraft:mystery"))
        .expect("held");
    assert_eq!(recipe.kind, "");
    assert!(recipe.raw.get("ingredients").is_some());

    // And the registry map still shows the resource, because a warning never
    // means silence about where things went.
    assert!(data
        .registry(&RegistryId::new("recipe"))
        .expect("registry")
        .contains_key(&location("minecraft:mystery")));
}
