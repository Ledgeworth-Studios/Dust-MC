//! The reader for the oracle's item-to-block table.
//!
//! Read these the way `tests/constants.rs` asks to be read: they are
//! self-consistency checks over a file format, and they say nothing about
//! whether `minecraft:wheat_seeds` really places `minecraft:wheat`. Nothing in
//! this repository holds Mojang's answer to compare against — that is decision
//! record 0008 and not an omission here. What they check is that the parser
//! refuses what it claims to refuse, and in particular that it refuses the
//! failure with no error of its own: a table from a different version of
//! Minecraft, where every row is well-formed and every id means something else.

use dust_registry::placement::{ItemBlocks, PlacementError};
use dust_registry::{Block, Item};

/// A complete table where every item places the block of its own name if there
/// is one, and nothing otherwise.
///
/// Arbitrary, and deliberately *not* what Minecraft says: the sixteen items
/// the two disagree about are the point of the real table, and a fixture that
/// reproduced them would be Mojang's data in this repository.
fn full() -> String {
    let mut text = String::from("# item_id\titem\tplaces\n");
    for item in Item::all() {
        let places = Block::from_name(item.name()).map_or("-", Block::name);
        text.push_str(&format!(
            "{}\t{}\t{places}\n",
            item.protocol_id(),
            item.name()
        ));
    }
    text
}

/// The row [`full`] writes for `minecraft:stone`, which is item 1 and a block.
fn stone_row() -> String {
    let stone = Item::from_name("minecraft:stone").expect("this build has stone");
    format!(
        "{}\t{}\tminecraft:stone\n",
        stone.protocol_id(),
        stone.name()
    )
}

#[test]
fn the_row_the_tests_below_corrupt_is_the_row_they_think_it_is() {
    assert!(full().contains(&stone_row()));
}

#[test]
fn a_complete_table_reads_back_every_item() {
    let table = ItemBlocks::parse(&full()).expect("a complete table");
    assert_eq!(table.len(), Item::all().count());
    assert!(!table.is_empty());

    let stone = Item::from_name("minecraft:stone").expect("this build has stone");
    assert_eq!(
        table.places(stone).map(Block::name),
        Some("minecraft:stone")
    );
    let sword = Item::from_name("minecraft:diamond_sword").expect("this build has one");
    assert_eq!(table.places(sword), None, "a sword places nothing");

    // Most items place nothing, so a table where everything did would be one
    // whose `places` column was read from somewhere else.
    assert!(table.placing() > 0 && table.placing() < table.len());
}

#[test]
fn a_dash_is_an_item_that_places_nothing_and_not_a_block_called_dash() {
    let table = ItemBlocks::parse(&full()).expect("a complete table");
    let stick = Item::from_name("minecraft:stick").expect("this build has one");
    assert_eq!(table.places(stick), None);
}

#[test]
fn an_item_this_build_has_no_entry_for_names_the_version_skew() {
    let mut text = full();
    let beyond = Item::all().count() as u32;
    text.push_str(&format!("{beyond}\tminecraft:future_thing\t-\n"));
    let error = ItemBlocks::parse(&text).expect_err("an item beyond the table");
    assert!(
        matches!(error, PlacementError::UnknownItem { id, .. } if id == beyond),
        "{error}"
    );
    assert!(
        error.to_string().contains("different Minecraft version"),
        "{error}"
    );
}

#[test]
fn an_id_that_names_a_different_item_here_is_caught_on_its_own_row() {
    // The check the light table cannot make. A version that renumbered one
    // item leaves the row count identical and every field well-formed, and the
    // only thing that disagrees is the name the file carries beside the id.
    let text = full().replacen(&stone_row(), "1\tminecraft:granite\tminecraft:stone\n", 1);
    let error = ItemBlocks::parse(&text).expect_err("an id under the wrong name");
    assert!(
        matches!(&error, PlacementError::Renamed { table, .. } if table == "minecraft:granite"),
        "{error}"
    );
    let message = error.to_string();
    assert!(message.contains("minecraft:granite"), "{message}");
    assert!(message.contains("minecraft:stone"), "{message}");
}

#[test]
fn a_block_this_build_has_no_entry_for_is_refused_rather_than_dropped() {
    // Refused and not skipped: an item that silently placed nothing would be a
    // block a player cannot put down, on a server that says nothing about why.
    let text = full().replacen(&stone_row(), "1\tminecraft:stone\tminecraft:dust\n", 1);
    let error = ItemBlocks::parse(&text).expect_err("a block that is not one");
    assert!(
        matches!(&error, PlacementError::UnknownBlock { name, .. } if name == "minecraft:dust"),
        "{error}"
    );
}

#[test]
fn a_missing_item_is_refused_rather_than_defaulted() {
    let text = full()
        .lines()
        .filter(|line| !line.starts_with("1\t"))
        .collect::<Vec<_>>()
        .join("\n");
    let expected = Item::all().count();
    assert_eq!(
        ItemBlocks::parse(&text),
        Err(PlacementError::Incomplete {
            present: expected - 1,
            expected,
        })
    );
}

#[test]
fn one_item_described_twice_is_two_answers_to_one_question() {
    let text = full() + &stone_row();
    assert!(matches!(
        ItemBlocks::parse(&text),
        Err(PlacementError::DuplicateItem { id: 1, .. })
    ));
}

#[test]
fn columns_are_read_by_name_and_not_by_position() {
    let mut text = String::from("# places\titem_id\titem\n");
    for item in Item::all() {
        let places = Block::from_name(item.name()).map_or("-", Block::name);
        text.push_str(&format!(
            "{places}\t{}\t{}\n",
            item.protocol_id(),
            item.name()
        ));
    }
    let table = ItemBlocks::parse(&text).expect("order is not meaning");
    let stone = Item::from_name("minecraft:stone").expect("this build has stone");
    assert_eq!(
        table.places(stone).map(Block::name),
        Some("minecraft:stone")
    );
}

#[test]
fn a_file_with_no_header_is_not_a_table_this_reader_can_read() {
    assert_eq!(ItemBlocks::parse(""), Err(PlacementError::NoHeader));
    assert_eq!(
        ItemBlocks::parse("0\tminecraft:air\t-\n"),
        Err(PlacementError::NoHeader)
    );
}

#[test]
fn a_header_missing_a_column_the_reader_needs_names_it() {
    let text = full().replacen("places", "palces", 1);
    let error = ItemBlocks::parse(&text).expect_err("a misspelled column");
    assert!(
        matches!(
            error,
            PlacementError::MissingColumn {
                column: "places",
                ..
            }
        ),
        "{error}"
    );
}

#[test]
fn a_row_that_is_not_as_wide_as_the_header_is_refused() {
    let text = full().replacen(&stone_row(), "1\tminecraft:stone\n", 1);
    let error = ItemBlocks::parse(&text).expect_err("a row one field short");
    assert!(matches!(error, PlacementError::Malformed { .. }), "{error}");
}
