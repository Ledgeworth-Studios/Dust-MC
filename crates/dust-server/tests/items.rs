//! Items on the ground, without a socket.
//!
//! Everything a player feels about a drop is decided by
//! [`dust_server::net::items::ItemWorld`] and none of it needs a client: an
//! item pops out of a block, falls, settles, merges with its twin, is claimed
//! by somebody standing on it, and eventually goes away. Each of those is one
//! test here, and each was watched failing before it was believed — the merge
//! test with the merge pass removed, the settle test with the physics step
//! removed.

use std::sync::Arc;

use dust_protocol::types::Position;
use dust_registry::Item;
use dust_server::net::edits::EditedWorld;
use dust_server::net::items::{ItemChange, ItemWorld, LIFETIME_TICKS, PICKUP_DELAY_TICKS};
use dust_server::net::players::Roster;
use dust_server::net::source::Source;
use dust_server::net::world::{FlatWorld, Palette};

fn item(name: &str) -> Item {
    Item::from_name(name).expect("a 1.21.1 item")
}

/// A world with a floor. Nothing here reads a region file; the item physics
/// only ever asks what is solid, and a flat world answers.
fn world() -> Arc<EditedWorld> {
    let palette = Palette::resolve().expect("the generated block table");
    Arc::new(EditedWorld::new(Source::Flat(Box::new(FlatWorld::new(
        palette, 0, 64,
    )))))
}

fn drain(rx: &mut tokio::sync::broadcast::Receiver<ItemChange>) -> Vec<ItemChange> {
    let mut out = Vec::new();
    while let Ok(change) = rx.try_recv() {
        out.push(change);
    }
    out
}

fn tick(items: &ItemWorld, world: &EditedWorld, players: &[(f64, f64, f64)], times: u32) {
    let mut near = Vec::new();
    for _ in 0..times {
        items.tick(world, None, players, &mut near);
    }
}

/// A dropped item comes out of the block, not out of whoever broke it.
#[test]
fn an_item_pops_out_of_the_block_that_broke() {
    let items = ItemWorld::default();
    let roster = Roster::default();
    let mut changes = items.subscribe();
    items.pop(
        &roster,
        Position {
            x: 10,
            y: 70,
            z: -4,
        },
        item("minecraft:cobblestone"),
        1,
        7,
    );
    let announced = drain(&mut changes);
    assert_eq!(announced.len(), 1);
    let ItemChange::Spawned { x, y, z, vy, .. } = &announced[0] else {
        panic!("the first thing a drop announces is that it exists");
    };
    assert!((x - 10.5).abs() < 0.3, "{x} is not near the block's centre");
    assert!((z + 3.5).abs() < 0.3, "{z} is not near the block's centre");
    assert!((*y - 70.375).abs() < 0.01, "{y} is not inside the block");
    assert!(
        *vy > 0.0,
        "an item that does not pop does not read as a drop"
    );
}

/// It falls, it lands, and it says so once.
#[test]
fn an_item_falls_and_settles_once() {
    let world = world();
    let items = ItemWorld::default();
    let roster = Roster::default();
    items.pop(
        &roster,
        Position { x: 0, y: 70, z: 0 },
        item("minecraft:cobblestone"),
        1,
        3,
    );
    let mut changes = items.subscribe();
    // Three seconds. An item that has not come to rest in three seconds is
    // one a player is watching bounce, which is what a naive reading of
    // vanilla's `multiply(1, -0.5, 1)` produces — see `step`.
    tick(&items, &world, &[(0.0, 70.0, 0.0)], 60);
    let announced = drain(&mut changes);
    let settles = announced
        .iter()
        .filter(|change| matches!(change, ItemChange::Settled { .. }))
        .count();
    // **Exactly one**, and that is the wire-cost claim `net/items.rs` makes:
    // an item costs a spawn and one correction, whatever it does in between.
    // A server that streamed positions would have put sixty here.
    assert_eq!(settles, 1, "{announced:?}");
    assert_eq!(items.len(), 1, "the item went away instead of landing");
}

/// Nothing beyond the tick radius moves at all, however long the world runs.
#[test]
fn an_item_nobody_is_near_is_not_ticked() {
    let world = world();
    let items = ItemWorld::default();
    let roster = Roster::default();
    items.pop(
        &roster,
        Position { x: 0, y: 70, z: 0 },
        item("minecraft:cobblestone"),
        1,
        5,
    );
    // Twice the lifetime, with the only player a thousand blocks away.
    tick(
        &items,
        &world,
        &[(1000.0, 70.0, 1000.0)],
        LIFETIME_TICKS * 2,
    );
    assert_eq!(
        items.len(),
        1,
        "an item nobody is near aged out, so it was being ticked"
    );
    // And the moment somebody arrives it starts living again.
    tick(&items, &world, &[(0.0, 70.0, 0.0)], LIFETIME_TICKS + 1);
    assert_eq!(items.len(), 0, "an item beside a player never aged out");
}

/// Two of the same item lying together become one stack.
#[test]
fn two_of_the_same_item_merge() {
    let world = world();
    let items = ItemWorld::default();
    let roster = Roster::default();
    for seed in 0..2 {
        items.pop(
            &roster,
            Position { x: 0, y: 70, z: 0 },
            item("minecraft:cobblestone"),
            1,
            seed + 1,
        );
    }
    assert_eq!(items.len(), 2);
    tick(&items, &world, &[(0.0, 70.0, 0.0)], 100);
    assert_eq!(items.len(), 1, "two cobblestones in one cell stayed two");
}

/// Two *different* items lying together do not.
#[test]
fn two_different_items_do_not_merge() {
    let world = world();
    let items = ItemWorld::default();
    let roster = Roster::default();
    items.pop(
        &roster,
        Position { x: 0, y: 70, z: 0 },
        item("minecraft:cobblestone"),
        1,
        1,
    );
    items.pop(
        &roster,
        Position { x: 0, y: 70, z: 0 },
        item("minecraft:dirt"),
        1,
        2,
    );
    tick(&items, &world, &[(0.0, 70.0, 0.0)], 100);
    assert_eq!(items.len(), 2, "a cobblestone ate a dirt");
}

/// Standing on it takes it — but not before the delay, which is what stops a
/// drop being collected in the same instant it appears.
#[test]
fn walking_over_an_item_takes_it_but_not_at_once() {
    let world = world();
    let items = ItemWorld::default();
    let roster = Roster::default();
    items.pop(
        &roster,
        Position { x: 0, y: 70, z: 0 },
        item("minecraft:cobblestone"),
        1,
        9,
    );
    let mut taken = Vec::new();
    items.claim_near(1, (0.5, 70.0, 0.5), &mut taken);
    assert!(
        taken.is_empty(),
        "an item was taken before its pickup delay"
    );

    tick(
        &items,
        &world,
        &[(0.5, 70.0, 0.5)],
        u32::from(PICKUP_DELAY_TICKS) + 1,
    );
    items.claim_near(1, (0.5, 70.0, 0.5), &mut taken);
    assert_eq!(taken, vec![(item("minecraft:cobblestone"), 1)]);
    assert!(items.is_empty(), "the item was given away and kept");
}

/// A player standing well away from it does not.
#[test]
fn an_item_out_of_reach_is_not_taken() {
    let world = world();
    let items = ItemWorld::default();
    let roster = Roster::default();
    items.pop(
        &roster,
        Position { x: 0, y: 70, z: 0 },
        item("minecraft:cobblestone"),
        1,
        9,
    );
    tick(
        &items,
        &world,
        &[(0.5, 70.0, 0.5)],
        u32::from(PICKUP_DELAY_TICKS) + 1,
    );
    let mut taken = Vec::new();
    items.claim_near(1, (4.0, 70.0, 0.5), &mut taken);
    assert!(
        taken.is_empty(),
        "an item four blocks away flew to a player"
    );
}

/// Two players standing on one stack: one of them gets it, and only one.
#[test]
fn one_stack_reaches_one_player() {
    let world = world();
    let items = ItemWorld::default();
    let roster = Roster::default();
    items.pop(
        &roster,
        Position { x: 0, y: 70, z: 0 },
        item("minecraft:diamond"),
        1,
        4,
    );
    tick(
        &items,
        &world,
        &[(0.5, 70.0, 0.5)],
        u32::from(PICKUP_DELAY_TICKS) + 1,
    );
    let mut first = Vec::new();
    let mut second = Vec::new();
    items.claim_near(1, (0.5, 70.0, 0.5), &mut first);
    items.claim_near(2, (0.5, 70.0, 0.5), &mut second);
    assert_eq!(first.len(), 1);
    assert!(second.is_empty(), "one diamond was given to two players");
}
