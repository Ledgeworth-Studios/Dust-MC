//! What a furnace costs per tick, and what a thousand cost.
//!
//! `net/furnaces.rs` makes one resource claim and it is the reason a world can
//! hold as many furnaces as somebody cares to build: **a furnace with nothing
//! to do is not in the tick set at all**. Not a bounds check, not a branch — a
//! row in a map the tick never walks. These rows are that claim, measured.
//!
//! No framework, for the reason `dust-nbt`'s benches give: a fixed workload
//! timed by hand answers "how fast" without adding a dozen dependencies.
//!
//! Each group is the one above it plus a single named change, because a
//! furnace has two costs that differ by everything:
//!
//! ```text
//!   idle      furnaces that exist and are not burning — the ordinary state
//!             of every furnace in a village
//!   burning   furnaces with fuel and something to smelt, which is what a
//!             player is doing right now
//! ```
//!
//! **A bench that only measured burning furnaces would measure the wrong
//! thing**, because the number that decides whether a world can hold ten
//! thousand of them is what the other 9,990 cost. And a bench that only
//! measured idle ones would report a tick loop that does nothing, which is
//! true and useless. Both are here, at the same counts, so the difference is
//! the answer.
//!
//! ```text
//!   cargo bench -p dust-server --bench furnaces
//! ```
//!
//! Run it on an idle machine. Three other agents building at once is the
//! difference between these numbers and numbers twice their size.

use std::time::Instant;

use dust_protocol::types::Position;
use dust_registry::placement::ItemBlocks;
use dust_registry::{Block, Item};
use dust_server::net::furnaces::{Furnace, Furnaces, FUEL, INPUT};
use dust_server::net::inventory::Stack;
use dust_sim::cooking::{Cooking, Fire};

/// Ticks per row. Twenty is a second of game time; a thousand is fifty.
const TICKS: u32 = 1_000;
/// Rounds, of which the median is reported.
const ROUNDS: usize = 5;

/// A fuel table with one fuel in it.
///
/// Written here rather than read from `dust-items.tsv`: that file is Mojang's,
/// arrives from the operator's jar and is not in this repository. What is being
/// timed is the machine, and the machine does not care what the number is.
fn fuel_table() -> ItemBlocks {
    let mut text = String::from("# item_id\titem\tplaces\tburn\n");
    for item in Item::all() {
        let places = Block::from_name(item.name()).map_or("-", Block::name);
        let burn = if item.name() == "minecraft:coal" {
            // Longer than the whole run, so no row spends its time measuring a
            // furnace that has gone out.
            "60000"
        } else {
            "-"
        };
        text.push_str(&format!(
            "{}\t{}\t{places}\t{burn}\n",
            item.protocol_id(),
            item.name()
        ));
    }
    ItemBlocks::parse(&text).expect("a complete table")
}

fn smelting() -> Cooking {
    let mut cooking = Cooking::new();
    cooking
        .add(
            &serde_json::json!({
                "type": "minecraft:smelting",
                "cookingtime": 200,
                "experience": 0.7,
                "ingredient": { "item": "minecraft:raw_iron" },
                "result": { "id": "minecraft:iron_ingot" }
            }),
            &dust_sim::crafting::ItemTags::new(),
        )
        .expect("compiles");
    cooking
}

fn item(name: &str) -> Item {
    Item::from_name(name).expect("this build has it")
}

/// A world of `idle` furnaces that will never do anything and `burning` ones
/// that will burn for the whole run.
fn world(idle: usize, burning: usize, cooking: &Cooking, fuel: &ItemBlocks) -> Furnaces {
    let furnaces = Furnaces::new();
    let mut at = 0i32;
    for _ in 0..idle {
        // Fuel and no input: a furnace a player has loaded half of and left,
        // which is the commonest thing a furnace is and which must cost
        // nothing.
        furnaces.with(
            Position::new(at, 64, 0),
            Fire::Furnace,
            Some(cooking),
            Some(fuel),
            |furnace: &mut Furnace| {
                furnace.slots[FUEL] = Some(Stack::new(item("minecraft:coal"), 64));
            },
        );
        at += 1;
    }
    for _ in 0..burning {
        furnaces.with(
            Position::new(at, 64, 0),
            Fire::Furnace,
            Some(cooking),
            Some(fuel),
            |furnace: &mut Furnace| {
                furnace.slots[FUEL] = Some(Stack::new(item("minecraft:coal"), 64));
                furnace.slots[INPUT] = Some(Stack::new(item("minecraft:raw_iron"), 64));
            },
        );
        at += 1;
    }
    furnaces
}

fn row(label: &str, idle: usize, burning: usize, cooking: &Cooking, fuel: &ItemBlocks) {
    let mut nanos = Vec::with_capacity(ROUNDS);
    let mut active = 0;
    let mut lit = Vec::new();
    for _ in 0..ROUNDS {
        let furnaces = world(idle, burning, cooking, fuel);
        active = furnaces.active();
        let started = Instant::now();
        for _ in 0..TICKS {
            furnaces.tick(Some(cooking), Some(fuel), &mut lit);
        }
        nanos.push(started.elapsed().as_nanos() / u128::from(TICKS));
    }
    nanos.sort_unstable();
    let median = nanos[ROUNDS / 2];
    // Fifty milliseconds is a tick. The percentage is the number that decides
    // whether this scales, and it is printed rather than left to be worked out.
    #[allow(clippy::cast_precision_loss)]
    let share = median as f64 / 50_000_000.0 * 100.0;
    println!("  {label:<44} {median:>10} ns/tick  {share:>7.4}% of a tick  ({active} ticking)");
}

fn main() {
    let cooking = smelting();
    let fuel = fuel_table();
    println!("median of {ROUNDS} rounds of {TICKS} ticks\n");
    println!("  idle — furnaces that exist and have nothing to do");
    for count in [0usize, 100, 1_000, 10_000] {
        row(&format!("{count} idle"), count, 0, &cooking, &fuel);
    }
    println!("\n  burning — fuel and something to smelt");
    for count in [1usize, 10, 100, 1_000] {
        row(&format!("{count} burning"), 0, count, &cooking, &fuel);
    }
    println!("\n  both — a world where almost nothing is happening in it");
    row("10,000 idle and 10 burning", 10_000, 10, &cooking, &fuel);
}
