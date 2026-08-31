//! The reader for the light oracle's table.
//!
//! These tests are self-consistency checks over a file format and they should
//! be read as ones: they say the parser refuses what it claims to refuse, and
//! they say nothing at all about whether the *numbers* Minecraft gives a block
//! are what this table holds. Nothing in a test suite can say that, because
//! nothing in this repository holds Mojang's numbers to compare against —
//! which is decision record 0008 and not an omission here. What checks the
//! values is `cargo xtask harness light`, against a world Minecraft lit itself.
//!
//! What they *can* check, and the reason the parser is strict, is the failure
//! that produces no error of its own: a table extracted from a different
//! version of Minecraft, where every row is well-formed and every state id
//! means a different block than it does in this build.

use dust_registry::light::{LightTable, LightTableError};
use dust_registry::STATE_COUNT;

/// A complete table where state `n` has opacity `n % 16`, emits `(n * 7) % 16`
/// and occludes on even ids. Arbitrary, and the point is that it is *complete*:
/// the parser refuses anything else, so every test that is not about
/// incompleteness has to start from a whole one.
fn full() -> String {
    let mut text = String::from("# state_id\topacity\temission\tocclude\n");
    for state in 0..STATE_COUNT {
        let opacity = state % 16;
        let emission = (state * 7) % 16;
        let occlude = u32::from(state % 2 == 0);
        text.push_str(&format!("{state}\t{opacity}\t{emission}\t{occlude}\n"));
    }
    text
}

#[test]
fn a_complete_table_reads_back_every_state() {
    let table = LightTable::parse(&full()).expect("a complete table");
    assert_eq!(table.len(), STATE_COUNT as usize);
    assert!(!table.is_empty());
    for state in [0, 1, 15, 16, 1234, STATE_COUNT - 1] {
        assert_eq!(table.opacity(state), (state % 16) as u8, "opacity {state}");
        assert_eq!(
            table.emission(state),
            ((state * 7) % 16) as u8,
            "emission {state}"
        );
        assert_eq!(table.occludes(state), state % 2 == 0, "occlude {state}");
    }
}

#[test]
fn a_state_the_table_does_not_reach_is_a_wall_that_emits_nothing() {
    // The direction matters and is argued at `LightTable::opacity`: an unknown
    // id under-lights, which is where every other known gap already errs.
    let table = LightTable::parse(&full()).expect("a complete table");
    assert_eq!(table.opacity(STATE_COUNT), 15);
    assert_eq!(table.emission(STATE_COUNT), 0);
    assert!(table.occludes(STATE_COUNT));
}

#[test]
fn emitting_counts_the_states_that_give_off_anything() {
    let table = LightTable::parse(&full()).expect("a complete table");
    let expected = (0..STATE_COUNT).filter(|s| (s * 7) % 16 > 0).count();
    assert_eq!(table.emitting(), expected);
}

#[test]
fn comments_and_blank_lines_are_not_rows() {
    let mut text = full();
    text.push_str("\n# a trailing note\n\n");
    let table = LightTable::parse(&text).expect("a complete table with noise in it");
    assert_eq!(table.len(), STATE_COUNT as usize);
}

#[test]
fn a_table_written_before_the_occlude_column_reads_as_all_occluding() {
    let mut text = String::from("# state_id\topacity\temission\n");
    for state in 0..STATE_COUNT {
        text.push_str(&format!("{state}\t0\t0\n"));
    }
    let table = LightTable::parse(&text).expect("three columns is a table too");
    assert!(table.occludes(0));
    assert!(table.occludes(STATE_COUNT - 1));
}

#[test]
fn a_missing_state_is_refused_rather_than_defaulted() {
    // The version-skew case seen from the low end. A defaulted state is a
    // block that lights wrongly and says nothing.
    let text = full()
        .lines()
        .filter(|l| !l.starts_with("1234\t"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        LightTable::parse(&text),
        Err(LightTableError::Incomplete {
            present: STATE_COUNT as usize - 1,
            expected: STATE_COUNT as usize,
        })
    );
}

#[test]
fn a_state_this_build_has_no_block_for_names_the_version_skew() {
    let mut text = full();
    text.push_str(&format!("{}\t0\t0\t0\n", STATE_COUNT));
    let error = LightTable::parse(&text).expect_err("a state beyond the table");
    assert!(
        matches!(
            error,
            LightTableError::UnknownState { state, states, .. }
                if state == STATE_COUNT && states == STATE_COUNT
        ),
        "{error}"
    );
    assert!(
        error.to_string().contains("different Minecraft version"),
        "the message has to say what it actually means: {error}"
    );
}

#[test]
fn one_state_described_twice_is_two_answers_to_one_question() {
    let mut text = full();
    text.push_str("7\t0\t0\t0\n");
    assert!(matches!(
        LightTable::parse(&text),
        Err(LightTableError::DuplicateState { state: 7, .. })
    ));
}

#[test]
fn a_level_that_does_not_fit_in_a_nibble_is_the_wrong_java_member() {
    for (column, row) in [(2, "0\t16\t0\t0\n"), (3, "0\t0\t99\t0\n")] {
        let text = full().replacen("0\t0\t0\t1\n", row, 1);
        let error = LightTable::parse(&text).expect_err("a level above fifteen");
        assert!(
            matches!(error, LightTableError::OutOfRange { .. }),
            "column {column}: {error}"
        );
        assert!(
            error.to_string().contains("wrong member"),
            "the message has to point at the cause: {error}"
        );
    }
}

#[test]
fn a_row_that_is_not_numbers_names_the_field_and_the_line() {
    let text = full().replacen("0\t0\t0\t1\n", "0\tglass\t0\t1\n", 1);
    let error = LightTable::parse(&text).expect_err("a word where a number goes");
    let message = error.to_string();
    assert!(message.contains("opacity"), "{message}");
    assert!(message.contains("line 2"), "{message}");
}

#[test]
fn a_short_row_is_refused() {
    let text = full().replacen("0\t0\t0\t1\n", "0\t0\n", 1);
    assert!(matches!(
        LightTable::parse(&text),
        Err(LightTableError::Malformed { line: 2, .. })
    ));
}

#[test]
fn occlude_is_a_flag_and_not_a_number() {
    let text = full().replacen("0\t0\t0\t1\n", "0\t0\t0\t2\n", 1);
    let error = LightTable::parse(&text).expect_err("2 is not a boolean");
    assert!(error.to_string().contains("0 or 1"), "{error}");
}

#[test]
fn an_empty_table_is_incomplete_and_says_so() {
    assert_eq!(
        LightTable::parse(""),
        Err(LightTableError::Incomplete {
            present: 0,
            expected: STATE_COUNT as usize,
        })
    );
}
