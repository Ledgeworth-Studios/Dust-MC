//! What a broken block yields.
//!
//! Two halves, and they answer different questions. The first is a set of
//! **written-here** tables: small files in the loot language, none of them
//! copied from Minecraft, each exercising one construct so that a rule which
//! breaks announces which rule it was. The second reads the **operator's real
//! tables** if this machine has any, and reports what the compiler made of all
//! of them — the only thing that can say the language was read whole.
//!
//! The second half asserts nothing about what any block drops, for the reason
//! decision record 0008 gives about `dust-constants.tsv`: the data is the
//! operator's, it is not committed, and a test that failed without it would be
//! a test about this machine. What it does assert needs no Mojang value to be
//! true — that every construct in every file was read rather than refused.

use std::path::{Path, PathBuf};

use dust_registry::{Block, BlockState, Item};
use dust_sim::drops::{self, Break, Drop, Rng, Tables, Tool};

fn item(name: &str) -> Item {
    Item::from_name(name).expect("a 1.21.1 item")
}

fn state(name: &str) -> BlockState {
    Block::from_name(name)
        .expect("a 1.21.1 block")
        .default_state()
}

fn plain(state: BlockState) -> Break<'static> {
    Break {
        state,
        tool: Tool::default(),
        broken_by_entity: true,
        requires_tool: false,
        neighbours: &[],
    }
}

fn roll(json: &str, ctx: &Break<'_>, seed: u64) -> Vec<Drop> {
    let table = drops::compile(json).expect("a table this test wrote");
    assert_eq!(table.refused(), 0, "the test's own table was refused");
    let mut out = Vec::new();
    table.roll(ctx, &mut Rng::from_seed(seed), &mut out);
    out
}

/// The rule a player feels in the first ten seconds of a survival world.
///
/// The block requires a correct tool, the hand is empty, and nothing comes
/// out. The same break with a pickaxe gives the cobblestone — so the check has
/// both halves, and a gate that refused everything would fail the second one.
#[test]
fn a_block_that_wants_a_tool_gives_a_bare_hand_nothing() {
    let json = r#"{"type":"minecraft:block","pools":[{"rolls":1.0,"bonus_rolls":0.0,
      "entries":[{"type":"minecraft:item","name":"minecraft:cobblestone"}]}]}"#;
    let mut ctx = plain(state("minecraft:stone"));
    ctx.requires_tool = true;
    assert_eq!(roll(json, &ctx, 1), vec![]);
    ctx.tool = Tool {
        item: Some(item("minecraft:wooden_pickaxe")),
        enchantments: &[],
    };
    assert_eq!(
        roll(json, &ctx, 1),
        vec![Drop {
            item: item("minecraft:cobblestone"),
            count: 1,
        }]
    );
}

/// The tier, which is the half of the rule a shovel cannot show. A wooden
/// pickaxe mines diamond ore faster than a hand and gets nothing for it; an
/// iron one gets the diamond.
#[test]
fn a_tool_below_the_tier_breaks_the_block_and_yields_nothing() {
    let json = r#"{"type":"minecraft:block","pools":[{"rolls":1.0,"bonus_rolls":0.0,
      "entries":[{"type":"minecraft:item","name":"minecraft:diamond"}]}]}"#;
    let mut ctx = plain(state("minecraft:diamond_ore"));
    ctx.requires_tool = true;
    ctx.tool = Tool {
        item: Some(item("minecraft:wooden_pickaxe")),
        enchantments: &[],
    };
    assert_eq!(roll(json, &ctx, 1), vec![]);
    ctx.tool = Tool {
        item: Some(item("minecraft:iron_pickaxe")),
        enchantments: &[],
    };
    assert_eq!(roll(json, &ctx, 1).len(), 1);
}

/// A block that does not ask is never refused, whatever is in the hand. The
/// direction that would otherwise go wrong quietly: dirt with a bare hand is
/// most of what a new player breaks.
#[test]
fn a_block_that_does_not_ask_yields_to_anything() {
    let json = r#"{"type":"minecraft:block","pools":[{"rolls":1.0,"bonus_rolls":0.0,
      "entries":[{"type":"minecraft:item","name":"minecraft:dirt"}]}]}"#;
    let ctx = plain(state("minecraft:dirt"));
    assert_eq!(roll(json, &ctx, 1).len(), 1);
}

/// One file, two blocks. `minecraft:oak_wall_sign` draws from
/// `blocks/oak_sign.json`, which is a fact about `Block.getLootTable` and not
/// about either name; what this pins is that `Tables` can hold it.
#[test]
fn one_table_can_serve_several_blocks() {
    let json = r#"{"type":"minecraft:block","pools":[{"rolls":1.0,"bonus_rolls":0.0,
      "entries":[{"type":"minecraft:item","name":"minecraft:oak_sign"}]}]}"#;
    let sign = Block::from_name("minecraft:oak_sign").expect("a 1.21.1 block");
    let wall = Block::from_name("minecraft:oak_wall_sign").expect("a 1.21.1 block");
    let mut tables = Tables::default();
    tables
        .insert_for(&[sign, wall], json)
        .expect("a table this test wrote");
    assert_eq!(tables.len(), 2, "two blocks covered");
    assert_eq!(tables.distinct(), 1, "out of one compiled table");
    for block in [sign, wall] {
        let mut out = Vec::new();
        tables.table(block).expect("a table").roll(
            &plain(block.default_state()),
            &mut Rng::from_seed(7),
            &mut out,
        );
        assert_eq!(
            out,
            vec![Drop {
                item: item("minecraft:oak_sign"),
                count: 1,
            }],
            "{}",
            block.name()
        );
    }
}

/// One pool, one item entry, no conditions: the simplest table there is.
#[test]
fn one_item_drops_once() {
    let json = r#"{"type":"minecraft:block","pools":[{"rolls":1.0,"bonus_rolls":0.0,
      "entries":[{"type":"minecraft:item","name":"minecraft:cobblestone"}]}]}"#;
    let out = roll(json, &plain(state("minecraft:stone")), 1);
    assert_eq!(
        out,
        vec![Drop {
            item: item("minecraft:cobblestone"),
            count: 1,
        }]
    );
}

/// `alternatives` stops at the first child that passes. Two children whose
/// conditions both pass must yield one drop and not two, which is the property
/// that makes a silk-touch branch *replace* a drop rather than add to it.
#[test]
fn alternatives_take_the_first_that_passes() {
    let json = r#"{"type":"minecraft:block","pools":[{"rolls":1.0,"bonus_rolls":0.0,
      "entries":[{"type":"minecraft:alternatives","children":[
        {"type":"minecraft:item","name":"minecraft:diamond"},
        {"type":"minecraft:item","name":"minecraft:cobblestone"}]}]}]}"#;
    let out = roll(json, &plain(state("minecraft:stone")), 1);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].item, item("minecraft:diamond"));
}

/// A silk-touch branch fires for a silk-touched tool and not for a bare hand.
/// Both questions are asked of one table, because a check that only ever asked
/// one of them would pass on a rule that always answered it.
#[test]
fn silk_touch_changes_which_branch_wins() {
    let json = r#"{"type":"minecraft:block","pools":[{"rolls":1.0,"bonus_rolls":0.0,
      "entries":[{"type":"minecraft:alternatives","children":[
        {"type":"minecraft:item","name":"minecraft:glass","conditions":[
          {"condition":"minecraft:match_tool","predicate":{"predicates":{
            "minecraft:enchantments":[{"enchantments":"minecraft:silk_touch",
            "levels":{"min":1}}]}}}]},
        {"type":"minecraft:item","name":"minecraft:glass_bottle"}]}]}]}"#;
    let bare = roll(json, &plain(state("minecraft:glass")), 7);
    assert_eq!(bare[0].item, item("minecraft:glass_bottle"));

    let silked = Break {
        tool: Tool {
            item: Some(item("minecraft:diamond_pickaxe")),
            enchantments: &[("minecraft:silk_touch", 1)],
        },
        ..plain(state("minecraft:glass"))
    };
    assert_eq!(roll(json, &silked, 7)[0].item, item("minecraft:glass"));
}

/// A `block_state_property` condition reads the state that was broken, so one
/// table gives two answers for two states of one block. This is the shape of
/// relation a name-matching rule cannot express, and the sharp case has a name
/// in this project already: `minecraft:wheat`.
#[test]
fn the_state_decides_which_entry_passes() {
    let json = r#"{"type":"minecraft:block","pools":[{"rolls":1.0,"bonus_rolls":0.0,
      "entries":[{"type":"minecraft:alternatives","children":[
        {"type":"minecraft:item","name":"minecraft:wheat","conditions":[
          {"condition":"minecraft:block_state_property","block":"minecraft:wheat",
           "properties":{"age":"7"}}]},
        {"type":"minecraft:item","name":"minecraft:wheat_seeds"}]}]}]}"#;
    let young = state("minecraft:wheat");
    assert_eq!(young.property("age"), Some("0"));
    assert_eq!(
        roll(json, &plain(young), 3)[0].item,
        item("minecraft:wheat_seeds")
    );

    let grown = young.with("age", "7").expect("wheat has an age of 7");
    assert_eq!(
        roll(json, &plain(grown), 3)[0].item,
        item("minecraft:wheat")
    );
}

/// `set_count` replaces the count, and with `add` it adds to it.
#[test]
fn set_count_replaces_or_adds() {
    let replace = r#"{"type":"minecraft:block","pools":[{"rolls":1.0,"bonus_rolls":0.0,
      "entries":[{"type":"minecraft:item","name":"minecraft:snowball","functions":[
        {"function":"minecraft:set_count","count":4.0,"add":false}]}]}]}"#;
    assert_eq!(
        roll(replace, &plain(state("minecraft:snow")), 2)[0].count,
        4
    );

    let add = replace.replace("\"add\":false", "\"add\":true");
    assert_eq!(roll(&add, &plain(state("minecraft:snow")), 2)[0].count, 5);
}

/// A uniform count stays inside its range over many rolls **and covers it**.
/// A stand-in that always answered the minimum would satisfy the range on its
/// own, so the range alone is not the check.
#[test]
fn a_uniform_count_covers_its_range() {
    let json = r#"{"type":"minecraft:block","pools":[{"rolls":1.0,"bonus_rolls":0.0,
      "entries":[{"type":"minecraft:item","name":"minecraft:lapis_lazuli","functions":[
        {"function":"minecraft:set_count","count":
         {"type":"minecraft:uniform","min":4.0,"max":9.0}}]}]}]}"#;
    let table = drops::compile(json).expect("a table this test wrote");
    let mut rng = Rng::from_seed(0xd0d0);
    let mut seen = [false; 10];
    for _ in 0..2_000 {
        let mut out = Vec::new();
        table.roll(&plain(state("minecraft:lapis_ore")), &mut rng, &mut out);
        let count = out[0].count as usize;
        assert!((4..=9).contains(&count), "rolled {count}, outside 4..=9");
        seen[count] = true;
    }
    assert!(
        seen[4..=9].iter().all(|hit| *hit),
        "some of 4..=9 never came up"
    );
}

/// Fortune's ore formula multiplies, and at level zero it does nothing at all.
#[test]
fn fortune_multiplies_an_ore_and_zero_fortune_does_not() {
    let json = r#"{"type":"minecraft:block","pools":[{"rolls":1.0,"bonus_rolls":0.0,
      "entries":[{"type":"minecraft:item","name":"minecraft:diamond","functions":[
        {"function":"minecraft:apply_bonus","enchantment":"minecraft:fortune",
         "formula":"minecraft:ore_drops"}]}]}]}"#;
    let table = drops::compile(json).expect("a table this test wrote");
    let mut rng = Rng::from_seed(11);
    let ore = state("minecraft:diamond_ore");

    for _ in 0..500 {
        let mut out = Vec::new();
        table.roll(&plain(ore), &mut rng, &mut out);
        assert_eq!(out[0].count, 1, "fortune 0 changed an ore drop");
    }

    let fortuned = Break {
        tool: Tool {
            item: Some(item("minecraft:diamond_pickaxe")),
            enchantments: &[("minecraft:fortune", 3)],
        },
        ..plain(ore)
    };
    let mut best = 0;
    for _ in 0..500 {
        let mut out = Vec::new();
        table.roll(&fortuned, &mut rng, &mut out);
        assert!(
            (1..=4).contains(&out[0].count),
            "{} is not 1..=4",
            out[0].count
        );
        best = best.max(out[0].count);
    }
    assert_eq!(best, 4, "fortune 3 never reached four");
}

/// A condition nobody has heard of refuses its entry and says so. It must not
/// be read as false, which deletes a drop, nor as true, which invents one.
#[test]
fn an_unknown_condition_refuses_its_entry_rather_than_guessing() {
    let json = r#"{"type":"minecraft:block","pools":[{"rolls":1.0,"bonus_rolls":0.0,
      "entries":[{"type":"minecraft:item","name":"minecraft:cobblestone","conditions":[
        {"condition":"minecraft:phase_of_the_moon","full":true}]}]}]}"#;
    let table = drops::compile(json).expect("the file itself is readable");
    assert_eq!(table.refused(), 1);
    let mut out = Vec::new();
    table.roll(
        &plain(state("minecraft:stone")),
        &mut Rng::from_seed(1),
        &mut out,
    );
    assert!(out.is_empty());
}

/// A table of another kind is refused whole. A chest table read as a block
/// table would answer a question it was never asked.
#[test]
fn a_table_of_another_kind_is_refused() {
    assert!(drops::compile(r#"{"type":"minecraft:chest","pools":[]}"#).is_err());
}

/// A pool that rolls twice is refused whole rather than rolled once.
#[test]
fn a_pool_that_rolls_twice_is_refused() {
    let json = r#"{"type":"minecraft:block","pools":[{"rolls":2.0,"bonus_rolls":0.0,
      "entries":[{"type":"minecraft:item","name":"minecraft:cobblestone"}]}]}"#;
    assert!(drops::compile(json).is_err());
}

/// One seed and one table give one answer, twice — and a half chance does not
/// fire every time, which is what says the seed is being used at all.
#[test]
fn a_seed_decides_the_roll() {
    let json = r#"{"type":"minecraft:block","pools":[{"rolls":1.0,"bonus_rolls":0.0,
      "entries":[{"type":"minecraft:item","name":"minecraft:apple","conditions":[
        {"condition":"minecraft:random_chance","chance":0.5}]}]}]}"#;
    let table = drops::compile(json).expect("a table this test wrote");
    let leaves = plain(state("minecraft:oak_leaves"));
    let mut first = Vec::new();
    let mut second = Vec::new();
    for seed in 0..40 {
        table.roll(&leaves, &mut Rng::from_seed(seed), &mut first);
        table.roll(&leaves, &mut Rng::from_seed(seed), &mut second);
    }
    assert_eq!(first, second);
    assert!(
        !first.is_empty() && first.len() < 40,
        "a half chance fired {} times in 40",
        first.len()
    );
}

// ---------------------------------------------------------------------------
// The operator's own tables, if this machine has any.
// ---------------------------------------------------------------------------

/// Where the block tables are: `DUST_TEST_LOOT`, or the extract cache.
fn real_tables() -> Option<PathBuf> {
    if let Ok(set) = std::env::var("DUST_TEST_LOOT") {
        let path = PathBuf::from(set);
        return path.is_dir().then_some(path);
    }
    let cache = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.dust-extract/data-1.21.1/data/minecraft/loot_table/blocks");
    cache.is_dir().then_some(cache)
}

fn load(root: &Path) -> (Tables, Vec<String>, Vec<String>) {
    let mut tables = Tables::default();
    let mut unnamed = Vec::new();
    let mut errors = Vec::new();
    for file in std::fs::read_dir(root).expect("a readable directory") {
        let file = file.expect("a readable entry").path();
        if file.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let stem = file
            .file_stem()
            .and_then(|s| s.to_str())
            .expect("a utf-8 name")
            .to_owned();
        let Some(block) = drops::block_of_file("minecraft", &stem) else {
            unnamed.push(stem);
            tables.refuse();
            continue;
        };
        let text = std::fs::read_to_string(&file).expect("a readable file");
        if let Err(why) = tables.insert(block, &text) {
            errors.push(format!("{stem}: {why}"));
        }
    }
    (tables, unnamed, errors)
}

/// Compile every block table the operator's data holds, and refuse nothing.
///
/// The counts printed are facts about that data. The assertion is not: it is
/// that the compiler read every construct it was handed, which is the claim
/// the module documentation makes and the only one checkable without putting a
/// Minecraft value in this file.
#[test]
fn every_real_block_table_compiles() {
    let Some(root) = real_tables() else {
        println!(
            "no loot tables on this machine, so nothing was read. Run \
             `cargo xtask extract --version 1.21.1 --only loot`, or point \
             DUST_TEST_LOOT at a <[data] path>/minecraft/loot_table/blocks."
        );
        return;
    };
    let (tables, unnamed, errors) = load(&root);

    println!("  {} files, {} compiled", tables.files(), tables.len());
    println!("  {} entries refused", tables.refused_entries());
    println!(
        "  {} functions want a block entity",
        tables.needs_block_entity()
    );
    println!(
        "  {} of {} blocks have no table of their own name",
        drops::blocks_without_a_table(&tables).len(),
        Block::all().count()
    );

    assert!(
        errors.is_empty(),
        "tables that would not compile: {errors:?}"
    );
    assert!(
        unnamed.is_empty(),
        "files named after nothing in the block registry: {unnamed:?}"
    );
    assert_eq!(
        tables.refused_entries(),
        1,
        "the one refusal 1.21.1 is expected to have is decorated_pot's dynamic sherds entry"
    );
}

/// Roll every state of every block that has a table, once, with a bare hand.
/// A drop of nothing is a real answer; a panic is not.
#[test]
fn every_real_table_rolls() {
    let Some(root) = real_tables() else {
        return;
    };
    let (tables, _, _) = load(&root);

    let mut rng = Rng::from_seed(0x1_2111);
    let mut out = Vec::new();
    let (mut yielded, mut nothing) = (0u32, 0u32);
    for block in Block::all() {
        let Some(table) = tables.table(block) else {
            continue;
        };
        for state in block.states() {
            out.clear();
            table.roll(&plain(state), &mut rng, &mut out);
            if out.is_empty() {
                nothing += 1;
            } else {
                yielded += 1;
            }
        }
    }
    println!("  {yielded} states yielded something, {nothing} yielded nothing");
    assert!(yielded > 0, "not one state yielded anything");
}
